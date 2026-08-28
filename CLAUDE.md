# CODE GUIDANCE to AIs

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
├── lib.rs               # Crate root: module declarations + re-exports
├── main.rs              # CLI parsing, config loading/merge, logging init
├── config.rs            # Configuration types, defaults, validation
├── protocol.rs          # WebSocket message types (serde tagged enum)
├── session.rs           # Session manager, multi-client support
├── audit.rs             # JSONL audit logging
├── rate_limit.rs        # Sliding-window rate limiting
├── validation.rs        # Input validation
├── assets.rs            # Static asset embedding (rust-embed over `static/`)
├── auth.rs              # Auth module declaration
├── auth/
│   ├── basic.rs         # Basic authentication (Argon2id)
│   └── token.rs         # Token authentication (SHA-256 + constant-time eq)
├── pty.rs               # PTY module declaration
├── pty/
│   ├── process.rs       # PTY process spawning and management
│   └── session.rs       # PTY session wrapper
├── server.rs            # Server module declaration
└── server/
    ├── http.rs          # Routing, static file serving, gzip, background tasks
    ├── api.rs           # REST API endpoints + HTTP auth middleware
    ├── files.rs         # File transfer (upload/download/list)
    ├── websocket.rs     # WS handler: upgrade, connection slots, lifecycle
    └── websocket/
        ├── handshake.rs         # resize/join handshake before the main loop
        ├── auth.rs              # WS-phase authentication
        ├── message_loop.rs      # input/resize/ping/file_list dispatch
        ├── pty_io.rs            # PTY <-> WS bridging, output coalescing, heartbeat
        ├── session_lifecycle.rs # join/create/leave, session cleanup
        └── utils.rs             # send helpers, client IP resolution
```

Other top-level paths: `build.rs` (emits a `rerun-if-changed` watch over
`static/`), `static/` (xterm.js frontend, embedded at compile time), `tests/`
(integration tests). The original ttyd C checkout lives in `ttyd/`, which is
gitignored and usually absent.

### Key Design Decisions

- **Error handling**: All errors typed via `thiserror`, propagated with `?`. Never silenced.
- **Concurrent I/O**: PTY output coalesced to reduce syscall frequency.
- **Memory**: `broadcast::channel(1024)` bounds per-session memory. `Arc` for shared state.
- **PTY cleanup**: close master fd + SIGHUP + non-blocking reap, with a global SIGCHLD handler reaping terminated children asynchronously.
- **Upload safety**: `UploadFileGuard` (RAII) ensures partial files are removed on all error paths. The target directory's write permission is checked *before* the multipart body is read, so an unwritable target fails fast with 403 instead of a mid-stream 500.
- **Auth validators built once**: `AuthMethod` is constructed at router startup, not per request — Argon2 verification costs ~100ms and must not sit on the hot path.
- **Separate rate-limit buckets**: WebSocket auth and the file endpoints have their own limiters, so browsing files cannot exhaust the auth budget (and vice versa).
- **Idle-connection liveness**: the server sends protocol-level WebSocket pings every 30s and drops the connection after 90s without a pong.

## Configuration & CLI

Config file lookup (`load_config` in `src/main.rs`): `-c/--config <path>` →
else `config.toml` next to the running executable → else built-in defaults.
The repo-root `config.toml` is gitignored; `config.example.toml` is the reference.

Rules that must keep holding (enforced by `deny_unknown_fields` and `Config::validate`):

- A config file must define top-level `bind` and `command`; every other
  top-level key and all `[session]` / `[validation]` / `[rate_limit]` /
  `[file_transfer]` / `[compression]` / `[audit]` / `[auth]` fields may be
  omitted and fall back to defaults.
- Only the **top-level** `Config` rejects unknown keys. The nested tables do
  not use `deny_unknown_fields`, so an unknown key *inside* a table is
  silently ignored.
- CLI flags override config values only when explicitly passed. `--auth`
  replaces the entire `[auth]` table with basic auth from `--username`/`--password`.
  `--trust-proxy` and `--allow-unauthenticated` are three-state
  (`--flag=true|false`, bare flag = `true`) so they can override in either direction.
- Logging filter precedence differs: `--log-level` → `RUST_LOG` → config `log_level`.
- Validation runs after the merge, before the socket binds. Refused at startup:
  empty `command`, `max_connections = 0`, invalid `log_level`, unknown
  `session.mode`, inverted terminal size ranges, `rate_limit` zeros,
  `compression.level` outside 1..=9, and any unauthenticated non-loopback bind
  or unauthenticated file transfer without its explicit opt-in.
- Check any config without binding a port: `ttyd-rs -t --config <path>` (exit 0 = valid).

## WebSocket Protocol

JSON messages with types: `auth`/`auth_ok`/`auth_fail`, `input`/`output`, `resize`, `ping`/`pong`, `error`/`disconnect`, `ready`, `join`, `file_list`/`file_list_result`.

Handshake order: `auth` (when configured) → `resize` and optionally `join`
(accepted in either order; missing dimensions fall back to 80x24) → `ready`.
After `ready`, the main loop only handles `input`, `resize`, `ping`, `file_list`.

Full spec, including the error codes the server actually emits:
[docs/PROTOCOL.md](docs/PROTOCOL.md)

## REST API

See [docs/PROJECT_STATUS.md](docs/PROJECT_STATUS.md) for the complete endpoint reference with examples.

## Security

See [docs/PROJECT_STATUS.md](docs/PROJECT_STATUS.md) for the full security assessment.

Key implementation notes:
- Constant-time comparison via `subtle` crate (timing-attack resistant)
- Argon2id password hashing with random salt (basic auth); the configured `password` value (config file or `--password`) may be a plaintext password or an Argon2id PHC hash — plaintext logs a startup warning, hashes are generated with `ttyd-rs --hash-password`; SHA-256 + constant-time comparison for token auth
- WebSocket auth **fails closed**: if `[auth]` is configured but the validator cannot be built, connections are rejected rather than allowed through
- Rate limiting is per-IP with a 10 requests / 60s sliding window default; `trust_proxy` disabled by default to prevent IP spoofing
- Reconnection window (default 60s) preserves session state

## Important Notes

- Linux only; no Windows or macOS support (simplifies PTY handling)
- TLS is not built-in; use a reverse proxy (nginx, Caddy) for HTTPS
- Performance targets: <50ms startup, <10MB idle memory, >1000 concurrent connections
- `docs/PROJECT_STATUS.md` is the technical reference; keep `README.md`, `docs/`, and `config.example.toml` consistent with it when behavior changes
