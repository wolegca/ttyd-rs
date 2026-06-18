/// Token-based authentication implementation
pub struct TokenAuth {
    token: String,
}

impl TokenAuth {
    pub fn new(token: String) -> Self {
        Self { token }
    }

    /// Validate the provided token against the configured token.
    /// Uses constant-time comparison to prevent timing attacks.
    pub fn validate(&self, credentials: &str) -> bool {
        constant_time_eq(self.token.as_bytes(), credentials.as_bytes())
    }
}

/// Constant-time byte comparison to prevent timing attacks.
/// Returns true if both slices are equal.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_auth_valid() {
        let auth = TokenAuth::new("my-secret-token".to_string());
        assert!(auth.validate("my-secret-token"));
    }

    #[test]
    fn test_token_auth_invalid() {
        let auth = TokenAuth::new("my-secret-token".to_string());
        assert!(!auth.validate("wrong-token"));
    }

    #[test]
    fn test_token_auth_empty() {
        let auth = TokenAuth::new("my-secret-token".to_string());
        assert!(!auth.validate(""));
    }

    #[test]
    fn test_token_auth_same_length_different_content() {
        let auth = TokenAuth::new("abcdef".to_string());
        assert!(!auth.validate("abcdeg"));
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"", b"hello"));
        assert!(constant_time_eq(b"", b""));
    }
}
