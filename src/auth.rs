/// Authentication module
mod basic;
mod token;

pub use basic::{ARGON2ID_PREFIX, BasicAuth, is_valid_argon2_hash};
pub use token::TokenAuth;

/// Shared authenticator interface.
///
/// Both `BasicAuth` and `TokenAuth` implement this so callers can hold
/// `Arc<dyn Authenticator>` without depending on the concrete enum in
/// `server::websocket::auth`. This decouples the HTTP API auth middleware
/// from WebSocket-internal types.
pub trait Authenticator: Send + Sync {
    /// Return true when the raw `Authorization` header value is valid.
    ///
    /// Implementations strip their own scheme prefix ("Basic " / "Bearer ")
    /// before verifying the credential, so callers pass the raw header.
    fn validate_header(&self, header: &str) -> bool;
}

impl Authenticator for BasicAuth {
    fn validate_header(&self, header: &str) -> bool {
        header
            .strip_prefix("Basic ")
            .is_some_and(|credentials| self.validate(credentials))
    }
}

impl Authenticator for TokenAuth {
    fn validate_header(&self, header: &str) -> bool {
        header
            .strip_prefix("Bearer ")
            .is_some_and(|token| self.validate(token))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose;

    #[test]
    fn test_basic_validate_header_accepts_only_basic_prefix() {
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string()).unwrap();
        let creds = general_purpose::STANDARD.encode("admin:secret");

        assert!(auth.validate_header(&format!("Basic {creds}")));
        // Wrong scheme, missing scheme, and empty header must all fail.
        assert!(!auth.validate_header(&format!("Bearer {creds}")));
        assert!(!auth.validate_header(&creds));
        assert!(!auth.validate_header(""));
        assert!(!auth.validate_header("Basic "));
    }

    #[test]
    fn test_basic_validate_header_rejects_wrong_password() {
        let auth = BasicAuth::new("admin".to_string(), "secret".to_string()).unwrap();
        let wrong = general_purpose::STANDARD.encode("admin:wrong");
        assert!(!auth.validate_header(&format!("Basic {wrong}")));
    }

    #[test]
    fn test_token_validate_header_accepts_only_bearer_prefix() {
        let auth = TokenAuth::new("tok-123".to_string());

        assert!(auth.validate_header("Bearer tok-123"));
        assert!(!auth.validate_header("Basic tok-123"));
        assert!(!auth.validate_header("tok-123"));
        assert!(!auth.validate_header("Bearer wrong"));
        assert!(!auth.validate_header(""));
        assert!(!auth.validate_header("Bearer "));
    }

    #[test]
    fn test_validate_header_scheme_is_case_sensitive() {
        // The scheme prefix must match exactly ("Bearer ", not "bearer ").
        let auth = TokenAuth::new("tok".to_string());
        assert!(!auth.validate_header("bearer tok"));
    }
}
