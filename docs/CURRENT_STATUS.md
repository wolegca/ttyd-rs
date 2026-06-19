# ttyd-rs Current Status

**Updated**: 2026-06-18
**Version**: 0.1.0
**Phase**: All core milestones (M1-M6) completed

---

## Code Quality

All checks pass per [CLAUDE.md](../../CLAUDE.md):

1. **Format** (`cargo fmt -- --check`) — ✅ Pass
2. **Clippy** (`cargo clippy -- -D warnings`) — ✅ Zero warnings
3. **Tests** (`cargo test`) — ✅ 104 tests passing
4. **Release build** — ✅ Success

---

## Project Statistics

- **Rust source**: ~4,800 lines across 14 .rs files
- **Tests**: 104 (unit + integration)
- **Frontend**: index.html with xterm.js integration
- **Dependencies**: See Cargo.toml for current list

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

## Platform Support

- ✅ **Linux**: Full support
- ✅ **macOS**: Full support
- ❌ **Windows**: Not supported (Unix PTY required)

---

## Known Limitations

1. **No built TLS**: Use a reverse proxy (nginx, Caddy) for HTTPS
2. **No session persistence**: Sessions are lost on server restart
3. **No file transfer**: No upload/download support

---

*Last updated: 2026-06-18*
