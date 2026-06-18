/// Basic authentication implementation
use base64::{Engine as _, engine::general_purpose};
use sha2::{Digest, Sha256};

pub struct BasicAuth {
    username: String,
    /// SHA-256 hex digest of the configured password
    password_hash: String,
}

impl BasicAuth {
    pub fn new(username: String, password: String) -> Self {
        let password_hash = Self::hash_password(&password);
        Self {
            username,
            password_hash,
        }
    }

    /// Compute SHA-256 hex digest of a password
    fn hash_password(password: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(password.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Validate credentials encoded as "username:password" in base64
    pub fn validate(&self, credentials: &str) -> bool {
        match general_purpose::STANDARD.decode(credentials) {
            Ok(decoded) => match String::from_utf8(decoded) {
                Ok(decoded_str) => {
                    let parts: Vec<&str> = decoded_str.splitn(2, ':').collect();
                    if parts.len() == 2 {
                        let incoming_hash = Self::hash_password(parts[1]);
                        // Use constant-time comparison for both username and
                        // password hash to prevent timing side-channel attacks.
                        super::token::constant_time_eq(
                            parts[0].as_bytes(),
                            self.username.as_bytes(),
                        ) & super::token::constant_time_eq(
                            incoming_hash.as_bytes(),
                            self.password_hash.as_bytes(),
                        )
                    } else {
                        false
                    }
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    /// Extract credentials from Authorization header
    #[allow(dead_code)]
    pub fn extract_from_header(header: &str) -> Option<String> {
        header
            .strip_prefix("Basic ")
            .map(|credentials| credentials.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_auth_valid() {
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string());

        // "admin:secret" in base64
        let credentials = general_purpose::STANDARD.encode("admin:secret");
        assert!(auth.validate(&credentials));
    }

    #[test]
    fn test_basic_auth_invalid() {
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string());

        // Wrong password
        let credentials = general_purpose::STANDARD.encode("admin:wrong");
        assert!(!auth.validate(&credentials));
    }

    #[test]
    fn test_extract_from_header() {
        let header = "Basic YWRtaW46c2VjcmV0";
        let result = BasicAuth::extract_from_header(header);
        assert_eq!(result, Some("YWRtaW46c2VjcmV0".to_string()));

        let invalid_header = "Bearer token";
        let result = BasicAuth::extract_from_header(invalid_header);
        assert_eq!(result, None);
    }

    #[test]
    fn test_password_is_hashed_internally() {
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string());
        // The stored hash should be the SHA-256 hex digest, not plaintext
        assert_ne!(auth.password_hash, "secret");
        assert_eq!(
            auth.password_hash,
            "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"
        );
    }

    #[test]
    fn test_hash_consistency() {
        let auth1 = BasicAuth::new("user".to_string(), "pass".to_string());
        let auth2 = BasicAuth::new("user".to_string(), "pass".to_string());
        // Same password must produce the same hash
        assert_eq!(auth1.password_hash, auth2.password_hash);
    }

    #[test]
    fn test_different_passwords_different_hashes() {
        let auth1 = BasicAuth::new("user".to_string(), "pass1".to_string());
        let auth2 = BasicAuth::new("user".to_string(), "pass2".to_string());
        assert_ne!(auth1.password_hash, auth2.password_hash);
    }
}
