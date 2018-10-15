const Promise = require("promise")
const Process = require("process")
const Bootstrap = require("../modules/bootstrap")

let stopedHeart = false
let motives = []


class HealthCheckModel {

    constructor() {
        console.log(Process.getuid(), Process.env.PORT);
    }

    heartBeat() {
        console.log(Process.getuid())
        let interval = setInterval(() => {
            checkModulesLife()

            if (stopedHeart){
                notifyMonitor()
                clearInterval(interval)
            }
        }, 10000)
    }
    
    notifyMonitor() {
        //TODO Notify monitor by RabbitMQ
        // The monitor will be the one responsible for take care of this broken service
        // Here we are supposed to clean the list of motives
    }

    checkModulesLife() {
        let modules = Bootstrap.getModules()

        if (modules) {
            modules.forEach(mod => { 
                let motive =  mod.checkHealth()

                if (motive)
                    motives.add(motive)
            })

            stopedHeart = motives.length > 0;
        }
    }    

}

module.exports = HealthCheckModel;