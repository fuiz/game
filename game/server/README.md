# Fuiz Server

Self-hostable game server built with actix-web.

## Running

```bash
cargo run -p fuiz-server
```

Listens on port 8080 (or the `PORT` environment variable).

To expose on the local network:

```bash
cargo run -p fuiz-server --features expose-network
```

Set `NETWORK_ORIGIN` to the exposed URL if you encounter CORS issues.

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
