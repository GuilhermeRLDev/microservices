# Stage 1: Builder
FROM rust:1.75-slim-bullseye AS builder

# Create appuser on builder stage for consistent ownership (optional but good practice)
RUN useradd --create-home --shell /bin/bash appuser
WORKDIR /home/appuser/app

# Copy manifests and source
# Ensure Cargo.lock is present or generated before this step if not in repo
COPY Cargo.toml Cargo.lock ./ 
COPY src ./src

# Build the application
# Ensure a dummy main.rs or full src is available if only building deps first
RUN cargo build --release

# Stage 2: Runtime
FROM debian:bullseye-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ffmpeg \
    python3 \
    python3-pip \
    libsasl2-modules \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Install yt-dlp using pip
RUN pip3 install --no-cache-dir yt-dlp

# Create a non-root user to run the application
RUN useradd --create-home --shell /bin/bash appuser
WORKDIR /home/appuser/app

# Copy the compiled binary from the builder stage
COPY --from=builder /home/appuser/app/target/release/youtube_audio_pipeline .

# Set permissions for the appuser
RUN chown appuser:appuser youtube_audio_pipeline

# Switch to the non-root user
USER appuser

# Expose the port the app runs on
EXPOSE 8080

# Set default environment variables
ENV RUST_LOG="info"
ENV KAFKA_BROKERS="localhost:9092"
ENV KAFKA_TOPIC="youtube_audio"
ENV APP_PORT="8080" 

# Command to run the application
CMD ["./youtube_audio_pipeline"]
