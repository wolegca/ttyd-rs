# ttyd-rs Project Status

**Last Updated**: 2026-08-21
**Version**: 0.6.1
**Status**: Production Ready

---

## Quality Gate Status

| Check | Status |
|-------|--------|
| `cargo fmt -- --check` | ✅ Pass |
| `cargo clippy -- -D warnings` | ✅ Pass |
| `cargo test` | ✅ 234 tests passing |
| `cargo build --release` | ✅ Success |

---

## Project Statistics

- **Rust source**: ~7,000 lines across 19 .rs files
- **Tests**: 234 (unit + integration)
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
- **Password storage**: Argon2id hashed (random salt per instance), raw credentials never persist beyond construction
- **Input validation**: Terminal size bounds, payload size limits, credential format checks
- **No path traversal**: Static files embedded at compile time via `rust-embed`; `..` path segments are explicitly rejected
- **No XSS risk**: Server does not reflect user input into HTML
- **Security headers on static responses**: strict same-origin `Content-Security-Policy`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`
- **Rate limiting**: Sliding window algorithm, per-IP tracking
- **Audit logging**: 8 event types (connection, auth, session, error)

### 3. Resource Management — Excellent

- **PTY cleanup**: 5-stage process cleanup (SIGHUP → poll → SIGKILL → non-blocking reap → background reaper thread)
- **FD management**: `FD_CLOEXEC` set on PTY FDs, child calls `close_fds_above()`, parent uses `dup()` for independent FDs per task
- **Memory safety**: `Arc` for reference counting, `broadcast::channel(512)` bounds memory per session

### 4. Concurrency Safety — Good

- Lock ordering consistent: sessions → clients (no deadlock risk)
- Session cleanup uses atomic operations to eliminate TOCTOU races
- Connection limit enforced atomically (`compare_exchange` loop in `AppState::try_acquire_connection`), so `active_connections` can never exceed `max_connections` under concurrency
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
- Basic Auth with Argon2id password hashing (random salt)
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
- Upload error handling: drains multipart body before returning 409/413 (prevents `ERR_CONNECTION_ABORTED`)
- Frontend pre-checks file size and existence before upload (avoids unnecessary request)
- `max_upload_size` and `file_transfer_enabled` exposed via `/api/config`
- Streaming file download with path traversal protection
- Directory listing via WebSocket (`file_list` / `file_list_result` messages)
- File panel with subdirectory navigation and hidden file toggle
- Dynamic base directory: follows terminal session's `$PWD` via `/proc/<pid>/cwd`
- Protected by existing auth middleware (basic/token)
- Overwrite protection (409 Conflict + client confirm)
- Hidden file filtering (default off, user-toggleable)
- Content-Disposition filename sanitization
- Session isolation: invalid session_id returns 404 (no fallback)
- `UploadFileGuard` (RAII): automatic partial-file cleanup on all error paths
- 6 dedicated upload integration tests (success, conflict, overwrite, size-exceeded, missing field, binary)
- Configurable: enable/disable, fixed directory override, max upload size

### Static File Serving ✅
- Embedded at compile time via `rust-embed` (no filesystem access at runtime)
- **Gzip compression** for static assets only (API and WebSocket responses are never compressed); configurable via `[compression]` (`enabled`, `level`)
- **Already-compressed formats skipped**: `font/*` and `image/x-icon` are not gzipped (avoids wasted CPU and size bloat)
- **Caching**: vendor assets served with `Cache-Control: public, max-age=31536000, immutable`; `index.html` served with `Cache-Control: no-cache` so clients pick up a new entry point after an upgrade
- **Security headers**: strict same-origin `Content-Security-Policy`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`
- **Path traversal**: `..` segments explicitly rejected (404)
- 4 dedicated tests covering font non-compression, cache headers, security headers, and path traversal

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
| GET | /api/config | Client-facing config (auth method, max_upload_size, file_transfer_enabled) |
| GET | /api/sessions | List active sessions |
| GET | /api/sessions/:id | Get session info |
| DELETE | /api/sessions/:id | Terminate session |
| GET | /api/stats | Server statistics |
| POST | /api/files/upload | Upload file (multipart, auth required) |
| GET | /api/files/download?path= | Download file (auth required) |
| GET | /api/files/list | List files in working dir (auth required) |

### Examples

```bash
curl http://localhost:7681/api/health
curl http://localhost:7681/api/sessions
curl http://localhost:7681/api/stats

# File operations (with auth)
curl -u admin:secret -F "file=@myfile.txt" http://localhost:7681/api/files/upload
curl -u admin:secret -o out.txt "http://localhost:7681/api/files/download?path=myfile.txt"
curl -u admin:secret http://localhost:7681/api/files/list
```

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

None. Previously listed items have been resolved or reclassified:

- **SHA-256 without salt** — Resolved in v0.6.1 (see below).
- **`Box<dyn Error>` for top-level handlers** — Reclassified as an accepted design decision: `start_server` (`http.rs`) is the process entry point and its error is only logged by `main`; all lower layers already use typed `thiserror` enums, so a top-level error type would add no actionable information.

### Resolved

| Issue | Resolution |
|-------|------------|
| SHA-256 without salt (basic auth) | Fixed in v0.6.1: password hashing migrated to Argon2id with a per-instance random salt (`argon2` crate). To keep authentication cheap under load, the API middleware now builds its authenticator once at router construction instead of on every request, and WebSocket auth defers hashing until after the rate-limit check and the client's auth message (connection spam that never authenticates costs no hashing). |
| Token validation rejects valid tokens | Fixed in v0.2.10: separated `validate_token_credentials` (length-only) from `validate_credentials` (base64 charset for basic auth) |
| Connection counter race (`active_connections` could exceed `max_connections`) | Fixed: `load` + `fetch_add` replaced by atomic `compare_exchange` loop in `AppState::try_acquire_connection`; covered by concurrency regression tests in `websocket.rs` |
| Audit log reopened on every write | Fixed: `AuditLogger` holds a persistent `Arc<tokio::sync::Mutex<Option<tokio::fs::File>>>` handle opened once at startup via `prepare()` |

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
