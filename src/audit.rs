/// Audit logging module
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info};

#[derive(Clone)]
pub struct AuditLogger {
    log_file: Option<PathBuf>,
    enabled: bool,
    /// Open audit log file handle, shared across all clones of the logger.
    /// Opened once during [`Self::prepare`] (or lazily on first write);
    /// `None` when file logging is disabled or no path is configured.
    file: Arc<tokio::sync::Mutex<Option<tokio::fs::File>>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: AuditEventType,
    pub remote_addr: String,
    pub username: Option<String>,
    pub session_id: Option<String>,
    pub details: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    ConnectionOpened,
    ConnectionClosed,
    AuthSuccess,
    AuthFailure,
    #[allow(dead_code)]
    CommandExecuted,
    SessionStarted,
    #[allow(dead_code)]
    SessionEnded,
    ErrorOccurred,
}

impl AuditLogger {
    pub fn new(log_file: Option<PathBuf>, enabled: bool) -> Self {
        Self {
            log_file,
            enabled,
            file: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    /// Prepare the audit log for use: create the parent directory (if any)
    /// and verify the log file can be opened for appending.
    ///
    /// Call once at startup. Failing fast here is intentional: an operator
    /// who enabled file-based audit logging should not have events silently
    /// dropped (or every event spam an error) because of a bad path.
    pub async fn prepare(&self) -> std::io::Result<()> {
        let Some(log_file) = &self.log_file else {
            return Ok(());
        };

        if let Some(parent) = log_file.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Open (creating if needed) so permission/path problems surface at
        // startup instead of on every audit event. Hold the file open for the
        // lifetime of the logger so each event is a single write() instead of
        // a fresh open() on every event.
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .await?;

        *self.file.lock().await = Some(file);

        Ok(())
    }

    /// Log a connection event
    pub async fn log_connection(&self, remote_addr: &str, client_id: &str) {
        self.log_event(AuditEvent {
            timestamp: Utc::now(),
            event_type: AuditEventType::ConnectionOpened,
            remote_addr: remote_addr.to_string(),
            username: None,
            session_id: Some(client_id.to_string()),
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

        // Log to tracing. When a log file is configured the event is also
        // written there, so the tracing mirror is demoted to debug to avoid
        // duplicating every event at info level.
        if self.log_file.is_some() {
            debug!(
                event_type = ?event.event_type,
                remote_addr = %event.remote_addr,
                session_id = %event.session_id.as_deref().unwrap_or("-"),
                username = %event.username.as_deref().unwrap_or("-"),
                "Audit event: {}",
                event.details
            );
        } else {
            info!(
                event_type = ?event.event_type,
                remote_addr = %event.remote_addr,
                session_id = %event.session_id.as_deref().unwrap_or("-"),
                username = %event.username.as_deref().unwrap_or("-"),
                "Audit event: {}",
                event.details
            );
        }

        // Write to file if configured
        if self.log_file.is_some()
            && let Err(e) = self.write_to_file(&event).await
        {
            error!("Failed to write audit log to file: {}", e);
        }
    }

    /// Write event to the audit log file.
    ///
    /// Reuses the file handle opened by [`Self::prepare`] so each event is a
    /// single `write()` instead of `open()` + `write()` + `flush()`. If
    /// `prepare` was not called (e.g. in unit tests), the file is opened
    /// lazily on first write and cached for subsequent writes.
    ///
    /// The mutex serializes concurrent writes so each JSONL line is emitted
    /// atomically (no interleaving between tasks).
    async fn write_to_file(&self, event: &AuditEvent) -> std::io::Result<()> {
        // Serialize the event and append the newline into a single buffer so
        // the JSONL line is written with one write() call.
        let mut line = serde_json::to_string(event)
            .map_err(|e| std::io::Error::other(format!("JSON error: {}", e)))?
            .into_bytes();
        line.push(b'\n');

        let mut guard = self.file.lock().await;

        // Open lazily if prepare() hasn't already opened the file.
        if guard.is_none()
            && let Some(log_file) = &self.log_file
        {
            *guard = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_file)
                    .await?,
            );
        }

        let Some(file) = guard.as_mut() else {
            return Err(std::io::Error::other("no audit log file configured"));
        };

        file.write_all(&line).await?;

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

    #[tokio::test]
    async fn test_log_auth_failure_writes_to_file() {
        let dir = std::env::temp_dir().join("ttyd-rs-audit-auth-fail");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("audit.log");

        let logger = AuditLogger::new(Some(log_path.clone()), true);
        logger
            .log_auth_attempt("10.0.0.1", "baduser", false, "s1")
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("auth_failure"));
        assert!(content.contains("baduser"));
        assert!(content.contains("failed"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_log_disconnect_writes_to_file() {
        let dir = std::env::temp_dir().join("ttyd-rs-audit-disconnect");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("audit.log");

        let logger = AuditLogger::new(Some(log_path.clone()), true);
        logger
            .log_disconnect("10.0.0.2", "s2", "client closed")
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("connection_closed"));
        assert!(content.contains("10.0.0.2"));
        assert!(content.contains("client closed"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_log_session_started_writes_to_file() {
        let dir = std::env::temp_dir().join("ttyd-rs-audit-session");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("audit.log");

        let logger = AuditLogger::new(Some(log_path.clone()), true);
        logger
            .log_session_started("10.0.0.3", Some("alice"), "s3")
            .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("session_started"));
        assert!(content.contains("alice"));
        assert!(content.contains("s3"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_log_error_writes_to_file() {
        let dir = std::env::temp_dir().join("ttyd-rs-audit-error");
        let _ = std::fs::create_dir_all(&dir);
        let log_path = dir.join("audit.log");

        let logger = AuditLogger::new(Some(log_path.clone()), true);
        logger.log_error("10.0.0.4", "s4", "PTY spawn failed").await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.contains("error_occurred"));
        assert!(content.contains("PTY spawn failed"));

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

    #[tokio::test]
    async fn test_prepare_creates_parent_directory() {
        let dir = std::env::temp_dir()
            .join("ttyd-rs-audit-prepare")
            .join("nested")
            .join("deep");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());

        let log_path = dir.join("audit.log");
        let logger = AuditLogger::new(Some(log_path.clone()), true);

        // Directory does not exist yet
        assert!(!dir.exists());

        // Should create the full directory chain and the log file
        logger.prepare().await.unwrap();

        assert!(dir.exists());
        assert!(log_path.exists());

        // Clean up
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[tokio::test]
    async fn test_prepare_noop_when_no_file() {
        let logger = AuditLogger::new(None, true);
        // Should not panic or error when no log_file is set
        logger.prepare().await.unwrap();
    }

    #[tokio::test]
    async fn test_prepare_fails_when_parent_path_is_a_file() {
        let base = std::env::temp_dir().join("ttyd-rs-audit-prepare-fail");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        // Block the parent path with a regular file
        let parent_file = base.join("blocker");
        std::fs::write(&parent_file, "blocker").unwrap();

        let logger = AuditLogger::new(Some(parent_file.join("audit.log")), true);
        assert!(logger.prepare().await.is_err());

        // Clean up
        let _ = std::fs::remove_dir_all(&base);
    }
}
