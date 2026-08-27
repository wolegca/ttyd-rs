# Changelog

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
