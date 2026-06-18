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
/// Delegates to the `subtle` crate which is audited for constant-time
/// guarantees.  Returns false immediately when lengths differ (length is
/// not considered secret in typical token-auth scenarios).
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
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

    #[test]
    fn test_constant_time_eq_symmetric() {
        // Ensure symmetric: constant_time_eq(a, b) == constant_time_eq(b, a)
        assert_eq!(
            constant_time_eq(b"short", b"longer"),
            constant_time_eq(b"longer", b"short")
        );
        assert_eq!(
            constant_time_eq(b"abc", b"xyz"),
            constant_time_eq(b"xyz", b"abc")
        );
    }
}
