# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

ttyd-rs is a Rust rewrite of [ttyd](https://github.com/tsl0922/ttyd), a tool for sharing terminals over the web using WebSocket. This project focuses on enhanced security, memory safety, and modern async architecture.

**Target Platforms**: Linux and macOS only (no Windows support)

**Current Status**: All core features implemented. 86 tests passing, all clippy lints clean.

## Build & Development Commands

```bash
# Build the project
cargo build

# Run in development mode
cargo run

# Run with release optimizations
cargo run --release

# Run tests
cargo test

# Run a specific test
cargo test test_name

# Check code without building
cargo check

# Run clippy lints (strict mode)
cargo clippy --all-targets -- -D warnings

# Format code
cargo fmt

# Check formatting without modifying files
cargo fmt -- --check

# Add dependencies (always use latest version)
cargo add <crate_name>
cargo add <crate_name> --features <feature1>,<feature2>
```

## Code Quality Requirements (Must Pass Before Commit)

Before committing any code, ensure these commands pass:

```bash
# 1. Format check
cargo fmt -- --check

# 2. Clippy with zero warnings
cargo clippy --all-targets -- -D warnings

# 3. All tests pass
cargo test
```

**All three must be green before code is considered complete.**

## Code Quality Requirements

This project has **strict lint rules** configured in `Cargo.toml`:

```toml
[workspace.lints.clippy]
unwrap-used = "deny"
expect-used = "deny"
panic = "deny"
```

**Critical**: Never use `.unwrap()`, `.expect()`, or `panic!()` in code. Always use proper error handling with `Result` and `?` operator, or handle `Option` values explicitly with `match`, `if let`, or combinators like `.ok_or()`.

## Dependency Management

**Always use `cargo add` to add dependencies** - never manually edit `Cargo.toml` for dependencies.

```bash
# Add a dependency (latest version)
cargo add tokio --features full

# Add a dev dependency
cargo add --dev criterion

# Add a build dependency
cargo add --build cc
```

This ensures:
- Latest stable versions are used
- Proper version resolution
- Consistent dependency management

## Architecture

### Technology Stack

- **Async runtime**: tokio
- **Web framework**: axum (with WebSocket support)
- **PTY management**: nix crate (Unix-specific PTY operations)
- **Frontend**: xterm.js (embedded via rust-embed)
- **CLI parsing**: clap
- **Serialization**: serde / serde_json
- **Logging**: tracing / tracing-subscriber

### Module Structure

**Important**: Use the new module style (Rust 2018+) without `mod.rs` files.

```
src/
├── main.rs           # Entry point, CLI parsing, config loading
├── config.rs         # Configuration types and validation
├── server.rs         # Server module declaration
├── server/
│   ├── http.rs       # HTTP server, routing, static files
│   ├── websocket.rs  # WebSocket handler, session management
│   └── api.rs        # REST API endpoints
├── pty.rs            # PTY module declaration
├── pty/
│   ├── process.rs    # PTY process spawning and management
│   └── session.rs    # PTY session wrapper
├── auth.rs           # Auth module declaration
├── auth/
│   ├── basic.rs      # Basic authentication
│   └── token.rs      # Token authentication
├── protocol.rs       # WebSocket message types
├── session.rs        # Session manager, multi-client support
├── audit.rs          # Audit logging
├── rate_limit.rs     # Rate limiting
├── validation.rs     # Input validation
└── assets.rs         # Static asset embedding
```

**Module organization rule**: Instead of `module/mod.rs`, use `module.rs` at the parent level to declare the module and its submodules.

### Key Design Principles

1. **Security-first**: Default configurations must be secure. Authentication is supported via Basic Auth and Token Auth with constant-time comparison.

2. **Memory safety**: Leverage Rust's ownership system. Any `unsafe` code must be thoroughly documented and reviewed.

3. **Async I/O**: All I/O operations use tokio's async runtime for performance.

4. **PTY handling**: Use the `nix` crate for Unix PTY operations. Handle signals properly (SIGHUP for graceful shutdown, SIGKILL as fallback, TIOCSWINSZ for terminal resize).

5. **Error handling**: All errors must be properly typed (using `thiserror`) and propagated. Never silence errors.

## Security Implementation

All security features are implemented:

- **Authentication**: Basic Auth and Token Auth with constant-time comparison via `subtle` crate. Passwords are stored as SHA-256 hashes.
- **Input validation**: Terminal size bounds, payload size limits, credential length limits
- **Rate limiting**: Sliding window algorithm, per-IP tracking
- **Audit logging**: Connection events, authentication attempts, session lifecycle
- **Proxy support**: Reads real client IP from `X-Real-IP` / `X-Forwarded-For` headers by default (`trust_proxy = false`). Use `--no-trust-proxy` to disable when not behind a reverse proxy.
- **Reconnection**: Clients can reconnect within a configurable window (default 60s) without losing session state. Controlled by `--reconnect-window` / `session.reconnect_window`.

## WebSocket Protocol

The WebSocket protocol uses JSON messages with the following types:
- `auth` / `auth_ok` / `auth_fail` - Authentication flow
- `input` / `output` - Terminal I/O
- `resize` - Terminal resize
- `ping` / `pong` - Keepalive
- `error` / `disconnect` - Error handling
- `ready` - Session ready notification
- `join` - Join an existing session by ID

## REST API Endpoints

```
GET    /api/sessions          - List all active sessions
GET    /api/sessions/:id      - Get session info
DELETE /api/sessions/:id      - Terminate session
GET    /api/stats             - Server statistics
GET    /api/health            - Health check
```

## Development Stage

**Current Status**: All core milestones (M1-M6) completed.

**Implemented Features**:
- CLI with all flags from original ttyd
- Configuration file support (TOML)
- HTTP server with static file serving
- WebSocket terminal communication
- PTY process management
- Basic Auth and Token Auth
- Rate limiting
- Audit logging
- Session management (isolated/shared modes)
- REST API for session management
- xterm.js frontend

## Reference Material

- Original ttyd source code is in the `ttyd/` directory (for reference only)
- See `DEVELOPMENT_GOALS.md` for detailed roadmap and feature matrix
- Example configuration: `config.example.toml`
- Project documentation: `docs/` directory

## Important Notes

- The `ttyd/` directory contains the original C implementation for reference only
- Focus on Unix-like systems; explicitly no Windows support to simplify PTY handling
- Performance targets: <50ms startup, <10MB idle memory, <5ms latency, >1000 concurrent connections
- TLS is not planned; use a reverse proxy (nginx, Caddy) for HTTPS
