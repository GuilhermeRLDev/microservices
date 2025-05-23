use std::process::{Command, Output};
use std::path::{Path, PathBuf};
use std::fs::{self, read, remove_file, File};
use std::env;
use std::time::Duration;
use std::error::Error as StdError;
use std::io::Write; 
use std::sync::Mutex;

// Kafka related imports
use rdkafka::producer::{FutureProducer, FutureRecord};
use rdkafka::config::ClientConfig;
use tokio::time::timeout;

// Logging imports
use log::{info, warn, error, debug};

// Actix-web imports
use actix_web::{web, App, HttpResponse, HttpServer, Responder, Error as ActixWebError};
use serde::Deserialize;

// Actix-multipart import
use actix_multipart::Multipart;
use futures_util::TryStreamExt; 

// UUID import
use uuid::Uuid;

// Test-specific static flags
#[cfg(test)]
static MOCK_DOWNLOAD_RESULT: Mutex<Result<PathBuf, String>> = Mutex::new(Ok(PathBuf::from("/tmp/mocked_video.mp3")));
#[cfg(test)]
static MOCK_KAFKA_RESULT: Mutex<Result<(), String>> = Mutex::new(Ok(()));
#[cfg(test)]
static CLEANUP_CALLED_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
#[cfg(test)]
static MOCK_UPLOAD_CLEANUP_PATH: Mutex<Option<PathBuf>> = Mutex::new(None); // New flag for upload cleanup


// AppState struct
#[derive(Clone)]
struct AppState {
    kafka_brokers: String,
    kafka_topic: String,
}

// Placeholder request structures
#[derive(Deserialize, Debug)]
struct YouTubeProcessRequest {
    youtube_url: String,
}

// Custom Error Enum
#[derive(Debug)]
enum AppError {
    Io(std::io::Error),
    Kafka(rdkafka::error::KafkaError),
    YtDlp(String), 
    KafkaSendTimeout,
    BlockingError(String),
    UploadError(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AppError::Io(err) => write!(f, "I/O error: {}", err),
            AppError::Kafka(err) => write!(f, "Kafka error: {}", err),
            AppError::YtDlp(msg) => write!(f, "yt-dlp execution error: {}", msg),
            AppError::KafkaSendTimeout => write!(f, "Kafka message send timed out"),
            AppError::BlockingError(msg) => write!(f, "Blocking task error: {}", msg),
            AppError::UploadError(msg) => write!(f, "File upload error: {}", msg),
        }
    }
}

impl StdError for AppError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            AppError::Io(err) => Some(err),
            AppError::Kafka(err) => Some(err),
            _ => None,
        }
    }
}

impl From<actix_web::error::BlockingError> for AppError {
    fn from(err: actix_web::error::BlockingError) -> Self {
        AppError::BlockingError(err.to_string())
    }
}


impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Io(err)
    }
}

impl From<rdkafka::error::KafkaError> for AppError {
    fn from(err: rdkafka::error::KafkaError) -> Self {
        AppError::Kafka(err)
    }
}

fn new_yt_dlp_error(msg: String) -> AppError {
    AppError::YtDlp(msg)
}


fn app_error_to_response(err: &AppError) -> HttpResponse {
    match err {
        AppError::Io(e) => {
            error!("Responding with I/O Error: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({"error": format!("I/O error: {}", e)}))
        }
        AppError::Kafka(e) => {
            error!("Responding with Kafka Error: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({"error": format!("Kafka error: {}", e)}))
        }
        AppError::YtDlp(msg) => {
            error!("Responding with YtDlp Error: {}", msg);
            HttpResponse::InternalServerError().json(serde_json::json!({"error": format!("Video processing error: {}", msg)}))
        }
        AppError::KafkaSendTimeout => {
            error!("Responding with Kafka Send Timeout");
            HttpResponse::InternalServerError().json(serde_json::json!({"error": "Kafka message send timed out"}))
        }
        AppError::BlockingError(msg) => {
            error!("Responding with Blocking Task Error: {}", msg);
            HttpResponse::InternalServerError().json(serde_json::json!({"error": format!("Internal server error (blocking task): {}", msg)}))
        }
        AppError::UploadError(msg) => {
            error!("Responding with Upload Error: {}", msg);
            HttpResponse::BadRequest().json(serde_json::json!({"error": format!("File upload error: {}", msg)}))
        }
    }
}

fn execute_yt_dlp_command(command: &mut std::process::Command) -> Result<std::process::Output, AppError> {
    debug!("Executing command: {:?}", command);
    let output = command.output().map_err(|e| new_yt_dlp_error(format!("Failed to execute yt-dlp command: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("yt-dlp command failed with status {}. Stderr: {}", output.status, stderr);
        return Err(new_yt_dlp_error(format!(
            "yt-dlp command failed with status {}: {}",
            output.status, stderr
        )));
    }
    Ok(output)
}


async fn process_youtube_url_handler(
    req: web::Json<YouTubeProcessRequest>,
    state: web::Data<AppState>,
) -> impl Responder {
    info!("Received request to process YouTube URL: {}", req.youtube_url);

    let youtube_url_clone = req.youtube_url.clone(); 

    let downloaded_path_result = web::block(move || download_video(&youtube_url_clone)).await;

    let downloaded_path = match downloaded_path_result {
        Ok(Ok(path)) => {
            info!("Video downloaded successfully to: {:?}", path);
            path
        }
        Ok(Err(app_err)) => { 
            error!("Download video failed: {}", app_err);
            return app_error_to_response(&app_err);
        }
        Err(blocking_err) => { 
            error!("Download video failed (blocking error): {}", blocking_err);
            return app_error_to_response(&AppError::from(blocking_err));
        }
    };

    info!("Sending downloaded audio {:?} to Kafka...", downloaded_path);
    match send_audio_to_kafka(&downloaded_path, &state.kafka_brokers, &state.kafka_topic).await {
        Ok(()) => {
            info!("Successfully sent audio to Kafka for URL: {}", req.youtube_url);
            
            let cleanup_path_for_mock = downloaded_path.clone();
            let remove_result = remove_file(&downloaded_path);
            #[cfg(test)]
            {
                *CLEANUP_CALLED_PATH.lock().unwrap() = Some(cleanup_path_for_mock);
            }
            if let Err(e) = remove_result {
                 warn!("Failed to delete temporary file {:?}: {}", downloaded_path, e);
            } else {
                info!("Successfully deleted temporary file: {:?}", downloaded_path);
            }

            HttpResponse::Ok().json(serde_json::json!({
                "status": "success",
                "message": "YouTube audio processed and sent to Kafka.",
                "original_url": req.youtube_url,
                "audio_file_path_on_server": downloaded_path 
            }))
        }
        Err(app_err) => {
            error!("Send to Kafka failed: {}", app_err);
            
            let cleanup_path_for_mock = downloaded_path.clone();
            let remove_result = remove_file(&downloaded_path);
            #[cfg(test)]
            {
                *CLEANUP_CALLED_PATH.lock().unwrap() = Some(cleanup_path_for_mock);
            }
            if let Err(e) = remove_result {
                 warn!("Failed to delete temporary file {:?} after Kafka send failure: {}", downloaded_path, e);
            } else {
                info!("Cleaned up temporary file {:?} after Kafka send failure.", downloaded_path);
            }
            app_error_to_response(&app_err)
        }
    }
}

async fn upload_audio_handler(
    mut multipart: Multipart,
    state: web::Data<AppState>,
) -> impl Responder {
    info!("Received request to upload audio.");
    let mut temp_file_path_option: Option<PathBuf> = None;

    while let Ok(Some(mut field)) = multipart.try_next().await {
        let content_disposition = field.content_disposition().clone();
        let filename = content_disposition.get_filename();
        
        if let Some(name) = filename {
            if name.ends_with(".mp3") { 
                let file_id = Uuid::new_v4(); 
                let path = std::env::temp_dir().join(format!("{}.mp3", file_id));
                info!("Attempting to save uploaded file to: {:?}", path);

                let mut f = match File::create(&path) {
                    Ok(file) => file,
                    Err(e) => {
                        error!("Failed to create temporary file for upload at {:?}: {}", path, e);
                        return app_error_to_response(&AppError::Io(e)); 
                    }
                };

                while let Ok(Some(chunk)) = field.try_next().await {
                    if let Err(e) = f.write_all(&chunk) {
                        error!("Failed to write chunk to temporary file {:?}: {}", path, e);
                        let path_to_remove = path.clone();
                        let remove_result = remove_file(&path);
                        #[cfg(test)]
                        {
                            *MOCK_UPLOAD_CLEANUP_PATH.lock().unwrap() = Some(path_to_remove);
                        }
                        if let Err(remove_err) = remove_result {
                             warn!("Failed to remove partially written temp file {:?}: {}", path, remove_err);
                        }
                        return app_error_to_response(&AppError::Io(e));
                    }
                }
                info!("Successfully saved uploaded file to: {:?}", path);
                temp_file_path_option = Some(path);
                break; 
            } else {
                warn!("Skipping non-MP3 file in upload: {}", name);
            }
        } else {
            warn!("Skipping field without a filename in upload.");
        }
    }

    if let Some(path_to_process) = temp_file_path_option { // Renamed for clarity
        let kafka_result = send_audio_to_kafka(&path_to_process, &state.kafka_brokers, &state.kafka_topic).await;
        
        let cleanup_path_for_mock = path_to_process.clone();
        let remove_result = remove_file(&path_to_process);
        #[cfg(test)]
        {
            *MOCK_UPLOAD_CLEANUP_PATH.lock().unwrap() = Some(cleanup_path_for_mock);
        }
        if let Err(e) = remove_result {
            warn!("Failed to remove temporary uploaded file {}: {}", path_to_process.display(), e);
        } else {
            info!("Successfully removed temporary uploaded file {}", path_to_process.display());
        }

        match kafka_result {
            Ok(_) => {
                info!("Successfully sent uploaded audio {} to Kafka.", path_to_process.display());
                HttpResponse::Ok().json(serde_json::json!({"status": "success", "message": "Audio uploaded and sent to Kafka."}))
            }
            Err(e) => {
                error!("Failed to send uploaded audio {} to Kafka: {}", path_to_process.display(), e);
                // File already attempted to be removed above
                app_error_to_response(&e)
            }
        }
    } else {
        warn!("No valid MP3 file found in upload or file processing error.");
        app_error_to_response(&AppError::UploadError("No MP3 file uploaded or file processing error.".to_string()))
    }
}


fn download_video(url: &str) -> Result<PathBuf, AppError> {
    #[cfg(test)]
    {
        let result_guard = MOCK_DOWNLOAD_RESULT.lock().unwrap();
        match result_guard.as_ref() {
            Ok(path) => {
                if !path.parent().unwrap_or_else(|| Path::new("/tmp")).exists() {
                     if path.parent().unwrap_or_else(|| Path::new("/tmp")) == Path::new("/tmp") {
                        std::fs::create_dir_all("/tmp").ok(); 
                    }
                }
                if !path.exists() { 
                    File::create(path).map_err(|e| AppError::Io(e))?; 
                }
                return Ok(path.clone());
            }
            Err(err_msg) => return Err(AppError::YtDlp(err_msg.clone())),
        }
    }
    let temp_dir = env::temp_dir().join("youtube_downloads");
    fs::create_dir_all(&temp_dir)?;
    let temp_dir_str = temp_dir.to_str().ok_or_else(|| new_yt_dlp_error("Temporary directory path is not valid UTF-8".into()))?;

    info!("(download_video) Fetching video ID for URL: {}", url);
    let mut cmd_id = Command::new("yt-dlp");
    cmd_id.arg("--get-id").arg(url);
    let id_output = execute_yt_dlp_command(&mut cmd_id)?;
    
    let video_id = String::from_utf8(id_output.stdout)
        .map_err(|e| new_yt_dlp_error(format!("Failed to parse yt-dlp --get-id output (not UTF-8): {}", e)))?
        .trim()
        .to_string();
    
    if video_id.is_empty() {
        return Err(new_yt_dlp_error("yt-dlp --get-id returned an empty string".to_string()));
    }
    info!("(download_video) Fetched video ID: {}", video_id);

    let output_template = format!("{}/%(id)s.%(ext)s", temp_dir_str);

    info!("(download_video) Fetching expected filename for URL: {}", url);
    let mut cmd_filename = Command::new("yt-dlp");
    cmd_filename.arg("--get-filename")
        .arg("-f").arg("bestaudio/best")
        .arg("--extract-audio").arg("--audio-format").arg("mp3")
        .arg("-o").arg(&output_template)
        .arg(url);
    let filename_output = execute_yt_dlp_command(&mut cmd_filename)?;

    let expected_filename_str = String::from_utf8(filename_output.stdout)
        .map_err(|e| new_yt_dlp_error(format!("Failed to parse yt-dlp --get-filename output (not UTF-8): {}",e)))?
        .trim()
        .to_string();

    if expected_filename_str.is_empty() {
        return Err(new_yt_dlp_error("yt-dlp --get-filename returned an empty string".to_string()));
    }
    let expected_path = PathBuf::from(expected_filename_str);
    info!("(download_video) Expected file path: {:?}", expected_path);

    info!("(download_video) Downloading audio for URL: {} to template {}", url, output_template);
    let mut cmd_download = Command::new("yt-dlp");
    cmd_download.arg("-f").arg("bestaudio/best")
        .arg("--extract-audio").arg("--audio-format").arg("mp3")
        .arg("-o").arg(&output_template)
        .arg(url);
    execute_yt_dlp_command(&mut cmd_download)?;
    
    info!("(download_video) Download command executed successfully. Verifying file exists at: {:?}", expected_path);

    if !expected_path.exists() {
        error!("(download_video) Downloaded file not found at expected path: {:?}", expected_path);
        return Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Downloaded file not found at expected path: {:?}", expected_path)
        )));
    }
    Ok(expected_path)
}

async fn send_audio_to_kafka(file_path: &Path, brokers: &str, topic: &str) -> Result<(), AppError> {
    #[cfg(test)]
    {
        let result_guard = MOCK_KAFKA_RESULT.lock().unwrap();
        if result_guard.is_err() { 
            return Err(AppError::Kafka(rdkafka::error::KafkaError::BrokerNotFound));
        }
        return Ok(());
    }
    info!("(send_audio_to_kafka) Reading audio file from: {:?}", file_path);
    let audio_data = read(file_path)?; 

    info!("(send_audio_to_kafka) Initializing Kafka producer with brokers: {}", brokers);
    let producer: FutureProducer = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("message.timeout.ms", "5000") 
        .create()?; 

    let file_name_key_string = file_path.file_name()
        .map_or_else(
            || "unknown_file".to_string(),
            |os_str| os_str.to_string_lossy().into_owned()
        );
    
    info!("(send_audio_to_kafka) Preparing to send audio data ({} bytes) from file '{}' to Kafka topic: '{}'", audio_data.len(), file_name_key_string, topic);
    
    let record = FutureRecord::to(topic)
        .payload(&audio_data) 
        .key(file_name_key_string.as_bytes());

    info!("(send_audio_to_kafka) Sending Kafka message...");
    match timeout(Duration::from_secs(5), producer.send(record, Duration::from_secs(0))).await {
        Ok(Ok(delivery_report)) => { 
            info!("(send_audio_to_kafka) Audio data successfully sent to Kafka topic: {}, partition: {}, offset: {}", topic, delivery_report.0, delivery_report.1);
            Ok(())
        }
        Ok(Err((kafka_error, _owned_message))) => { 
            error!("(send_audio_to_kafka) Failed to send audio data to Kafka (Kafka error): {}", kafka_error);
            Err(AppError::Kafka(kafka_error))
        }
        Err(_timeout_elapsed) => { 
            error!("(send_audio_to_kafka) Kafka message send timed out");
            Err(AppError::KafkaSendTimeout)
        }
    }
}


#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init(); 

    let app_state = web::Data::new(AppState {
        kafka_brokers: env::var("KAFKA_BROKERS").unwrap_or_else(|_| "localhost:9092".to_string()),
        kafka_topic: env::var("KAFKA_TOPIC").unwrap_or_else(|_| "youtube_audio".to_string()),
    });

    info!("Using Kafka brokers: {}", app_state.kafka_brokers);
    info!("Using Kafka topic: {}", app_state.kafka_topic);
    
    info!("Starting HTTP server on 127.0.0.1:8080");

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone()) 
            .route("/process_youtube_url", web::post().to(process_youtube_url_handler))
            .route("/upload_audio", web::post().to(upload_audio_handler))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;
    use actix_web::{test, http::{StatusCode, header::{CONTENT_TYPE, ContentDisposition, DispositionType, DispositionParam}}, App as ActixApp, web::Bytes};
    use serde_json::json;
    use mime; // Required for constructing Content-Type for multipart

    fn reset_mocks() {
        *MOCK_DOWNLOAD_RESULT.lock().unwrap() = Ok(PathBuf::from("/tmp/default_mock_video.mp3"));
        *MOCK_KAFKA_RESULT.lock().unwrap() = Ok(());
        *CLEANUP_CALLED_PATH.lock().unwrap() = None;
        *MOCK_UPLOAD_CLEANUP_PATH.lock().unwrap() = None; // Reset new mock
    }


    #[test]
    fn test_app_error_io_display_and_source() {
        let original_io_error = std::io::Error::new(ErrorKind::NotFound, "file not found for test");
        let original_io_error_str = original_io_error.to_string(); 
        let app_error = AppError::Io(original_io_error);

        assert_eq!(app_error.to_string(), format!("I/O error: {}", original_io_error_str));
        assert!(app_error.source().is_some());
        assert_eq!(app_error.source().unwrap().to_string(), original_io_error_str);
    }

    #[test]
    fn test_app_error_kafka_display_and_source() {
        let kafka_sdk_error = rdkafka::error::KafkaError::ClientCreation("Test Kafka client creation error".to_string());
        let kafka_sdk_error_str = kafka_sdk_error.to_string();
        let app_error = AppError::Kafka(kafka_sdk_error);
        
        assert_eq!(app_error.to_string(), format!("Kafka error: {}", kafka_sdk_error_str));
        assert!(app_error.source().is_some()); 
        assert_eq!(app_error.source().unwrap().to_string(), kafka_sdk_error_str);

    }

    #[test]
    fn test_app_error_yt_dlp_display_and_source() {
        let error_msg = "yt-dlp command execution failed badly".to_string();
        let app_error = AppError::YtDlp(error_msg.clone());

        assert_eq!(app_error.to_string(), format!("yt-dlp execution error: {}", error_msg));
        assert!(app_error.source().is_none());
    }

    #[test]
    fn test_app_error_kafka_send_timeout_display_and_source() {
        let app_error = AppError::KafkaSendTimeout;
        assert_eq!(app_error.to_string(), "Kafka message send timed out");
        assert!(app_error.source().is_none());
    }
    
    #[test]
    fn test_app_error_blocking_error_display_and_source() {
        let error_msg = "Task was cancelled".to_string();
        let app_error = AppError::BlockingError(error_msg.clone());
        assert_eq!(app_error.to_string(), format!("Blocking task error: {}", error_msg));
        assert!(app_error.source().is_none());
    }

    #[test]
    fn test_app_error_upload_error_display_and_source() {
        let error_msg = "No valid file in upload".to_string();
        let app_error = AppError::UploadError(error_msg.clone());
        assert_eq!(app_error.to_string(), format!("File upload error: {}", error_msg));
        assert!(app_error.source().is_none());
    }

    #[test]
    fn test_app_error_to_response_io() {
        let app_error = AppError::Io(std::io::Error::new(ErrorKind::Other, "test"));
        let response = app_error_to_response(&app_error);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_app_error_to_response_kafka() {
        let app_error = AppError::Kafka(rdkafka::error::KafkaError::ClientCreation("test".to_string()));
        let response = app_error_to_response(&app_error);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_app_error_to_response_yt_dlp() {
        let app_error = AppError::YtDlp("test".to_string());
        let response = app_error_to_response(&app_error);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
    
    #[test]
    fn test_app_error_to_response_kafka_send_timeout() {
        let app_error = AppError::KafkaSendTimeout;
        let response = app_error_to_response(&app_error);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_app_error_to_response_blocking_error() {
        let app_error = AppError::BlockingError("test".to_string());
        let response = app_error_to_response(&app_error);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_app_error_to_response_upload_error() {
        let app_error = AppError::UploadError("test".to_string());
        let response = app_error_to_response(&app_error);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    
    #[actix_web::test]
    async fn test_process_youtube_url_success() {
        reset_mocks();
        let mock_file_path = PathBuf::from("/tmp/test_video_success.mp3");
        
        *MOCK_DOWNLOAD_RESULT.lock().unwrap() = Ok(mock_file_path.clone());
        *MOCK_KAFKA_RESULT.lock().unwrap() = Ok(());

        if !mock_file_path.parent().unwrap().exists() { fs::create_dir_all(mock_file_path.parent().unwrap()).unwrap(); }
        File::create(&mock_file_path).expect("Failed to create dummy mock file for success test");

        let app_state = web::Data::new(AppState {
            kafka_brokers: "mock_brokers".to_string(),
            kafka_topic: "mock_topic".to_string(),
        });
        let app = test::init_service(
            ActixApp::new()
                .app_data(app_state.clone())
                .route("/process_youtube_url", web::post().to(process_youtube_url_handler))
        ).await;

        let req_body = json!({"youtube_url": "https://some_youtube_url.com"});
        let req = test::TestRequest::post().uri("/process_youtube_url").set_json(&req_body).to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK);

        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "success");
        assert_eq!(body["message"], "YouTube audio processed and sent to Kafka.");
        
        assert_eq!(*CLEANUP_CALLED_PATH.lock().unwrap(), Some(mock_file_path.clone()));

        let _ = remove_file(&mock_file_path); 
    }

    #[actix_web::test]
    async fn test_process_youtube_url_download_failure() {
        reset_mocks();
        let mock_error_msg = "mocked download failure from test".to_string();
        *MOCK_DOWNLOAD_RESULT.lock().unwrap() = Err(mock_error_msg.clone());

        let app_state = web::Data::new(AppState {
            kafka_brokers: "mock_brokers".to_string(),
            kafka_topic: "mock_topic".to_string(),
        });
        let app = test::init_service(
            ActixApp::new()
                .app_data(app_state.clone())
                .route("/process_youtube_url", web::post().to(process_youtube_url_handler))
        ).await;

        let req_body = json!({"youtube_url": "https://some_youtube_url.com"});
        let req = test::TestRequest::post().uri("/process_youtube_url").set_json(&req_body).to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains(&mock_error_msg));
        assert!(CLEANUP_CALLED_PATH.lock().unwrap().is_none());
    }

    #[actix_web::test]
    async fn test_process_youtube_url_kafka_failure() {
        reset_mocks();
        let mock_file_path = PathBuf::from("/tmp/test_video_kafka_fail.mp3");

        *MOCK_DOWNLOAD_RESULT.lock().unwrap() = Ok(mock_file_path.clone());
        *MOCK_KAFKA_RESULT.lock().unwrap() = Err("mocked kafka failure".to_string());

        if !mock_file_path.parent().unwrap().exists() { fs::create_dir_all(mock_file_path.parent().unwrap()).unwrap(); }
        File::create(&mock_file_path).expect("Failed to create dummy mock file for kafka fail test");
        
        let app_state = web::Data::new(AppState {
            kafka_brokers: "mock_brokers".to_string(),
            kafka_topic: "mock_topic".to_string(),
        });
        let app = test::init_service(
            ActixApp::new()
                .app_data(app_state.clone())
                .route("/process_youtube_url", web::post().to(process_youtube_url_handler))
        ).await;

        let req_body = json!({"youtube_url": "https://some_youtube_url.com"});
        let req = test::TestRequest::post().uri("/process_youtube_url").set_json(&req_body).to_request();
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("Kafka error: Broker transport failure")); 
        
        assert_eq!(*CLEANUP_CALLED_PATH.lock().unwrap(), Some(mock_file_path.clone())); 

        let _ = remove_file(&mock_file_path);
    }

    // Integration tests for /upload_audio
    #[actix_web::test]
    async fn test_upload_audio_success() {
        reset_mocks();
        *MOCK_KAFKA_RESULT.lock().unwrap() = Ok(());

        let app_state = web::Data::new(AppState {
            kafka_brokers: "mock_brokers".to_string(),
            kafka_topic: "mock_topic".to_string(),
        });
        let app = test::init_service(
            ActixApp::new()
                .app_data(app_state.clone())
                .route("/upload_audio", web::post().to(upload_audio_handler))
        ).await;

        let boundary = "----WebKitFormBoundaryTestUploadSuccess";
        let dummy_mp3_content = Bytes::from_static(b"short mp3 content");
        let payload_body = format!(
            "--{boundary}\r\n\
            Content-Disposition: form-data; name=\"audio_file\"; filename=\"test_upload.mp3\"\r\n\
            Content-Type: audio/mpeg\r\n\r\n\
            {}\r\n\
            --{boundary}--\r\n",
            std::str::from_utf8(dummy_mp3_content.as_ref()).unwrap()
        );
        
        let req = test::TestRequest::post()
            .uri("/upload_audio")
            .insert_header((CONTENT_TYPE, format!("multipart/form-data; boundary={}", boundary)))
            .set_payload(payload_body)
            .to_request();
        
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::OK, "Response body: {:?}", test::read_body(resp).await);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "success");
        assert_eq!(body["message"], "Audio uploaded and sent to Kafka.");
        
        let cleanup_path = MOCK_UPLOAD_CLEANUP_PATH.lock().unwrap().clone();
        assert!(cleanup_path.is_some());
        assert!(cleanup_path.unwrap().to_string_lossy().contains(".mp3")); // Check if it's an mp3 file
    }

    #[actix_web::test]
    async fn test_upload_audio_kafka_failure() {
        reset_mocks();
        *MOCK_KAFKA_RESULT.lock().unwrap() = Err("mocked kafka failure".to_string());

        let app_state = web::Data::new(AppState {
            kafka_brokers: "mock_brokers".to_string(),
            kafka_topic: "mock_topic".to_string(),
        });
        let app = test::init_service(
            ActixApp::new()
                .app_data(app_state.clone())
                .route("/upload_audio", web::post().to(upload_audio_handler))
        ).await;
        
        let boundary = "----WebKitFormBoundaryTestUploadKafkaFail";
        let dummy_mp3_content = Bytes::from_static(b"short mp3 content for kafka fail");
        let payload_body = format!(
             "--{boundary}\r\n\
            Content-Disposition: form-data; name=\"audio_file\"; filename=\"test_upload_kfail.mp3\"\r\n\
            Content-Type: audio/mpeg\r\n\r\n\
            {}\r\n\
            --{boundary}--\r\n",
            std::str::from_utf8(dummy_mp3_content.as_ref()).unwrap()
        );

        let req = test::TestRequest::post()
            .uri("/upload_audio")
            .insert_header((CONTENT_TYPE, format!("multipart/form-data; boundary={}", boundary)))
            .set_payload(payload_body)
            .to_request();
            
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("Kafka error: Broker transport failure"));
        
        let cleanup_path = MOCK_UPLOAD_CLEANUP_PATH.lock().unwrap().clone();
        assert!(cleanup_path.is_some());
        assert!(cleanup_path.unwrap().to_string_lossy().contains(".mp3"));
    }

    #[actix_web::test]
    async fn test_upload_audio_invalid_file_type() {
        reset_mocks();
        let app_state = web::Data::new(AppState {
            kafka_brokers: "mock_brokers".to_string(),
            kafka_topic: "mock_topic".to_string(),
        });
        let app = test::init_service(
            ActixApp::new()
                .app_data(app_state.clone())
                .route("/upload_audio", web::post().to(upload_audio_handler))
        ).await;

        let boundary = "----WebKitFormBoundaryTestUploadInvalidType";
        let dummy_txt_content = Bytes::from_static(b"this is not an mp3");
        let payload_body = format!(
             "--{boundary}\r\n\
            Content-Disposition: form-data; name=\"audio_file\"; filename=\"test_upload.txt\"\r\n\
            Content-Type: text/plain\r\n\r\n\
            {}\r\n\
            --{boundary}--\r\n",
            std::str::from_utf8(dummy_txt_content.as_ref()).unwrap()
        );
        
        let req = test::TestRequest::post()
            .uri("/upload_audio")
            .insert_header((CONTENT_TYPE, format!("multipart/form-data; boundary={}", boundary)))
            .set_payload(payload_body)
            .to_request();
            
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("No MP3 file uploaded"));
        assert!(MOCK_UPLOAD_CLEANUP_PATH.lock().unwrap().is_none()); // No file should have been saved to temp
    }

    #[actix_web::test]
    async fn test_upload_audio_no_file() {
        reset_mocks();
        let app_state = web::Data::new(AppState {
            kafka_brokers: "mock_brokers".to_string(),
            kafka_topic: "mock_topic".to_string(),
        });
        let app = test::init_service(
            ActixApp::new()
                .app_data(app_state.clone())
                .route("/upload_audio", web::post().to(upload_audio_handler))
        ).await;

        // Sending an empty multipart body or one with no relevant fields
        let boundary = "----WebKitFormBoundaryTestUploadNoFile";
        let payload_body = format!("--{boundary}--\r\n"); // Empty multipart

        let req = test::TestRequest::post()
            .uri("/upload_audio")
            .insert_header((CONTENT_TYPE, format!("multipart/form-data; boundary={}", boundary)))
            .set_payload(payload_body)
            .to_request();
            
        let resp = test::call_service(&app, req).await;

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = test::read_body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("No MP3 file uploaded"));
        assert!(MOCK_UPLOAD_CLEANUP_PATH.lock().unwrap().is_none());
    }
}
