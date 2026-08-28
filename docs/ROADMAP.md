# ttyd-rs Roadmap

**Target Platforms**: Linux only (no Windows or macOS support)

**Current Status**: v1.2.0 released; post-1.2.0 work in progress (see
[CHANGELOG.md](../CHANGELOG.md)). All core milestones (M1–M6) are
complete; see [PROJECT_STATUS.md](PROJECT_STATUS.md) for release verification status.

---

## Development History

### Phase 1: Foundation (M1) ✅

**Goal**: Project scaffolding and basic CLI

- Cargo project, `clap` CLI with all flags from original ttyd
- TOML config file + CLI args, tracing/logging, `thiserror` error handling

---

### Phase 2: Core Server (M2) ✅

**Goal**: HTTP server with WebSocket support

- axum HTTP server with routing, WebSocket upgrade handler
- Bidirectional message handling, static file serving (rust-embed)
- WebSocket protocol spec (see [PROTOCOL.md](PROTOCOL.md))

---

### Phase 3: PTY Management (M3) ✅

**Goal**: Pseudo-terminal management

- `openpty()` + `fork()`, `setsid()`, `dup2()` for PTY allocation
- Terminal resize via `TIOCSWINSZ`, SIGHUP lifecycle management
- Process cleanup via close master fd + SIGHUP + global SIGCHLD reaper

---

### Phase 4: Security Layer (M4) ✅

**Goal**: Authentication and security features

- Basic Auth + Token Auth with constant-time comparison (`subtle` crate)
- Sliding-window rate limiting (per-IP), input validation (size, payload, credentials)
- Audit logging (8 event types defined, 6 currently emitted)

---

### Phase 5: Session Management (M5) ✅

**Goal**: Multi-client session management

- SessionManager with isolated / shared-readonly / shared-readwrite modes
- Session timeout + auto-cleanup, REST API for session management
- Broadcast channel for shared-session output

---

### Phase 6: Frontend Integration (M6) ✅

**Goal**: xterm.js browser frontend

- Embedded HTML/CSS/JS via rust-embed, xterm.js terminal rendering
- Login form, auto-reconnect with exponential backoff
- File panel with upload/download/browse, connection status indicator

---

### Post-1.0 Hardening (v1.1.0) ✅

**Goal**: Configuration robustness and operational tooling

- Strict config validation: unknown fields rejected (`deny_unknown_fields`),
  non-empty `command`, `max_connections > 0`, `log_level` validated (bare
  level names or EnvFilter directives)
- `--check-config` / `-t` pre-flight validation mode (like `nginx -t`)
- Three-state CLI boolean flags (`--trust-proxy=true|false`,
  `--allow-unauthenticated=true|false`) that can override the config file in
  either direction
- Case-insensitive auth `method` matching; invalid `--log-level` is a hard
  error instead of a silent filter

---

## Feature Matrix

All planned features through M6 and post-M6 (file transfer, signal handling, process cleanup) are **✅ complete**. See [Development History](#development-history) for details.

Post-1.0 hardening (strict config validation, `--check-config`) shipped in v1.1.0.

---

## Performance Targets

| Metric | Target | Notes |
|--------|--------|-------|
| Startup time | < 50ms | Cold start to first connection |
| Idle memory | < 10MB | Without active connections |
| Connection latency | < 5ms | Time to establish WebSocket |
| Max concurrent | > 1000 | Configurable via `--max-connections` |
| Message throughput | > 10MB/s | Terminal output streaming |

---

## Design Notes

### Why Rust?

1. **Memory safety**: No buffer overflows, null pointer dereferences
2. **Performance**: Comparable to C, much better than Node.js
3. **Concurrency**: Fearless concurrency with tokio
4. **Type safety**: Catch errors at compile time
5. **Modern tooling**: cargo, clippy, rustfmt

### Compatibility Goals

- **CLI**: Aims at the original's flag names for common options, but is **not
  verified flag by flag** — the `ttyd/` reference checkout is gitignored and
  usually absent, so treat `ttyd --help` as the authority before relying on
  equivalence. ttyd-rs at least gives `-b` (bind address) and `-c` (config
  file) meanings that differ from a naive reading of the original's flags
- **WebSocket Protocol**: **Not wire-compatible** with original ttyd clients.
  The message set is conceptually similar but the JSON envelope and message
  types differ, so an adaptation layer is required (see
  [PROTOCOL.md → Compatibility](PROTOCOL.md#compatibility))
- **Configuration**: ttyd has no configuration file format (CLI only), so the
  TOML schema is a ttyd-rs extension rather than a compatibility target

---
