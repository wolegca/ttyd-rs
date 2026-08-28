# ttyd-rs Project Status

**Status**: Stable release

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
- **Safe network exposure**: unauthenticated terminals may bind only to loopback by default; a non-loopback bind requires authentication or the explicit `allow_unauthenticated = true` reverse-proxy opt-in
- **Password storage**: Argon2id hashed (random salt per instance), raw credentials never persist beyond construction; the config file / CLI accepts either a plaintext password (logged with a startup warning) or a pre-hashed Argon2id PHC string generated via `ttyd-rs --hash-password`
- **Input validation**: Terminal size bounds, payload size limits, credential format checks
- **No path traversal**: Static files embedded at compile time via `rust-embed`; `..` path segments are explicitly rejected
- **No XSS risk**: Server does not reflect user input into HTML
- **Security headers on static responses**: strict same-origin `Content-Security-Policy`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` (see Static File Serving below)
- **Rate limiting**: Sliding window algorithm, per-IP tracking, with a separate bucket for the file endpoints
- **Audit logging**: JSONL events; six types are emitted (`connection_opened`, `connection_closed`, `auth_success`, `auth_failure`, `session_started`, `error_occurred`) and two more — `command_executed`, `session_ended` — are defined in the enum but not wired up yet

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
- Rate limiting: sliding window, per-IP, with a separate limiter for file operations
- Input validation: terminal size, payload, credentials
- Audit logging: `connection_opened` / `connection_closed`, `auth_success` / `auth_failure`, `session_started`, `error_occurred`
- WebSocket auth fails closed when `[auth]` cannot be built, and defers credential hashing until after the rate-limit check

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
- Pre-flight directory write-permission check (`access(2)`/`W_OK`): an unwritable target returns 403 before any upload bytes are read, instead of a mid-stream 500
- 9 dedicated upload integration tests (success, conflict, overwrite, size-exceeded, missing field, binary, read-only directory, no leftover temp files, original intact after a failed size check)
- Configurable: enable/disable, fixed directory override, max upload size

### Static File Serving ✅
- Embedded at compile time via `rust-embed` (no filesystem access at runtime)
- **Gzip compression** for static assets only (API and WebSocket responses are never compressed); configurable via `[compression]` (`enabled`, `level`)
- **Already-compressed formats skipped**: `font/*` and `image/x-icon` are not gzipped (avoids wasted CPU and size bloat)
- **Caching**: vendor assets served with `Cache-Control: public, max-age=31536000, immutable`; `index.html` served with `Cache-Control: no-cache` so clients pick up a new entry point after an upgrade
- **Security headers**: strict same-origin `Content-Security-Policy`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`
- **Path traversal**: `..` segments explicitly rejected (404)
- 6 dedicated tests covering font non-compression, cache headers, security headers, path traversal, `[compression] enabled = false`, and API responses staying uncompressed

---

## Module Structure

See [Project Structure](../CLAUDE.md#project-structure) in CLAUDE.md for the annotated `src/` directory tree.

---

## REST API

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| GET | /api/health | public | Health check — `{status, version}` |
| GET | /api/config | public | Client-facing config (`auth_method` always; `max_upload_size` and `file_transfer_enabled` only when auth is disabled) |
| GET | /ws | over socket | WebSocket endpoint. Authentication happens on the socket, not via HTTP headers. Returns 503 once `max_connections` is reached |
| GET | /api/sessions | required | List active sessions — `{sessions, total}` |
| GET | /api/sessions/:id | required | Session info; 404 `{error}` if unknown |
| DELETE | /api/sessions/:id | required | Terminate session; 204 on success, 404 if unknown |
| GET | /api/stats | required | Server statistics |
| POST | /api/files/upload | required¹ | Multipart upload; query `session_id`, `overwrite`. Returns `{filename, size}`; 403 if the target directory is not writable (checked before the body is read), 409 if the file exists and `overwrite` is not set, 413 if it exceeds `max_upload_size`, 404 for an unknown `session_id` |
| GET | /api/files/download?path= | required¹ | Streaming download with `Content-Disposition`; 403 on traversal or an unresolvable path, 404 if missing |
| GET | /api/files/list | required¹ | Directory listing (`path` defaults to `.`); dotfiles hidden unless `show_hidden=true` |
| GET | / and /* | public | Embedded static assets (`index.html`, `vendor/*`), gzipped and security-headered |

¹ Present only when `[file_transfer] enabled = true`, and additionally guarded by
a dedicated rate limiter that answers 429 with
`{error: "Rate limit exceeded. Try again in N seconds"}`.
The auth middleware wraps the whole protected group (sessions, stats, files)
whenever `[auth]` is configured, so file routes are never reachable without
credentials in that setup. `create_router` applies it uniformly;
`[file_transfer] allow_unauthenticated` is not a routing exemption but a
startup gate — it must be `true` for the server to boot at all when file
transfer is enabled and no `[auth]` section exists.

Protected routes answer 401 `{error: "Unauthorized"}` when credentials are
missing or invalid: send `Authorization: Basic <base64 user:password>` for
basic auth or `Authorization: Bearer <token>` for token auth.

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
| -p, --port | config `bind` / 7681 | Listen port; applied to the configured bind address |
| -b, --bind | 127.0.0.1 | Bind address: a bare IP (IPv4 or IPv6) or an `ip:port` socket address |
| -c, --config | — | Configuration file path. When omitted, a `config.toml` next to the executable is loaded if present, otherwise built-in defaults apply |
| -s, --shell | bash --login | Shell command; supports shell-style quoting/escaping (e.g. `-s 'bash -c "echo hi"'`) |
| -w, --working-dir | `$HOME` | Working directory for the shell |
| --log-level | info | Log level: bare level name (`trace`, `debug`, `info`, `warn`, `error`, `off`, case-insensitive) or an EnvFilter directive (e.g. `ttyd_rs=debug`); invalid values are rejected at startup. Resolution order: `--log-level` → `RUST_LOG` → config `log_level` |
| --session-mode | isolated | Session mode: `isolated`, `shared-ro`/`shared_readonly`, `shared-rw`/`shared_readwrite` |
| --session-timeout | 3600 | Session timeout (seconds); 0 disables it |
| --reconnect-window | 60 | How long empty sessions are kept for reconnection (seconds) |
| --max-connections | 100 | Max concurrent connections |
| --auth | false | Enable basic authentication. Replaces the whole `[auth]` table from the config file; requires `--username` and `--password` |
| --username | — | Basic auth username; requires `--auth` |
| --password | — | Basic auth password (plaintext or an Argon2id PHC hash); requires `--auth` |
| --audit | false | Enable audit logging |
| --audit-file | — | Audit log file path (requires `--audit`) |
| --trust-proxy | false | Trust `X-Real-IP` / `X-Forwarded-For` for the client IP. Three-state: accepts `--flag=true\|false`; the bare flag means `true` |
| --allow-unauthenticated | false | Explicitly allow an unauthenticated non-loopback terminal (trusted reverse proxy only). Three-state, same rules as `--trust-proxy` |
| --no-file-transfer | false | Disable the upload/download/list endpoints entirely |
| --hash-password | — | Read a password from stdin, print its Argon2id hash, and exit |
| -t, --check-config | — | Load the configuration, apply CLI overrides, run validation, and exit without starting the server. Exit code 0 = valid |

Token auth and the `[validation]`, `[rate_limit]`, `[compression]`, and
`[file_transfer] allow_unauthenticated` settings have no CLI equivalents — they
are config-file only.

### Configuration Validation

The configuration is validated after merging config file + CLI overrides
(`Config::validate()`), before any port is bound:

- `command` must not be empty.
- `max_connections` must be greater than 0.
- `log_level` must be a bare level name or an EnvFilter directive containing
  `=`; anything else is rejected (previously it silently filtered out nearly
  all log output).
- `session.mode` must be one of `isolated`, `shared-ro`/`shared_readonly`, or
  `shared-rw`/`shared_readwrite` (case-insensitive).
- `[validation]` ranges must be ordered: `min_cols` < `max_cols` and
  `min_rows` < `max_rows`.
- `[rate_limit]` `max_requests` and `window_seconds` must both be greater than 0.
- `[compression] level` must be within 1..=9 when compression is enabled.
- Auth `method` matching is case-insensitive (`basic` / `token`); `basic`
  requires both `username` and `password`, `token` requires `token`, and a
  `password` starting with `$argon2id$` must be a well-formed PHC string.
- Safety guards refuse startup unless explicitly overridden: file transfer
  without `[auth]` (needs `[file_transfer] allow_unauthenticated = true` or
  `--no-file-transfer`), and an unauthenticated terminal bound to a
  non-loopback address (needs `allow_unauthenticated = true`).
- Unknown **top-level** fields or sections in the config file are rejected
  (`deny_unknown_fields` on `Config`), so a typo like `[file_tranfer]` fails
  loudly instead of falling back to defaults. Only the top level is strict
  today: the nested tables do not declare `deny_unknown_fields`, so an
  unrecognized key inside one of them is still silently ignored.

Pre-flight check for deployments: `ttyd-rs -t --config /etc/ttyd-rs/config.toml`.

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
3. **Validate before restarting** — run `ttyd-rs -t --config /etc/ttyd-rs/config.toml` (or as a systemd `ExecStartPre=`) to catch configuration errors before they take the service down
4. **Enable audit logging** — configure `[audit]` section for security monitoring
5. **Use reverse proxy** — nginx/Caddy for HTTPS termination (TLS not built-in)
6. **Tune limits** — adjust `max_connections` and rate limit parameters for expected load
7. **Set `trust_proxy`** — enable only when behind a trusted reverse proxy (a startup warning is logged otherwise)
8. **Configure file transfer** — set `[file_transfer]` dir or rely on dynamic `$PWD` tracking
9. **Rotate audit logs** — configure logrotate (copytruncate) for the audit log file

---

## Platform Support

- ✅ **Linux**: Full support (kernel 5.9+ recommended for `close_range`)
- ❌ **macOS**: Not supported (removed to simplify codebase)
- ❌ **Windows**: Not supported (Unix PTY required)

---

## Known Limitations

1. **No built-in TLS**: Use a reverse proxy (nginx, Caddy) for HTTPS
2. **No session persistence**: Sessions are lost on server restart
