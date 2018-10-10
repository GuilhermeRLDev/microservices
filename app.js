var express = require("express")
var AppBootstrap = require("./src/modules/bootstrap") 
var RabbitMQBootstrap = require("./src/modules/rabbitMQBootstrap")

app = express()

//Register methods
app.get("/health/check", (req, res, next) =>  {
    res.json({success: true, status: "Alive and working" })
})

app.get("/people", (req, res, next ) =>  {
    res.json([
        {firstName: "Guilherme", lastName: "Rossetti Lima"}, 
        {firstName: "Guilherme", lastName: "Rossetti Lima"}
    ])
})

let bootstrap = new AppBootstrap(app)

bootstrap.start()


