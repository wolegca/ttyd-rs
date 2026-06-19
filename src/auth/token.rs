/// Token-based authentication implementation
use sha2::{Digest, Sha256};

pub struct TokenAuth {
    /// SHA-256 hex digest of the configured token
    token_hash: String,
}

impl TokenAuth {
    pub fn new(token: String) -> Self {
        let token_hash = Self::hash_token(&token);
        Self { token_hash }
    }

    /// Compute SHA-256 hex digest of a token
    fn hash_token(token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Validate the provided token against the configured token.
    /// Hashes the incoming token before comparison and uses constant-time
    /// comparison to prevent timing attacks.
    pub fn validate(&self, credentials: &str) -> bool {
        let incoming_hash = Self::hash_token(credentials);
        constant_time_eq(self.token_hash.as_bytes(), incoming_hash.as_bytes())
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
    fn test_token_is_hashed_internally() {
        let auth = TokenAuth::new("my-secret-token".to_string());
        // The stored hash should be the SHA-256 hex digest, not plaintext
        assert_ne!(auth.token_hash, "my-secret-token");
        assert_eq!(
            auth.token_hash,
            "ea5add57437cbf20af59034d7ed17968dcc56767b41965fcc5b376d45db8b4a3"
        );
    }

    #[test]
    fn test_token_hash_consistency() {
        let auth1 = TokenAuth::new("my-secret-token".to_string());
        let auth2 = TokenAuth::new("my-secret-token".to_string());
        // Same token must produce the same hash
        assert_eq!(auth1.token_hash, auth2.token_hash);
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
