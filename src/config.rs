/// Configuration module for ttyd-rs
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during configuration loading and validation
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    FileRead(#[from] std::io::Error),

    #[error("Failed to parse config file: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("Invalid session mode: {0}")]
    InvalidSessionMode(String),

    #[error("Invalid terminal size range: {0}")]
    InvalidTerminalSize(String),

    #[error("Invalid rate limit: {0}")]
    InvalidRateLimit(String),

    #[error("Invalid auth configuration: {0}")]
    InvalidAuth(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Server bind address
    pub bind: SocketAddr,

    /// Shell command to execute
    pub command: Vec<String>,

    /// Working directory for the shell
    pub working_dir: Option<PathBuf>,

    /// Authentication configuration
    pub auth: Option<AuthConfig>,

    /// Logging configuration
    pub log_level: String,

    /// Maximum number of concurrent connections
    pub max_connections: usize,

    /// Audit log configuration
    pub audit: AuditConfig,

    /// Session configuration
    pub session: SessionConfig,

    /// Validation configuration
    pub validation: ValidationConfig,

    /// Rate limiting configuration
    pub rate_limit: RateLimitConfig,

    /// Trust proxy headers (X-Real-IP / X-Forwarded-For) for client IP
    #[serde(default)]
    pub trust_proxy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Authentication method: "basic" or "token"
    pub method: String,

    /// Username for basic auth
    pub username: Option<String>,

    /// Password for basic auth
    pub password: Option<String>,

    /// Token for token-based auth
    pub token: Option<String>,

    /// Enable audit logging for auth events
    pub audit_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuditConfig {
    /// Enable audit logging
    pub enabled: bool,

    /// Audit log file path
    pub log_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Session mode: "isolated", "shared_readonly", or "shared_readwrite"
    pub mode: String,

    /// Session timeout in seconds (0 = no timeout)
    pub timeout: u64,

    /// Reconnect window in seconds — how long to keep empty sessions alive
    /// for client reconnection (default: 60)
    #[serde(default = "default_reconnect_window")]
    pub reconnect_window: u64,
}

fn default_reconnect_window() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationConfig {
    /// Maximum terminal columns
    pub max_cols: u16,

    /// Minimum terminal columns
    pub min_cols: u16,

    /// Maximum terminal rows
    pub max_rows: u16,

    /// Minimum terminal rows
    pub min_rows: u16,

    /// Maximum input payload size in bytes
    pub max_input_size: usize,

    /// Maximum credentials length
    pub max_credentials_length: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Maximum requests allowed
    pub max_requests: u32,

    /// Time window in seconds
    pub window_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
                7681,
            ),
            command: vec!["bash".to_string()],
            working_dir: None,
            auth: None,
            log_level: "info".to_string(),
            max_connections: 100,
            audit: AuditConfig::default(),
            session: SessionConfig::default(),
            validation: ValidationConfig::default(),
            rate_limit: RateLimitConfig::default(),
            trust_proxy: false,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            method: "basic".to_string(),
            username: None,
            password: None,
            token: None,
            audit_enabled: true,
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            mode: "isolated".to_string(),
            timeout: 3600, // 1 hour
            reconnect_window: 60,
        }
    }
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            max_cols: 500,
            min_cols: 10,
            max_rows: 200,
            min_rows: 5,
            max_input_size: 16384,
            max_credentials_length: 1024,
        }
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 10,
            window_seconds: 60,
        }
    }
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file(path: &PathBuf) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Validate session mode (accept both hyphenated and underscore forms)
        if ![
            "isolated",
            "shared_readonly",
            "shared_readwrite",
            "shared-ro",
            "shared-rw",
        ]
        .contains(&self.session.mode.as_str())
        {
            return Err(ConfigError::InvalidSessionMode(self.session.mode.clone()));
        }

        // Validate terminal size ranges
        if self.validation.min_cols >= self.validation.max_cols {
            return Err(ConfigError::InvalidTerminalSize(
                "min_cols must be less than max_cols".to_string(),
            ));
        }

        if self.validation.min_rows >= self.validation.max_rows {
            return Err(ConfigError::InvalidTerminalSize(
                "min_rows must be less than max_rows".to_string(),
            ));
        }

        // Validate rate limit
        if self.rate_limit.max_requests == 0 {
            return Err(ConfigError::InvalidRateLimit(
                "max_requests must be greater than 0".to_string(),
            ));
        }

        if self.rate_limit.window_seconds == 0 {
            return Err(ConfigError::InvalidRateLimit(
                "window_seconds must be greater than 0".to_string(),
            ));
        }

        // Validate auth configuration consistency
        if let Some(auth) = &self.auth {
            match auth.method.as_str() {
                "basic" => {
                    if auth.username.is_none() || auth.password.is_none() {
                        return Err(ConfigError::InvalidAuth(
                            "basic auth requires both username and password".to_string(),
                        ));
                    }
                }
                "token" => {
                    if auth.token.is_none() {
                        return Err(ConfigError::InvalidAuth(
                            "token auth requires a token value".to_string(),
                        ));
                    }
                }
                other => {
                    return Err(ConfigError::InvalidAuth(format!(
                        "unknown auth method: '{}'",
                        other
                    )));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.command, vec!["bash".to_string()]);
        assert_eq!(config.session.mode, "isolated");
        assert_eq!(config.session.timeout, 3600);
    }

    #[test]
    fn test_config_validation_valid() {
        let config = Config::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation_invalid_mode() {
        let mut config = Config::default();
        config.session.mode = "invalid_mode".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_invalid_terminal_size() {
        let mut config = Config::default();
        config.validation.min_cols = 100;
        config.validation.max_cols = 50;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_equal_min_max_rows() {
        let mut config = Config::default();
        config.validation.min_rows = 24;
        config.validation.max_rows = 24;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_zero_rate_limit() {
        let mut config = Config::default();
        config.rate_limit.max_requests = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validation_zero_window() {
        let mut config = Config::default();
        config.rate_limit.window_seconds = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_from_file() {
        let dir = std::env::temp_dir().join("ttyd-rs-config-test");
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("test.toml");

        std::fs::write(
            &config_path,
            r#"
bind = "0.0.0.0:9090"
command = ["/bin/zsh"]
log_level = "debug"
max_connections = 50

[session]
mode = "shared_readwrite"
timeout = 7200

[validation]
max_cols = 300
min_cols = 20
max_rows = 150
min_rows = 10
max_input_size = 8192
max_credentials_length = 512

[rate_limit]
max_requests = 20
window_seconds = 120

[audit]
enabled = true
"#,
        )
        .unwrap();

        let config = Config::from_file(&config_path).unwrap();
        assert_eq!(config.command, vec!["/bin/zsh"]);
        assert_eq!(config.log_level, "debug");
        assert_eq!(config.max_connections, 50);
        assert_eq!(config.session.mode, "shared_readwrite");
        assert_eq!(config.session.timeout, 7200);
        assert_eq!(config.validation.max_cols, 300);
        assert_eq!(config.validation.min_cols, 20);
        assert_eq!(config.rate_limit.max_requests, 20);
        assert!(config.audit.enabled);
        assert!(config.validate().is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_config_from_file_not_found() {
        let result = Config::from_file(&std::path::PathBuf::from("/nonexistent/path.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_config_from_file_invalid_toml() {
        let dir = std::env::temp_dir().join("ttyd-rs-config-test-invalid");
        let _ = std::fs::create_dir_all(&dir);
        let config_path = dir.join("bad.toml");

        std::fs::write(&config_path, "this is not valid toml [[[[").unwrap();

        let result = Config::from_file(&config_path);
        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
