# Fuiz (Serverless)

This is fuiz's serverless module intended for its use with Cloudflare worker's platform. For a hosted server, please refer to [fuiz/hosted-server](https://gitlab.com/fuiz/hosted-server).

## Why Serverless?

Due to the nature of "live game", it happens that latency is a big factor in people's experience using Fuiz. If we were to deploy multiple all-time running machines around the world, the cost would be prohibitive. More so considering that the server wouldn't be used to its potential most of the time.

The other reason is sandboxing. Through serverless hosting, if you're able to crash Fuiz, you would only be crashing your own game, and you wouldn't be affecting other games in the network. This gives us a peace of mind knowing no bad actor can take interrupt the service for everyone by simply discovering a place where we divide by zero.

## Modules

To improve co-ordination and consistency, we split it to multiple modules.

1. `fuiz-cloudflare`. This is the root directory. It contains an entry point to the program and a [durable object](https://developers.cloudflare.com/durable-objects/). This is where the game actually runs. Active websocket connections are also handled here.
2. `fuiz-game-manager`. A singleton durable object that coordinates between all game instances on generating new ids, deleting exired ids, and retrieving the durable object for the given id. Since it's a singleton, consistency is guaranteed. It does provide additional latency but not much in the grand scheme of things.
3. `counter`. A simple singleton that contains live metric data. This is referenced by the components above (and by the frontend - if binded).

Note that for fuiz to run, you also need an image server, check [corkboard-serverless](https://gitlab.com/fuiz/corkboard-serverless) for a serverless implementation of that.

## Local Development

Each module can be run with `bunx wrangler dev`. Sometimes `fuiz-cloudflare` fails if other modules were started before it, so you might want to run it first, _and after it's ready_, run the other modules.

Note that all of the modules here reference [fuiz/game](https://gitlab.com/fuiz/game). If you would like to test your own version of that, you might want to edit `Cargo.toml` to reference your own fork.

## Deployment

All components can be deployed to cloudflare with `bunx wrangler deploy`. If it's the first time deploying them, you should deploy `counter`, `fuiz-game-manager`, and `fuiz-cloudflare` in that order.
