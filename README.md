# Fuiz

A Rust monorepo for the Fuiz live quiz platform — real-time quiz games with multiple question types, team support, and leaderboards.

## Structure

### [`game/`](game/) — Game Engine

| Crate                                | Description                                 |
| ------------------------------------ | ------------------------------------------- |
| [`game/logic`](game/logic/)          | Core game engine library (WASM compatible)  |
| [`game/server`](game/server/)        | Self-hostable game server (actix-web)       |
| [`game/cloudflare`](game/cloudflare/)| Serverless deployment on Cloudflare Workers |

### [`corkboard/`](corkboard/) — Image Storage

| Crate                                        | Description                                        |
| -------------------------------------------- | -------------------------------------------------- |
| [`corkboard/server`](corkboard/server/)      | Self-hostable image storage server (actix-web)     |
| [`corkboard/cloudflare`](corkboard/cloudflare/) | Serverless image storage on Cloudflare Workers  |

## Getting Started

Requires Rust 1.88+ (Edition 2024).

```bash
cargo build
```

To run the game server locally:

```bash
cargo run -p fuiz-server
```

To run the corkboard server locally:

```bash
cargo run -p corkboard-server
```

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).

Developed by [Beyond Expiry](https://beyondexpiry.org/).
