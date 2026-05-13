# LLM-API-Key-Proxy (Rust)

Rust rewrite of the LLM API Key Proxy.

## Quick Start

```bash
cargo run -p proxy_app
```

## Architecture

- `proxy_app` — Axum web server (was FastAPI)
- `crates/rotator` — key rotation, retry, circuit breaker (was `rotator_library`)
- `crates/models` — shared serde data models (was Pydantic)
- `crates/config` — configuration management (was `python-dotenv` + Python dataclasses)

## Development

```bash
cargo check
cargo test -p proxy_app
```
