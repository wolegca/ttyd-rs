/// WebSocket handler for terminal connections - Refactored to use SessionManager
use crate::audit::AuditLogger;
use crate::auth::BasicAuth;
use crate::config::Config;
use crate::protocol::*;
use crate::rate_limit::RateLimiter;
use crate::session::{Client, SessionManager};
use crate::validation::ValidationConfig;
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message as WsMessage, WebSocket},
    },
    response::Response,
};
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

/// Shared application state
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub audit_logger: Arc<AuditLogger>,
    pub validation: Arc<ValidationConfig>,
    pub rate_limiter: Arc<RateLimiter>,
    pub session_manager: Arc<SessionManager>,
}

/// WebSocket upgrade handler
pub async fn websocket_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle a WebSocket connection
async fn handle_socket(socket: WebSocket, state: AppState) {
    info!("New WebSocket connection established");

    match handle_terminal_session(socket, state).await {
        Ok(()) => info!("WebSocket connection closed normally"),
        Err(e) => error!("WebSocket error: {}", e),
    }
}

/// Handle a terminal session using SessionManager
async fn handle_terminal_session(
    socket: WebSocket,
    state: AppState,
) -> Result<(), Box<dyn std::error::Error>> {
    let (ws_sender, mut ws_receiver) = socket.split();
    let ws_sender = Arc::new(tokio::sync::Mutex::new(ws_sender));

    let client_id = uuid::Uuid::new_v4().to_string();
    let remote_addr = "unknown"; // Could be extracted from connection info if needed

    // Handle authentication if enabled
    let username = if let Some(auth_config) = &state.config.auth
        && auth_config.method == "basic"
        && let (Some(username), Some(password)) = (&auth_config.username, &auth_config.password)
    {
        // Check rate limit before processing auth
        if let Err(duration) = state.rate_limiter.check(remote_addr).await {
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
                        if let Err(e) = state.validation.validate_auth_method(&auth_data.method) {
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
                                .log_auth_attempt(remote_addr, "unknown", false, &client_id)
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
                                .log_auth_attempt(remote_addr, username, false, &client_id)
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
                            .log_auth_attempt(remote_addr, username, true, &client_id)
                            .await;

                        // Reset rate limit on successful auth
                        state.rate_limiter.reset(remote_addr).await;

                        let ok_msg = Message::AuthOk(AuthOkData {
                            session_id: client_id.clone(),
                            readonly: false,
                        });
                        ws_sender
                            .lock()
                            .await
                            .send(WsMessage::Text(ok_msg.to_json()?.into()))
                            .await?;
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
        Some(username.clone())
    } else {
        None
    };

    // Wait for initial resize message
    let (cols, rows) = match ws_receiver.next().await {
        Some(Ok(WsMessage::Text(text))) => {
            let msg = Message::from_json(&text)?;
            match msg {
                Message::Resize(data) => {
                    // Validate terminal size
                    if let Err(e) = state
                        .validation
                        .validate_terminal_size(data.cols, data.rows)
                    {
                        error!("Invalid terminal size: {}", e);
                        state
                            .audit_logger
                            .log_error(
                                remote_addr,
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
                    debug!("Initial terminal size: {}x{}", data.cols, data.rows);
                    (data.cols, data.rows)
                }
                _ => {
                    warn!("Expected resize message, got other message type");
                    (80, 24) // default size
                }
            }
        }
        _ => (80, 24), // default size
    };

    // Create or get session using SessionManager
    let session_id = uuid::Uuid::new_v4().to_string();
    let session = state
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
            None, // Use default mode from config
        )
        .await
        .map_err(|e| format!("Failed to create session: {}", e))?;

    info!(
        "Session created: id={}, mode={}",
        session_id,
        session.metadata().mode
    );

    // Add this client to the session
    let client = Client {
        client_id: client_id.clone(),
        remote_addr: remote_addr.to_string(),
        username,
        connected_at: Instant::now(),
        readonly: false,
    };

    session
        .add_client(client)
        .await
        .map_err(|e| format!("Failed to add client: {}", e))?;

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
            &session_id,
        )
        .await;

    // Send ready message
    let ready_msg = Message::Ready(ReadyData {
        session_id: session_id.clone(),
        cols,
        rows,
        readonly: false,
    });

    ws_sender
        .lock()
        .await
        .send(WsMessage::Text(ready_msg.to_json()?.into()))
        .await?;

    // Get PTY session for I/O
    let pty_session_arc = session.pty_session();

    // Spawn task to read from PTY and send to WebSocket
    let ws_sender_clone = ws_sender.clone();
    let pty_session_for_read = pty_session_arc.clone();
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

        // Convert OwnedFd to File (transfers ownership, no double-close)
        let pty_file = std::fs::File::from(dup_fd);
        let mut pty_reader = tokio::fs::File::from_std(pty_file);
        let mut buffer = vec![0u8; 4096];

        loop {
            match tokio::io::AsyncReadExt::read(&mut pty_reader, &mut buffer).await {
                Ok(0) => {
                    debug!("PTY EOF");
                    break;
                }
                Ok(n) => {
                    let output = String::from_utf8_lossy(&buffer[..n]).to_string();
                    let msg = Message::Output(OutputData { payload: output });

                    if let Ok(json) = msg.to_json()
                        && ws_sender_clone
                            .lock()
                            .await
                            .send(WsMessage::Text(json.into()))
                            .await
                            .is_ok()
                    {
                        // Message sent successfully
                    } else {
                        error!("Failed to send to WebSocket");
                        break;
                    }
                }
                Err(e) => {
                    error!("Error reading from PTY: {}", e);
                    break;
                }
            }
        }
    });

    // Handle WebSocket messages
    while let Some(msg) = ws_receiver.next().await {
        match msg {
            Ok(WsMessage::Text(text)) => {
                match Message::from_json(&text) {
                    Ok(Message::Input(data)) => {
                        // Validate input payload
                        if let Err(e) = state.validation.validate_input_payload(&data.payload) {
                            warn!("Invalid input payload: {}", e);
                            state
                                .audit_logger
                                .log_error(
                                    remote_addr,
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
                        let pty_session_for_write = pty_session_arc.clone();
                        let payload = data.payload.clone();

                        tokio::spawn(async move {
                            use std::os::fd::BorrowedFd;

                            let pty_guard = pty_session_for_write.lock().await;
                            let master_fd = pty_guard.master_fd();

                            // Duplicate the file descriptor so we have our own independent fd
                            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(master_fd) };
                            let dup_fd = match nix::unistd::dup(borrowed_fd) {
                                Ok(fd) => fd,
                                Err(e) => {
                                    error!("Failed to duplicate PTY fd for write: {}", e);
                                    return;
                                }
                            };

                            drop(pty_guard);

                            // Convert OwnedFd to File (transfers ownership, no double-close)
                            let pty_file = std::fs::File::from(dup_fd);
                            let mut pty_writer = tokio::fs::File::from_std(pty_file);
                            if let Err(e) = pty_writer.write_all(payload.as_bytes()).await {
                                error!("Failed to write to PTY: {}", e);
                            }
                        });
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
                                    remote_addr,
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

    // Remove client from session
    session.remove_client(&client_id).await;

    // Log disconnection
    state
        .audit_logger
        .log_disconnect(remote_addr, &session_id, "normal closure")
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
