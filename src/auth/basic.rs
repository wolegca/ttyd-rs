use argon2::Argon2;
/// Basic authentication implementation
///
/// argon2 0.6 moved `PasswordHash` into the `phc` submodule of the
/// `password-hash` crate (it used to sit directly under `password_hash::`).
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use base64::{Engine as _, engine::general_purpose};

/// PHC prefix identifying an Argon2id hash. A configured `password` value
/// starting with this prefix is treated as a pre-hashed credential instead
/// of plaintext.
pub const ARGON2ID_PREFIX: &str = "$argon2id$";

/// Returns true if the string is a well-formed Argon2id PHC hash.
pub fn is_valid_argon2_hash(s: &str) -> bool {
    s.starts_with(ARGON2ID_PREFIX) && parse_argon2id_hash(s).is_ok()
}

/// Parse and structurally validate an Argon2id PHC string.
///
/// `PasswordHash::new` alone is lenient (it accepts partial strings such as
/// `$argon2id$malformed`), so the salt and digest fields and the algorithm
/// identifier are checked explicitly.
fn parse_argon2id_hash(s: &str) -> Result<PasswordHash, argon2::password_hash::Error> {
    let parsed = PasswordHash::new(s)?;
    if parsed.algorithm != argon2::Algorithm::Argon2id.ident() {
        return Err(argon2::password_hash::Error::Algorithm);
    }
    if parsed.salt.is_none() {
        return Err(argon2::password_hash::Error::SaltInvalid);
    }
    if parsed.hash.as_ref().is_none_or(|h| h.as_ref().is_empty()) {
        return Err(argon2::password_hash::Error::OutputSize);
    }
    Ok(parsed)
}

#[derive(Clone)]
pub struct BasicAuth {
    username: String,
    /// Argon2id hash of the configured password (PHC string, random salt)
    password_hash: String,
}

impl BasicAuth {
    /// Build an authenticator from a username and a password value that is
    /// either plaintext or a pre-hashed Argon2id PHC string.
    ///
    /// When the value starts with `$argon2id$` it is stored as-is (after
    /// format validation), so the plaintext password never needs to appear
    /// in the configuration file or on the command line. Otherwise it is
    /// hashed with Argon2id and a random salt: Argon2id is memory-hard, so a
    /// leaked in-memory hash cannot be cracked with rainbow tables or cheap
    /// GPU brute force the way an unsalted SHA-256 digest can.
    pub fn new(username: String, password: String) -> Result<Self, argon2::password_hash::Error> {
        if password.starts_with(ARGON2ID_PREFIX) {
            return Self::from_hash(username, password);
        }
        let password_hash = Self::hash_password(&password)?;
        Ok(Self {
            username,
            password_hash,
        })
    }

    /// Build an authenticator from an existing Argon2id PHC hash string.
    ///
    /// The hash is validated up front so a malformed configured hash fails
    /// at construction instead of silently failing every verification later.
    pub fn from_hash(
        username: String,
        password_hash: String,
    ) -> Result<Self, argon2::password_hash::Error> {
        // Validate the PHC structure (algorithm, salt, digest) eagerly.
        parse_argon2id_hash(&password_hash)?;
        Ok(Self {
            username,
            password_hash,
        })
    }

    /// Hash a plaintext password with Argon2id and a random salt, returning
    /// the PHC string suitable for the configuration file.
    ///
    /// As of argon2 0.6, `PasswordHasher::hash_password` no longer takes an
    /// explicit salt argument: it generates one internally using the OS RNG
    /// (requires the `getrandom` feature, which is on by default). To supply
    /// your own salt bytes instead, use `hash_password_with_salt` from the
    /// `PasswordHasher` trait.
    pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
        Ok(Argon2::default()
            .hash_password(password.as_bytes())?
            .to_string())
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

    #[test]
    fn test_new_accepts_pre_hashed_password() {
        let hash = BasicAuth::hash_password("secret").unwrap();
        // A value starting with the Argon2id prefix is stored as-is
        let auth = BasicAuth::new("admin".to_string(), hash.clone()).unwrap();
        assert_eq!(auth.password_hash, hash);
        // The original plaintext still verifies
        let credentials = general_purpose::STANDARD.encode("admin:secret");
        assert!(auth.validate(&credentials));
    }

    #[test]
    fn test_from_hash_rejects_malformed_hash() {
        let result = BasicAuth::from_hash(
            "admin".to_string(),
            "$argon2id$not-a-valid-hash".to_string(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_hash_password_produces_verifiable_phc_string() {
        let hash = BasicAuth::hash_password("pass").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(is_valid_argon2_hash(&hash));
        let auth = BasicAuth::from_hash("user".to_string(), hash).unwrap();
        let credentials = general_purpose::STANDARD.encode("user:pass");
        assert!(auth.validate(&credentials));
    }

    #[test]
    fn test_is_valid_argon2_hash() {
        assert!(!is_valid_argon2_hash("secret"));
        assert!(!is_valid_argon2_hash("$argon2id$malformed"));
        let hash = BasicAuth::hash_password("pass").unwrap();
        assert!(is_valid_argon2_hash(&hash));
    }
}
