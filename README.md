# Fuiz Game Engine

A Rust-based game engine for creating and managing live quiz games with real-time synchronization, multiple question types, team support, and comprehensive leaderboards.

<div align="center">
  <img src="https://gitlab.com/opencode-mit/fuiz/website/-/raw/main/static/favicon.svg?ref_type=heads" width="128" height="128" alt="Fuiz Logo">
</div>

<div align="center">

[![License: AGPL v3](https://img.shields.io/gitlab/license/opencode-mit/fuiz/game?style=for-the-badge)](https://gitlab.com/opencode-mit/fuiz/game/-/raw/main/LICENSE)
[![Rust Edition](https://img.shields.io/badge/Rust-2024-orange?style=for-the-badge&logo=rust)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)

</div>

## Features

- **Multiple Question Types** - Multiple choice, type answer, and drag-and-drop ordering
- **Team-Based Games** - Optional team formation with configurable sizes and random assignment
- **Smart Name Generation** - Automatic pet names or Roman-style names for players
- **Media Rich Questions** - Image support through integrated Corkboard system
- **Content Safety** - Built-in profanity filtering and content moderation
- **Flexible Scoring** - Customizable point systems and leaderboard management
- **Input Validation** - Comprehensive validation of all game configurations and player inputs
- **WASM Compatible** - Runs in browsers and on servers

### Question Types

| Type                | Description                   | Features                              |
| ------------------- | ----------------------------- | ------------------------------------- |
| **Multiple Choice** | Traditional A/B/C/D questions | Configurable options, media support   |
| **Type Answer**     | Free-text input questions     | Fuzzy matching, case-insensitive      |
| **Order**           | Drag-and-drop ordering tasks  | Sequence arrangement, partial scoring |

## Getting Started

### Prerequisites

- Rust 1.88+ (Edition 2024)
- Cargo package manager

### Installation

```bash
git clone https://gitlab.com/fuiz/game.git
cd game
cargo build --release
```

### Using as a Library

This is a library crate for building quiz game applications. To use it in your project:

```toml
[dependencies]
fuiz = { git = "https://gitlab.com/fuiz/game.git" }
```

## Library Usage

### Basic Quiz Game

```rust
use fuiz::{Game, game::Options, fuiz::config::Fuiz, watcher::Id};

// Create a simple quiz configuration
let config = Fuiz {
    title: "Math Quiz".to_string(),
    slides: vec![
        // Add your questions here
    ],
};

let options = Options::default();
let host_id = Id::new();
let game = Game::new(config, options, host_id);
```

### Team-based Game

```rust
use fuiz::game::{Options, TeamOptions, NameStyle};

let options = Options {
    teams: Some(TeamOptions {
        size: 3,
        assign_random: false,
    }),
    random_names: Some(NameStyle::Petname(2)),
    ..Default::default()
};
```

### Game Configuration

The `Fuiz` struct contains the quiz configuration:

```rust
pub struct Fuiz {
    title: String,
    pub slides: Vec<SlideConfig>,
}
```

Questions are defined using the `SlideConfig` enum:

```rust
pub enum SlideConfig {
    MultipleChoice(multiple_choice::SlideConfig),
    TypeAnswer(type_answer::SlideConfig),
    Order(order::SlideConfig),
}
```

Game behavior is configured with the `Options` struct:

```rust
pub struct Options {
    random_names: Option<NameStyle>,
    show_answers: bool,
    no_leaderboard: bool,
    teams: Option<TeamOptions>,
}
```

## Hosted Service

While you can self-host this engine, a live version is available:

- **API Endpoint**: [api.fuiz.org](https://api.fuiz.org)
- **Status Page**: [status.fuiz.org](https://status.fuiz.org)
- **Main Website**: [fuiz.org](https://fuiz.org)

## License

This project is licensed under the GNU Affero General Public License v3.0 (AGPL-3.0). See the [LICENSE](LICENSE) file for details.

## About

Fuiz is developed and maintained by [Beyond Expiry](https://beyondexpiry.org/), a dedicated non-profit, as part of the effort to create open-source educational tools.
