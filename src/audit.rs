/// Audit logging module
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tracing::{error, info};

#[allow(dead_code)]
#[derive(Clone)]
pub struct AuditLogger {
    log_file: Option<PathBuf>,
    enabled: bool,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: AuditEventType,
    pub remote_addr: String,
    pub username: Option<String>,
    pub session_id: Option<String>,
    pub details: String,
}

#[allow(dead_code)]
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    ConnectionOpened,
    ConnectionClosed,
    AuthSuccess,
    AuthFailure,
    CommandExecuted,
    SessionStarted,
    SessionEnded,
    ErrorOccurred,
}

#[allow(dead_code)]
impl AuditLogger {
    pub fn new(log_file: Option<PathBuf>, enabled: bool) -> Self {
        Self { log_file, enabled }
    }

    /// Log a connection event
    pub async fn log_connection(&self, remote_addr: &str, session_id: &str) {
        self.log_event(AuditEvent {
            timestamp: Utc::now(),
            event_type: AuditEventType::ConnectionOpened,
            remote_addr: remote_addr.to_string(),
            username: None,
            session_id: Some(session_id.to_string()),
            details: "WebSocket connection established".to_string(),
        })
        .await;
    }

    /// Log an authentication attempt
    pub async fn log_auth_attempt(
        &self,
        remote_addr: &str,
        username: &str,
        success: bool,
        session_id: &str,
    ) {
        let event_type = if success {
            AuditEventType::AuthSuccess
        } else {
            AuditEventType::AuthFailure
        };

        self.log_event(AuditEvent {
            timestamp: Utc::now(),
            event_type,
            remote_addr: remote_addr.to_string(),
            username: Some(username.to_string()),
            session_id: Some(session_id.to_string()),
            details: format!(
                "Authentication attempt: {}",
                if success { "success" } else { "failed" }
            ),
        })
        .await;
    }

    /// Log a disconnection event
    pub async fn log_disconnect(&self, remote_addr: &str, session_id: &str, reason: &str) {
        self.log_event(AuditEvent {
            timestamp: Utc::now(),
            event_type: AuditEventType::ConnectionClosed,
            remote_addr: remote_addr.to_string(),
            username: None,
            session_id: Some(session_id.to_string()),
            details: format!("Connection closed: {}", reason),
        })
        .await;
    }

    /// Log a session started event
    pub async fn log_session_started(
        &self,
        remote_addr: &str,
        username: Option<&str>,
        session_id: &str,
    ) {
        self.log_event(AuditEvent {
            timestamp: Utc::now(),
            event_type: AuditEventType::SessionStarted,
            remote_addr: remote_addr.to_string(),
            username: username.map(|s| s.to_string()),
            session_id: Some(session_id.to_string()),
            details: "Terminal session started".to_string(),
        })
        .await;
    }

    /// Log an error event
    pub async fn log_error(&self, remote_addr: &str, session_id: &str, error: &str) {
        self.log_event(AuditEvent {
            timestamp: Utc::now(),
            event_type: AuditEventType::ErrorOccurred,
            remote_addr: remote_addr.to_string(),
            username: None,
            session_id: Some(session_id.to_string()),
            details: error.to_string(),
        })
        .await;
    }

    /// Internal method to log an event
    async fn log_event(&self, event: AuditEvent) {
        if !self.enabled {
            return;
        }

        // Log to tracing
        info!(
            event_type = ?event.event_type,
            remote_addr = %event.remote_addr,
            session_id = ?event.session_id,
            username = ?event.username,
            "Audit event: {}",
            event.details
        );

        // Write to file if configured
        if let Some(log_file) = &self.log_file
            && let Err(e) = self.write_to_file(log_file, &event).await
        {
            error!("Failed to write audit log to file: {}", e);
        }
    }

    /// Write event to log file
    async fn write_to_file(&self, log_file: &PathBuf, event: &AuditEvent) -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .await?;

        let json = serde_json::to_string(event)
            .map_err(|e| std::io::Error::other(format!("JSON error: {}", e)))?;

        file.write_all(json.as_bytes()).await?;
        file.write_all(b"\n").await?;
        file.flush().await?;

        Ok(())
    }
}

impl Default for AuditLogger {
    fn default() -> Self {
        Self::new(None, false)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_audit_logger_creation() {
        let logger = AuditLogger::new(None, true);
        assert!(logger.enabled);
        assert!(logger.log_file.is_none());
    }

    #[tokio::test]
    async fn test_audit_event_serialization() {
        let event = AuditEvent {
            timestamp: Utc::now(),
            event_type: AuditEventType::AuthSuccess,
            remote_addr: "127.0.0.1".to_string(),
            username: Some("test".to_string()),
            session_id: Some("session123".to_string()),
            details: "Test event".to_string(),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("auth_success"));
        assert!(json.contains("127.0.0.1"));
        assert!(json.contains("test"));
    }

    #[tokio::test]
    async fn test_log_methods_when_disabled() {
        let logger = AuditLogger::new(None, false);

        // All logging methods should be no-ops when disabled
        logger.log_connection("127.0.0.1", "s1").await;
        logger
            .log_auth_attempt("127.0.0.1", "user", true, "s1")
            .await;
        logger
            .log_auth_attempt("127.0.0.1", "user", false, "s1")
            .await;
        logger.log_disconnect("127.0.0.1", "s1", "test").await;
        logger
            .log_session_started("127.0.0.1", Some("user"), "s1")
            .await;
        logger.log_error("127.0.0.1", "s1", "oops").await;
        // No panic or error — just no-ops
    }

    #[tokio::test]
    async fn test_log_methods_when_enabled_no_file() {
        let logger = AuditLogger::new(None, true);

        // Should log to tracing but not fail (no file configured)
        logger.log_connection("10.0.0.1", "s1").await;
        logger
            .log_auth_attempt("10.0.0.1", "admin", true, "s1")
            .await;
        logger.log_disconnect("10.0.0.1", "s1", "done").await;
        logger.log_session_started("10.0.0.1", None, "s1").await;
        logger.log_error("10.0.0.1", "s1", "test error").await;
    }

    #[tokio::test]
    async fn test_log_writes_to_file() {
        let dir = std::env::temp_dir().join("ttyd-rs-audit-test");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("audit.log");

        let logger = AuditLogger::new(Some(log_path.clone()), true);

        logger.log_connection("192.168.1.1", "session-abc").await;
        logger
            .log_auth_attempt("192.168.1.1", "admin", true, "session-abc")
            .await;

        // Give async writes a moment to flush
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("192.168.1.1"));
        assert!(content.contains("session-abc"));
        assert!(content.contains("connection_opened"));
        assert!(content.contains("auth_success"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_default_audit_logger() {
        let logger = AuditLogger::default();
        assert!(!logger.enabled);
        assert!(logger.log_file.is_none());
    }

    #[test]
    fn test_audit_event_type_serialization() {
        let types = vec![
            (AuditEventType::ConnectionOpened, "connection_opened"),
            (AuditEventType::ConnectionClosed, "connection_closed"),
            (AuditEventType::AuthSuccess, "auth_success"),
            (AuditEventType::AuthFailure, "auth_failure"),
            (AuditEventType::CommandExecuted, "command_executed"),
            (AuditEventType::SessionStarted, "session_started"),
            (AuditEventType::SessionEnded, "session_ended"),
            (AuditEventType::ErrorOccurred, "error_occurred"),
        ];

        for (event_type, expected) in types {
            let event = AuditEvent {
                timestamp: Utc::now(),
                event_type,
                remote_addr: "127.0.0.1".to_string(),
                username: None,
                session_id: None,
                details: "test".to_string(),
            };
            let json = serde_json::to_string(&event).unwrap();
            assert!(
                json.contains(expected),
                "Expected '{}' in {}",
                expected,
                json
            );
        }
    }
}
