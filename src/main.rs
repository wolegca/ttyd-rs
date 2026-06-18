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
    #[arg(short, long, default_value = "bash")]
    shell: String,

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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Initialize tracing/logging
    init_logging(&args.log_level)?;

    // Load configuration
    let config = load_config(&args)?;

    tracing::info!("Starting ttyd-rs v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Configuration: {:?}", config);

    // Start the server
    server::start_server(config).await?;

    Ok(())
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
        Config::default()
    };

    // Override with command line arguments
    let bind_addr = format!("{}:{}", args.bind, args.port);
    config.bind = bind_addr.parse()?;
    config.command = vec![args.shell.clone()];
    config.working_dir = args.working_dir.clone();
    config.log_level = args.log_level.clone();
    config.max_connections = args.max_connections;

    // Session configuration
    config.session.mode = args.session_mode.clone();
    config.session.timeout = args.session_timeout;

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
