# Fuiz

A Rust monorepo for the Fuiz live quiz platform — real-time quiz games with multiple question types, team support, and leaderboards.

## Crates

| Crate                       | Description                                 |
| --------------------------- | ------------------------------------------- |
| [`logic`](logic/)           | Core game engine library (WASM compatible)  |
| [`server`](server/)         | Self-hostable game server (actix-web)       |
| [`cloudflare`](cloudflare/) | Serverless deployment on Cloudflare Workers |

## Getting Started

Requires Rust 1.88+ (Edition 2024).

```bash
cargo build
```

To run the server locally:

```bash
cargo run -p fuiz-server
```

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).

Developed by [Beyond Expiry](https://beyondexpiry.org/).
