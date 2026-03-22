# Fuiz Server

Self-hostable game server built with actix-web.

## Running

```bash
cargo run -p fuiz-server
```

## Configuration

Server settings are loaded from environment variables prefixed with `FUIZ_`:

| Variable               | Default   | Description                       |
| ---------------------- | --------- | --------------------------------- |
| `FUIZ_HOSTNAME`        | `0.0.0.0` | Address to bind to                |
| `FUIZ_PORT`            | `8080`    | Port to listen on                 |
| `FUIZ_ALLOWED_ORIGINS` | `[]`      | Allowed CORS origins (JSON array) |

When `FUIZ_ALLOWED_ORIGINS` is empty, CORS is fully permissive.

Game-logic settings use the `FUIZ_SETTINGS_` prefix with `__` as the nesting separator:

```bash
FUIZ_SETTINGS_FUIZ__MAX_PLAYER_COUNT=500
FUIZ_SETTINGS_QUESTION__MAX_TIME_LIMIT=300
```

## API

### Create a game

```http
POST /add
```

| Parameter | Type          | Description        |
| --------- | ------------- | ------------------ |
| `config`  | `FuizConfig`  | Quiz configuration |
| `options` | `FuizOptions` | Game options       |

Returns `{ "game_id": string, "watcher_id": string }`.

### Check if a game is alive

```http
GET /alive/:gameid
```

Returns `true` or `false`.

### Join a game (WebSocket)

```http
GET /watch/:gameid
```
