# ttyd-rs Project Status

**Last Updated**: 2026-06-19
**Version**: 0.1.0
**Status**: Near Production Ready (one blocking issue)

---

## Quality Gate Status

| Check | Status |
|-------|--------|
| `cargo fmt -- --check` | ✅ Pass |
| `cargo clippy -- -D warnings` | ✅ Pass |
| `cargo test` | ✅ 161 tests passing |
| `cargo build --release` | ✅ Success |

---

## Project Statistics

- **Rust source**: ~6,260 lines across 18 .rs files
- **Tests**: 161 (unit + integration)
- **Frontend**: index.html with xterm.js integration
- **Dependencies**: See Cargo.toml for current list

---

## Production Readiness Assessment

### 1. Error Handling — Excellent

- Strict lint rules: `unwrap-used = "deny"`, `expect-used = "deny"`, `panic = "deny"`
- Zero `unwrap()`/`expect()`/`panic!()` in production code
- All modules use `Result` + `?` operator for error propagation
- Typed error enums: `ConfigError`, `SessionError`, `PtyError`, `ValidationError` (all via `thiserror`)

### 2. Security — Good

- **Authentication**: Constant-time comparison via `subtle` crate (prevents timing attacks)
- **Password storage**: SHA-256 hashed, raw credentials never persist beyond construction
- **Input validation**: Terminal size bounds, payload size limits, credential format checks
- **No path traversal**: Static files embedded at compile time via `rust-embed`
- **No XSS risk**: Server does not reflect user input into HTML
- **Rate limiting**: Sliding window algorithm, per-IP tracking
- **Audit logging**: 8 event types (connection, auth, session, error)

### 3. Resource Management — Excellent

- **PTY cleanup**: 5-stage process cleanup (SIGHUP → poll → SIGKILL → non-blocking reap → background reaper thread)
- **FD management**: `FD_CLOEXEC` set on PTY FDs, child calls `close_fds_above()`, parent uses `dup()` for independent FDs per task
- **Memory safety**: `Arc` for reference counting, `broadcast::channel(512)` bounds memory per session

### 4. Concurrency Safety — Good

- Lock ordering consistent: sessions → clients (no deadlock risk)
- Session cleanup uses atomic operations to eliminate TOCTOU races
- `CancellationToken` for coordinated graceful shutdown

---

## Implemented Features

### M1: Foundation ✅
- CLI with clap (all flags from original ttyd)
- TOML configuration file support
- tracing / tracing-subscriber logging
- thiserror error handling

### M2: Core Server ✅
- axum HTTP server with routing
- WebSocket upgrade handler
- Bidirectional message handling
- Static file serving via rust-embed

### M3: PTY Management ✅
- PTY creation via nix openpty + fork
- Signal handling (SIGHUP, SIGKILL, TIOCSWINSZ)
- Process lifecycle management
- Zombie process reaping

### M4: Security Layer ✅
- Basic Auth with SHA-256 password hashing
- Token Auth with constant-time comparison (subtle crate)
- Rate limiting (sliding window, per-IP)
- Input validation (terminal size, payload, credentials)
- Audit logging (connection, auth, session events)

### M5: Session Management ✅
- SessionManager with lifecycle management
- Session modes: isolated, shared_readonly, shared_readwrite
- Session timeout and auto-cleanup (30s interval)
- REST API for session management
- Broadcast channel for shared-session output

### M6: Frontend Integration ✅
- xterm.js terminal emulation
- Login form (basic auth / token auth)
- Auto-reconnect with exponential backoff
- Session join via URL parameter
- Terminal resize handling

---

## Module Structure

```
src/
├── main.rs              Entry point, CLI, config loading
├── config.rs            Configuration types and validation
├── server.rs            Module declaration
├── server/
│   ├── http.rs          HTTP server, routing, static files
│   ├── websocket.rs     WebSocket handler, session management
│   └── api.rs           REST API endpoints
├── pty.rs               Module declaration
├── pty/
│   ├── process.rs       PTY process spawning and management
│   └── session.rs       PTY session wrapper
├── auth.rs              Module declaration
├── auth/
│   ├── basic.rs         Basic authentication
│   └── token.rs         Token authentication
├── protocol.rs          WebSocket message types
├── session.rs           Session manager, multi-client support
├── audit.rs             Audit logging
├── rate_limit.rs        Rate limiting
├── validation.rs        Input validation
└── assets.rs            Static asset embedding
```

---

## REST API

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | /api/health | Health check |
| GET | /api/config | Client-facing config (auth method) |
| GET | /api/sessions | List active sessions |
| GET | /api/sessions/:id | Get session info |
| DELETE | /api/sessions/:id | Terminate session |
| GET | /api/stats | Server statistics |

---

## WebSocket Protocol

| Direction | Type | Description |
|-----------|------|-------------|
| C→S | auth | Authentication request |
| S→C | auth_ok | Auth success (with client_id) |
| S→C | auth_fail | Auth failure (with reason) |
| C→S | input | Terminal input |
| S→C | output | Terminal output |
| C→S | resize | Terminal resize |
| C→S | join | Join existing session |
| C→S / S→C | ping / pong | Keepalive |
| S→C | ready | Session ready notification |
| S→C | disconnect | Session ended |
| S→C | error | Error message |

---

## Configuration

| Flag | Default | Description |
|------|---------|-------------|
| -p, --port | 7681 | Listen port |
| -b, --bind | 127.0.0.1 | Bind address |
| -s, --shell | bash | Shell command |
| --session-mode | isolated | Session mode |
| --session-timeout | 3600 | Session timeout (seconds) |
| --reconnect-window | 60 | Reconnect window (seconds) |
| --max-connections | 100 | Max concurrent connections |
| --auth | false | Enable authentication |
| --trust-proxy | false | Trust proxy headers |
| --audit | false | Enable audit logging |

---

## Known Issues

### Blocking (Must Fix Before Production)

| Issue | Location | Description |
|-------|----------|-------------|
| Token validation rejects valid tokens | `validation.rs:66-82` | `validate_credentials` enforces base64 charset, blocking tokens with `-`, `_`, or other non-base64 characters. Token auth should skip this validation or use a different path. |

### Non-Blocking (Fix in Next Release)

| Severity | Issue | Location | Description |
|----------|-------|----------|-------------|
| Low | Connection counter race | `websocket.rs:96-109` | `load` + `fetch_add` with `Relaxed` ordering is not atomic. Use `compare_exchange`. |
| Low | Audit log reopened on every write | `audit.rs:155-170` | No persistent file handle or log rotation. Risk of syscall overhead and disk exhaustion. |
| Low | SHA-256 without salt | `auth/basic.rs:22-29` | Acceptable for single-user in-memory scenario, but bcrypt/argon2 more robust against hash leaks. |
| Info | `Box<dyn Error>` for top-level handlers | `http.rs:25`, `websocket.rs:161` | Typed error enums would improve debuggability. |

---

## Deployment Recommendations

1. **Fix Token validation bug** — blocking issue for token auth users
2. **Enable authentication** — configure `[auth]` section in config
3. **Enable audit logging** — configure `[audit]` section for security monitoring
4. **Use reverse proxy** — nginx/Caddy for HTTPS termination (TLS not built-in)
5. **Tune limits** — adjust `max_connections` and rate limit parameters for expected load
6. **Set `trust_proxy`** — enable only when behind a trusted reverse proxy

---

## Platform Support

- ✅ **Linux**: Full support (kernel 5.9+ recommended for `close_range`)
- ❌ **macOS**: Not supported (removed to simplify codebase)
- ❌ **Windows**: Not supported (Unix PTY required)

---

## Known Limitations

1. **No built-in TLS**: Use a reverse proxy (nginx, Caddy) for HTTPS
2. **No session persistence**: Sessions are lost on server restart
3. **No file transfer**: No upload/download support

---

*Last updated: 2026-06-19*
