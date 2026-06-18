/// Session management module for multi-client support
use crate::pty::PtySession;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{info, warn};

/// Session mode determines how clients interact with the session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// Each client gets its own isolated PTY
    Isolated,
    /// Multiple clients share one PTY, all read-only
    SharedReadOnly,
    /// Multiple clients share one PTY, all can write
    SharedReadWrite,
}

impl std::str::FromStr for SessionMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "isolated" => Ok(SessionMode::Isolated),
            "shared-ro" | "shared_readonly" => Ok(SessionMode::SharedReadOnly),
            "shared-rw" | "shared_readwrite" => Ok(SessionMode::SharedReadWrite),
            _ => Err(format!("Invalid session mode: {}", s)),
        }
    }
}

impl std::fmt::Display for SessionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionMode::Isolated => write!(f, "isolated"),
            SessionMode::SharedReadOnly => write!(f, "shared_readonly"),
            SessionMode::SharedReadWrite => write!(f, "shared_readwrite"),
        }
    }
}

/// Client information within a session
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Client {
    pub client_id: String,
    pub remote_addr: String,
    pub username: Option<String>,
    pub connected_at: Instant,
    pub readonly: bool,
}

/// Session metadata
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SessionMetadata {
    pub session_id: String,
    pub mode: SessionMode,
    pub created_at: Instant,
    pub command: Vec<String>,
    pub working_dir: Option<String>,
}

/// A terminal session that can be shared among multiple clients
pub struct Session {
    metadata: SessionMetadata,
    pty_session: Arc<Mutex<PtySession>>,
    clients: Arc<RwLock<HashMap<String, Client>>>,
    last_activity: Arc<Mutex<Instant>>,
    /// Broadcast channel for terminal output
    #[allow(dead_code)]
    output_tx: broadcast::Sender<Vec<u8>>,
}

impl Session {
    /// Create a new session
    #[allow(dead_code)]
    pub fn new(
        session_id: String,
        mode: SessionMode,
        command: &[String],
        working_dir: Option<String>,
        cols: u16,
        rows: u16,
    ) -> Result<Self, String> {
        let pty_session = PtySession::new(command, cols, rows)
            .map_err(|e| format!("Failed to create PTY: {}", e))?;

        let (output_tx, _) = broadcast::channel(100);

        Ok(Self {
            metadata: SessionMetadata {
                session_id,
                mode,
                created_at: Instant::now(),
                command: command.to_vec(),
                working_dir,
            },
            pty_session: Arc::new(Mutex::new(pty_session)),
            clients: Arc::new(RwLock::new(HashMap::new())),
            last_activity: Arc::new(Mutex::new(Instant::now())),
            output_tx,
        })
    }

    /// Add a client to this session
    #[allow(dead_code)]
    pub async fn add_client(&self, client: Client) -> Result<(), String> {
        let mut clients = self.clients.write().await;

        // Check if this is a shared session and enforce read-only for SharedReadOnly mode
        if self.metadata.mode == SessionMode::Isolated && !clients.is_empty() {
            return Err("Cannot add multiple clients to an isolated session".to_string());
        }

        clients.insert(client.client_id.clone(), client);
        *self.last_activity.lock().await = Instant::now();

        Ok(())
    }

    /// Remove a client from this session
    #[allow(dead_code)]
    pub async fn remove_client(&self, client_id: &str) -> bool {
        let mut clients = self.clients.write().await;
        let removed = clients.remove(client_id).is_some();

        if removed {
            *self.last_activity.lock().await = Instant::now();
        }

        removed
    }

    /// Get the number of connected clients
    pub async fn client_count(&self) -> usize {
        self.clients.read().await.len()
    }

    /// Check if the session has no clients
    pub async fn is_empty(&self) -> bool {
        self.clients.read().await.is_empty()
    }

    /// Get session metadata
    pub fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    /// Get PTY session for direct access
    pub fn pty_session(&self) -> Arc<Mutex<PtySession>> {
        self.pty_session.clone()
    }

    /// Subscribe to terminal output
    #[allow(dead_code)]
    pub fn subscribe_output(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    /// Broadcast output to all clients
    #[allow(dead_code)]
    pub fn broadcast_output(&self, data: Vec<u8>) {
        // Ignore send errors (no receivers)
        let _ = self.output_tx.send(data);
    }

    /// Get last activity time
    pub async fn last_activity(&self) -> Instant {
        *self.last_activity.lock().await
    }

    /// Check if the client can write to this session
    #[allow(dead_code)]
    pub async fn can_write(&self, client_id: &str) -> bool {
        match self.metadata.mode {
            SessionMode::Isolated => true,
            SessionMode::SharedReadOnly => false,
            SessionMode::SharedReadWrite => {
                // Check if client exists and is not marked readonly
                if let Some(client) = self.clients.read().await.get(client_id) {
                    !client.readonly
                } else {
                    false
                }
            }
        }
    }

    /// Get list of connected clients
    #[allow(dead_code)]
    pub async fn list_clients(&self) -> Vec<Client> {
        self.clients.read().await.values().cloned().collect()
    }
}

/// Session manager for managing all active sessions
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, Arc<Session>>>>,
    session_timeout: Duration,
    #[allow(dead_code)]
    default_mode: SessionMode,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(session_timeout: Duration, default_mode: SessionMode) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_timeout,
            default_mode,
        }
    }

    /// Create a new session
    #[allow(dead_code)]
    pub async fn create_session(
        &self,
        session_id: String,
        command: &[String],
        working_dir: Option<String>,
        cols: u16,
        rows: u16,
        mode: Option<SessionMode>,
    ) -> Result<Arc<Session>, String> {
        let mode = mode.unwrap_or(self.default_mode);

        let session = Session::new(session_id.clone(), mode, command, working_dir, cols, rows)?;

        let session = Arc::new(session);
        self.sessions
            .write()
            .await
            .insert(session_id.clone(), session.clone());

        info!("Created session {} with mode {}", session_id, mode);
        Ok(session)
    }

    /// Get an existing session
    pub async fn get_session(&self, session_id: &str) -> Option<Arc<Session>> {
        self.sessions.read().await.get(session_id).cloned()
    }

    /// List all active sessions
    pub async fn list_sessions(&self) -> Vec<Arc<Session>> {
        self.sessions.read().await.values().cloned().collect()
    }

    /// Remove a session
    pub async fn remove_session(&self, session_id: &str) -> bool {
        let removed = self.sessions.write().await.remove(session_id).is_some();
        if removed {
            info!("Removed session {}", session_id);
        }
        removed
    }

    /// Get the total number of sessions
    #[allow(dead_code)]
    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Clean up inactive sessions
    pub async fn cleanup_inactive(&self) -> usize {
        let now = Instant::now();
        let mut to_remove = Vec::new();

        {
            let sessions = self.sessions.read().await;
            for (session_id, session) in sessions.iter() {
                let last_activity = session.last_activity().await;
                let idle_time = now.duration_since(last_activity);

                // Remove if session has no clients and has been idle too long
                if session.is_empty().await && idle_time > self.session_timeout {
                    to_remove.push(session_id.clone());
                }
            }
        }

        let count = to_remove.len();
        if count > 0 {
            let mut sessions = self.sessions.write().await;
            for session_id in to_remove {
                sessions.remove(&session_id);
                warn!("Cleaned up inactive session: {}", session_id);
            }
        }

        count
    }

    /// Get statistics
    pub async fn stats(&self) -> SessionStats {
        let sessions = self.sessions.read().await;
        let mut total_clients = 0;
        let mut isolated_count = 0;
        let mut shared_count = 0;

        for session in sessions.values() {
            total_clients += session.client_count().await;
            match session.metadata().mode {
                SessionMode::Isolated => isolated_count += 1,
                SessionMode::SharedReadOnly | SessionMode::SharedReadWrite => shared_count += 1,
            }
        }

        SessionStats {
            total_sessions: sessions.len(),
            isolated_sessions: isolated_count,
            shared_sessions: shared_count,
            total_clients,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionStats {
    pub total_sessions: usize,
    pub isolated_sessions: usize,
    pub shared_sessions: usize,
    pub total_clients: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_creation() {
        let session = Session::new(
            "test-session".to_string(),
            SessionMode::Isolated,
            &["bash".to_string()],
            None,
            80,
            24,
        );
        assert!(session.is_ok());
    }

    #[tokio::test]
    async fn test_session_add_client() {
        let session = Session::new(
            "test-session".to_string(),
            SessionMode::SharedReadWrite,
            &["bash".to_string()],
            None,
            80,
            24,
        )
        .unwrap();

        let client = Client {
            client_id: "client1".to_string(),
            remote_addr: "127.0.0.1".to_string(),
            username: Some("test".to_string()),
            connected_at: Instant::now(),
            readonly: false,
        };

        assert!(session.add_client(client).await.is_ok());
        assert_eq!(session.client_count().await, 1);
    }

    #[tokio::test]
    async fn test_isolated_session_rejects_multiple_clients() {
        let session = Session::new(
            "test-session".to_string(),
            SessionMode::Isolated,
            &["bash".to_string()],
            None,
            80,
            24,
        )
        .unwrap();

        let client1 = Client {
            client_id: "client1".to_string(),
            remote_addr: "127.0.0.1".to_string(),
            username: None,
            connected_at: Instant::now(),
            readonly: false,
        };

        let client2 = Client {
            client_id: "client2".to_string(),
            remote_addr: "127.0.0.1".to_string(),
            username: None,
            connected_at: Instant::now(),
            readonly: false,
        };

        assert!(session.add_client(client1).await.is_ok());
        assert!(session.add_client(client2).await.is_err());
    }

    #[tokio::test]
    async fn test_session_manager_create() {
        let manager = SessionManager::new(Duration::from_secs(3600), SessionMode::Isolated);

        let result = manager
            .create_session(
                "test-session".to_string(),
                &["bash".to_string()],
                None,
                80,
                24,
                None,
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(manager.session_count().await, 1);
    }

    #[tokio::test]
    async fn test_session_manager_get() {
        let manager = SessionManager::new(Duration::from_secs(3600), SessionMode::Isolated);

        manager
            .create_session(
                "test-session".to_string(),
                &["bash".to_string()],
                None,
                80,
                24,
                None,
            )
            .await
            .unwrap();

        let session = manager.get_session("test-session").await;
        assert!(session.is_some());
    }

    #[tokio::test]
    async fn test_session_mode_from_str() {
        assert_eq!(
            "isolated".parse::<SessionMode>().unwrap(),
            SessionMode::Isolated
        );
        assert_eq!(
            "shared-ro".parse::<SessionMode>().unwrap(),
            SessionMode::SharedReadOnly
        );
        assert_eq!(
            "shared-rw".parse::<SessionMode>().unwrap(),
            SessionMode::SharedReadWrite
        );
        assert!("invalid".parse::<SessionMode>().is_err());
    }
}
