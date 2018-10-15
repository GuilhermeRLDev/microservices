//Created by Guilherme Rossetti Lima on 2018-09-24
let CommandFactory = require("hystrixjs").commandFactory;  
let Promise = require("promise")
let MongoDBBootstrap = require("./mongoDBBootstrap")
let fs = require("fs");

let ConfigPropertiesNames = {
    MODULES : "modules", 
    PORT: "port"
};  

var modulesInitialized = []; 
var Configuration =  {}
var app = {};

class AppBootstrap {

    constructor(app_) {
        console.log("Seting up all congifurations!")
        app = app_
    }

    readPropertiesFromFile() {
        return new Promise((fulfill, reject) =>  {
            fs.readFile("app.config", "utf8", (err, data) => {
                if (err) reject(err)
                else {
                    Configuration = JSON.parse(data);
                    fulfill();
                }
            })
        })
    }

    getConfiguration() {
        let command = CommandFactory.getOrCreate("Reading configuration")
            .run(this.readPropertiesFromFile)
            .fallbackTo(this.fallBack)
            .build()
        
        return command.execute()
    }

    fallBack(err) {
        console.log("Error while reading configuration from file!"+ err )
    }

    startModules() {
        let modules = Configuration[ConfigPropertiesNames.MODULES]

        if (!modules)
            return

        modules.forEach((mod) =>  {
            let moduleBoot = require(mod)
            //It creates a instance of module
            let moduleInstance = new moduleBoot(Configuration)

            if (moduleInstance)
                modules.add({
                    ModuleName:  mod,
                    ModuleIntsance: moduleInstance}); 

        })
    }

    static getModules() {
        return modules
    }

    startService() {
        if (!app)
            throw "We couldn't start to listen"

        app.listen(Configuration[ConfigPropertiesNames.PORT], () => {
            console.log("Starting to listen service on port "+ Configuration[ConfigPropertiesNames.PORT])
        })
    }

    start(call) {
        this.getConfiguration()
            .then(this.startModules)
            .then(this.startService)
    }
}

module.exports = AppBootstrap;