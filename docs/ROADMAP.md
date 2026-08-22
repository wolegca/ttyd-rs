# ttyd-rs Roadmap

**Target Platforms**: Linux only (no Windows or macOS support)

**Current Status**: All core milestones (M1–M6) completed. See [PROJECT_STATUS.md](PROJECT_STATUS.md) for current test counts and quality gate status.

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
- Terminal resize via `TIOCSWINSZ`, SIGHUP/SIGKILL lifecycle management
- 5-stage process cleanup + background zombie reaper thread

---

### Phase 4: Security Layer (M4) ✅

**Goal**: Authentication and security features

- Basic Auth + Token Auth with constant-time comparison (`subtle` crate)
- Sliding-window rate limiting (per-IP), input validation (size, payload, credentials)
- Audit logging (8 event types)

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

## Feature Matrix

All planned features through M6 and post-M6 (file transfer, signal handling, process cleanup) are **✅ complete**. See [Development History](#development-history) for details.

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

## Future Enhancements

1. **Enhanced Terminal Features**
   - Terminal recording/playback
   - Screenshot capture
   - Copy button overlay

2. **Deployment Features**
   - Docker container support
   - systemd service file
   - Reverse proxy configuration examples
   - Credential injection via environment variables / systemd `LoadCredential` (e.g. `TTYD_RS_AUTH_PASSWORD`), complementing the v0.7.0 Argon2id hash-in-config support

3. **Performance Optimizations**
   - Connection pooling
   - Message batching
   - Binary WebSocket frames for better performance

---

## Design Notes

### Why Rust?

1. **Memory safety**: No buffer overflows, null pointer dereferences
2. **Performance**: Comparable to C, much better than Node.js
3. **Concurrency**: Fearless concurrency with tokio
4. **Type safety**: Catch errors at compile time
5. **Modern tooling**: cargo, clippy, rustfmt

### Compatibility Goals

- **CLI**: 100% compatible with original ttyd CLI flags
- **WebSocket Protocol**: Compatible with original ttyd protocol
- **Configuration**: Support same config file format (with extensions)

---

Last Updated: 2026-08-21
