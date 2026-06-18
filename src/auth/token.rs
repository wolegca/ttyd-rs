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
/// Always iterates over the full length of both inputs, even when they
/// differ in length, so the comparison time depends only on the longer
/// input — not on where the first mismatch occurs.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len_a = a.len();
    let len_b = b.len();
    let max_len = if len_a > len_b { len_a } else { len_b };

    // Accumulate XOR differences across all positions.
    // For the shorter slice, treat out-of-bounds bytes as 0 and mark a
    // difference via the length-mismatch byte so that differing lengths
    // always produce a non-zero result.
    let mut result = 0u8;
    for i in 0..max_len {
        let byte_a = if i < len_a { a[i] } else { 0 };
        let byte_b = if i < len_b { b[i] } else { 0 };
        // When indices are beyond the shorter slice, the bytes are
        // artificial zeros — XOR alone would be 0 even though the real
        // inputs differ.  Mix in a length-mismatch marker so these
        // positions always contribute a non-zero value when lengths differ.
        let len_mismatch = ((i >= len_a) as u8) ^ ((i >= len_b) as u8);
        result |= (byte_a ^ byte_b) | len_mismatch;
    }

    // Also check length equality — the loop above may produce 0 if both
    // inputs are empty, so a direct length comparison is still needed.
    result == 0 && len_a == len_b
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
