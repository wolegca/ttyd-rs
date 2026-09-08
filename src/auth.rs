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
