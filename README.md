# ttyd-rs

A Rust rewrite of [ttyd](https://github.com/tsl0922/ttyd) - Share your terminal over the web using WebSocket.

**Current Version**: v0.1.0
**Status**: Production-ready for single-user scenarios

## Features

### Core Functionality ✅
- **Web-based Terminal**: Access your terminal through any modern web browser
- **WebSocket Communication**: Real-time bidirectional communication
- **PTY Management**: Full pseudo-terminal support using nix crate
- **xterm.js Integration**: Professional terminal emulation in the browser

### Security (M2) ✅
- **Authentication**: Basic Auth support with username/password
- **Rate Limiting**: Prevents brute force attacks (10 attempts per 60 seconds)
- **Input Validation**: Terminal size, payload size, and credential validation
- **Audit Logging**: Comprehensive event logging (connections, auth, errors)

### Session Management (M3 + M4) ✅
- **SessionManager**: Centralized session management
- **Session Modes**: Isolated / SharedReadOnly / SharedReadWrite
- **Automatic Cleanup**: Sessions timeout after inactivity
- **REST API**: Monitor and manage sessions via HTTP endpoints

### Configuration (M3) ✅
- **TOML Config Files**: Complete configuration file support
- **CLI Arguments**: All features configurable via command line
- **Validation**: Configuration validation at startup
- **Flexible**: Command line overrides config file

## Quick Start

### Build from Source

```bash
# Clone the repository
git clone https://github.com/your-username/ttyd-rs.git
cd ttyd-rs

# Build release version
cargo build --release

# Run
./target/release/ttyd-rs
```

### Basic Usage

```bash
# Start with defaults (localhost:7681, no auth)
ttyd-rs

# With authentication
ttyd-rs --auth --username admin --password secret

# Custom port and shell
ttyd-rs -p 8080 -s /bin/zsh

# Use config file
ttyd-rs --config config.toml

# Enable audit logging
ttyd-rs --audit --audit-file /var/log/ttyd-rs/audit.log
```

Then open your browser to `http://localhost:7681`

## Configuration

### Command Line Options

```
Options:
  -p, --port <PORT>              Port to listen on [default: 7681]
  -b, --bind <ADDR>              Address to bind to [default: 127.0.0.1]
  -c, --config <FILE>            Configuration file path
  -s, --shell <SHELL>            Shell command [default: bash]
  -w, --working-dir <DIR>        Working directory
  
  --session-mode <MODE>          Session mode: isolated|shared-ro|shared-rw
                                 (also accepts: shared_readonly|shared_readwrite)
  --session-timeout <SECS>       Session timeout in seconds [default: 3600]
  --reconnect-window <SECS>      Reconnect window in seconds [default: 60]
  --max-connections <NUM>        Max connections [default: 100]
  
  --auth                         Enable authentication
  --username <USER>              Username for basic auth
  --password <PASS>              Password for basic auth
  
  --audit                        Enable audit logging
  --audit-file <FILE>            Audit log file path
  
  --log-level <LEVEL>            Log level [default: info]
  -h, --help                     Print help
  -V, --version                  Print version
```

### Configuration File

See [config.example.toml](config.example.toml) for a complete example.

```toml
bind = "127.0.0.1:7681"
command = ["bash", "-l"]

[session]
mode = "isolated"
timeout = 3600

[auth]
method = "basic"
username = "admin"
password = "changeme"

[audit]
enabled = true
log_file = "/var/log/ttyd-rs/audit.log"
```

## REST API

### Endpoints

- `GET /api/health` - Health check
- `GET /api/config` - Client-facing configuration (auth method)
- `GET /api/sessions` - List all active sessions
- `GET /api/sessions/:id` - Get session details
- `DELETE /api/sessions/:id` - Terminate a session
- `GET /api/stats` - Server statistics

### Examples

```bash
# Check server health
curl http://localhost:7681/api/health

# List sessions
curl http://localhost:7681/api/sessions

# Get server stats
curl http://localhost:7681/api/stats
```

## Architecture

```
┌─────────────────┐
│   Web Browser   │
│   (xterm.js)    │
└────────┬────────┘
         │ WebSocket
         ▼
┌─────────────────┐
│  HTTP Server    │
│  (axum)         │
├─────────────────┤
│ SessionManager  │
│  └─ Sessions    │
│     └─ PTY      │
└─────────────────┘
```

### Key Components

- **axum**: Web framework with WebSocket support
- **tokio**: Async runtime
- **nix**: Unix PTY operations
- **xterm.js**: Frontend terminal emulator
- **SessionManager**: Centralized session lifecycle management

## Development

### Prerequisites

- Rust 1.85+ (edition 2024)
- Linux or macOS (Unix-like system required)

### Build

```bash
# Debug build
cargo build

# Release build with optimizations
cargo build --release

# Run tests
cargo test

# Check code quality
cargo clippy -- -D warnings
cargo fmt -- --check
```

### Code Quality

This project maintains strict code quality standards:
- **No unwrap/expect/panic**: All errors properly handled
- **Zero clippy warnings**: Enforced in CI
- **Formatted code**: cargo fmt enforced
- **Comprehensive tests**: Unit and integration tests

## Security

### Best Practices

1. **Always enable authentication in production**
   ```bash
   ttyd-rs --auth --username admin --password STRONG_PASSWORD
   ```

2. **Use HTTPS/WSS**
   - Deploy behind a reverse proxy (Nginx/Caddy)
   - Terminate SSL at the proxy level

3. **Limit access**
   - Bind to localhost and use SSH tunneling
   - Or configure firewall rules

4. **Monitor audit logs**
   - Enable audit logging
   - Review failed authentication attempts
   - Set up alerts for suspicious activity

5. **Keep sessions short**
   - Configure appropriate timeout values
   - Monitor active sessions via API

## Project Status

### Completed Milestones

- ✅ **M1: Basic Functionality** - CLI, config, logging, error handling
- ✅ **M2: Core Server** - HTTP/WebSocket server, routing, static files
- ✅ **M3: PTY Management** - Process spawning, signals, terminal resize
- ✅ **M4: Security Layer** - Auth, rate limiting, input validation, audit logs
- ✅ **M5: Session Management** - SessionManager, shared modes, REST API
- ✅ **M6: Frontend Integration** - xterm.js, login form, reconnection

### Roadmap

See [DEVELOPMENT_GOALS.md](DEVELOPMENT_GOALS.md) for detailed roadmap.

## Comparison with Original ttyd

| Feature | ttyd (C) | ttyd-rs (Rust) |
|---------|----------|----------------|
| Memory Safety | ⚠️ Manual | ✅ Guaranteed |
| Async I/O | libev | tokio |
| Security | Basic | Enhanced (M2) |
| Session Management | Single | Multi-mode (M3) |
| API | Limited | REST API (M3) |
| Configuration | CLI only | CLI + TOML |
| Platform | Cross-platform | Unix-only (intentional) |

## Documentation

- [CLAUDE.md](CLAUDE.md) - Development guidelines and code quality requirements
- [Development Goals](DEVELOPMENT_GOALS.md) - Roadmap and milestone details
- [Project Status](docs/PROJECT_STATUS.md) - Production readiness assessment and known issues
- [Current Status](docs/CURRENT_STATUS.md) - Feature list and module structure
- [WebSocket Protocol](docs/PROTOCOL.md) - Protocol specification

## License

MIT License

## Contributing

Contributions are welcome! Please:
1. Check existing issues or create one
2. Fork the repository
3. Create a feature branch
4. Ensure all tests pass and clippy is clean
5. Submit a pull request

## Acknowledgments

- Original [ttyd](https://github.com/tsl0922/ttyd) by tsl0922
- [xterm.js](https://github.com/xtermjs/xterm.js) terminal emulator
- The Rust community for excellent async ecosystem

---

**Built with ❤️ in Rust**
