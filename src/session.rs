/// Session management module for multi-client support
use crate::pty::PtySession;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{info, warn};

/// Errors that can occur during session operations
#[derive(Debug, Error)]
pub enum SessionError {
    #[error("Failed to create PTY: {0}")]
    PtyCreation(#[from] crate::pty::PtyError),

    #[error("Invalid session mode: {0}")]
    InvalidMode(String),

    #[error("Cannot add multiple clients to an isolated session")]
    IsolatedSessionFull,
}

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
    type Err = SessionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "isolated" => Ok(SessionMode::Isolated),
            "shared-ro" | "shared_readonly" => Ok(SessionMode::SharedReadOnly),
            "shared-rw" | "shared_readwrite" => Ok(SessionMode::SharedReadWrite),
            _ => Err(SessionError::InvalidMode(s.to_string())),
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
pub struct Client {
    pub client_id: String,
    #[allow(dead_code)]
    pub remote_addr: String,
    #[allow(dead_code)]
    pub username: Option<String>,
    #[allow(dead_code)]
    pub connected_at: Instant,
    pub readonly: bool,
}

/// Session metadata
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub session_id: String,
    pub mode: SessionMode,
    pub created_at: Instant,
    #[allow(dead_code)]
    pub command: Vec<String>,
    #[allow(dead_code)]
    pub working_dir: Option<String>,
}

/// A terminal session that can be shared among multiple clients
pub struct Session {
    metadata: SessionMetadata,
    pty_session: Arc<Mutex<PtySession>>,
    clients: Arc<RwLock<HashMap<String, Client>>>,
    last_activity: Arc<Mutex<Instant>>,
    /// Broadcast channel for terminal output
    output_tx: broadcast::Sender<Vec<u8>>,
}

impl Session {
    /// Create a new session
    pub fn new(
        session_id: String,
        mode: SessionMode,
        command: &[String],
        working_dir: Option<String>,
        cols: u16,
        rows: u16,
    ) -> Result<Self, SessionError> {
        let pty_session = PtySession::new(command, cols, rows)?;

        let (output_tx, _) = broadcast::channel(512);

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
    pub async fn add_client(&self, client: Client) -> Result<(), SessionError> {
        let mut clients = self.clients.write().await;

        // Check if this is a shared session and enforce read-only for SharedReadOnly mode
        if self.metadata.mode == SessionMode::Isolated && !clients.is_empty() {
            return Err(SessionError::IsolatedSessionFull);
        }

        clients.insert(client.client_id.clone(), client);
        *self.last_activity.lock().await = Instant::now();

        Ok(())
    }

    /// Remove a client from this session
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
    pub fn subscribe_output(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    /// Broadcast output to all clients
    pub fn broadcast_output(&self, data: Vec<u8>) {
        // Ignore send errors (no receivers)
        let _ = self.output_tx.send(data);
    }

    /// Get last activity time
    pub async fn last_activity(&self) -> Instant {
        *self.last_activity.lock().await
    }

    /// Check if the client can write to this session
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

/// Time to keep an empty session alive for client reconnection
pub const RECONNECT_WINDOW: Duration = Duration::from_secs(60);

/// Session manager for managing all active sessions
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<String, Arc<Session>>>>,
    session_timeout: Duration,
    reconnect_window: Duration,
    default_mode: SessionMode,
}

impl SessionManager {
    /// Create a new session manager
    pub fn new(session_timeout: Duration, default_mode: SessionMode) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_timeout,
            reconnect_window: RECONNECT_WINDOW,
            default_mode,
        }
    }

    /// Set the reconnect window duration
    pub fn with_reconnect_window(mut self, window: Duration) -> Self {
        self.reconnect_window = window;
        self
    }

    /// Create a new session
    pub async fn create_session(
        &self,
        session_id: String,
        command: &[String],
        working_dir: Option<String>,
        cols: u16,
        rows: u16,
        mode: Option<SessionMode>,
    ) -> Result<Arc<Session>, SessionError> {
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

    /// Atomically check if a session has no clients and remove it if so.
    /// Returns true if the session was removed.  This eliminates the TOCTOU
    /// race between `session.is_empty()` and `remove_session()` by holding
    /// the sessions write lock and the clients write lock simultaneously.
    pub async fn remove_if_empty(&self, session_id: &str) -> bool {
        let mut sessions = self.sessions.write().await;
        // Clone the Arc so we can release the borrow on `sessions` before
        // acquiring the clients lock — otherwise the borrow checker prevents
        // calling sessions.remove() while the session is still borrowed.
        let session = match sessions.get(session_id) {
            Some(s) => Arc::clone(s),
            None => return false,
        };
        // Hold the clients *write* lock through the removal so that no
        // concurrent add_client can insert a client between the emptiness
        // check and the removal from the sessions map.
        let clients = session.clients.write().await;
        if clients.is_empty() {
            sessions.remove(session_id);
            // clients guard is dropped here, after removal is complete
            info!("Removed empty session {}", session_id);
            return true;
        }
        false
    }

    /// Get the total number of sessions
    #[allow(dead_code)]
    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Clean up inactive sessions.  Candidates are identified under a read
    /// lock, then each removal goes through `remove_if_empty` which atomically
    /// re-verifies emptiness under the sessions write lock — closing the
    /// TOCTOU window that a two-phase read-then-write approach would leave.
    ///
    /// Empty sessions are kept alive for `reconnect_window` to allow clients
    /// to reconnect without losing session state.  Non-empty sessions whose
    /// idle time exceeds `session_timeout` are forcefully removed (clients
    /// are disconnected and the session is dropped).
    pub async fn cleanup_inactive(&self) -> usize {
        let now = Instant::now();
        let mut empty_candidates = Vec::new();
        let mut stale_candidates = Vec::new();

        {
            let sessions = self.sessions.read().await;
            for (session_id, session) in sessions.iter() {
                let last_activity = session.last_activity().await;
                let idle_time = now.duration_since(last_activity);

                if session.is_empty().await {
                    // Empty sessions: remove after reconnect window
                    if idle_time > self.reconnect_window {
                        empty_candidates.push(session_id.clone());
                    }
                } else {
                    // Non-empty sessions: remove after session timeout
                    if idle_time > self.session_timeout {
                        stale_candidates.push(session_id.clone());
                    }
                }
            }
        }

        let mut count = 0;

        // Remove empty sessions via the atomic check
        for session_id in empty_candidates {
            if self.remove_if_empty(&session_id).await {
                warn!("Cleaned up inactive session: {}", session_id);
                count += 1;
            }
        }

        // Force-remove stale non-empty sessions
        for session_id in stale_candidates {
            let mut sessions = self.sessions.write().await;
            if let Some(session) = sessions.remove(&session_id) {
                // Clear clients to disconnect them; the session's PtyProcess
                // will be dropped when the last Arc reference is released.
                let mut clients = session.clients.write().await;
                let n = clients.len();
                clients.clear();
                warn!(
                    "Force-removed stale session {} (had {} clients)",
                    session_id, n
                );
                count += 1;
            }
        }

        count
    }

    /// Shut down all sessions. Called during graceful server shutdown.
    /// Drops all sessions, which triggers PtyProcess::drop() to kill child processes.
    pub async fn shutdown(&self) {
        let mut sessions = self.sessions.write().await;
        let count = sessions.len();
        sessions.clear();
        if count > 0 {
            info!("Shutdown: removed {} active sessions", count);
        }
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tokio::time::{sleep, timeout};

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

    #[tokio::test]
    async fn test_session_remove_client() {
        let session = Session::new(
            "test-rm".to_string(),
            SessionMode::SharedReadWrite,
            &["bash".to_string()],
            None,
            80,
            24,
        )
        .unwrap();

        let client = Client {
            client_id: "c1".to_string(),
            remote_addr: "127.0.0.1".to_string(),
            username: None,
            connected_at: Instant::now(),
            readonly: false,
        };
        session.add_client(client).await.unwrap();
        assert_eq!(session.client_count().await, 1);

        assert!(session.remove_client("c1").await);
        assert_eq!(session.client_count().await, 0);

        // Removing non-existent client returns false
        assert!(!session.remove_client("c1").await);
    }

    #[tokio::test]
    async fn test_session_is_empty() {
        let session = Session::new(
            "test-empty".to_string(),
            SessionMode::SharedReadWrite,
            &["bash".to_string()],
            None,
            80,
            24,
        )
        .unwrap();

        assert!(session.is_empty().await);

        let client = Client {
            client_id: "c1".to_string(),
            remote_addr: "127.0.0.1".to_string(),
            username: None,
            connected_at: Instant::now(),
            readonly: false,
        };
        session.add_client(client).await.unwrap();
        assert!(!session.is_empty().await);

        session.remove_client("c1").await;
        assert!(session.is_empty().await);
    }

    #[tokio::test]
    async fn test_session_can_write_modes() {
        // Isolated: always writable
        let isolated = Session::new(
            "iso".to_string(),
            SessionMode::Isolated,
            &["bash".to_string()],
            None,
            80,
            24,
        )
        .unwrap();
        assert!(isolated.can_write("anyone").await);

        // SharedReadOnly: never writable
        let sro = Session::new(
            "sro".to_string(),
            SessionMode::SharedReadOnly,
            &["bash".to_string()],
            None,
            80,
            24,
        )
        .unwrap();
        assert!(!sro.can_write("anyone").await);

        // SharedReadWrite: writable for non-readonly clients
        let srw = Session::new(
            "srw".to_string(),
            SessionMode::SharedReadWrite,
            &["bash".to_string()],
            None,
            80,
            24,
        )
        .unwrap();
        // Unknown client
        assert!(!srw.can_write("unknown").await);

        let client = Client {
            client_id: "writer".to_string(),
            remote_addr: "127.0.0.1".to_string(),
            username: None,
            connected_at: Instant::now(),
            readonly: false,
        };
        srw.add_client(client).await.unwrap();
        assert!(srw.can_write("writer").await);

        let ro_client = Client {
            client_id: "reader".to_string(),
            remote_addr: "127.0.0.1".to_string(),
            username: None,
            connected_at: Instant::now(),
            readonly: true,
        };
        srw.add_client(ro_client).await.unwrap();
        assert!(!srw.can_write("reader").await);
    }

    #[tokio::test]
    async fn test_session_list_clients() {
        let session = Session::new(
            "test-list".to_string(),
            SessionMode::SharedReadWrite,
            &["bash".to_string()],
            None,
            80,
            24,
        )
        .unwrap();

        assert!(session.list_clients().await.is_empty());

        session
            .add_client(Client {
                client_id: "c1".to_string(),
                remote_addr: "127.0.0.1".to_string(),
                username: Some("alice".to_string()),
                connected_at: Instant::now(),
                readonly: false,
            })
            .await
            .unwrap();

        session
            .add_client(Client {
                client_id: "c2".to_string(),
                remote_addr: "127.0.0.2".to_string(),
                username: None,
                connected_at: Instant::now(),
                readonly: true,
            })
            .await
            .unwrap();

        let clients = session.list_clients().await;
        assert_eq!(clients.len(), 2);
    }

    #[tokio::test]
    async fn test_session_broadcast_output() {
        let session = Session::new(
            "test-bcast".to_string(),
            SessionMode::Isolated,
            &["bash".to_string()],
            None,
            80,
            24,
        )
        .unwrap();

        let mut rx = session.subscribe_output();
        session.broadcast_output(b"hello".to_vec());

        let received = timeout(Duration::from_millis(500), rx.recv()).await;
        assert!(received.is_ok());
        assert_eq!(received.unwrap().unwrap(), b"hello");
    }

    #[tokio::test]
    async fn test_session_dimensions() {
        let session = Session::new(
            "test-dim".to_string(),
            SessionMode::Isolated,
            &["bash".to_string()],
            None,
            120,
            40,
        )
        .unwrap();

        assert_eq!(session.pty_session().lock().await.dimensions(), (120, 40));
    }

    #[tokio::test]
    async fn test_session_manager_list_sessions() {
        let manager = SessionManager::new(Duration::from_secs(3600), SessionMode::Isolated);

        assert!(manager.list_sessions().await.is_empty());

        manager
            .create_session("s1".to_string(), &["bash".to_string()], None, 80, 24, None)
            .await
            .unwrap();

        manager
            .create_session("s2".to_string(), &["bash".to_string()], None, 80, 24, None)
            .await
            .unwrap();

        let sessions = manager.list_sessions().await;
        assert_eq!(sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_session_manager_remove_session() {
        let manager = SessionManager::new(Duration::from_secs(3600), SessionMode::Isolated);

        manager
            .create_session(
                "to-remove".to_string(),
                &["bash".to_string()],
                None,
                80,
                24,
                None,
            )
            .await
            .unwrap();
        assert_eq!(manager.session_count().await, 1);

        assert!(manager.remove_session("to-remove").await);
        assert_eq!(manager.session_count().await, 0);

        // Removing non-existent session returns false
        assert!(!manager.remove_session("to-remove").await);
    }

    #[tokio::test]
    async fn test_session_manager_shutdown() {
        let manager = SessionManager::new(Duration::from_secs(3600), SessionMode::Isolated);

        manager
            .create_session("s1".to_string(), &["bash".to_string()], None, 80, 24, None)
            .await
            .unwrap();
        manager
            .create_session("s2".to_string(), &["bash".to_string()], None, 80, 24, None)
            .await
            .unwrap();

        manager.shutdown().await;
        assert_eq!(manager.session_count().await, 0);
    }

    #[tokio::test]
    async fn test_session_manager_stats() {
        let manager = SessionManager::new(Duration::from_secs(3600), SessionMode::Isolated);

        manager
            .create_session(
                "iso1".to_string(),
                &["bash".to_string()],
                None,
                80,
                24,
                Some(SessionMode::Isolated),
            )
            .await
            .unwrap();

        manager
            .create_session(
                "shared1".to_string(),
                &["bash".to_string()],
                None,
                80,
                24,
                Some(SessionMode::SharedReadWrite),
            )
            .await
            .unwrap();

        let stats = manager.stats().await;
        assert_eq!(stats.total_sessions, 2);
        assert_eq!(stats.isolated_sessions, 1);
        assert_eq!(stats.shared_sessions, 1);
    }

    #[tokio::test]
    async fn test_session_manager_cleanup_inactive() {
        let mut manager = SessionManager::new(Duration::from_millis(500), SessionMode::Isolated);
        manager.reconnect_window = Duration::from_millis(200);

        manager
            .create_session(
                "stale".to_string(),
                &["bash".to_string()],
                None,
                80,
                24,
                None,
            )
            .await
            .unwrap();

        // Session is fresh, cleanup should not remove it
        assert_eq!(manager.cleanup_inactive().await, 0);
        assert_eq!(manager.session_count().await, 1);

        // Wait for session to exceed timeout
        sleep(Duration::from_millis(600)).await;

        // Now cleanup should remove it (session has no clients)
        let cleaned = manager.cleanup_inactive().await;
        assert_eq!(cleaned, 1);
        assert_eq!(manager.session_count().await, 0);
    }

    #[test]
    fn test_session_mode_display() {
        assert_eq!(SessionMode::Isolated.to_string(), "isolated");
        assert_eq!(SessionMode::SharedReadOnly.to_string(), "shared_readonly");
        assert_eq!(SessionMode::SharedReadWrite.to_string(), "shared_readwrite");
    }
}
