/// Configuration module for ttyd-rs
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

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
            bind: "127.0.0.1:7681"
                .parse()
                .map_err(|e| format!("Failed to parse default address: {}", e))
                .ok()
                .unwrap_or_else(|| {
                    "0.0.0.0:7681".parse().ok().unwrap_or_else(|| {
                        // This should never happen with valid IP addresses
                        panic!("Invalid default socket address");
                    })
                }),
            command: vec!["bash".to_string()],
            working_dir: None,
            auth: None,
            log_level: "info".to_string(),
            max_connections: 100,
            audit: AuditConfig::default(),
            session: SessionConfig::default(),
            validation: ValidationConfig::default(),
            rate_limit: RateLimitConfig::default(),
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
    pub fn from_file(path: &PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<(), String> {
        // Validate session mode
        if !["isolated", "shared_readonly", "shared_readwrite"]
            .contains(&self.session.mode.as_str())
        {
            return Err(format!("Invalid session mode: {}", self.session.mode));
        }

        // Validate terminal size ranges
        if self.validation.min_cols >= self.validation.max_cols {
            return Err("min_cols must be less than max_cols".to_string());
        }

        if self.validation.min_rows >= self.validation.max_rows {
            return Err("min_rows must be less than max_rows".to_string());
        }

        // Validate rate limit
        if self.rate_limit.max_requests == 0 {
            return Err("max_requests must be greater than 0".to_string());
        }

        if self.rate_limit.window_seconds == 0 {
            return Err("window_seconds must be greater than 0".to_string());
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
}
