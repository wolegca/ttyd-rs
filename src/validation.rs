/// Input validation module for security
use crate::config::ValidationConfig;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ValidationError {
    #[error("Terminal size out of range: {0}")]
    TerminalSizeOutOfRange(String),

    #[error("Input payload too large: {0} bytes (max: {1} bytes)")]
    PayloadTooLarge(usize, usize),

    #[allow(dead_code)]
    #[error("Invalid UTF-8 in input")]
    InvalidUtf8,

    #[error("Credentials too long")]
    CredentialsTooLong,

    #[error("Invalid message format: {0}")]
    InvalidFormat(String),

    #[allow(dead_code)]
    #[error("Rate limit exceeded")]
    RateLimitExceeded,
}

impl ValidationConfig {
    /// Validate terminal size
    pub fn validate_terminal_size(&self, cols: u16, rows: u16) -> Result<(), ValidationError> {
        if cols < self.min_cols || cols > self.max_cols {
            return Err(ValidationError::TerminalSizeOutOfRange(format!(
                "cols={} (valid range: {}-{})",
                cols, self.min_cols, self.max_cols
            )));
        }

        if rows < self.min_rows || rows > self.max_rows {
            return Err(ValidationError::TerminalSizeOutOfRange(format!(
                "rows={} (valid range: {}-{})",
                rows, self.min_rows, self.max_rows
            )));
        }

        Ok(())
    }

    /// Validate input payload size
    pub fn validate_input_payload(&self, payload: &str) -> Result<(), ValidationError> {
        let size = payload.len();
        if size > self.max_input_size {
            return Err(ValidationError::PayloadTooLarge(size, self.max_input_size));
        }

        // Ensure it's valid UTF-8 (already guaranteed by String type, but explicit check)
        if !payload.is_empty() && payload.chars().any(|c| c == '\0') {
            return Err(ValidationError::InvalidFormat(
                "Null bytes not allowed".to_string(),
            ));
        }

        Ok(())
    }

    /// Validate credentials format and length
    pub fn validate_credentials(&self, credentials: &str) -> Result<(), ValidationError> {
        if credentials.len() > self.max_credentials_length {
            return Err(ValidationError::CredentialsTooLong);
        }

        // Basic format check - should be base64
        if !credentials
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
        {
            return Err(ValidationError::InvalidFormat(
                "Invalid base64 format".to_string(),
            ));
        }

        Ok(())
    }

    /// Validate authentication method
    pub fn validate_auth_method(&self, method: &str) -> Result<(), ValidationError> {
        match method {
            "basic" | "token" => Ok(()),
            _ => Err(ValidationError::InvalidFormat(format!(
                "Unsupported auth method: {}",
                method
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_terminal_size() {
        let config = ValidationConfig::default();
        assert!(config.validate_terminal_size(80, 24).is_ok());
        assert!(config.validate_terminal_size(100, 40).is_ok());
    }

    #[test]
    fn test_invalid_terminal_size() {
        let config = ValidationConfig::default();

        // Too small
        assert!(config.validate_terminal_size(5, 24).is_err());
        assert!(config.validate_terminal_size(80, 2).is_err());

        // Too large
        assert!(config.validate_terminal_size(600, 24).is_err());
        assert!(config.validate_terminal_size(80, 300).is_err());
    }

    #[test]
    fn test_valid_input_payload() {
        let config = ValidationConfig::default();
        assert!(config.validate_input_payload("ls -la").is_ok());
        assert!(config.validate_input_payload("echo 'hello world'").is_ok());
    }

    #[test]
    fn test_payload_too_large() {
        let config = ValidationConfig::default();
        let large_payload = "x".repeat(20000);
        assert!(config.validate_input_payload(&large_payload).is_err());
    }

    #[test]
    fn test_valid_credentials() {
        let config = ValidationConfig::default();
        assert!(config.validate_credentials("YWRtaW46c2VjcmV0").is_ok());
    }

    #[test]
    fn test_invalid_credentials() {
        let config = ValidationConfig::default();

        // Invalid base64 characters
        assert!(config.validate_credentials("admin:secret").is_err());

        // Too long
        let long_creds = "a".repeat(2000);
        assert!(config.validate_credentials(&long_creds).is_err());
    }

    #[test]
    fn test_auth_method_validation() {
        let config = ValidationConfig::default();
        assert!(config.validate_auth_method("basic").is_ok());
        assert!(config.validate_auth_method("token").is_ok());
        assert!(config.validate_auth_method("invalid").is_err());
    }

    #[test]
    fn test_terminal_size_boundary_min() {
        let config = ValidationConfig::default();
        // Exactly at min boundary should pass
        assert!(config.validate_terminal_size(10, 5).is_ok());
        // One below min should fail
        assert!(config.validate_terminal_size(9, 5).is_err());
        assert!(config.validate_terminal_size(10, 4).is_err());
    }

    #[test]
    fn test_terminal_size_boundary_max() {
        let config = ValidationConfig::default();
        // Exactly at max boundary should pass
        assert!(config.validate_terminal_size(500, 200).is_ok());
        // One above max should fail
        assert!(config.validate_terminal_size(501, 200).is_err());
        assert!(config.validate_terminal_size(500, 201).is_err());
    }

    #[test]
    fn test_input_payload_with_null_bytes() {
        let config = ValidationConfig::default();
        let payload = "hello\0world";
        assert!(config.validate_input_payload(payload).is_err());
    }
}
