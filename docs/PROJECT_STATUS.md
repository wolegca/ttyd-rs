# ttyd-rs Project Status

**Last Updated**: 2026-07-30
**Version**: 0.4.0
**Status**: Production Ready

---

## Quality Gate Status

| Check | Status |
|-------|--------|
| `cargo fmt -- --check` | ✅ Pass |
| `cargo clippy -- -D warnings` | ✅ Pass |
| `cargo test` | ✅ 190 tests passing |
| `cargo build --release` | ✅ Success |

---

## Project Statistics

- **Rust source**: ~7,000 lines across 19 .rs files
- **Tests**: 190 (unit + integration)
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
- Three-dot kebab menu (upload/browse files, visible only when authenticated)
- Connection status indicator (green=connected, yellow=login required, red=disconnected)

### File Transfer ✅
- HTTP multipart upload with streaming size limit enforcement
- Streaming file download with path traversal protection
- Directory listing via WebSocket (`file_list` / `file_list_result` messages)
- File panel with subdirectory navigation and hidden file toggle
- Dynamic base directory: follows terminal session's `$PWD` via `/proc/<pid>/cwd`
- Protected by existing auth middleware (basic/token)
- Overwrite protection (409 Conflict + client confirm)
- Hidden file filtering (default off, user-toggleable)
- Content-Disposition filename sanitization
- Session isolation: invalid session_id returns 404 (no fallback)
- Configurable: enable/disable, fixed directory override, max upload size

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
│   ├── api.rs           REST API endpoints
│   └── files.rs         File transfer (upload/download/list)
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
| POST | /api/files/upload | Upload file (multipart, auth required) |
| GET | /api/files/download?path= | Download file (auth required) |
| GET | /api/files/list | List files in working dir (auth required) |

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
| C→S | file_list | Request directory listing |
| S→C | file_list_result | Directory listing response |
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

None — all blocking issues resolved.

### Non-Blocking (Fix in Next Release)

| Severity | Issue | Location | Description |
|----------|-------|----------|-------------|
| Low | Connection counter race | `websocket.rs:96-109` | `load` + `fetch_add` with `Relaxed` ordering is not atomic. Use `compare_exchange`. |
| Low | Audit log reopened on every write | `audit.rs:155-170` | No persistent file handle or log rotation. Risk of syscall overhead and disk exhaustion. |
| Low | SHA-256 without salt | `auth/basic.rs:22-29` | Acceptable for single-user in-memory scenario, but bcrypt/argon2 more robust against hash leaks. |
| Info | `Box<dyn Error>` for top-level handlers | `http.rs:25`, `websocket.rs:161` | Typed error enums would improve debuggability. |

### Resolved

| Issue | Resolution |
|-------|------------|
| Token validation rejects valid tokens | Fixed in v0.2.10: separated `validate_token_credentials` (length-only) from `validate_credentials` (base64 charset for basic auth) |

---

## Deployment Recommendations

1. **Enable authentication** — configure `[auth]` section in config
2. **Enable audit logging** — configure `[audit]` section for security monitoring
3. **Use reverse proxy** — nginx/Caddy for HTTPS termination (TLS not built-in)
4. **Tune limits** — adjust `max_connections` and rate limit parameters for expected load
5. **Set `trust_proxy`** — enable only when behind a trusted reverse proxy
6. **Configure file transfer** — set `[file_transfer]` dir or rely on dynamic `$PWD` tracking

---

## Platform Support

- ✅ **Linux**: Full support (kernel 5.9+ recommended for `close_range`)
- ❌ **macOS**: Not supported (removed to simplify codebase)
- ❌ **Windows**: Not supported (Unix PTY required)

---

## Known Limitations

1. **No built-in TLS**: Use a reverse proxy (nginx, Caddy) for HTTPS
2. **No session persistence**: Sessions are lost on server restart

---

*Last updated: 2026-07-30*
*Version: 0.4.0*
