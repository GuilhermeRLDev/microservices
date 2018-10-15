// Created by Guilherme Rossetti Lima at 2018-09-25 
const RabbitMQ = require("amqplib/callback_api")
const Promise = require("promise")
const CommandsFactory = require("hystrixjs").commandFactory

let RabbitMQPropertiesName = {
    MESSAGE_SERVER : "message_server"
}

let  Configuration = {
    "message_server":  "amqp://localhost"
}

class RabbitMQBootstrap {

    constructor(config) {
        if (config)
            this.applyConfiguration(config)

        this.createCommand()
            .then((conn_) => {
                RabbitMQBootstrap.setConnection(conn_)
            }) 
    }

    static getConnection() {
        return this.conn
    }

    static setConnection(conn) {
        this.conn = conn
    }

    static checkHealth() {
        return this.conn !== null;
    }

    createCommand() {
        let command = CommandsFactory.getOrCreate("Starting service" + Configuration[RabbitMQPropertiesName.MESSAGE_SERVER])
            .run(this.startConnection)
            .fallbackTo(this.fallBack)
            .build()

        return command.execute()
    }

    setMessageServer(message_server) {
        Configuration[RabbitMQPropertiesName.MESSAGE_SERVER] = message_server
    }

    getMessageServer( message_server ) {
        return Configuration[RabbitMQPropertiesName.MESSAGE_SERVER]  
    }

    applyConfiguration(config) {
        if (config[RabbitMQPropertiesName.MESSAGE_SERVER])
            this.setMessageServer(config[RabbitMQPropertiesName.MESSAGE_SERVER])
    }

    fallBack(err) {
        console.log("Please check your database connection:"+ err, Configuration[RabbitMQPropertiesName.MESSAGE_SERVER])
    }

    startConnection() {
        return new Promise((fullFill, refused) => {
            RabbitMQ.connect(Configuration[RabbitMQPropertiesName.MESSAGE_SERVER], (err, conn) => {
                if (err) return refused(err)
                else return fullFill(conn)
            })
        })       
    }
}

module.exports = RabbitMQBootstrap; 