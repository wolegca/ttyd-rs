/// WebSocket handler for terminal connections - Refactored to use SessionManager
use crate::audit::AuditLogger;
use crate::auth::{BasicAuth, TokenAuth};
use crate::config::Config;
use crate::protocol::*;
use crate::rate_limit::RateLimiter;
use crate::session::{Client, SessionManager, SessionMode};
use crate::validation::ValidationConfig;
use axum::{
    extract::{
        ConnectInfo, State, WebSocketUpgrade,
        ws::{Message as WsMessage, WebSocket},
    },
    response::Response,
};
use futures::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

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

/// Extract the real client IP from proxy headers.
///
/// When `trust_proxy` is enabled, checks (in order):
/// 1. `X-Real-IP` header — the canonical real IP set by nginx/Caddy
/// 2. `X-Forwarded-For` header — first entry (client IP) from the chain
///
/// Falls back to `connect_addr` if neither header is present or valid.
/// Only accepts valid IP addresses from headers to prevent spoofing with
/// arbitrary strings.
fn extract_real_ip(
    headers: &axum::http::HeaderMap,
    connect_addr: std::net::IpAddr,
    trust_proxy: bool,
) -> String {
    if !trust_proxy {
        return connect_addr.to_string();
    }

    // Prefer X-Real-IP (single value, set by most reverse proxies)
    if let Some(val) = headers.get("x-real-ip")
        && let Ok(s) = val.to_str()
    {
        let trimmed = s.trim();
        if let Ok(ip) = trimmed.parse::<std::net::IpAddr>() {
            return ip.to_string();
        }
    }

    // Fall back to first entry of X-Forwarded-For
    if let Some(val) = headers.get("x-forwarded-for")
        && let Ok(s) = val.to_str()
    {
        // X-Forwarded-For: client, proxy1, proxy2
        if let Some(first) = s.split(',').next() {
            let trimmed = first.trim();
            if let Ok(ip) = trimmed.parse::<std::net::IpAddr>() {
                return ip.to_string();
            }
        }
    }

    connect_addr.to_string()
}

/// WebSocket upgrade handler
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
) -> Response {
    let remote_addr = extract_real_ip(&headers, addr.ip(), state.config.trust_proxy);

    // Check max connections limit
    let current = state.active_connections.load(Ordering::Relaxed);
    if current >= state.config.max_connections {
        warn!(
            "Connection limit reached ({}/{}), rejecting {}",
            current, state.config.max_connections, remote_addr
        );
        return Response::builder()
            .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
            .body(axum::body::Body::from("Connection limit reached"))
            .unwrap_or_default();
    }

    // Increment active connection count
    state.active_connections.fetch_add(1, Ordering::Relaxed);

    ws.on_upgrade(move |socket| handle_socket(socket, state, remote_addr))
}

/// Handle a WebSocket connection
async fn handle_socket(socket: WebSocket, state: AppState, remote_addr: String) {
    info!("New WebSocket connection from {}", remote_addr);

    let result = handle_terminal_session(socket, state.clone(), remote_addr).await;

    // Decrement active connection count
    state.active_connections.fetch_sub(1, Ordering::Relaxed);

    match result {
        Ok(()) => info!("WebSocket connection closed normally"),
        Err(e) => error!("WebSocket error: {}", e),
    }
}

/// Handle a terminal session using SessionManager
async fn handle_terminal_session(
    socket: WebSocket,
    state: AppState,
    remote_addr: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let (ws_sender, mut ws_receiver) = socket.split();
    let ws_sender = Arc::new(tokio::sync::Mutex::new(ws_sender));

    let client_id = uuid::Uuid::new_v4().to_string();

    // Handle authentication if enabled
    let username = if let Some(auth_config) = &state.config.auth {
        match auth_config.method.as_str() {
            "basic"
                if let (Some(username), Some(password)) =
                    (&auth_config.username, &auth_config.password) =>
            {
                // Check rate limit before processing auth
                if let Err(duration) = state.rate_limiter.check(&remote_addr).await {
                    warn!("Rate limit exceeded for {}", remote_addr);

                    let fail_msg = Message::AuthFail(AuthFailData {
                        reason: format!(
                            "Rate limit exceeded. Try again in {} seconds",
                            duration.as_secs()
                        ),
                    });
                    ws_sender
                        .lock()
                        .await
                        .send(WsMessage::Text(fail_msg.to_json()?.into()))
                        .await?;
                    return Ok(());
                }

                let basic_auth = BasicAuth::new(username.clone(), password.clone());

                // Wait for auth message
                match ws_receiver.next().await {
                    Some(Ok(WsMessage::Text(text))) => {
                        let msg = Message::from_json(&text)?;
                        match msg {
                            Message::Auth(auth_data) => {
                                // Validate auth method
                                if let Err(e) =
                                    state.validation.validate_auth_method(&auth_data.method)
                                {
                                    warn!("Invalid auth method: {}", e);
                                    let fail_msg = Message::AuthFail(AuthFailData {
                                        reason: format!("Invalid authentication method: {}", e),
                                    });
                                    ws_sender
                                        .lock()
                                        .await
                                        .send(WsMessage::Text(fail_msg.to_json()?.into()))
                                        .await?;
                                    return Ok(());
                                }

                                // Validate credentials format
                                if let Err(e) = state
                                    .validation
                                    .validate_credentials(&auth_data.credentials)
                                {
                                    warn!("Invalid credentials format: {}", e);
                                    state
                                        .audit_logger
                                        .log_auth_attempt(
                                            &remote_addr,
                                            "unknown",
                                            false,
                                            &client_id,
                                        )
                                        .await;

                                    let fail_msg = Message::AuthFail(AuthFailData {
                                        reason: "Invalid credentials format".to_string(),
                                    });
                                    ws_sender
                                        .lock()
                                        .await
                                        .send(WsMessage::Text(fail_msg.to_json()?.into()))
                                        .await?;
                                    return Ok(());
                                }

                                let valid = if auth_data.method == "basic" {
                                    basic_auth.validate(&auth_data.credentials)
                                } else {
                                    false
                                };

                                if !valid {
                                    state
                                        .audit_logger
                                        .log_auth_attempt(&remote_addr, username, false, &client_id)
                                        .await;

                                    let fail_msg = Message::AuthFail(AuthFailData {
                                        reason: "Invalid credentials".to_string(),
                                    });
                                    ws_sender
                                        .lock()
                                        .await
                                        .send(WsMessage::Text(fail_msg.to_json()?.into()))
                                        .await?;
                                    return Ok(());
                                }

                                state
                                    .audit_logger
                                    .log_auth_attempt(&remote_addr, username, true, &client_id)
                                    .await;

                                // Reset rate limit on successful auth
                                state.rate_limiter.reset(&remote_addr).await;

                                let ok_msg = Message::AuthOk(AuthOkData {
                                    client_id: client_id.clone(),
                                    readonly: false,
                                });
                                ws_sender
                                    .lock()
                                    .await
                                    .send(WsMessage::Text(ok_msg.to_json()?.into()))
                                    .await?;

                                Some(username.clone())
                            }
                            _ => {
                                let fail_msg = Message::AuthFail(AuthFailData {
                                    reason: "Expected auth message".to_string(),
                                });
                                ws_sender
                                    .lock()
                                    .await
                                    .send(WsMessage::Text(fail_msg.to_json()?.into()))
                                    .await?;
                                return Ok(());
                            }
                        }
                    }
                    _ => {
                        return Ok(());
                    }
                }
            }
            "token" if let Some(token) = &auth_config.token => {
                // Check rate limit before processing auth
                if let Err(duration) = state.rate_limiter.check(&remote_addr).await {
                    warn!("Rate limit exceeded for {}", remote_addr);

                    let fail_msg = Message::AuthFail(AuthFailData {
                        reason: format!(
                            "Rate limit exceeded. Try again in {} seconds",
                            duration.as_secs()
                        ),
                    });
                    ws_sender
                        .lock()
                        .await
                        .send(WsMessage::Text(fail_msg.to_json()?.into()))
                        .await?;
                    return Ok(());
                }

                let token_auth = TokenAuth::new(token.clone());

                // Wait for auth message
                match ws_receiver.next().await {
                    Some(Ok(WsMessage::Text(text))) => {
                        let msg = Message::from_json(&text)?;
                        match msg {
                            Message::Auth(auth_data) => {
                                // Validate auth method
                                if let Err(e) =
                                    state.validation.validate_auth_method(&auth_data.method)
                                {
                                    warn!("Invalid auth method: {}", e);
                                    let fail_msg = Message::AuthFail(AuthFailData {
                                        reason: format!("Invalid authentication method: {}", e),
                                    });
                                    ws_sender
                                        .lock()
                                        .await
                                        .send(WsMessage::Text(fail_msg.to_json()?.into()))
                                        .await?;
                                    return Ok(());
                                }

                                // Validate credentials format
                                if let Err(e) = state
                                    .validation
                                    .validate_credentials(&auth_data.credentials)
                                {
                                    warn!("Invalid credentials format: {}", e);
                                    state
                                        .audit_logger
                                        .log_auth_attempt(
                                            &remote_addr,
                                            "token-user",
                                            false,
                                            &client_id,
                                        )
                                        .await;

                                    let fail_msg = Message::AuthFail(AuthFailData {
                                        reason: "Invalid credentials format".to_string(),
                                    });
                                    ws_sender
                                        .lock()
                                        .await
                                        .send(WsMessage::Text(fail_msg.to_json()?.into()))
                                        .await?;
                                    return Ok(());
                                }

                                let valid = if auth_data.method == "token" {
                                    token_auth.validate(&auth_data.credentials)
                                } else {
                                    false
                                };

                                if !valid {
                                    state
                                        .audit_logger
                                        .log_auth_attempt(
                                            &remote_addr,
                                            "token-user",
                                            false,
                                            &client_id,
                                        )
                                        .await;

                                    let fail_msg = Message::AuthFail(AuthFailData {
                                        reason: "Invalid token".to_string(),
                                    });
                                    ws_sender
                                        .lock()
                                        .await
                                        .send(WsMessage::Text(fail_msg.to_json()?.into()))
                                        .await?;
                                    return Ok(());
                                }

                                state
                                    .audit_logger
                                    .log_auth_attempt(&remote_addr, "token-user", true, &client_id)
                                    .await;

                                // Reset rate limit on successful auth
                                state.rate_limiter.reset(&remote_addr).await;

                                let ok_msg = Message::AuthOk(AuthOkData {
                                    client_id: client_id.clone(),
                                    readonly: false,
                                });
                                ws_sender
                                    .lock()
                                    .await
                                    .send(WsMessage::Text(ok_msg.to_json()?.into()))
                                    .await?;

                                None
                            }
                            _ => {
                                let fail_msg = Message::AuthFail(AuthFailData {
                                    reason: "Expected auth message".to_string(),
                                });
                                ws_sender
                                    .lock()
                                    .await
                                    .send(WsMessage::Text(fail_msg.to_json()?.into()))
                                    .await?;
                                return Ok(());
                            }
                        }
                    }
                    _ => {
                        return Ok(());
                    }
                }
            }
            _ => {
                // Misconfigured auth: method doesn't match available credentials
                // Reject the connection rather than allowing unauthenticated access
                error!(
                    "Auth method '{}' is misconfigured — missing credentials",
                    auth_config.method
                );
                let fail_msg = Message::AuthFail(AuthFailData {
                    reason: "Server authentication misconfigured".to_string(),
                });
                ws_sender
                    .lock()
                    .await
                    .send(WsMessage::Text(fail_msg.to_json()?.into()))
                    .await?;
                return Ok(());
            }
        }
    } else {
        None
    };

    // Read initial handshake messages: Resize (required) and optionally Join.
    // The client may send them in either order, but we must not consume
    // messages that belong to the main I/O loop (Input, Ping, etc.).
    let mut cols: u16 = 80;
    let mut rows: u16 = 24;
    let mut join_session_id: Option<String> = None;
    let mut resize_received = false;

    // Read first message
    match ws_receiver.next().await {
        Some(Ok(WsMessage::Text(text))) => {
            let msg = Message::from_json(&text)?;
            match msg {
                Message::Resize(data) => {
                    if let Err(e) = state
                        .validation
                        .validate_terminal_size(data.cols, data.rows)
                    {
                        error!("Invalid terminal size: {}", e);
                        state
                            .audit_logger
                            .log_error(
                                &remote_addr,
                                &client_id,
                                &format!("Invalid terminal size: {}", e),
                            )
                            .await;
                        let error_msg = Message::Error(ErrorData {
                            code: "INVALID_SIZE".to_string(),
                            message: format!("Invalid terminal size: {}", e),
                            fatal: true,
                        });
                        ws_sender
                            .lock()
                            .await
                            .send(WsMessage::Text(error_msg.to_json()?.into()))
                            .await?;
                        return Ok(());
                    }
                    cols = data.cols;
                    rows = data.rows;
                    resize_received = true;
                }
                Message::Join(data) => {
                    join_session_id = Some(data.session_id);
                }
                _ => {
                    warn!("Expected resize or join, got other message type");
                }
            }
        }
        _ => {
            warn!("No handshake message received");
        }
    }

    // If we got Join first but haven't received Resize yet, read the next
    // message expecting Resize.
    if join_session_id.is_some()
        && !resize_received
        && let Some(Ok(WsMessage::Text(text))) = ws_receiver.next().await
        && let Ok(Message::Resize(data)) = Message::from_json(&text)
    {
        if let Err(e) = state
            .validation
            .validate_terminal_size(data.cols, data.rows)
        {
            error!("Invalid terminal size: {}", e);
            state
                .audit_logger
                .log_error(
                    &remote_addr,
                    &client_id,
                    &format!("Invalid terminal size: {}", e),
                )
                .await;
            let error_msg = Message::Error(ErrorData {
                code: "INVALID_SIZE".to_string(),
                message: format!("Invalid terminal size: {}", e),
                fatal: true,
            });
            ws_sender
                .lock()
                .await
                .send(WsMessage::Text(error_msg.to_json()?.into()))
                .await?;
            return Ok(());
        }
        cols = data.cols;
        rows = data.rows;
    }

    // Create or join session based on whether a Join message was received
    let (session, session_id, is_readonly) = if let Some(target_id) = join_session_id {
        // Try to join an existing session
        match state.session_manager.get_session(&target_id).await {
            Some(existing_session) => {
                let mode = existing_session.metadata().mode;
                if mode == SessionMode::Isolated {
                    let error_msg = Message::Error(ErrorData {
                        code: "CANNOT_JOIN".to_string(),
                        message: "Cannot join an isolated session".to_string(),
                        fatal: true,
                    });
                    ws_sender
                        .lock()
                        .await
                        .send(WsMessage::Text(error_msg.to_json()?.into()))
                        .await?;
                    return Ok(());
                }
                let readonly = mode == SessionMode::SharedReadOnly;
                info!(
                    "Client joining session {} (mode={}, readonly={})",
                    target_id, mode, readonly
                );
                (existing_session, target_id, readonly)
            }
            None => {
                // Session expired or not found — create a new one instead of erroring.
                // This handles reconnection gracefully: the client's old session may
                // have been cleaned up, so we silently create a fresh session.
                info!(
                    "Session '{}' not found, creating new session for rejoining client",
                    target_id
                );
                let new_id = uuid::Uuid::new_v4().to_string();
                let new_session = state
                    .session_manager
                    .create_session(
                        new_id.clone(),
                        &state.config.command,
                        state
                            .config
                            .working_dir
                            .as_ref()
                            .map(|p| p.to_string_lossy().to_string()),
                        cols,
                        rows,
                        None,
                    )
                    .await?;
                info!("Session created: id={}", new_id);
                (new_session, new_id, false)
            }
        }
    } else {
        // Create a new session
        let session_id = uuid::Uuid::new_v4().to_string();
        let new_session = state
            .session_manager
            .create_session(
                session_id.clone(),
                &state.config.command,
                state
                    .config
                    .working_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string()),
                cols,
                rows,
                None,
            )
            .await?;
        info!("Session created: id={}", session_id);
        (new_session, session_id, false)
    };

    // Add this client to the session
    let client = Client {
        client_id: client_id.clone(),
        remote_addr: remote_addr.to_string(),
        username,
        connected_at: Instant::now(),
        readonly: is_readonly,
    };

    session.add_client(client).await?;

    // Log session started
    state
        .audit_logger
        .log_session_started(
            &remote_addr,
            state
                .config
                .auth
                .as_ref()
                .and_then(|a| a.username.as_deref()),
            &session_id,
        )
        .await;

    // Send ready message
    let ready_msg = Message::Ready(ReadyData {
        session_id: session_id.clone(),
        cols,
        rows,
        readonly: is_readonly,
    });

    ws_sender
        .lock()
        .await
        .send(WsMessage::Text(ready_msg.to_json()?.into()))
        .await?;

    // Get PTY session for I/O
    let pty_session_arc = session.pty_session();

    // Duplicate the PTY master fd once for writing, so we don't need to
    // dup/close on every keystroke.  The read task does its own dup.
    let mut pty_writer = {
        use std::os::fd::BorrowedFd;
        let pty_guard = pty_session_arc.lock().await;
        let master_fd = pty_guard.master_fd();
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(master_fd) };
        let dup_fd = nix::unistd::dup(borrowed_fd)
            .map_err(|e| format!("Failed to duplicate PTY fd for write: {}", e))?;
        let pty_file = std::fs::File::from(dup_fd);
        tokio::fs::File::from_std(pty_file)
    };

    // Spawn task to read from PTY and broadcast to all subscribers
    let pty_session_for_read = pty_session_arc.clone();
    let session_for_broadcast = session.clone();
    let ws_sender_for_pty = ws_sender.clone();
    let pty_to_ws = tokio::spawn(async move {
        use std::os::fd::BorrowedFd;

        let pty_guard = pty_session_for_read.lock().await;
        let master_fd = pty_guard.master_fd();

        // Duplicate the file descriptor so we have our own independent fd
        // This prevents double-close issues when the File is dropped
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(master_fd) };
        let dup_fd = match nix::unistd::dup(borrowed_fd) {
            Ok(fd) => fd,
            Err(e) => {
                error!("Failed to duplicate PTY fd: {}", e);
                return;
            }
        };

        drop(pty_guard); // Release lock before async operations

        // Set the duplicated fd non-blocking and drive reads through AsyncFd.
        // tokio::fs::File runs a synchronous read() on a spawn_blocking thread
        // that abort() cannot interrupt; combined with a shell that outlives
        // the connection, that thread blocks forever and hangs runtime
        // shutdown. AsyncFd makes the read truly async and cancellable.
        let flags = nix::fcntl::fcntl(&dup_fd, nix::fcntl::FcntlArg::F_GETFL).unwrap_or(0);
        if let Err(e) = nix::fcntl::fcntl(
            &dup_fd,
            nix::fcntl::FcntlArg::F_SETFL(
                nix::fcntl::OFlag::from_bits_truncate(flags) | nix::fcntl::OFlag::O_NONBLOCK,
            ),
        ) {
            error!("Failed to set PTY fd non-blocking: {}", e);
            return;
        }

        let async_fd = match tokio::io::unix::AsyncFd::new(dup_fd) {
            Ok(fd) => fd,
            Err(e) => {
                error!("Failed to register PTY fd with the reactor: {}", e);
                return;
            }
        };
        let mut buffer = vec![0u8; 4096];

        loop {
            let mut guard = match async_fd.readable().await {
                Ok(guard) => guard,
                Err(e) => {
                    error!("Error waiting for PTY readability: {}", e);
                    break;
                }
            };

            let read_result = guard.try_io(|inner| {
                nix::unistd::read(inner.get_ref(), &mut buffer).map_err(std::io::Error::from)
            });

            match read_result {
                Ok(Ok(0)) => {
                    debug!("PTY EOF");
                    break;
                }
                Ok(Ok(n)) => {
                    // Broadcast PTY output to all connected clients
                    session_for_broadcast.broadcast_output(buffer[..n].to_vec());
                }
                Ok(Err(e)) => {
                    // EIO is expected when the shell exits (Ctrl-D closes the
                    // slave side of the PTY). Treat it as a normal EOF.
                    if e.raw_os_error() == Some(libc::EIO) {
                        debug!("PTY EIO (shell exited)");
                    } else {
                        error!("Error reading from PTY: {}", e);
                    }
                    break;
                }
                Err(_would_block) => continue,
            }
        }

        // Notify clients that the shell has exited
        let disconnect = Message::Disconnect(DisconnectData {
            reason: "Shell exited".to_string(),
            code: 0,
        });
        if let Ok(json) = disconnect.to_json() {
            let _ = ws_sender_for_pty
                .lock()
                .await
                .send(WsMessage::Text(json.into()))
                .await;
        }
    });

    // Spawn task to receive broadcast output and forward to this client's WebSocket
    let ws_sender_for_sub = ws_sender.clone();
    let mut output_rx = session.subscribe_output();
    let subscriber_task = tokio::spawn(async move {
        loop {
            match output_rx.recv().await {
                Ok(data) => {
                    let output = String::from_utf8_lossy(&data).to_string();
                    let msg = Message::Output(OutputData { payload: output });
                    if let Ok(json) = msg.to_json()
                        && ws_sender_for_sub
                            .lock()
                            .await
                            .send(WsMessage::Text(json.into()))
                            .await
                            .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Skip lagged messages
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    });

    // Handle WebSocket messages
    loop {
        let msg = tokio::select! {
            msg = ws_receiver.next() => msg,
            _ = state.shutdown_token.cancelled() => {
                info!("Shutdown signal received, closing WebSocket connection");
                break;
            }
        };
        let Some(msg) = msg else { break };
        match msg {
            Ok(WsMessage::Text(text)) => {
                match Message::from_json(&text) {
                    Ok(Message::Input(data)) => {
                        // Check if client can write (read-only enforcement)
                        if !session.can_write(&client_id).await {
                            let error_msg = Message::Error(ErrorData {
                                code: "READONLY".to_string(),
                                message: "This session is read-only".to_string(),
                                fatal: false,
                            });
                            if let Ok(json) = error_msg.to_json() {
                                let _ = ws_sender
                                    .lock()
                                    .await
                                    .send(WsMessage::Text(json.into()))
                                    .await;
                            }
                            continue;
                        }

                        // Validate input payload
                        if let Err(e) = state.validation.validate_input_payload(&data.payload) {
                            warn!("Invalid input payload: {}", e);
                            state
                                .audit_logger
                                .log_error(
                                    &remote_addr,
                                    &session_id,
                                    &format!("Invalid input: {}", e),
                                )
                                .await;

                            let error_msg = Message::Error(ErrorData {
                                code: "INVALID_INPUT".to_string(),
                                message: format!("Invalid input: {}", e),
                                fatal: false,
                            });
                            if let Ok(json) = error_msg.to_json() {
                                let _ = ws_sender
                                    .lock()
                                    .await
                                    .send(WsMessage::Text(json.into()))
                                    .await;
                            }
                            continue;
                        }

                        // Write user input to PTY
                        if let Err(e) = pty_writer.write_all(data.payload.as_bytes()).await {
                            error!("Failed to write to PTY: {}", e);
                        }
                    }
                    Ok(Message::Resize(data)) => {
                        // Validate terminal size
                        if let Err(e) = state
                            .validation
                            .validate_terminal_size(data.cols, data.rows)
                        {
                            warn!("Invalid resize request: {}", e);
                            state
                                .audit_logger
                                .log_error(
                                    &remote_addr,
                                    &session_id,
                                    &format!("Invalid resize: {}", e),
                                )
                                .await;

                            let error_msg = Message::Error(ErrorData {
                                code: "INVALID_SIZE".to_string(),
                                message: format!("Invalid terminal size: {}", e),
                                fatal: false,
                            });
                            if let Ok(json) = error_msg.to_json() {
                                let _ = ws_sender
                                    .lock()
                                    .await
                                    .send(WsMessage::Text(json.into()))
                                    .await;
                            }
                            continue;
                        }

                        // Resize PTY
                        let mut pty_guard = pty_session_arc.lock().await;
                        if let Err(e) = pty_guard.resize(data.cols, data.rows) {
                            error!("Failed to resize PTY: {}", e);
                        } else {
                            debug!("PTY resized to {}x{}", data.cols, data.rows);
                        }
                    }
                    Ok(Message::Ping(data)) => {
                        // Respond to ping
                        let pong = Message::Pong(PongData {
                            timestamp: data.timestamp,
                        });
                        if let Ok(json) = pong.to_json() {
                            let _ = ws_sender
                                .lock()
                                .await
                                .send(WsMessage::Text(json.into()))
                                .await;
                        }
                    }
                    Ok(_) => {
                        warn!("Unexpected message type");
                    }
                    Err(e) => {
                        error!("Failed to parse message: {}", e);
                    }
                }
            }
            Ok(WsMessage::Close(_)) => {
                info!("WebSocket close received");
                break;
            }
            Err(e) => {
                error!("WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    // Cleanup
    pty_to_ws.abort();
    subscriber_task.abort();

    // Remove client from session.
    session.remove_client(&client_id).await;

    // For isolated sessions, immediately reclaim resources when the last
    // client disconnects — there is no benefit to keeping the session alive
    // for reconnection since no other client can join.
    // For shared sessions, keep the session alive so clients can reconnect
    // within the reconnection window.
    if session.metadata().mode == SessionMode::Isolated {
        if state.session_manager.remove_if_empty(&session_id).await {
            info!(
                "Client {} removed, isolated session {} cleaned up immediately",
                client_id, session_id
            );
        }
    } else {
        info!(
            "Client {} removed from session {} (session kept alive for reconnection)",
            client_id, session_id
        );
    }

    // Log disconnection
    state
        .audit_logger
        .log_disconnect(&remote_addr, &session_id, "normal closure")
        .await;

    // Send disconnect message
    let disconnect = Message::Disconnect(DisconnectData {
        reason: "Session ended".to_string(),
        code: 0,
    });
    if let Ok(json) = disconnect.to_json() {
        let _ = ws_sender
            .lock()
            .await
            .send(WsMessage::Text(json.into()))
            .await;
    }

    let _ = ws_sender.lock().await.close().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use std::net::{IpAddr, Ipv4Addr};

    fn make_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (k, v) in pairs {
            headers.insert(
                k.parse::<axum::http::header::HeaderName>().unwrap(),
                v.parse().unwrap(),
            );
        }
        headers
    }

    #[test]
    fn test_extract_real_ip_no_proxy() {
        let headers = HeaderMap::new();
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, false), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_trust_disabled_ignores_headers() {
        let headers = make_headers(&[("x-real-ip", "10.0.0.1")]);
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        // trust_proxy = false → header ignored
        assert_eq!(extract_real_ip(&headers, addr, false), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_x_real_ip() {
        let headers = make_headers(&[("x-real-ip", "10.0.0.1")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.1");
    }

    #[test]
    fn test_extract_real_ip_x_forwarded_for() {
        let headers = make_headers(&[("x-forwarded-for", "10.0.0.1, 10.0.0.2, 10.0.0.3")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.1");
    }

    #[test]
    fn test_extract_real_ip_x_real_ip_takes_priority() {
        let headers = make_headers(&[("x-real-ip", "10.0.0.1"), ("x-forwarded-for", "10.0.0.99")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.1");
    }

    #[test]
    fn test_extract_real_ip_fallback_to_connect_addr() {
        let headers = HeaderMap::new();
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        // trust_proxy = true but no headers → fallback
        assert_eq!(extract_real_ip(&headers, addr, true), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_empty_x_real_ip_falls_back() {
        let headers = make_headers(&[("x-real-ip", ""), ("x-forwarded-for", "10.0.0.5")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        // Empty X-Real-IP → try X-Forwarded-For
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.5");
    }

    #[test]
    fn test_extract_real_ip_whitespace_trimmed() {
        let headers = make_headers(&[("x-real-ip", "  10.0.0.1  ")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.1");
    }

    #[test]
    fn test_extract_real_ip_ipv6() {
        let headers = make_headers(&[("x-real-ip", "2001:db8::1")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "2001:db8::1");
    }

    #[test]
    fn test_extract_real_ip_rejects_non_ip_values() {
        // A non-IP string in the header should be rejected
        let headers = make_headers(&[("x-real-ip", "not-an-ip-address")]);
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, true), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_rejects_hostname() {
        // A hostname is not a valid IP
        let headers = make_headers(&[("x-forwarded-for", "attacker.example.com")]);
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, true), "192.168.1.100");
    }
}
