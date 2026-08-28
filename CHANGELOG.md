# Changelog

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
