# ttyd-rs Project Status

**Last Updated**: 2026-08-21
**Version**: 0.7.0
**Status**: Production Ready

---

## Quality Gate Status

| Check | Status |
|-------|--------|
| `cargo fmt -- --check` | ✅ Pass |
| `cargo clippy -- -D warnings` | ✅ Pass |
| `cargo test` | ✅ All tests passing |
| `cargo build --release` | ✅ Success |

---

## Production Readiness Assessment

### 1. Error Handling — Excellent

- Strict lint rules: `unwrap-used = "deny"`, `expect-used = "deny"`, `panic = "deny"`
- Zero `unwrap()`/`expect()`/`panic!()` in production code
- All modules use `Result` + `?` operator for error propagation
- Typed error enums: `ConfigError`, `SessionError`, `PtyError`, `ValidationError` (all via `thiserror`)

### 2. Security — Good

- **Authentication**: Constant-time comparison via `subtle` crate (prevents timing attacks)
- **Password storage**: Argon2id hashed (random salt per instance), raw credentials never persist beyond construction; the config file / CLI accepts either a plaintext password (logged with a startup warning) or a pre-hashed Argon2id PHC string generated via `ttyd-rs --hash-password`
- **Input validation**: Terminal size bounds, payload size limits, credential format checks
- **No path traversal**: Static files embedded at compile time via `rust-embed`; `..` path segments are explicitly rejected
- **No XSS risk**: Server does not reflect user input into HTML
- **Security headers on static responses**: strict same-origin `Content-Security-Policy`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` (see [Static File Serving](#static-file-serving--))
- **Rate limiting**: Sliding window algorithm, per-IP tracking
- **Audit logging**: 8 event types (connection, auth, session, error)

### 3. Resource Management — Excellent

- **PTY cleanup**: close master fd + SIGHUP + non-blocking reap, with a global SIGCHLD handler reaping terminated children asynchronously
- **FD management**: `FD_CLOEXEC` set on PTY FDs, child calls `close_fds_above()`, parent uses `dup()` for independent FDs per task
- **Memory safety**: `Arc` for reference counting, `broadcast::channel(1024)` bounds memory per session

### 4. Concurrency Safety — Good

- Lock ordering consistent: sessions → clients (no deadlock risk)
- Session cleanup uses atomic operations to eliminate TOCTOU races
- Connection limit enforced atomically (`compare_exchange` loop in `AppState::try_acquire_connection`), so `active_connections` can never exceed `max_connections` under concurrency
- `CancellationToken` for coordinated graceful shutdown

---

## Implemented Features

Milestone history (M1–M6) is maintained in [docs/ROADMAP.md](ROADMAP.md#development-history). Below are implementation details beyond that summary.

### Security Layer (M4) — details
- Password can be configured as an Argon2id PHC hash (`ttyd-rs --hash-password` generates it; plaintext triggers a startup warning)
- Rate limiting: sliding window, per-IP
- Input validation: terminal size, payload, credentials
- Audit logging: connection, auth, session events

### Session Management (M5) — details
- Session timeout and auto-cleanup (30s interval)

### Frontend Integration (M6) — details
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

See [Project Structure](../CLAUDE.md#project-structure) in CLAUDE.md for the annotated `src/` directory tree.

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

See [docs/PROTOCOL.md](PROTOCOL.md) for the full message type reference, state machine, and error codes.

---

## Configuration

| Flag | Default | Description |
|------|---------|-------------|
| -p, --port | 7681 | Listen port |
| -b, --bind | 127.0.0.1 | Bind address: a bare IP (IPv4 or IPv6) or an `ip:port` socket address |
| -s, --shell | bash | Shell command; supports shell-style quoting/escaping (e.g. `-s 'bash -c "echo hi"'`) |
| --session-mode | isolated | Session mode |
| --session-timeout | 3600 | Session timeout (seconds) |
| --reconnect-window | 60 | Reconnect window (seconds) |
| --max-connections | 100 | Max concurrent connections |
| --auth | false | Enable authentication |
| --trust-proxy | false | Trust proxy headers |
| --audit | false | Enable audit logging |
| --audit-file | — | Audit log file path (requires `--audit`) |
| --hash-password | — | Read a password from stdin, print its Argon2id hash, and exit |

---

## Known Issues

### Blocking (Must Fix Before Production)

None — all blocking issues resolved.

### Non-Blocking (Fix in Next Release)

None. Previously listed items have been resolved or reclassified:

- **SHA-256 without salt** — Resolved in v0.6.1 (see below).
- **`Box<dyn Error>` for top-level handlers** — Reclassified as an accepted design decision: `start_server` (`http.rs`) is the process entry point and its error is only logged by `main`; all lower layers already use typed `thiserror` enums, so a top-level error type would add no actionable information.
- **Token auth uses unsalted SHA-256** — Accepted trade-off (documented): token comparison hashes the *incoming* credential and compares digests in constant time, so the config file stores a digest rather than plaintext. Because tokens are high-entropy random strings (not user-chosen passwords), a salt adds little; use long random tokens (`openssl rand -hex 32`). Prefer Argon2id basic auth when password-based login is acceptable.
- **Audit log has no rotation** — Accepted trade-off (documented): the audit file grows unboundedly and write failures are only logged via tracing. Run logrotate (copytruncate mode) against `audit.log_file`, or ship the file with a log collector.
- **`/api/config` exposes the auth method publicly** — Accepted: the frontend calls it unauthenticated on page load to decide whether to show the login overlay. Deployment details (`max_upload_size`, `file_transfer_enabled`) are only revealed when auth is disabled.

### Resolved

| Issue | Resolution |
|-------|------------|
| Plaintext password in configuration file | Fixed in v0.7.0: the `[auth]` password value (and `--password`) may be an Argon2id PHC hash string starting with `$argon2id$`; it is validated at startup (malformed hashes fail fast) and stored as-is, so the plaintext password never touches the config file. `ttyd-rs --hash-password` reads a password from stdin and prints the hash. Plaintext remains supported for backward compatibility but logs a startup warning. |
| SHA-256 without salt (basic auth) | Fixed in v0.6.1: password hashing migrated to Argon2id with a per-instance random salt (`argon2` crate). To keep authentication cheap under load, the API middleware now builds its authenticator once at router construction instead of on every request, and WebSocket auth defers hashing until after the rate-limit check and the client's auth message (connection spam that never authenticates costs no hashing). |
| Token validation rejects valid tokens | Fixed in v0.2.10: separated `validate_token_credentials` (length-only) from `validate_credentials` (base64 charset for basic auth) |
| Connection counter race (`active_connections` could exceed `max_connections`) | Fixed: `load` + `fetch_add` replaced by atomic `compare_exchange` loop in `AppState::try_acquire_connection`; covered by concurrency regression tests in `websocket.rs` |
| Audit log reopened on every write | Fixed: `AuditLogger` holds a persistent `Arc<tokio::sync::Mutex<Option<tokio::fs::File>>>` handle opened once at startup via `prepare()` |

---

## Deployment Recommendations

1. **Enable authentication** — configure `[auth]` section in config
2. **Store the password as an Argon2id hash** — generate with `ttyd-rs --hash-password`; keep the config file readable only by the service user (`chmod 600`)
3. **Enable audit logging** — configure `[audit]` section for security monitoring
4. **Use reverse proxy** — nginx/Caddy for HTTPS termination (TLS not built-in)
5. **Tune limits** — adjust `max_connections` and rate limit parameters for expected load
6. **Set `trust_proxy`** — enable only when behind a trusted reverse proxy (a startup warning is logged otherwise)
7. **Configure file transfer** — set `[file_transfer]` dir or rely on dynamic `$PWD` tracking
8. **Rotate audit logs** — configure logrotate (copytruncate) for the audit log file

---

## Platform Support

- ✅ **Linux**: Full support (kernel 5.9+ recommended for `close_range`)
- ❌ **macOS**: Not supported (removed to simplify codebase)
- ❌ **Windows**: Not supported (Unix PTY required)

---

## Known Limitations

1. **No built-in TLS**: Use a reverse proxy (nginx, Caddy) for HTTPS
2. **No session persistence**: Sessions are lost on server restart
