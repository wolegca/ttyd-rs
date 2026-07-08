/// Library interface for ttyd-rs
/// Exposes modules for integration testing
pub mod assets;
pub mod audit;
pub mod auth;
pub mod config;
pub mod protocol;
pub mod pty;
pub mod rate_limit;
pub mod server;
pub mod session;
pub mod validation;

// Re-export commonly used types for convenience
pub use config::Config;
pub use session::{Client, Session, SessionManager, SessionMode};
