/// WebSocket handler for terminal connections — modular implementation.
///
/// This module orchestrates the full lifecycle of a WebSocket terminal session:
/// 1. Connection upgrade and authentication ([`auth`])
/// 2. Handshake (Resize/Join) parsing ([`handshake`])
/// 3. Session creation/join and client registration ([`session_lifecycle`])
/// 4. PTY I/O tasks and heartbeat monitoring ([`pty_io`])
/// 5. Main message loop dispatching Input/Resize/Ping/FileList ([`message_loop`])
/// 6. Cleanup and disconnection ([`session_lifecycle::cleanup_client`])
mod auth;
mod handshake;
mod message_loop;
mod pty_io;
mod session_lifecycle;
mod utils;

use crate::audit::AuditLogger;
use crate::config::Config;
use crate::config::ValidationConfig;
use crate::rate_limit::RateLimiter;
use crate::session::SessionManager;
use axum::{
    extract::{
        ConnectInfo, State, WebSocketUpgrade,
        ws::{Message as WsMessage, WebSocket},
    },
    response::Response,
};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub(crate) use utils::extract_real_ip;

/// Type alias for the shared WebSocket sender used across tasks.
pub(crate) type WsSender = Arc<tokio::sync::Mutex<SplitSink<WebSocket, WsMessage>>>;

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub audit_logger: Arc<AuditLogger>,
    pub validation: Arc<ValidationConfig>,
    pub rate_limiter: Arc<RateLimiter>,
    pub session_manager: Arc<SessionManager>,
    pub shutdown_token: CancellationToken,
    pub active_connections: Arc<AtomicUsize>,
}

impl AppState {
    /// Atomically claim a connection slot if the connection limit has not
    /// been reached.
    ///
    /// Uses a `compare_exchange` loop so the limit check and the counter
    /// increment happen as one atomic step: concurrent connections can
    /// never both pass the check and both increment, so
    /// `active_connections` can never exceed `max_connections`. Returns
    /// `false` without touching the counter when the limit is reached.
    pub(crate) fn try_acquire_connection(&self) -> bool {
        let max = self.config.max_connections;
        let mut current = self.active_connections.load(Ordering::Relaxed);
        loop {
            if current >= max {
                return false;
            }
            match self.active_connections.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(next) => current = next,
            }
        }
    }
}

/// Maximum allowed WebSocket message size (64 KB).
///
/// The largest legitimate message is an `Input` payload (capped by
/// `ValidationConfig::max_input_size` = 16 KB) plus JSON envelope overhead.
/// 64 KB provides ample headroom while preventing memory-exhaustion attacks
/// from oversized frames.
const MAX_WS_MESSAGE_SIZE: usize = 64 * 1024;

/// WebSocket upgrade handler
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
) -> Response {
    let remote_addr = extract_real_ip(&headers, addr.ip(), state.config.trust_proxy);

    // Check max connections limit and increment the counter atomically
    if !state.try_acquire_connection() {
        let current = state.active_connections.load(Ordering::Relaxed);
        warn!(
            "Connection limit reached ({}/{}), rejecting {}",
            current, state.config.max_connections, remote_addr
        );
        return Response::builder()
            .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
            .body(axum::body::Body::from("Connection limit reached"))
            .unwrap_or_default();
    }

    ws.max_message_size(MAX_WS_MESSAGE_SIZE)
        .on_upgrade(move |socket| handle_socket(socket, state, remote_addr))
}

/// Why a WebSocket connection ended.
///
/// Carried through the session handler so the `ConnectionClosed` audit
/// event records the actual cause instead of a generic "normal closure".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseReason {
    /// The client closed the connection (or dropped it before we finished).
    ClientClosed,
    /// Authentication failed, was rejected, or server auth is misconfigured.
    AuthFailed,
    /// The initial Resize/Join handshake failed.
    HandshakeFailed,
    /// Creating/joining the session or registering the client failed.
    SessionSetupFailed,
    /// The heartbeat monitor detected a silent client.
    HeartbeatTimeout,
    /// The server was shutting down.
    Shutdown,
    /// An I/O or protocol error terminated the connection.
    IoError,
}

impl CloseReason {
    /// Human-readable description used in audit and server logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClientClosed => "client closed the connection",
            Self::AuthFailed => "authentication failed",
            Self::HandshakeFailed => "handshake failed",
            Self::SessionSetupFailed => "session setup failed",
            Self::HeartbeatTimeout => "heartbeat timeout",
            Self::Shutdown => "server shutdown",
            Self::IoError => "I/O error",
        }
    }
}

/// Handle a WebSocket connection
async fn handle_socket(socket: WebSocket, state: AppState, remote_addr: String) {
    info!("New WebSocket connection from {}", remote_addr);

    let reason = handle_terminal_session(socket, state.clone(), remote_addr).await;

    // Decrement active connection count
    state.active_connections.fetch_sub(1, Ordering::Relaxed);

    // Only log non-shutdown closures — shutdown already logs its own messages,
    // and the "closed" log just adds noise after "Shutdown complete".
    if !state.shutdown_token.is_cancelled() {
        info!("WebSocket connection closed: {}", reason.as_str());
    }
}

/// Handle a terminal session using SessionManager.
///
/// Guarantees that a `ConnectionClosed` audit event is recorded no matter
/// how the session ends (auth failure, handshake failure, setup error,
/// heartbeat timeout, shutdown), and that it carries the actual reason.
async fn handle_terminal_session(
    socket: WebSocket,
    state: AppState,
    remote_addr: String,
) -> CloseReason {
    let (ws_sender, mut ws_receiver) = socket.split();
    let ws_sender = Arc::new(tokio::sync::Mutex::new(ws_sender));

    let client_id = uuid::Uuid::new_v4().to_string();

    // Audit: WebSocket connection established
    state
        .audit_logger
        .log_connection(&remote_addr, &client_id)
        .await;

    let reason = match run_session(
        &state,
        &ws_sender,
        &mut ws_receiver,
        &remote_addr,
        &client_id,
    )
    .await
    {
        Ok(reason) => reason,
        Err(e) => {
            warn!("WebSocket connection error: {}", e);
            state
                .audit_logger
                .log_error(
                    &remote_addr,
                    &client_id,
                    &format!("WebSocket connection error: {e}"),
                )
                .await;
            CloseReason::IoError
        }
    };

    // Always record the closing event with the actual reason. The
    // `session_id` field carries the same client id as `ConnectionOpened`
    // so the two events can be paired; the terminal session id is recorded
    // separately in `SessionStarted` events.
    state
        .audit_logger
        .log_disconnect(&remote_addr, &client_id, reason.as_str())
        .await;

    reason
}

/// Run the session body: auth, handshake, session setup, message loop, cleanup.
///
/// Returns the reason the connection ended, or an error for I/O and other
/// unexpected failures.
async fn run_session(
    state: &AppState,
    ws_sender: &WsSender,
    ws_receiver: &mut SplitStream<WebSocket>,
    remote_addr: &str,
    client_id: &str,
) -> Result<CloseReason, Box<dyn std::error::Error + Send + Sync>> {
    // ── Authentication ──────────────────────────────────────────────
    let username =
        match auth::authenticate(state, ws_sender, ws_receiver, remote_addr, client_id).await? {
            auth::AuthResult::Success(username) => username,
            auth::AuthResult::Close(reason) => return Ok(reason),
        };

    // ── Handshake (Resize/Join) ─────────────────────────────────────
    let handshake = match handshake::read_handshake(
        state,
        ws_sender,
        ws_receiver,
        remote_addr,
        client_id,
    )
    .await
    {
        Ok(handshake) => handshake,
        Err(()) => return Ok(CloseReason::HandshakeFailed),
    };

    // ── Create or join session ──────────────────────────────────────
    let resolved = match session_lifecycle::create_or_join_session(
        state,
        ws_sender,
        handshake.join_session_id,
        handshake.cols,
        handshake.rows,
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(()) => return Ok(CloseReason::SessionSetupFailed),
    };

    let session = &resolved.session;
    let session_id = &resolved.session_id;
    let is_readonly = resolved.is_readonly;

    // Add this client to the session
    if let Err(e) =
        session_lifecycle::add_client(session, client_id, remote_addr, username, is_readonly).await
    {
        warn!("Failed to register client in session: {}", e);
        return Ok(CloseReason::SessionSetupFailed);
    }

    // Log session started
    state
        .audit_logger
        .log_session_started(
            remote_addr,
            state
                .config
                .auth
                .as_ref()
                .and_then(|a| a.username.as_deref()),
            session_id,
        )
        .await;

    // Send ready message
    let ready_msg = crate::protocol::Message::Ready(crate::protocol::ReadyData {
        session_id: session_id.clone(),
        cols: handshake.cols,
        rows: handshake.rows,
        readonly: is_readonly,
    });
    utils::send_message(ws_sender, &ready_msg).await;

    // Get PTY session for I/O
    let pty_session_arc = session.pty_session();

    // Duplicate the PTY master fd once for writing
    let mut pty_writer = pty_io::create_pty_writer(&pty_session_arc)
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;

    // Spawn PTY reader task
    let pty_to_ws = pty_io::spawn_pty_reader(pty_session_arc.clone(), session.clone());

    // Spawn output subscriber task. If the PTY already exited, notify this
    // client directly and use a dummy handle so cleanup can abort it
    // uniformly.
    let subscriber_task = match session.subscribe_output() {
        Some(output_rx) => pty_io::spawn_output_subscriber(ws_sender.clone(), output_rx),
        None => {
            let disconnect =
                crate::protocol::Message::Disconnect(crate::protocol::DisconnectData {
                    reason: "Shell exited".to_string(),
                    code: 0,
                });
            if let Ok(json) = disconnect.to_json() {
                let _ = ws_sender
                    .lock()
                    .await
                    .send(axum::extract::ws::Message::Text(json.into()))
                    .await;
            }
            tokio::task::spawn(async {})
        }
    };

    // Heartbeat timeout tracking. The monitor actively sends protocol-level
    // ping frames (see pty_io::spawn_heartbeat_monitor) and treats a pong or
    // app-level ping as proof of life.
    let last_ping_time = Arc::new(tokio::sync::Mutex::new(std::time::Instant::now()));
    let mut heartbeat_task =
        pty_io::spawn_heartbeat_monitor(ws_sender.clone(), last_ping_time.clone());

    // ── Main message loop ───────────────────────────────────────────
    let mut ctx = message_loop::MessageLoopContext {
        state,
        ws_sender,
        ws_receiver,
        heartbeat_task: &mut heartbeat_task,
        session,
        pty_session: &pty_session_arc,
        pty_writer: &mut pty_writer,
        last_ping_time: &last_ping_time,
        client_id,
        session_id,
        remote_addr,
    };
    let reason = message_loop::run(&mut ctx).await;

    // ── Cleanup ─────────────────────────────────────────────────────
    heartbeat_task.abort();
    pty_to_ws.abort();
    subscriber_task.abort();

    session_lifecycle::cleanup_client(state, session, session_id, client_id).await;

    // Send disconnect message
    let disconnect = crate::protocol::Message::Disconnect(crate::protocol::DisconnectData {
        reason: "Session ended".to_string(),
        code: 0,
    });
    utils::send_message(ws_sender, &disconnect).await;

    let _ = ws_sender.lock().await.close().await;

    Ok(reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionMode;
    use std::time::Duration;

    fn test_state(max_connections: usize) -> AppState {
        let config = Config {
            max_connections,
            ..Config::default()
        };
        let audit_logger = AuditLogger::new(config.audit.log_file.clone(), config.audit.enabled);
        let validation = config.validation.clone();
        let rate_limiter = RateLimiter::new(
            config.rate_limit.max_requests,
            config.rate_limit.window_seconds,
        );
        let session_mode: SessionMode = config.session.mode.parse().unwrap();
        let session_manager = Arc::new(SessionManager::new(
            Duration::from_secs(config.session.timeout),
            session_mode,
        ));

        AppState {
            config: Arc::new(config),
            audit_logger: Arc::new(audit_logger),
            validation: Arc::new(validation),
            rate_limiter: Arc::new(rate_limiter),
            session_manager,
            shutdown_token: CancellationToken::new(),
            active_connections: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Concurrency regression test for the connection limit: when many
    /// connections race to claim slots, exactly `max` may succeed and the
    /// counter must never exceed the limit. A check-then-increment that
    /// yields between the two steps would let far more than `max` tasks
    /// succeed and fail these assertions.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_try_acquire_connection_never_exceeds_limit() {
        const MAX: usize = 100;
        const TASKS: usize = 500;
        let state = test_state(MAX);

        let handles: Vec<_> = (0..TASKS)
            .map(|_| {
                let state = state.clone();
                tokio::spawn(async move {
                    if state.try_acquire_connection() {
                        Some(state.active_connections.load(Ordering::Relaxed))
                    } else {
                        None
                    }
                })
            })
            .collect();

        let mut acquired = 0;
        for handle in handles {
            if let Some(count) = handle.await.unwrap() {
                acquired += 1;
                assert!(
                    count <= MAX,
                    "connection counter exceeded limit: {} > {}",
                    count,
                    MAX
                );
            }
        }

        assert_eq!(acquired, MAX, "exactly {} tasks should acquire a slot", MAX);
        assert_eq!(
            state.active_connections.load(Ordering::Relaxed),
            MAX,
            "final counter should equal the limit"
        );
    }

    /// When the limit is reached, `try_acquire_connection` must reject
    /// without touching the counter; a released slot can be claimed again.
    #[tokio::test]
    async fn test_try_acquire_connection_rejects_at_limit() {
        let state = test_state(2);

        assert!(state.try_acquire_connection());
        assert!(state.try_acquire_connection());
        assert!(!state.try_acquire_connection());

        // The counter must be exactly 2 — a rejected attempt must not
        // increment it.
        assert_eq!(state.active_connections.load(Ordering::Relaxed), 2);

        // A released slot (simulated by the handler's fetch_sub on close)
        // can be claimed again.
        state.active_connections.fetch_sub(1, Ordering::Relaxed);
        assert!(state.try_acquire_connection());
        assert_eq!(state.active_connections.load(Ordering::Relaxed), 2);
    }
}
