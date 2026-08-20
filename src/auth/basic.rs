use argon2::Argon2;
/// Basic authentication implementation
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng,
};
use base64::{Engine as _, engine::general_purpose};

#[derive(Clone)]
pub struct BasicAuth {
    username: String,
    /// Argon2id hash of the configured password (PHC string, random salt)
    password_hash: String,
}

impl BasicAuth {
    /// Hash the configured password with Argon2id and a random salt.
    ///
    /// Argon2id is memory-hard, so a leaked in-memory hash cannot be cracked
    /// with rainbow tables or cheap GPU brute force the way an unsalted
    /// SHA-256 digest can.
    pub fn new(username: String, password: String) -> Result<Self, argon2::password_hash::Error> {
        let salt = SaltString::generate(&mut OsRng);
        let password_hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)?
            .to_string();
        Ok(Self {
            username,
            password_hash,
        })
    }

    /// Validate credentials encoded as "username:password" in base64
    pub fn validate(&self, credentials: &str) -> bool {
        let Ok(decoded) = general_purpose::STANDARD.decode(credentials) else {
            return false;
        };
        let Ok(decoded_str) = String::from_utf8(decoded) else {
            return false;
        };
        let parts: Vec<&str> = decoded_str.splitn(2, ':').collect();
        if parts.len() != 2 {
            return false;
        }
        // Use constant-time comparison for the username to prevent timing
        // side-channel attacks. The Argon2 verification below always runs,
        // even when the username does not match, so connection timing does
        // not reveal which part was wrong.
        let username_ok =
            super::token::constant_time_eq(parts[0].as_bytes(), self.username.as_bytes());
        username_ok & self.verify_password(parts[1])
    }

    /// Verify a candidate password against the stored Argon2id hash.
    ///
    /// Fails closed when the stored hash is malformed.
    fn verify_password(&self, password: &str) -> bool {
        let Ok(parsed) = PasswordHash::new(&self.password_hash) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    }

    /// Extract credentials from Authorization header
    pub fn extract_from_header(header: &str) -> Option<String> {
        header
            .strip_prefix("Basic ")
            .map(|credentials| credentials.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_auth_valid() {
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string()).unwrap();

        // "admin:secret" in base64
        let credentials = general_purpose::STANDARD.encode("admin:secret");
        assert!(auth.validate(&credentials));
    }

    #[test]
    fn test_basic_auth_invalid() {
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string()).unwrap();

        // Wrong password
        let credentials = general_purpose::STANDARD.encode("admin:wrong");
        assert!(!auth.validate(&credentials));
    }

    #[test]
    fn test_wrong_username_fails() {
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string()).unwrap();

        // Correct password but wrong username
        let credentials = general_purpose::STANDARD.encode("root:secret");
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
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string()).unwrap();
        // The stored hash must be a PHC string, not plaintext
        assert_ne!(auth.password_hash, "secret");
        assert!(auth.password_hash.starts_with("$argon2id$"));
    }

    #[test]
    fn test_random_salt_per_instance() {
        let auth1 = BasicAuth::new("user".to_string(), "pass".to_string()).unwrap();
        let auth2 = BasicAuth::new("user".to_string(), "pass".to_string()).unwrap();
        // Same password must produce different hashes (random salt)
        assert_ne!(auth1.password_hash, auth2.password_hash);
    }

    #[test]
    fn test_same_password_validates_across_instances() {
        let auth1 = BasicAuth::new("user".to_string(), "pass".to_string()).unwrap();
        let auth2 = BasicAuth::new("user".to_string(), "pass".to_string()).unwrap();
        let credentials = general_purpose::STANDARD.encode("user:pass");
        assert!(auth1.validate(&credentials));
        assert!(auth2.validate(&credentials));
    }

    #[test]
    fn test_different_password_fails() {
        let auth = BasicAuth::new("user".to_string(), "pass1".to_string()).unwrap();
        let credentials = general_purpose::STANDARD.encode("user:pass2");
        assert!(!auth.validate(&credentials));
    }
}
