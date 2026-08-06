/// Handshake message processing: parsing Resize/Join messages before the main I/O loop.
use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::StreamExt;
use futures::stream::SplitStream;
use std::time::Duration;
use tracing::warn;

use crate::protocol::Message;

use super::utils::send_ws_error;
use super::{AppState, WsSender};

/// Result of the initial handshake phase.
pub(crate) struct Handshake {
    pub cols: u16,
    pub rows: u16,
    pub join_session_id: Option<String>,
}

/// Validate a terminal size, logging errors and sending `INVALID_SIZE`
/// to the client.  Returns `Ok((cols, rows))` or `Err(())` on failure.
async fn validate_size(
    state: &AppState,
    ws_sender: &WsSender,
    remote_addr: &str,
    client_id: &str,
    cols: u16,
    rows: u16,
) -> Result<(u16, u16), ()> {
    if let Err(e) = state.validation.validate_terminal_size(cols, rows) {
        warn!("Invalid terminal size: {}", e);
        state
            .audit_logger
            .log_error(
                remote_addr,
                client_id,
                &format!("Invalid terminal size: {}", e),
            )
            .await;
        let _ = send_ws_error(
            ws_sender,
            "INVALID_SIZE",
            format!("Invalid terminal size: {}", e),
            true,
        )
        .await;
        return Err(());
    }
    Ok((cols, rows))
}

/// Process a handshake message (Resize or Join), updating state accordingly.
///
/// Returns `Err(())` if a Resize message has an invalid size (caller should
/// close the connection).
async fn process_handshake_message(
    state: &AppState,
    ws_sender: &WsSender,
    remote_addr: &str,
    client_id: &str,
    msg: Message,
    cols: &mut u16,
    rows: &mut u16,
    resize_received: &mut bool,
    join_session_id: &mut Option<String>,
) -> Result<(), ()> {
    match msg {
        Message::Resize(data) => {
            let (c, r) = validate_size(
                state,
                ws_sender,
                remote_addr,
                client_id,
                data.cols,
                data.rows,
            )
            .await?;
            *cols = c;
            *rows = r;
            *resize_received = true;
        }
        Message::Join(data) => {
            *join_session_id = Some(data.session_id);
        }
        _ => {
            warn!("Expected resize or join, got other message type");
        }
    }
    Ok(())
}

/// Read initial handshake messages: Resize (required) and optionally Join.
///
/// The client may send them in either order, but we must not consume
/// messages that belong to the main I/O loop (Input, Ping, etc.).
pub(crate) async fn read_handshake(
    state: &AppState,
    ws_sender: &WsSender,
    ws_receiver: &mut SplitStream<WebSocket>,
    remote_addr: &str,
    client_id: &str,
) -> Result<Handshake, ()> {
    let mut cols: u16 = 80;
    let mut rows: u16 = 24;
    let mut join_session_id: Option<String> = None;
    let mut resize_received = false;

    // Read first message
    match ws_receiver.next().await {
        Some(Ok(WsMessage::Text(text))) => {
            let msg = Message::from_json(&text).map_err(|_| ())?;
            process_handshake_message(
                state,
                ws_sender,
                remote_addr,
                client_id,
                msg,
                &mut cols,
                &mut rows,
                &mut resize_received,
                &mut join_session_id,
            )
            .await?;
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
        let (c, r) = validate_size(
            state,
            ws_sender,
            remote_addr,
            client_id,
            data.cols,
            data.rows,
        )
        .await?;
        cols = c;
        rows = r;
    }

    // If we got Resize first, the client may have sent a Join message right
    // after it (e.g. on reconnect). Try to consume it with a short timeout so
    // it doesn't leak into the main I/O loop.
    if resize_received
        && join_session_id.is_none()
        && let Ok(Some(Ok(WsMessage::Text(text)))) =
            tokio::time::timeout(Duration::from_millis(100), ws_receiver.next()).await
        && let Ok(Message::Join(data)) = Message::from_json(&text)
    {
        join_session_id = Some(data.session_id);
    }

    Ok(Handshake {
        cols,
        rows,
        join_session_id,
    })
}
