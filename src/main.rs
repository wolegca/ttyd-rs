mod assets;
mod audit;
mod auth;
mod config;
mod protocol;
mod pty;
mod rate_limit;
mod server;
mod session;
mod validation;

use clap::Parser;
use config::Config;
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "ttyd-rs")]
#[command(about = "Share your terminal over the web", long_about = None)]
#[command(version)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "7681")]
    port: u16,

    /// Address to bind to
    #[arg(short, long, default_value = "127.0.0.1")]
    bind: String,

    /// Configuration file path
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Shell command to execute
    #[arg(short, long)]
    shell: Option<String>,

    /// Working directory for the shell
    #[arg(short = 'w', long)]
    working_dir: Option<PathBuf>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Session mode: isolated, shared-ro, shared-rw
    #[arg(long, default_value = "isolated")]
    session_mode: String,

    /// Session timeout in seconds (0 = no timeout)
    #[arg(long, default_value = "3600")]
    session_timeout: u64,

    /// Reconnect window in seconds — how long to keep empty sessions alive
    #[arg(long, default_value = "60")]
    reconnect_window: u64,

    /// Maximum number of concurrent connections
    #[arg(long, default_value = "100")]
    max_connections: usize,

    /// Enable authentication
    #[arg(long)]
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
    #[arg(long)]
    audit_file: Option<PathBuf>,

    /// Trust proxy headers (X-Real-IP / X-Forwarded-For) for client IP
    #[arg(long)]
    trust_proxy: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    // Initialize tracing/logging
    if let Err(e) = init_logging(&args.log_level) {
        eprintln!("Failed to initialize logging: {}", e);
        std::process::exit(1);
    }

    // Load configuration
    let config = match load_config(&args) {
        Ok(config) => config,
        Err(e) => {
            tracing::error!("Failed to load configuration: {}", e);
            std::process::exit(1);
        }
    };

    tracing::info!("Starting ttyd-rs v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Configuration: {:?}", config);

    // Start the server
    if let Err(e) = server::start_server(config).await {
        tracing::error!("Server error: {}", e);
        std::process::exit(1);
    }
}

/// Initialize the tracing subscriber for logging
fn init_logging(log_level: &str) -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new(log_level))?;

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer())
        .init();

    Ok(())
}

/// Load configuration from file or command line arguments
fn load_config(args: &Args) -> Result<Config, Box<dyn std::error::Error>> {
    let mut config = if let Some(config_path) = &args.config {
        tracing::info!("Loading configuration from {:?}", config_path);
        Config::from_file(config_path)?
    } else {
        // Try to load config.toml from executable directory
        let default_config = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("config.toml")))
            .filter(|path| path.exists());

        if let Some(config_path) = default_config {
            tracing::info!("Loading configuration from {:?}", config_path);
            Config::from_file(&config_path)?
        } else {
            Config::default()
        }
    };

    // Override with command line arguments
    let bind_addr = format!("{}:{}", args.bind, args.port);
    config.bind = bind_addr.parse()?;
    if let Some(shell) = &args.shell {
        config.command = shell.split_whitespace().map(String::from).collect();
    }
    config.working_dir = args.working_dir.clone();
    config.log_level = args.log_level.clone();
    config.max_connections = args.max_connections;

    // Session configuration
    config.session.mode = args.session_mode.clone();
    config.session.timeout = args.session_timeout;
    config.session.reconnect_window = args.reconnect_window;

    // Proxy configuration
    if args.trust_proxy {
        config.trust_proxy = true;
    }

    // Audit configuration
    if args.audit {
        config.audit.enabled = true;
        if let Some(audit_file) = &args.audit_file {
            config.audit.log_file = Some(audit_file.clone());
        }
    }

    // Set up authentication if provided
    if args.auth
        && let (Some(username), Some(password)) = (&args.username, &args.password)
    {
        config.auth = Some(config::AuthConfig {
            method: "basic".to_string(),
            username: Some(username.clone()),
            password: Some(password.clone()),
            token: None,
            audit_enabled: true,
        });
    }

    // Validate configuration
    config.validate()?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_config_defaults() {
        let args = Args {
            port: 7681,
            bind: "127.0.0.1".to_string(),
            config: None,
            shell: Some("bash --login".to_string()),
            working_dir: None,
            log_level: "info".to_string(),
            session_mode: "isolated".to_string(),
            session_timeout: 3600,
            reconnect_window: 60,
            max_connections: 100,
            auth: false,
            username: None,
            password: None,
            audit: false,
            audit_file: None,
            trust_proxy: false,
        };

        let config = load_config(&args).unwrap();
        assert_eq!(config.command, vec!["bash", "--login"]);
        assert_eq!(config.session.mode, "isolated");
        assert_eq!(config.session.timeout, 3600);
        assert!(config.auth.is_none());
        assert!(!config.trust_proxy);
    }

    #[test]
    fn test_load_config_with_auth() {
        let args = Args {
            port: 8080,
            bind: "0.0.0.0".to_string(),
            config: None,
            shell: Some("/bin/zsh".to_string()),
            working_dir: Some(PathBuf::from("/tmp")),
            log_level: "debug".to_string(),
            session_mode: "shared_readwrite".to_string(),
            session_timeout: 7200,
            reconnect_window: 60,
            max_connections: 50,
            auth: true,
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
            audit: false,
            audit_file: None,
            trust_proxy: false,
        };

        let config = load_config(&args).unwrap();
        assert_eq!(config.command, vec!["/bin/zsh"]);
        assert_eq!(config.working_dir, Some(PathBuf::from("/tmp")));
        assert_eq!(config.log_level, "debug");
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
        let args = Args {
            port: 7681,
            bind: "127.0.0.1".to_string(),
            config: None,
            shell: Some("bash".to_string()),
            working_dir: None,
            log_level: "info".to_string(),
            session_mode: "isolated".to_string(),
            session_timeout: 3600,
            reconnect_window: 60,
            max_connections: 100,
            auth: false,
            username: None,
            password: None,
            audit: true,
            audit_file: Some(PathBuf::from("/tmp/audit.log")),
            trust_proxy: false,
        };

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

        let args = Args {
            port: 9999,
            bind: "127.0.0.1".to_string(),
            config: Some(config_path),
            shell: Some("bash".to_string()),
            working_dir: None,
            log_level: "info".to_string(),
            session_mode: "isolated".to_string(),
            session_timeout: 3600,
            reconnect_window: 60,
            max_connections: 100,
            auth: false,
            username: None,
            password: None,
            audit: false,
            audit_file: None,
            trust_proxy: false,
        };

        let config = load_config(&args).unwrap();
        // CLI overrides file values for these fields
        assert_eq!(config.command, vec!["bash"]);
        assert_eq!(config.log_level, "info");
        assert_eq!(config.max_connections, 100);
        // File values that are NOT overridden by CLI
        assert_eq!(config.session.mode, "isolated"); // CLI overrides
        assert!(config.validate().is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
