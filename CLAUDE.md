# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

ttyd-rs is a Rust rewrite of [ttyd](https://github.com/tsl0922/ttyd), a tool for sharing terminals over the web using WebSocket. This project focuses on enhanced security, memory safety, and modern async architecture.

**Target Platforms**: Linux and macOS only (no Windows support)

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

### Technology Stack (as planned in DEVELOPMENT_GOALS.md)

- **Async runtime**: tokio
- **Web framework**: axum (with WebSocket support)
- **PTY management**: nix crate (Unix-specific PTY operations)
- **Frontend**: xterm.js
- **CLI parsing**: clap

### Module Structure (New Module Style)

**Important**: Use the new module style (Rust 2018+) without `mod.rs` files.

```
src/
├── main.rs           # Entry point
├── config.rs         # Configuration parsing
├── server.rs         # Server module (or server/ with submodules)
├── server/
│   ├── http.rs
│   └── websocket.rs
├── pty.rs            # PTY module
├── pty/
│   ├── process.rs
│   └── session.rs
├── auth.rs           # Auth module
├── auth/
│   ├── basic.rs
│   └── token.rs
├── protocol.rs       # Protocol module
└── audit.rs          # Audit module
```

**Module organization rule**: Instead of `module/mod.rs`, use `module.rs` at the parent level to declare the module and its submodules.

### Key Design Principles

1. **Security-first**: Default configurations must be secure. Authentication should be enabled by default.

2. **Memory safety**: Leverage Rust's ownership system. Any `unsafe` code must be thoroughly documented and reviewed.

3. **Async I/O**: All I/O operations use tokio's async runtime for performance.

4. **PTY handling**: Use the `nix` crate for Unix PTY operations. Handle signals properly (SIGWINCH for terminal resize, SIGCHLD for process lifecycle).

5. **Error handling**: All errors must be properly typed (using `thiserror`) and propagated. Never silence errors.

## Security Considerations

When implementing features:

- **Authentication**: Basic Auth and token-based auth are planned. Default should require authentication.
- **Input validation**: Validate all WebSocket messages and user inputs to prevent injection attacks.
- **Rate limiting**: Implement to prevent brute force attacks.
- **Audit logging**: Log connection events, authentication attempts, and optionally record terminal sessions.

## WebSocket Protocol

The WebSocket protocol for terminal communication needs to be designed to be:
- Efficient for high-frequency terminal output
- Compatible with xterm.js on the frontend
- Optionally compatible with the original ttyd protocol

## Development Stage

**Current Status**: Early initialization phase. The project has basic scaffolding with only a "Hello, world!" in `main.rs`.

**Next Steps** (from DEVELOPMENT_GOALS.md):
1. Design WebSocket message protocol specification
2. Update Cargo.toml with core dependencies
3. Implement basic project structure
4. Implement HTTP server skeleton (axum)
5. Implement PTY basic functionality (nix)
6. Frontend integration PoC

## Reference Material

- Original ttyd source code is in the `ttyd/` directory (for reference, but will be rewritten in Rust)
- See `DEVELOPMENT_GOALS.md` for detailed roadmap, milestones, and feature planning
- Frontend will use xterm.js (see `README.md`)

## Important Notes

- The `ttyd/` directory contains the original C implementation for reference only
- Focus on Unix-like systems; explicitly no Windows support to simplify PTY handling
- Performance targets: <50ms startup, <10MB idle memory, <5ms latency, >1000 concurrent connections
