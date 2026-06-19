# ttyd-rs Development Goals

## Project Overview

ttyd-rs is a Rust rewrite of [ttyd](https://github.com/tsl0922/ttyd), a terminal sharing tool over the web. This document outlines the development goals, milestones, and feature planning for the project.

**Target Platforms**: Linux only (no Windows or macOS support)

**Current Status**: All core milestones (M1-M6) completed. 161 tests passing, all clippy lints clean.

---

## Development Roadmap

### Phase 1: Foundation (Milestone 1) - ✅ COMPLETED

**Goal**: Establish project scaffolding and basic CLI

**Status**: Completed in commit `683abdf`

#### Tasks:
- [x] Initialize Cargo project with workspace structure
- [x] Set up `clap` CLI argument parsing with all flags from original ttyd
- [x] Implement configuration loading from file and CLI args
- [x] Set up tracing/logging infrastructure
- [x] Create basic error handling with `thiserror`
- [x] Add development tooling (clippy, rustfmt, justfile)

#### CLI Flags Implemented:
- `-p, --port` - Port (default: 7681)
- `-b, --bind` - Bind address (default: 127.0.0.1)
- `-c, --config` - Config file path
- `-s, --shell` - Shell command (default: bash)
- `-w, --working-dir` - Working directory
- `--log-level` - Log level (trace/debug/info/warn/error)
- `--session-mode` - Session mode (isolated/shared-ro/shared-rw)
- `--session-timeout` - Session timeout in seconds
- `--reconnect-window` - Reconnect window in seconds (default: 60)
- `--max-connections` - Max concurrent connections
- `--auth` - Enable authentication
- `--username` / `--password` - Basic auth credentials
- `--audit` - Enable audit logging
- `--audit-file` - Audit log file path

---

### Phase 2: Core Server (Milestone 2) - ✅ COMPLETED

**Goal**: Implement HTTP server with WebSocket support

**Status**: Completed

#### Tasks:
- [x] Design WebSocket message protocol specification (see docs/PROTOCOL.md)
- [x] Set up axum HTTP server with routing
- [x] Implement WebSocket upgrade handler
- [x] Create bidirectional WebSocket message handling
- [x] Add static file serving for frontend assets
- [x] Implement connection lifecycle management

#### Dependencies:
```toml
axum = { version = "0.8.9", features = ["ws"] }
tokio = { version = "1.52.3", features = ["full"] }
tower = "0.5.3"
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.150"
uuid = { version = "1.23.3", features = ["v4"] }
```

---

### Phase 3: PTY Management (Milestone 3) - ✅ COMPLETED

**Goal**: Implement pseudo-terminal (PTY) management using nix crate

**Status**: Completed

#### Tasks:
- [x] Create PTY using `nix::pty::openpty`
- [x] Implement process forking with proper signal handling
- [x] Set up stdin/stdout/stderr redirection to PTY slave
- [x] Implement terminal resize (SIGWINCH) via `TIOCSWINSZ`
- [x] Add process lifecycle management (SIGHUP/SIGKILL)
- [x] Implement graceful shutdown with proper cleanup
- [x] Add zombie process cleanup via background reaper thread

#### Dependencies:
```toml
nix = { version = "0.31.3", features = ["process", "signal", "term", "ioctl", "fs"] }
libc = "0.2.186"
```

#### Implementation Details:
- Uses `openpty()` + `fork()` for PTY allocation
- Child process calls `setsid()` to create new session
- `dup2()` redirects stdin/stdout/stderr to PTY slave
- `TIOCSWINSZ` ioctl for terminal resize
- `Drop` implementation sends SIGHUP then SIGKILL
- Background thread reaps zombie processes via `waitpid()`

---

### Phase 4: Security Layer (Milestone 4) - ✅ COMPLETED

**Goal**: Implement authentication and security features

**Status**: Completed

#### Tasks:
- [x] Implement Basic Authentication with constant-time comparison
- [x] Add Token-based authentication
- [x] Create rate limiting for brute-force protection
- [x] Add input validation for all WebSocket messages
- [x] CORS not implemented (not needed — frontend is same-origin)
- [x] Add audit logging for security events
- [x] Prevent injection attacks via input validation

#### Security Implementation Details:

**Basic Auth**:
- Uses `subtle::ConstantTimeEq` for timing-attack-resistant comparison
- Base64 decoding of `username:password` credentials
- Rate limiting per IP address before auth processing

**Token Auth**:
- Constant-time token comparison using `subtle` crate
- Prevents timing side-channel attacks

**Rate Limiting**:
- Sliding window algorithm (configurable: default 10 attempts per 60 seconds)
- Per-IP tracking with automatic window expiry
- Returns rate limit exceeded message to client

**Input Validation**:
- Terminal size bounds (10-500 cols, 5-200 rows)
- Payload size limits (16KB per message)
- Credential length limits (1024 chars max)
- Configurable via `[validation]` section in config

#### Dependencies:
```toml
subtle = "2.6.1"        # Constant-time comparison
sha2 = "0.11.0"         # Password hashing
base64 = "0.22.1"       # Basic auth decoding
```

---

### Phase 5: Session Management (Milestone 5) - ✅ COMPLETED

**Goal**: Implement multi-client session management

**Status**: Completed

#### Tasks:
- [x] Create session manager with session lifecycle
- [x] Implement session modes:
  - **Isolated**: Each connection gets its own PTY (default)
  - **Shared Read-Only**: Multiple clients view one PTY
  - **Shared Read-Write**: Multiple clients control one PTY
- [x] Add session timeout and cleanup (configurable, default 3600s)
- [x] Implement session listing via REST API
- [x] Add session joining via session ID
- [x] Broadcast terminal output to all connected clients

#### REST API Endpoints:
```
GET    /api/health            - Health check
GET    /api/config            - Client-facing configuration (auth method)
GET    /api/sessions          - List all active sessions
GET    /api/sessions/:id      - Get session info
DELETE /api/sessions/:id      - Terminate session
GET    /api/stats             - Server statistics
```

#### Session Manager Features:
- Automatic cleanup of inactive sessions (every 30s)
- Client tracking with connection metadata
- Broadcast channel for terminal output distribution
- Session metadata (mode, command, working dir, timestamps)

---

### Phase 6: Frontend Integration (Milestone 6) - ✅ COMPLETED

**Goal**: Integrate xterm.js frontend

**Status**: Completed

#### Tasks:
- [x] Create embedded HTML/CSS/JS frontend using `rust-embed`
- [x] Integrate xterm.js for terminal rendering
- [x] Implement WebSocket client in JavaScript
- [x] Add terminal resize handling (fit addon)
- [x] Implement connection status indicators
- [x] Add copy/paste support (native xterm.js)
- [x] Handle reconnection on disconnect

#### Frontend Files:
```
static/
├── index.html           # Main terminal UI (262 lines)
└── vendor/
    ├── xterm.css        # xterm.js styles
    ├── xterm.js         # Terminal emulator
    ├── xterm-addon-fit.js      # Auto-fit addon
    └── xterm-addon-web-links.js # Link detection
```

#### Dependencies:
```toml
rust-embed = { version = "8.11.0", features = ["axum"] }
```

---

## Feature Matrix

| Feature | Priority | Status | Milestone |
|---------|----------|--------|-----------|
| CLI parsing | High | ✅ Done | M1 |
| Configuration | High | ✅ Done | M1 |
| HTTP server | High | ✅ Done | M2 |
| WebSocket | High | ✅ Done | M2 |
| PTY management | High | ✅ Done | M3 |
| Basic auth | High | ✅ Done | M4 |
| Token auth | Medium | ✅ Done | M4 |
| Rate limiting | High | ✅ Done | M4 |
| Audit logging | Medium | ✅ Done | M4 |
| Session modes | High | ✅ Done | M5 |
| REST API | Medium | ✅ Done | M5 |
| xterm.js frontend | High | ✅ Done | M6 |
| Signal handling | High | ✅ Done | M3 |
| Process cleanup | High | ✅ Done | M3 |

---

## Performance Targets

Based on original ttyd benchmarks:

| Metric | Target | Notes |
|--------|--------|-------|
| Startup time | < 50ms | Cold start to first connection |
| Idle memory | < 10MB | Without active connections |
| Connection latency | < 5ms | Time to establish WebSocket |
| Max concurrent | > 1000 | Configurable via `--max-connections` |
| Message throughput | > 10MB/s | Terminal output streaming |

---

## Quality Gates

### Code Quality Requirements

Before merging any PR, ensure:

```bash
# Format check
cargo fmt -- --check

# Clippy with zero warnings
cargo clippy -- -D warnings

# All tests pass
cargo test

# Security audit (when dependencies stabilize)
cargo audit
```

### Current Test Coverage

- **161 tests passing** across all modules
- Tests cover: config loading, validation, auth, rate limiting, audit, session management, HTTP server, WebSocket, PTY
- All clippy lints clean with `-D warnings`

### Testing Strategy

- **Unit tests**: Every public function has unit tests
- **Integration tests**: WebSocket connection tests via `tokio-tungstenite`
- **Validation tests**: Boundary conditions for all input validation

---

## Project Structure

```
src/
├── main.rs           # Entry point, CLI parsing, config loading (342 lines)
├── config.rs         # Configuration types and validation (410 lines)
├── server.rs         # Server module declaration
├── server/
│   ├── http.rs       # HTTP server, routing, static files (1114 lines)
│   ├── websocket.rs  # WebSocket handler, session management (1105 lines)
│   └── api.rs        # REST API endpoints (526 lines)
├── pty.rs            # PTY module declaration
├── pty/
│   ├── process.rs    # PTY process spawning and management (341 lines)
│   └── session.rs    # PTY session wrapper (117 lines)
├── auth.rs           # Auth module declaration
├── auth/
│   ├── basic.rs      # Basic authentication (125 lines)
│   └── token.rs      # Token authentication (115 lines)
├── protocol.rs       # WebSocket message types (296 lines)
├── session.rs        # Session manager, multi-client support (871 lines)
├── audit.rs          # Audit logging (386 lines)
├── rate_limit.rs     # Rate limiting (242 lines)
├── validation.rs     # Input validation (186 lines)
└── assets.rs         # Static asset embedding (65 lines)

Total: ~6,260 lines of Rust code
```

---

## Future Enhancements

### Planned Features (Post-M4):

1. **Enhanced Terminal Features**
   - Terminal recording/playback
   - Screenshot capture
   - Copy button overlay

3. **Deployment Features**
   - Docker container support
   - systemd service file
   - Reverse proxy configuration examples

4. **Performance Optimizations**
   - Connection pooling
   - Message batching
   - Binary WebSocket frames for better performance

---

## Reference Material

- Original ttyd source: `ttyd/` directory
- WebSocket protocol spec: `docs/PROTOCOL.md`
- Project documentation: `docs/` directory
- Example configuration: `config.example.toml`

---

## Notes

### Why Rust?

1. **Memory safety**: No buffer overflows, null pointer dereferences
2. **Performance**: Comparable to C, much better than Node.js
3. **Concurrency**: Fearless concurrency with tokio
4. **Type safety**: Catch errors at compile time
5. **Modern tooling**: cargo, clippy, rustfmt

### Compatibility Goals

- **CLI**: 100% compatible with original ttyd CLI flags
- **WebSocket Protocol**: Compatible with original ttyd protocol (if documented)
- **Configuration**: Support same config file format (with extensions)

---

Last Updated: 2026-06-18
