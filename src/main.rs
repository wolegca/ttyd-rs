use ttyd_rs::{
    auth::BasicAuth,
    config::{AuthConfig, Config},
    pty, server,
};

use clap::Parser;
use std::io::{BufRead, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "ttyd-rs")]
#[command(about = "Share your terminal over the web", long_about = None)]
#[command(version)]
struct Args {
    /// Port to listen on.
    /// Defaults to the `bind` value from the configuration file (or 7681).
    /// Only overrides the config when explicitly provided.
    #[arg(short, long)]
    port: Option<u16>,

    /// Address to bind to.
    /// Only overrides the config when explicitly provided.
    #[arg(short, long)]
    bind: Option<String>,

    /// Configuration file path
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Shell command to execute
    #[arg(short, long)]
    shell: Option<String>,

    /// Working directory for the shell
    #[arg(short = 'w', long)]
    working_dir: Option<PathBuf>,

    /// Log level (trace, debug, info, warn, error).
    /// Defaults to the `log_level` value from the configuration file
    /// (or `info` when no config file is used).
    #[arg(long)]
    log_level: Option<String>,

    /// Session mode: isolated, shared-ro, shared-rw.
    /// Only overrides the config when explicitly provided.
    #[arg(long)]
    session_mode: Option<String>,

    /// Session timeout in seconds (0 = no timeout).
    /// Only overrides the config when explicitly provided.
    #[arg(long)]
    session_timeout: Option<u64>,

    /// Reconnect window in seconds — how long to keep empty sessions alive.
    /// Only overrides the config when explicitly provided.
    #[arg(long)]
    reconnect_window: Option<u64>,

    /// Maximum number of concurrent connections.
    /// Only overrides the config when explicitly provided.
    #[arg(long)]
    max_connections: Option<usize>,

    /// Enable authentication
    #[arg(long, requires = "username", requires = "password")]
    auth: bool,

    /// Username for basic authentication
    #[arg(long, requires = "auth")]
    username: Option<String>,

    /// Password for basic authentication
    #[arg(long, requires = "auth")]
    password: Option<String>,

    /// Enable audit logging
    #[arg(long)]
    audit: bool,

    /// Audit log file path
    #[arg(long, requires = "audit")]
    audit_file: Option<PathBuf>,

    /// Trust proxy headers (X-Real-IP / X-Forwarded-For) for client IP.
    /// Accepts an explicit value (`--trust-proxy=true|false`) to override
    /// the config file in either direction; the bare flag means `true`.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    trust_proxy: Option<bool>,

    /// Allow an unauthenticated terminal on a non-loopback address.
    /// Only use when a trusted reverse proxy enforces authentication.
    /// Accepts `--allow-unauthenticated=true|false`; bare flag means `true`.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    allow_unauthenticated: Option<bool>,

    /// Read a password from stdin, print its Argon2id hash (PHC string) for
    /// the configuration file, and exit. The hash can replace the plaintext
    /// value of `password` in `[auth]` (or `--password`).
    #[arg(long)]
    hash_password: bool,

    /// Disable the file transfer endpoints (upload/download/list).
    /// Useful to satisfy the unauthenticated-file-transfer safety check in
    /// setups that run without [auth].
    #[arg(long)]
    no_file_transfer: bool,

    /// Test the configuration: load the config file,
    /// apply CLI overrides, run validation, and exit without starting the
    /// server. Exit code 0 means the configuration is valid.
    #[arg(short = 't', long)]
    check_config: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // `--check-config` (-t) is a standalone validation mode, like `nginx -t`:
    // load and validate the configuration, report the result, and exit
    // without binding any port or spawning a PTY.
    if args.check_config {
        match load_config(&args) {
            Ok(config) => {
                println!(
                    "ttyd-rs: configuration file {} is valid",
                    args.config.as_deref().map_or_else(
                        || "config.toml (default location)".to_string(),
                        |p| p.display().to_string()
                    )
                );
                println!("bind = {}", config.bind);
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("ttyd-rs: configuration test failed: {}", e);
                std::process::exit(1);
            }
        }
    }

    // `--hash-password` is a standalone utility mode: hash the password
    // read from stdin and exit without starting the server.
    if args.hash_password {
        if let Err(e) = run_hash_password() {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        std::process::exit(0);
    }

    // Load the configuration before initializing logging: the global
    // tracing subscriber can only be installed once, and the config file's
    // `log_level` must influence it.
    let config = match load_config(&args) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Failed to load configuration: {}", e);
            std::process::exit(1);
        }
    };

    // Initialize tracing/logging
    if let Err(e) = init_logging(args.log_level.as_deref(), &config.log_level) {
        eprintln!("Failed to initialize logging: {}", e);
        std::process::exit(1);
    }

    tracing::info!("Starting ttyd-rs v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Configuration: {:?}", config);

    if config
        .auth
        .as_ref()
        .is_some_and(AuthConfig::uses_plaintext_password)
    {
        tracing::warn!(
            "The basic-auth password is stored in plaintext (config file or \
             --password). Run `ttyd-rs --hash-password` to generate an \
             Argon2id hash and use it as the `password` value instead."
        );
    }

    // Register the global SIGCHLD handler.
    if let Err(e) = pty::process::register_sigchld_handler() {
        tracing::error!("Failed to register SIGCHLD handler: {}", e);
        std::process::exit(1);
    }

    // Start the server
    if let Err(e) = server::http::start_server(config, None).await {
        tracing::error!("Server error: {}", e);
        std::process::exit(1);
    }
}

/// Hash a password read from stdin and print the PHC string to stdout.
///
/// Usage: `printf 'pass\n' | ttyd-rs --hash-password`
fn run_hash_password() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::IsTerminal;

    // Prompt only when stdin is a terminal, so piped usage keeps stdout/stderr
    // clean and scriptable. The prompt goes to stderr (never stdout) so the
    // hash remains the only thing written to stdout.
    if std::io::stdin().is_terminal() {
        eprint!(
            "Enter password (input will be echoed; to avoid that, pipe it \
             instead, e.g.: echo my-password | ttyd-rs --hash-password): "
        );
        std::io::stderr().flush()?;
    }

    let mut password = String::new();
    std::io::stdin().lock().read_line(&mut password)?;
    // Trim the trailing newline (and an optional carriage return on CRLF).
    let password = password.trim_end_matches(['\r', '\n']);
    if password.is_empty() {
        return Err("password must not be empty".into());
    }
    let hash =
        BasicAuth::hash_password(password).map_err(|e| format!("failed to hash password: {e}"))?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{}", hash)?;
    Ok(())
}

/// Initialize the tracing subscriber for logging.
///
/// The log filter is resolved with the following precedence:
/// 1. explicit `--log-level` flag
/// 2. `RUST_LOG` environment variable
/// 3. `log_level` from the configuration file
fn init_logging(
    cli_log_level: Option<&str>,
    config_log_level: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // A bare level name is validated up front: `EnvFilter::try_new` accepts
    // unknown words as *target* directives, so a typo like "verbose" would
    // silently filter out nearly all log output instead of erroring.
    if let Some(level) = cli_log_level
        && !ttyd_rs::config::is_valid_log_level(level)
    {
        return Err(format!(
            "--log-level '{level}' is invalid: expected a bare level name \
             (trace, debug, info, warn, error, off) or an EnvFilter \
             directive containing '='"
        )
        .into());
    }

    let filter = if let Some(level) = cli_log_level {
        EnvFilter::try_new(level)?
    } else {
        EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new(config_log_level))?
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();

    Ok(())
}

/// Parse a `--bind` value into a socket address.
///
/// Accepts either a bare IP literal (`127.0.0.1`, `::1`) or an explicit
/// `ip:port` socket address. The port to use takes the following precedence:
/// the explicit port embedded in `bind`, then `cli_port`, then the current
/// configured port. This supports IPv6 literals, which the previous
/// `format!("{}:{}", ...)` concatenation could not represent.
fn parse_bind(
    bind: &str,
    cli_port: Option<u16>,
    current: SocketAddr,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    // Prefer an explicit `ip:port` socket address when the user provided one.
    if let Ok(addr) = bind.parse::<SocketAddr>() {
        return Ok(addr);
    }

    // Otherwise treat `bind` as a bare IP literal and combine with a port.
    let ip: std::net::IpAddr = bind
        .parse()
        .map_err(|_| format!("invalid --bind value '{bind}': expected an IP address (IPv4 or IPv6), optionally with a :port suffix"))?;
    let port = cli_port.unwrap_or_else(|| current.port());
    Ok(SocketAddr::new(ip, port))
}

/// Split a single `--shell` string into a command vector, honoring shell
/// quoting and escaping. Falls back to whitespace splitting for input that
/// cannot be parsed by `shlex` (for example, unbalanced quotes).
fn split_command(shell: &str) -> Vec<String> {
    shlex::split(shell).unwrap_or_else(|| shell.split_whitespace().map(String::from).collect())
}

/// Fallback working directory: the current user's home directory.
fn default_working_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// Load configuration from file or command line arguments
fn load_config(args: &Args) -> Result<Config, Box<dyn std::error::Error>> {
    let mut config = if let Some(config_path) = &args.config {
        Config::from_file(config_path)?
    } else {
        // Try to load config.toml from executable directory
        let default_config = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("config.toml")))
            .filter(|path| path.exists());

        if let Some(config_path) = default_config {
            Config::from_file(&config_path)?
        } else {
            Config::default()
        }
    };

    // Override with command line arguments. Each field is only applied
    // when explicitly provided on the CLI, so values from the config file
    // (or built-in defaults) are preserved otherwise.
    if let Some(bind) = &args.bind {
        // Accept a bare IP (IPv4 or IPv6) or an explicit `ip:port` socket
        // address. This avoids the old `format!("{}:{}", ...)` approach,
        // which could not represent IPv6 literals.
        // Warn when an embedded port silently overrides an explicit --port.
        if args.port.is_some() && bind.parse::<SocketAddr>().is_ok() {
            eprintln!(
                "Warning: --bind '{bind}' contains a port; the explicit \
                 --port value is ignored"
            );
        }
        config.bind = parse_bind(bind, args.port, config.bind)?;
    } else if let Some(port) = args.port {
        config.bind.set_port(port);
    }
    if let Some(shell) = &args.shell {
        config.command = split_command(shell);
    }
    if let Some(working_dir) = &args.working_dir {
        config.working_dir = Some(working_dir.clone());
    }
    // If no working directory was configured (no CLI flag and none in the
    // config file), fall back to the current user's home directory. This
    // keeps behavior consistent whether a config file was loaded or the
    // default configuration was used: shell then starts in $HOME.
    if config.working_dir.is_none() {
        config.working_dir = default_working_dir();
    }
    // Only override `log_level` when the flag was explicitly provided, so
    // the value from the configuration file (or the built-in default) is
    // preserved otherwise.
    if let Some(log_level) = &args.log_level {
        config.log_level = log_level.clone();
    }
    if let Some(max_connections) = args.max_connections {
        config.max_connections = max_connections;
    }

    // Session configuration
    if let Some(mode) = &args.session_mode {
        config.session.mode = mode.clone();
    }
    if let Some(timeout) = args.session_timeout {
        config.session.timeout = timeout;
    }
    if let Some(window) = args.reconnect_window {
        config.session.reconnect_window = window;
    }

    // Proxy / safety opt-in configuration. `Option<bool>` lets the CLI
    // override the config file in either direction (true→false included);
    // an unset flag leaves the config value untouched.
    if let Some(trust) = args.trust_proxy {
        config.trust_proxy = trust;
    }
    if let Some(allow) = args.allow_unauthenticated {
        config.allow_unauthenticated = allow;
    }

    // File transfer toggle
    if args.no_file_transfer {
        config.file_transfer.enabled = false;
    }

    // Audit configuration
    if args.audit {
        config.audit.enabled = true;
        if let Some(audit_file) = &args.audit_file {
            config.audit.log_file = Some(audit_file.clone());
        }
    }

    // Set up authentication if provided. clap's `requires` constraints
    // guarantee that `--auth` implies both `--username` and `--password`,
    // so this branch is always taken when `args.auth` is set.
    if args.auth {
        config.auth = Some(AuthConfig {
            method: "basic".to_string(),
            username: args.username.clone(),
            password: args.password.clone(),
            token: None,
        });
    }

    // Validate configuration
    config.validate()?;

    Ok(config)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Build minimal Args with all fields unset (None/false) except shell.
    fn base_args() -> Args {
        Args {
            port: None,
            bind: None,
            config: None,
            shell: Some("bash".to_string()),
            working_dir: None,
            log_level: None,
            session_mode: None,
            session_timeout: None,
            reconnect_window: None,
            max_connections: None,
            auth: false,
            username: None,
            password: None,
            audit: false,
            audit_file: None,
            trust_proxy: None,
            allow_unauthenticated: None,
            hash_password: false,
            no_file_transfer: true,
            check_config: false,
        }
    }

    #[test]
    fn test_load_config_defaults() {
        let mut args = base_args();
        args.shell = Some("bash --login".to_string());

        let config = load_config(&args).unwrap();
        assert_eq!(config.command, vec!["bash", "--login"]);
        assert_eq!(config.session.mode, "isolated"); // built-in default
        assert_eq!(config.session.timeout, 3600); // built-in default
        assert!(config.auth.is_none());
        assert!(!config.trust_proxy);
    }

    #[test]
    fn test_load_config_cli_overrides() {
        let mut args = base_args();
        args.port = Some(8080);
        args.bind = Some("0.0.0.0".to_string());
        args.session_mode = Some("shared_readwrite".to_string());
        args.session_timeout = Some(7200);
        args.max_connections = Some(50);
        args.auth = true;
        args.username = Some("admin".to_string());
        args.password = Some("secret".to_string());

        let config = load_config(&args).unwrap();
        assert_eq!(config.bind.to_string(), "0.0.0.0:8080");
        assert_eq!(config.max_connections, 50);
        assert_eq!(config.session.mode, "shared_readwrite");
        assert_eq!(config.session.timeout, 7200);

        let auth = config.auth.unwrap();
        assert_eq!(auth.method, "basic");
        assert_eq!(auth.username, Some("admin".to_string()));
        assert_eq!(auth.password, Some("secret".to_string()));
    }

    #[test]
    fn test_load_config_with_audit() {
        let mut args = base_args();
        args.audit = true;
        args.audit_file = Some(PathBuf::from("/tmp/audit.log"));

        let config = load_config(&args).unwrap();
        assert!(config.audit.enabled);
        assert_eq!(config.audit.log_file, Some(PathBuf::from("/tmp/audit.log")));
    }

    #[test]
    fn test_load_config_from_file() {
        let dir = std::env::temp_dir().join("ttyd-rs-main-test");
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("config.toml");

        std::fs::write(
            &config_path,
            r#"
bind = "0.0.0.0:3000"
allow_unauthenticated = true
command = ["/bin/sh"]
log_level = "warn"
max_connections = 200

[session]
mode = "shared_readonly"
timeout = 1800

[validation]
max_cols = 500
min_cols = 10
max_rows = 200
min_rows = 5
max_input_size = 16384
max_credentials_length = 1024

[rate_limit]
max_requests = 10
window_seconds = 60

[audit]
enabled = false
"#,
        )
        .unwrap();

        let mut args = base_args();
        args.config = Some(config_path);
        args.log_level = Some("info".to_string());

        let config = load_config(&args).unwrap();
        // CLI overrides file values for these fields
        assert_eq!(config.command, vec!["bash"]);
        assert_eq!(config.log_level, "info");
        // File values that are NOT overridden by CLI are preserved (M1 fix)
        assert_eq!(config.max_connections, 200);
        assert_eq!(config.session.mode, "shared_readonly");
        assert_eq!(config.session.timeout, 1800);
        assert_eq!(config.bind.to_string(), "0.0.0.0:3000");
        assert!(config.allow_unauthenticated);
        assert!(config.validate().is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_config_file_log_level_without_cli_override() {
        let dir = std::env::temp_dir().join("ttyd-rs-main-loglevel");
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("config.toml");

        std::fs::write(
            &config_path,
            r#"
bind = "0.0.0.0:9090"
allow_unauthenticated = true
command = ["/bin/zsh"]
log_level = "warn"
max_connections = 50

[session]
mode = "isolated"
timeout = 3600

[validation]
max_cols = 500
min_cols = 10
max_rows = 200
min_rows = 5
max_input_size = 16384
max_credentials_length = 1024

[rate_limit]
max_requests = 10
window_seconds = 60

[audit]
enabled = false
"#,
        )
        .unwrap();

        let mut args = base_args();
        args.config = Some(config_path);

        let config = load_config(&args).unwrap();
        assert_eq!(config.log_level, "warn");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_config_default_working_dir_to_home() {
        // Config file that omits `working_dir` must fall back to $HOME
        // rather than inheriting the process's current directory.
        let dir = std::env::temp_dir().join("ttyd-rs-main-workdir");
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("config.toml");

        std::fs::write(
            &config_path,
            r#"
bind = "0.0.0.0:3100"
allow_unauthenticated = true
command = ["/bin/sh"]
log_level = "warn"
max_connections = 100

[session]
mode = "isolated"
timeout = 3600

[validation]
max_cols = 500
min_cols = 10
max_rows = 200
min_rows = 5
max_input_size = 16384
max_credentials_length = 1024

[rate_limit]
max_requests = 10
window_seconds = 60

[audit]
enabled = false
"#,
        )
        .unwrap();

        let mut args = base_args();
        args.config = Some(config_path);

        let config = load_config(&args).unwrap();
        let home = std::env::var("HOME").expect("HOME must be set for this test");
        assert_eq!(config.working_dir, Some(PathBuf::from(home)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_bind_ipv6_literal() {
        // IPv6 bare literal with no CLI port → uses current port.
        let cur = "[::1]:7681".parse::<SocketAddr>().unwrap();
        assert_eq!(
            parse_bind("::1", None, cur).unwrap(),
            "[::1]:7681".parse::<SocketAddr>().unwrap()
        );

        // IPv6 literal combined with an explicit CLI port.
        assert_eq!(
            parse_bind("::1", Some(9000), cur).unwrap(),
            "[::1]:9000".parse::<SocketAddr>().unwrap()
        );

        // Explicit IPv6 list form with port wins over CLI port.
        let full = "[2001:db8::1]:8443".parse::<SocketAddr>().unwrap();
        let p = parse_bind("[2001:db8::1]:8443", Some(9000), cur).unwrap();
        assert_eq!(p, full);
    }

    #[test]
    fn test_parse_bind_bare_ipv4_and_with_port() {
        let cur = "0.0.0.0:80".parse::<SocketAddr>().unwrap();
        // Bare IPv4 uses current port.
        assert_eq!(
            parse_bind("127.0.0.1", None, cur).unwrap(),
            "127.0.0.1:80".parse::<SocketAddr>().unwrap()
        );
        // Bare IPv4 + CLI port.
        assert_eq!(
            parse_bind("127.0.0.1", Some(8080), cur).unwrap(),
            "127.0.0.1:8080".parse::<SocketAddr>().unwrap()
        );
        // Explicit ip:port beats CLI port.
        assert_eq!(
            parse_bind("127.0.0.1:9999", Some(8080), cur).unwrap(),
            "127.0.0.1:9999".parse::<SocketAddr>().unwrap()
        );
    }

    #[test]
    fn test_parse_bind_rejects_non_ip() {
        let cur = "[::1]:80".parse::<SocketAddr>().unwrap();
        assert!(parse_bind("not-an-ip", None, cur).is_err());
        assert!(parse_bind("", None, cur).is_err());
    }

    #[test]
    fn test_split_command_with_quotes() {
        assert_eq!(
            split_command(r#"bash -c "echo 'hi world'""#),
            vec!["bash", "-c", "echo 'hi world'"]
        );
        // Space-separated without quotes still works.
        assert_eq!(split_command("bash --login"), vec!["bash", "--login"]);
    }

    /// Three-state flags: `Some(false)` must be able to turn a config-file
    /// `true` off, and `None` must leave the config value untouched.
    #[test]
    fn test_load_config_tristate_flags() {
        let dir = std::env::temp_dir().join("ttyd-rs-main-tristate");
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("config.toml");

        std::fs::write(
            &config_path,
            concat!(
                "bind = \"127.0.0.1:7681\"\n",
                "command = [\"/bin/sh\"]\n",
                "trust_proxy = true\n",
                "allow_unauthenticated = true\n",
            ),
        )
        .unwrap();

        // No CLI flags → config values preserved.
        let mut args = base_args();
        args.config = Some(config_path.clone());
        let config = load_config(&args).unwrap();
        assert!(config.trust_proxy);
        assert!(config.allow_unauthenticated);

        // Explicit `=false` overrides the config file.
        let mut args = base_args();
        args.config = Some(config_path.clone());
        args.trust_proxy = Some(false);
        args.allow_unauthenticated = Some(false);
        let config = load_config(&args).unwrap();
        assert!(!config.trust_proxy);
        assert!(!config.allow_unauthenticated);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--check-config` mode validates the merged configuration (file +
    /// CLI overrides) without starting the server.
    #[test]
    fn test_check_config_mode() {
        let dir = std::env::temp_dir().join("ttyd-rs-main-check-config");
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("config.toml");

        // Valid configuration: load_config succeeds.
        std::fs::write(
            &config_path,
            concat!(
                "bind = \"127.0.0.1:7681\"\n",
                "command = [\"/bin/sh\"]\n",
                "file_transfer.enabled = false\n",
            ),
        )
        .unwrap();
        let mut args = base_args();
        args.config = Some(config_path.clone());
        assert!(load_config(&args).is_ok());

        // Invalid configuration: load_config fails with a clear error.
        std::fs::write(
            &config_path,
            concat!(
                "bind = \"127.0.0.1:7681\"\n",
                "command = [\"/bin/sh\"]\n",
                "log_level = \"tracert\"\n",
            ),
        )
        .unwrap();
        let mut args = base_args();
        args.config = Some(config_path.clone());
        let err = load_config(&args).unwrap_err();
        assert!(err.to_string().contains("Invalid log_level"));

        // CLI overrides are applied before validation, so an invalid CLI
        // value is caught too.
        let mut args = base_args();
        args.config = Some(config_path.clone());
        args.log_level = Some("verbose".to_string());
        let err = load_config(&args).unwrap_err();
        assert!(err.to_string().contains("Invalid log_level"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
