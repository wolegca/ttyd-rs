/// Main WebSocket message loop: dispatches Input/Resize/Ping/FileList messages.
use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::StreamExt;
use futures::stream::SplitStream;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

use crate::protocol::*;
use crate::session::Session;

use super::utils::{message_type_name, send_message, send_ws_error};
use super::{AppState, CloseReason, WsSender};

/// Context shared across all message handlers in the main loop.
pub(crate) struct MessageLoopContext<'a> {
    pub state: &'a AppState,
    pub ws_sender: &'a WsSender,
    pub ws_receiver: &'a mut SplitStream<WebSocket>,
    pub heartbeat_task: &'a mut tokio::task::JoinHandle<bool>,
    pub session: &'a Arc<Session>,
    pub pty_session: &'a Arc<tokio::sync::Mutex<crate::pty::PtySession>>,
    pub pty_writer: &'a mut tokio::fs::File,
    pub last_ping_time: &'a Arc<tokio::sync::Mutex<Instant>>,
    pub client_id: &'a str,
    pub session_id: &'a str,
    pub remote_addr: &'a str,
}

/// Run the main WebSocket message loop.
///
/// Returns the reason the connection ended (client disconnect, heartbeat
/// timeout, or shutdown signal).
pub(crate) async fn run(ctx: &mut MessageLoopContext<'_>) -> CloseReason {
    loop {
        let msg = tokio::select! {
            msg = ctx.ws_receiver.next() => msg,
            result = &mut ctx.heartbeat_task => {
                if let Ok(false) = result {
                    warn!("Heartbeat timeout for client {}", ctx.client_id);
                    let _ = send_ws_error(
                        ctx.ws_sender,
                        "HEARTBEAT_TIMEOUT",
                        "No heartbeat received from client".to_string(),
                        true,
                    )
                    .await;
                }
                return CloseReason::HeartbeatTimeout;
            }
            _ = ctx.state.shutdown_token.cancelled() => {
                info!("Shutdown signal received, closing WebSocket connection");
                return CloseReason::Shutdown;
            }
            // Session force-removed (stale cleanup or API delete): stop the
            // loop so the caller's cleanup path disconnects this client and
            // kills the PTY instead of leaking a ghost session.
            _ = ctx.session.cancel_token().cancelled() => {
                info!("Session {} was force-removed", ctx.session_id);
                return CloseReason::SessionClosed;
            }
        };
        let Some(msg) = msg else {
            return CloseReason::ClientClosed;
        };
        match msg {
            Ok(WsMessage::Text(text)) => match Message::from_json(&text) {
                Ok(Message::Input(data)) => {
                    if let Err(reason) = handle_input(ctx, &data).await {
                        return reason;
                    }
                }
                Ok(Message::Resize(data)) => {
                    handle_resize(ctx, &data).await;
                }
                Ok(Message::Ping(data)) => {
                    handle_ping(ctx, &data).await;
                }
                Ok(Message::FileList(data)) => {
                    handle_file_list(ctx, &data).await;
                }
                Ok(other) => {
                    warn!(
                        "Unexpected message type in main loop: {}",
                        message_type_name(&other)
                    );
                }
                Err(e) => {
                    warn!("Failed to parse message: {}", e);
                }
            },
            Ok(WsMessage::Pong(_)) => {
                // Protocol-level pong: the client is alive. Refresh the
                // heartbeat timestamp so the monitor does not time us out.
                *ctx.last_ping_time.lock().await = Instant::now();
            }
            Ok(WsMessage::Close(_)) => {
                info!("WebSocket close received");
                return CloseReason::ClientClosed;
            }
            Err(e) => {
                warn!("WebSocket receive error: {}", e);
                return CloseReason::IoError;
            }
            _ => {}
        }
    }
}

/// Handle an Input message: validate and write to PTY.
///
/// Returns `Err(CloseReason)` when the connection should be terminated
/// immediately (PTY write failure), so the client does not linger on a dead
/// session until the heartbeat times out.
async fn handle_input(
    ctx: &mut MessageLoopContext<'_>,
    data: &InputData,
) -> Result<(), CloseReason> {
    // Check if client can write (read-only enforcement)
    if !ctx.session.can_write(ctx.client_id).await {
        let _ = send_ws_error(
            ctx.ws_sender,
            "READONLY",
            "This session is read-only".to_string(),
            false,
        )
        .await;
        return Ok(());
    }

    // Validate input payload
    if let Err(e) = ctx.state.validation.validate_input_payload(&data.payload) {
        warn!("Invalid input payload: {}", e);
        ctx.state
            .audit_logger
            .log_error(
                ctx.remote_addr,
                ctx.session_id,
                &format!("Invalid input: {}", e),
            )
            .await;

        let _ = send_ws_error(
            ctx.ws_sender,
            "INVALID_INPUT",
            format!("Invalid input: {}", e),
            false,
        )
        .await;
        return Ok(());
    }

    // Write user input to PTY
    if let Err(e) = ctx.pty_writer.write_all(data.payload.as_bytes()).await {
        error!("Failed to write to PTY: {}", e);
        // A write failure almost always means the shell has exited (EIO on
        // the master side). Mark the PTY as exited so subscribers receive
        // their ordered "shell exited" disconnect, and end this connection's
        // loop right away instead of waiting for a heartbeat timeout.
        ctx.session.mark_pty_exited();
        return Err(CloseReason::IoError);
    }
    // Successful interaction refreshes the idle timer so that
    // `cleanup_inactive` never force-removes an actively used session.
    ctx.session.touch().await;
    Ok(())
}

/// Handle a Resize message: validate and resize the PTY.
async fn handle_resize(ctx: &MessageLoopContext<'_>, data: &ResizeData) {
    // Validate terminal size
    if let Err(e) = ctx
        .state
        .validation
        .validate_terminal_size(data.cols, data.rows)
    {
        warn!("Invalid resize request: {}", e);
        ctx.state
            .audit_logger
            .log_error(
                ctx.remote_addr,
                ctx.session_id,
                &format!("Invalid resize: {}", e),
            )
            .await;

        let _ = send_ws_error(
            ctx.ws_sender,
            "INVALID_SIZE",
            format!("Invalid terminal size: {}", e),
            false,
        )
        .await;
        return;
    }

    // Resize PTY
    let mut pty_guard = ctx.pty_session.lock().await;
    if let Err(e) = pty_guard.resize(data.cols, data.rows) {
        error!("Failed to resize PTY: {}", e);
    } else {
        debug!("PTY resized to {}x{}", data.cols, data.rows);
    }
}

/// Handle a Ping message: update last ping time and respond with Pong.
async fn handle_ping(ctx: &MessageLoopContext<'_>, data: &PingData) {
    // Update last ping time to prevent timeout
    *ctx.last_ping_time.lock().await = Instant::now();

    // Respond to ping
    let pong = Message::Pong(PongData {
        timestamp: data.timestamp,
    });
    send_message(ctx.ws_sender, &pong).await;
}

/// Handle a FileList message: list directory contents via the file transfer module.
async fn handle_file_list(ctx: &MessageLoopContext<'_>, data: &FileListData) {
    // Check if file transfer is enabled
    if !ctx.state.config.file_transfer.enabled {
        let _ = send_ws_error(
            ctx.ws_sender,
            "FILE_TRANSFER_DISABLED",
            "File transfer is not enabled".to_string(),
            false,
        )
        .await;
        return;
    }

    // Rate-limit directory listings with the same dedicated limiter the HTTP
    // file endpoints use. Without this, an authenticated client could hammer
    // `file_list` messages (each triggering read_dir + canonicalize) without
    // ever touching the HTTP rate limit.
    if let Err(retry_after) = ctx.state.file_rate_limiter.check(ctx.remote_addr).await {
        let _ = send_ws_error(
            ctx.ws_sender,
            "RATE_LIMITED",
            format!(
                "Too many file listings. Try again in {} seconds",
                retry_after.as_secs()
            ),
            false,
        )
        .await;
        return;
    }

    let file_state = super::super::files::FileTransferState {
        config: Arc::new(ctx.state.config.file_transfer.clone()),
        session_manager: ctx.state.session_manager.clone(),
    };

    match super::super::files::list_directory(
        &file_state,
        Some(ctx.session_id),
        &data.path,
        data.show_hidden,
    )
    .await
    {
        Ok((path, entries)) => {
            let result = Message::FileListResult(FileListResultData {
                path,
                entries: entries
                    .into_iter()
                    .map(|e| FileEntryData {
                        name: e.name,
                        size: e.size,
                        is_dir: e.is_dir,
                        modified: e.modified,
                    })
                    .collect(),
            });
            send_message(ctx.ws_sender, &result).await;
        }
        Err((_status, msg)) => {
            let _ = send_ws_error(ctx.ws_sender, "FILE_LIST_ERROR", msg, false).await;
        }
    }
}
