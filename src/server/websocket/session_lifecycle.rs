/// Session lifecycle management: create/join sessions, add clients, and clean up.
use std::sync::Arc;
use std::time::Instant;

use tracing::{info, warn};

use crate::session::{Client, Session, SessionMode};

use super::AppState;
use super::WsSender;
use super::utils::send_ws_error;

/// The resolved session for a new connection.
pub(crate) struct ResolvedSession {
    pub session: Arc<Session>,
    pub session_id: String,
    pub is_readonly: bool,
}

/// Create or join a session based on the handshake result.
///
/// If `join_session_id` is provided, attempts to join an existing session.
/// If the session doesn't exist, creates a new one (graceful reconnection).
/// If no `join_session_id` is provided, always creates a new session.
pub(crate) async fn create_or_join_session(
    state: &AppState,
    ws_sender: &WsSender,
    join_session_id: Option<String>,
    cols: u16,
    rows: u16,
) -> Result<ResolvedSession, ()> {
    if let Some(target_id) = join_session_id {
        // Try to join an existing session
        match state.session_manager.get_session(&target_id).await {
            Some(existing_session) => {
                let mode = existing_session.metadata().mode;
                if mode == SessionMode::Isolated {
                    let _ = send_ws_error(
                        ws_sender,
                        "CANNOT_JOIN",
                        "Cannot join an isolated session".to_string(),
                        true,
                    )
                    .await;
                    return Err(());
                }
                let readonly = mode == SessionMode::SharedReadOnly;
                info!(
                    "Client joining session {} (mode={}, readonly={})",
                    target_id, mode, readonly
                );
                return Ok(ResolvedSession {
                    session: existing_session,
                    session_id: target_id,
                    is_readonly: readonly,
                });
            }
            None => {
                // Session expired or not found — create a new one instead of erroring.
                info!(
                    "Session '{}' not found, creating new session for rejoining client",
                    target_id
                );
            }
        }
    }

    // Create a new session (either no Join was received, or the target was not found)
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
        .await
        .map_err(|e| {
            warn!("Failed to create session: {}", e);
        })?;

    info!("Session created: id={}", session_id);
    Ok(ResolvedSession {
        session: new_session,
        session_id,
        is_readonly: false,
    })
}

/// Add a client to a session.
pub(crate) async fn add_client(
    session: &Arc<Session>,
    client_id: &str,
    remote_addr: &str,
    username: Option<String>,
    is_readonly: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = Client {
        client_id: client_id.to_string(),
        remote_addr: remote_addr.to_string(),
        username,
        connected_at: Instant::now(),
        readonly: is_readonly,
    };
    session.add_client(client).await?;
    Ok(())
}

/// Remove a client and clean up the session if appropriate.
///
/// For isolated sessions, immediately reclaims resources when the last client
/// disconnects. For shared sessions, keeps the session alive for reconnection.
pub(crate) async fn cleanup_client(
    state: &AppState,
    session: &Arc<Session>,
    session_id: &str,
    client_id: &str,
) {
    session.remove_client(client_id).await;

    if session.metadata().mode == SessionMode::Isolated {
        if state.session_manager.remove_if_empty(session_id).await {
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
}
