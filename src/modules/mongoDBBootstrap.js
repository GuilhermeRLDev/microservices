// Created by Guilherme Rossetti Lima at 2018-09-25 
const MongoClient = require("mongodb").MongoClient
const Promise = require("promise")
const CommandsFactory = require("hystrixjs").commandFactory

let MongoDBPropertiesName = {
    DBPATH : "dbPath"
}

let  Configuration = {
    "dbPath":  "localhost:20017"
}


class MongoDBBootstrap {

    constructor(config) {
        if (config)
            this.applyConfiguration(config)

        this.createCommand()
            .then((db_) => {
                MongoDBBootstrap.setDB(db_)
            }) 
    }

    static checkHealth() {
        return this.db != null 
    }

    static getDB() {
        return this.db
    }

    static setDB(db_) {
        this.db = db_
    }

    createCommand() {
        let command = CommandsFactory.getOrCreate("Starting service" + Configuration[MongoDBPropertiesName.DBPATH])
            .run(this.startConnection)
            .fallbackTo(this.fallBack)
            .build()

        return command.execute()
    }

    setDBPATH( dbpath ) {
        
        Configuration[MongoDBPropertiesName.DBPATH] = dbpath
    }

    getDBPATH( dbpath ) {
        return Configuration[MongoDBPropertiesName.DBPATH]  
    }

    applyConfiguration(config) {
        if (config[MongoDBPropertiesName.DBPATH])
            this.setDBPATH(config[MongoDBPropertiesName.DBPATH])
    }

    fallBack(err) {
        console.log("Please check your database connection:"+ err, Configuration[MongoDBPropertiesName.DBPATH])

    }
    
    startConnection() {
        return new Promise((fullFill, refused) => {
            MongoClient.connect(Configuration[MongoDBPropertiesName.DBPATH], (err, db) => {
                if (err) return refused(err)
                else return  fullFill(db)
            })
        })       
    }
}

module.exports = MongoDBBootstrap; 