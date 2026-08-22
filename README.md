# ttyd-rs

A Rust rewrite of [ttyd](https://github.com/tsl0922/ttyd) — Share your terminal over the web using WebSocket.

**Version**: v0.7.0
**Status**: Production-ready
**Platform**: Linux only

## Features

- **Web Terminal**: Full xterm.js terminal in the browser via WebSocket
- **Security**: Basic/Token auth, rate limiting, input validation, audit logging
- **File Transfer**: HTTP upload/download with streaming, size limits, overwrite protection
- **Session Management**: Isolated / shared modes, auto-cleanup, REST API
- **Configuration**: TOML config file + CLI arguments with validation

## Quick Start

```bash
# Build and run
cargo build --release

# Auth with a password on the command line (plaintext, logged with a warning)
./target/release/ttyd-rs --auth --username admin --password secret

# Recommended: store an Argon2id hash instead of the plaintext password
printf 'secret\n' | ./target/release/ttyd-rs --hash-password
# → $argon2id$v=19$m=19456,t=2,p=1$... — put this in config.toml (or --password)

# Or use a config file
ttyd-rs --config config.toml
```

Then open `http://localhost:7681` in your browser.

### Basic Usage

```bash
# Defaults: localhost:7681, bash shell, no auth
ttyd-rs

# Custom port and shell
ttyd-rs -p 8080 -s /bin/zsh

# Shared session mode
ttyd-rs --session-mode shared-ro

# Full options
ttyd-rs --help
```

## Configuration

See [config.example.toml](config.example.toml) for a complete annotated example.

```toml
# Minimal config
bind = "127.0.0.1:7681"
command = ["bash", "-l"]

[auth]
method = "basic"
username = "admin"
# Argon2id hash (recommended) — see Quick Start above for how to generate one
password = "$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>"
# Plaintext also works, but logs a startup warning:
# password = "changeme"
#
# Note: any value starting with `$argon2id$` is treated as a pre-hashed
# credential (validated at startup), so a literal plaintext password that
# happens to begin with that prefix is not supported.

[file_transfer]
enabled = true
```

CLI arguments override config file values. Run `ttyd-rs --help` for all options.

## Security

- **Always enable authentication** in production
- **Store the password as an Argon2id hash** — see [Quick Start](#quick-start) for generation; plaintext triggers a startup warning
- **Use a reverse proxy** (nginx/Caddy) for HTTPS — TLS is not built-in
- **Bind to localhost** and use SSH tunneling, or configure firewall rules
- **Enable audit logging** and monitor for suspicious activity
- **Set appropriate timeouts** — default session timeout is 1 hour

Full deployment checklist: [docs/PROJECT_STATUS.md → Deployment Recommendations](docs/PROJECT_STATUS.md#deployment-recommendations).

## Comparison with Original ttyd

| Feature | ttyd (C) | ttyd-rs (Rust) |
|---------|----------|----------------|
| Memory Safety | ⚠️ Manual | ✅ Guaranteed |
| Async I/O | libev | tokio |
| Security | Basic | Enhanced (auth, rate limiting, audit) |
| Session Management | Single | Multi-mode (isolated/shared) |
| API | Limited | Full REST API |
| Configuration | CLI only | CLI + TOML |
| File Transfer | ❌ | ✅ HTTP upload/download + WS file listing |
| Platform | Cross-platform | Unix-only (intentional) |

## Documentation

| Document | Description |
|----------|-------------|
| [CLAUDE.md](CLAUDE.md) | Developer guide: build commands, architecture, code style, project structure |
| [docs/PROJECT_STATUS.md](docs/PROJECT_STATUS.md) | Technical reference: features, API, security, known issues, production readiness |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Roadmap: milestones, future plans, performance targets |
| [docs/PROTOCOL.md](docs/PROTOCOL.md) | WebSocket protocol specification |
| [config.example.toml](config.example.toml) | Annotated configuration example |

## License

GNU AFFERO GENERAL PUBLIC LICENSE

## Contributing

1. Check existing issues or create one
2. Fork the repository
3. Create a feature branch
4. Ensure `cargo test` passes and `cargo clippy -- -D warnings` is clean
5. Submit a pull request

## Acknowledgments

- Original [ttyd](https://github.com/tsl0922/ttyd) by tsl0922
- [xterm.js](https://github.com/xtermjs/xterm.js) terminal emulator
- The Rust community for excellent async ecosystem

---

**Built with ❤️ in Rust**
