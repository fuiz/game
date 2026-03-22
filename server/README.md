# Fuiz

Host live quizzes freely

<img src="https://gitlab.com/fuiz/website/-/raw/main/static/favicon.svg?ref_type=heads" width="128" height="128" alt="Fuiz icon">

[![License](https://img.shields.io/gitlab/license/fuiz/game-server?style=for-the-badge)](https://gitlab.com/fuiz/game-server/-/raw/main/LICENSE)

## Developing

This is a self-hostable version of the backend component. It can be run with:

```bash
cargo run
```

This will open a server listening to port 8080 (or the enviroment variable `PORT`).

You can also pass the feature flag:

```bash
cargo run --features expose-network
```

This will open a server listening to local network as well. You can also set the environment variable `NETWORK_ORIGIN` to be the exposed URL in case you encounter CORS issues.

### Creating a game

```http
POST /add
```

| Parameter | Type          | Description                                                                                               |
| :-------- | :------------ | :-------------------------------------------------------------------------------------------------------- |
| `config`  | `FuizConfig`  | **Required**. Config as defined in [src/game_manager/fuiz/config.rs](src/game_manager/fuiz/config.rs#L31) |
| `options` | `FuizOptions` | **Required**. Options as defined in [src/game_manager/game.rs](src/game_manager/game.rs#L41)              |

#### Response

```javascript
{
  "game_id"    : string,
  "watcher_id" : string
}
```

### Checking if game is alive

```http
GET /alive/:gameid
```

#### Response

```javascript
true | false;
```

### Joining a game (using WS protocol)

```http
GET /watch/:gameid
```

This establishes a websocket connection.
