# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

ttyd-rs is a Rust rewrite of [ttyd](https://github.com/tsl0922/ttyd) — a tool for sharing terminals over the web via WebSocket. Focused on security, memory safety, and modern async architecture.

**Platform**: Linux only
**Status**: Production ready. See [docs/PROJECT_STATUS.md](docs/PROJECT_STATUS.md) for full technical reference.

## Build & Development

```bash
cargo build              # Debug build
cargo build --release    # Release build
cargo run                # Run in dev mode
cargo test               # Run all tests
cargo test test_name     # Run a specific test
cargo check              # Type-check without building
cargo clippy -- -D warnings  # Lint (strict mode)
cargo fmt -- --check     # Format check
cargo fmt                # Auto-format
```

### Code Quality Gates (Must Pass Before Commit)

```bash
just qa
```

or

```bash
cargo fmt -- --check
cargo clippy -- -D warnings
cargo test
```

**All three must be green.**

### Lint Rules

Configured in `Cargo.toml`:

```toml
[lints.clippy]
unwrap-used = "deny"
expect-used = "deny"
panic = "deny"
```

**Never** use `.unwrap()`, `.expect()`, or `panic!()` in production code. Use `Result` + `?`, or handle `Option` with `match`/`if let`/`.ok_or()`.

**Test exception**: `#[cfg(test)]` modules may use `.unwrap()` with `#[allow(clippy::unwrap_used)]`.

### Dependency Management

Always use `cargo add` — never manually edit `Cargo.toml` for dependencies:

```bash
cargo add <crate> --features <feat1>,<feat2>
cargo add --dev <crate>
cargo add --build <crate>
```

## Architecture

```
┌─────────────────┐
│   Web Browser   │
│   (xterm.js)    │
└────────┬────────┘
         │ WebSocket
         ▼
┌─────────────────┐
│  HTTP Server    │
│  (axum)         │
├─────────────────┤
│ SessionManager  │
│  └─ Sessions    │
│     └─ PTY      │
└─────────────────┘
```

### Technology Stack

| Component | Choice |
|-----------|--------|
| Async runtime | tokio |
| Web framework | axum (WebSocket support) |
| PTY management | nix (Unix-specific) |
| Frontend | xterm.js (embedded via rust-embed) |
| CLI parsing | clap |
| Serialization | serde / serde_json |
| Logging | tracing / tracing-subscriber |
| Error handling | thiserror |

### Project Structure

```
src/
├── main.rs              # Entry point, CLI, config loading
├── config.rs            # Configuration types and validation
├── server.rs            # Server module declaration
├── server/
│   ├── http.rs          # HTTP server, routing, static files, body limits
│   ├── websocket.rs     # WebSocket handler, session management
│   ├── api.rs           # REST API endpoints
│   └── files.rs         # File transfer (upload/download/list)
├── pty.rs               # PTY module declaration
├── pty/
│   ├── process.rs       # PTY process spawning and management
│   └── session.rs       # PTY session wrapper
├── auth.rs              # Auth module declaration
├── auth/
│   ├── basic.rs         # Basic authentication
│   └── token.rs         # Token authentication
├── protocol.rs          # WebSocket message types
├── session.rs           # Session manager, multi-client support
├── audit.rs             # Audit logging
├── rate_limit.rs        # Rate limiting
├── validation.rs        # Input validation
└── assets.rs            # Static asset embedding
```

### Key Design Decisions

- **Error handling**: All errors typed via `thiserror`, propagated with `?`. Never silenced.
- **Concurrent I/O**: PTY output coalesced to reduce syscall frequency.
- **Memory**: `broadcast::channel(512)` bounds per-session memory. `Arc` for shared state.
- **PTY cleanup**: 5-stage process cleanup (SIGHUP → poll → SIGKILL → reap → background reaper).
- **Upload safety**: `UploadFileGuard` (RAII) ensures partial files are removed on all error paths.

## WebSocket Protocol

JSON messages with these types: `auth`/`auth_ok`/`auth_fail`, `input`/`output`, `resize`, `ping`/`pong`, `error`/`disconnect`, `ready`, `join`, `file_list`/`file_list_result`.

Full spec: [docs/PROTOCOL.md](docs/PROTOCOL.md)

## REST API

See [docs/PROJECT_STATUS.md](docs/PROJECT_STATUS.md) for the complete endpoint reference with examples.

## Security

See [docs/PROJECT_STATUS.md](docs/PROJECT_STATUS.md) for the full security assessment.

Key implementation notes:
- Constant-time comparison via `subtle` crate (timing-attack resistant)
- Argon2id password hashing with random salt (basic auth); the configured `password` value (config file or `--password`) may be a plaintext password or an Argon2id PHC hash — plaintext logs a startup warning, hashes are generated with `ttyd-rs --hash-password`; SHA-256 + constant-time comparison for token auth
- `trust_proxy` disabled by default to prevent IP spoofing
- Reconnection window (default 60s) preserves session state

## Important Notes

- The `ttyd/` directory contains the original C implementation for reference only
- Linux only; no Windows or macOS support (simplifies PTY handling)
- TLS is not built-in; use a reverse proxy (nginx, Caddy) for HTTPS
- Performance targets: <50ms startup, <10MB idle memory, >1000 concurrent connections
