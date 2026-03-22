# Fuiz Game Engine

Core game logic library for Fuiz. Handles quiz state, scoring, question types, and player management. WASM compatible.

## Features

- **Multiple Question Types** — Multiple choice, type answer, and drag-and-drop ordering
- **Team-Based Games** — Configurable team sizes with random assignment
- **Smart Name Generation** — Automatic pet names or Roman-style names for players
- **Media Support** — Image support through the Corkboard system
- **Content Safety** — Built-in profanity filtering
- **Input Validation** — Comprehensive validation via `garde`

## Usage

This crate is used as a dependency by the `server` and `cloudflare` crates in this workspace.
