# Fuiz (Serverless)

This is fuiz's serverless module intended for its use with Cloudflare worker's platform. For a hosted server, please refer to [fuiz/hosted-server](https://gitlab.com/fuiz/hosted-server).

## Why Serverless?

Due to the nature of "live game", it happens that latency is a big factor in people's experience using Fuiz. If we were to deploy multiple all-time running machines around the world, the cost would be prohibitive. More so considering that the server wouldn't be used to its potential most of the time.

The other reason is sandboxing. Through serverless hosting, if you're able to crash Fuiz, you would only be crashing your own game, and you wouldn't be affecting other games in the network. This gives us a peace of mind knowing no bad actor can take interrupt the service for everyone by simply discovering a place where we divide by zero.

## Modules

To improve co-ordination and consistency, we split it to multiple modules.

1. `fuiz-cloudflare`. This is the root worker. It contains an entry point to the program and two [durable objects](https://developers.cloudflare.com/durable-objects/), one managing a single instance of a game and another "coordinator" that generates ids and retrieves them. Active websocket connections are also handled here.
2. `counter`. A simple singleton that contains live metric data. This is used by the components above (and by the frontend - if binded).

Note that for fuiz to run, you also need an image server, check [corkboard-serverless](https://gitlab.com/fuiz/corkboard-serverless) for a serverless implementation of that.

## Local Development

Each module can be run with `bunx wrangler dev`.

Note that `fuiz-cloudflare` references [fuiz/game](https://gitlab.com/fuiz/game). If you would like to test your own version of that, you might want to edit `Cargo.toml` to reference your own fork.

## Deployment

All components can be deployed to cloudflare with `bunx wrangler deploy`. If it's the first time deploying them, you should deploy `counter`, and then `fuiz-cloudflare`. Unless you are not interested in metrics.
