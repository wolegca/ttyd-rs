# Changelog

## 1.6.0 — 2026-11-26

### Added

- Mobile status panel: on viewports up to 768px wide the header status view
  renders as a centered drawer with a blurred backdrop, a title row with an
  explicit close button, scrollable content, and backdrop-tap dismissal, so the
  panel no longer overflows the screen.

### Changed

- Session output buffering is capped at 256 coalesced messages per subscriber
  (worst case 64 MiB for a client that stopped reading, down from 256 MiB);
  this still covers a full burst for several concurrently lagging clients.
- Audit timestamps are produced with `jiff` instead of `chrono`.
- Duplicated WebSocket/file helpers were consolidated into shared functions
  (`client_rate_limit_key`, `send_message_result`, `send_disconnect`,
  `report_rejection`, `json_err`, `session_not_found_message`,
  `build_session_info`) with unit tests; externally visible behavior is
  unchanged.
- Expanded test coverage for WebSocket close reasons, auth validators, session
  subscribe/exit ordering, PTY I/O heartbeat and output-lag paths, handshake
  resize/join ordering, file open/rename helpers, and CLI password hashing.
  Four fast heartbeat integration tests that were previously ignored now run in
  CI.
- Updated `clap`, `nix` (0.32), `rust-embed`, `toml` (1.2), `uuid`, `rand`,
  `sha2`, and the lockfile.

### Fixed

- Shared sessions could interleave duplicate output when concurrent clients
  each spawned their own PTY reader; the reader is now a session-level
  singleton (`start_reader_once`) that every shutdown path aborts, so the PTY
  child and its `Arc<Session>` are released reliably.
- The rate-limit key is now built by a single helper shared by the WebSocket
  upgrade, the file HTTP middleware, and the WebSocket `file_list` handler.
  Under `trust_proxy` the socket address is mixed in, so rotating `X-Real-IP`
  can no longer reuse another client's bucket.
- `/proc` metrics are read asynchronously and concurrently, so status requests
  no longer park the reactor thread on filesystem I/O; the rate limiter hot
  path no longer allocates a key per call, and token hashing pre-allocates the
  SHA-256 hex digest.

## 1.5.0 — 2026-10-15

### Added

- Server status quick view: a new `GET /api/status` endpoint (auth-protected)
  reports version, uptime, live connection/session counters, and metrics
  split into `application` (process RSS and CPU usage) and `server`
  (host-wide CPU, total/used/available memory, load averages, host uptime,
  kernel release) groups, all read directly from `/proc` with background
  CPU samplers instead of an extra dependency.
- Header status panel: an activity-icon dropdown (shown after authentication)
  renders the status snapshot split into Application / Sessions / Server
  groups and refreshes every 5 seconds while open.

## 1.4.0 — 2026-09-22

### Added

- Debian package releases now include a dedicated `ttyd-rs` system user, a
  safe default configuration, and a systemd service with pre-start config
  validation.
- Shortcut management now supports drag-and-drop and arrow-based reordering,
  with persisted order and inline label/sequence editing.

### Changed

- Transfer history remains available after completion, with explicit clearing,
  upload/download indicators, and progress scoped to the active batch.
- Updated `clap` and `rand` dependencies and refreshed the lockfile.

## 1.3.0 — 2026-09-11

### Added

- Configurable terminal shortcut bar: toggle it from the header, and manage
  shortcut visibility and custom terminal sequences from Settings. Configured
  combinations are captured before browser defaults when the browser permits
  it, helping with shortcuts such as `Ctrl+T`.
- Debian packaging with a dedicated system user, safe default configuration,
  and a systemd service unit.

### Changed

- Authentication and API internals now share an `Authenticator` abstraction;
  session timestamps use Unix time, file-transfer configuration is cached, and
  handshake capabilities advertise file-transfer availability.
- Startup output and error handling are more concise and actionable, with
  warnings for previously silent WebSocket, PTY, and file-operation failures.
- Connection liveness no longer reports a false pong timeout after a browser
  tab resumes from the background.
- Dependabot now ignores `dtolnay/rust-toolchain` updates that caused bogus
  MSRV bump pull requests.

## 1.2.2 — 2026-09-07

### Fixed

- Reconnect loop when a stale `?session=` URL parameter pointed to an already
  reclaimed session: the fatal `SESSION_NOT_FOUND` error no longer keeps the
  dead session ID, so the next Reconnect starts a fresh session instead of
  failing forever (`static/js/main.js`).
- `file_list` WebSocket messages now go through `sendMsg()` for consistent
  try/catch and toast error handling, matching all other WS messages
  (`static/js/main.js`).

## 1.2.1 — 2026-08-28

### Fixed

- File browser: the breadcrumb path bar did not update when navigating into a
  subdirectory, going up via `..`, or clicking a breadcrumb crumb. The click
  handlers sent the `file_list` request directly, bypassing the code that
  tracks the current path and re-renders the breadcrumb; they now all go
  through `requestFileList()` in `static/js/files.js`.

## 1.2.0 — 2026-08-28

### Added

- Frontend refactor: the monolithic `static/index.html` (previously ~1.9k lines
  of inline CSS/JS) is split into eight ES modules under `static/js/` —
  `config.js` (constants + namespaced `localStorage` preference wrapper),
  `icons.js`, `auth.js`, `terminal.js`, `toast.js`, `transfer.js`, `files.js`,
  and `main.js` (WebSocket lifecycle). All modules are embedded via rust-embed
  and covered by new asset tests in `src/assets.rs`.
- Settings menu additions, all persisted via `localStorage`:
  - Terminal font size with increase/decrease/reset controls.
  - Cursor blink toggle.
  - Toast duration (seconds, `0` = sticky) with increase/decrease/reset and a
    direct numeric input.
- Upload progress indicator: a header ring button showing aggregate upload
  progress, expanding into a detail panel listing per-file status with a
  "Cancel all" control.
- Toast close buttons; toasts now stack from the bottom-right corner (newest
  at the bottom) instead of hanging below the header.
- "Clear terminal" menu item.

### Changed

- Upgraded `argon2` from 0.5 to 0.6. Adapted `src/auth/basic.rs` to the new
  API: `PasswordHash` now lives in `password_hash::phc`, `hash_password` no
  longer takes an explicit salt (it generates one internally via the OS RNG,
  so the direct `rand_core` dependency and its `getrandom` workaround were
  dropped from `Cargo.toml`), and the strict PHC hash validation now maps
  missing salt/digest to the more precise `SaltInvalid` / `OutputSize` errors.
  Existing `$argon2id$` PHC strings in configuration files remain compatible.
- File uploads now verify that the target directory is writable (`access(2)`
  with `W_OK`) before any multipart body is read. An unwritable target fails
  fast with 403 and a message naming the directory, instead of transferring
  (potentially large) file bytes and then surfacing an opaque 500. Covered by a
  new integration test that is skipped when running as root.

### Documentation

- Corrected the annotated `src/` tree in CLAUDE.md: it omitted `lib.rs` and the
  whole `server/websocket/` submodule split (`handshake`, `auth`,
  `message_loop`, `pty_io`, `session_lifecycle`, `utils`), and referenced a
  `ttyd/` reference checkout that is gitignored and normally absent. Added a
  Configuration & CLI section describing config discovery, override precedence,
  and the logging filter order.
- Scoped the "unknown config fields are rejected" claim to the top level in
  `README.md`, `config.example.toml`, and `docs/PROJECT_STATUS.md`: only
  `Config` declares `deny_unknown_fields`, so unrecognized keys nested inside a
  table are still silently ignored.
- Documented the previously missing CLI flags (`-c/--config`,
  `-w/--working-dir`, `--username`, `--password`, `--no-file-transfer`) and the
  complete set of `Config::validate` rules, including the two safety guards.
- `docs/PROTOCOL.md`: replaced the invented error-code table with the ten codes
  the server actually emits, corrected the heartbeat timings (30s server ping /
  90s timeout), distinguished the fixed 64 KB frame cap from `max_input_size`,
  listed the real `auth_fail` reasons and `disconnect` reasons, and noted that
  `auth_ok.readonly` is always `false` while `ready.readonly` is authoritative.
- `docs/PROJECT_STATUS.md`: expanded the REST API reference with `/ws`, the
  static routes, per-endpoint auth, and the real status codes (401/403/404/409/
  413/429/503); corrected the audit event count (six emitted, two defined but
  unwired) and the upload/static test counts.

## 1.1.0 — 2026-08-27

### Added

- `--check-config` / `-t` flag: load the configuration
  file, apply CLI overrides, run validation, and exit without starting the
  server. Exit code 0 means the configuration is valid. Useful as a
  `ExecStartPre=` check in systemd units and in deployment scripts.

### Changed

- Unknown fields in the configuration file are now rejected at startup
  (`deny_unknown_fields`). Previously, typos such as `[file_tranfer]` or
  `max_conections` were silently ignored while defaults were used.
- Stricter configuration validation at startup:
  - `command` must not be empty.
  - `max_connections` must be greater than 0.
  - `log_level` must be a bare level name (`trace`, `debug`, `info`,
    `warn`, `error`, `off`, case-insensitive) or an EnvFilter directive
    containing `=` (e.g. `"ttyd_rs=debug"`). Invalid values previously
    filtered out nearly all log output, including startup errors.
- Auth `method` matching is case-insensitive (`"TOKEN"` is accepted).
- `--trust-proxy` and `--allow-unauthenticated` accept explicit values
  (`--flag=true|false`) so the CLI can override the config file in either
  direction; the bare flag still means `true`.
- An invalid `--log-level` value is now a hard error instead of a warning.

### Upgrade note

Configuration files containing unknown keys or sections will now fail to
start with a clear error. Remove unused keys rather than leaving them in,
and run `ttyd-rs -t --config /path/to/config.toml` to verify before
restarting the service.

## 1.0.0 — 2026-08-25

### Security

- Refuse to start an unauthenticated terminal on a non-loopback address.
  Configure `[auth]`, bind to `127.0.0.1` / `::1`, or explicitly set
  `allow_unauthenticated = true` only when a trusted reverse proxy enforces
  authentication.
- Keep unauthenticated file transfer behind its separate explicit opt-in.

### Release readiness

- Update project, protocol, configuration, and roadmap documentation for
  version 1.0.0.
- Fix the invalid-PTY-FD regression test so cleanup never signals PID 1.

### Upgrade note

Deployments intentionally serving an unauthenticated terminal on `0.0.0.0`
or another non-loopback address must add `allow_unauthenticated = true` to
their configuration after independently confirming that a trusted proxy is
the authentication boundary.
