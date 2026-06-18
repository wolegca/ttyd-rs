/// REST API endpoints for session management
use crate::session::SessionManager;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Serialize;
use std::sync::Arc;

/// Shared API state
#[derive(Clone)]
pub struct ApiState {
    pub session_manager: Arc<SessionManager>,
}

/// Response for session list
#[derive(Debug, Serialize)]
pub struct SessionListResponse {
    pub sessions: Vec<SessionInfo>,
    pub total: usize,
}

/// Information about a session
#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub mode: String,
    pub clients: usize,
    pub created_at: String,
    pub last_activity: String,
    pub terminal: TerminalInfo,
}

#[derive(Debug, Serialize)]
pub struct TerminalInfo {
    pub cols: u16,
    pub rows: u16,
    pub pid: i32,
}

/// Statistics response
#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub total_sessions: usize,
    pub isolated_sessions: usize,
    pub shared_sessions: usize,
    pub total_clients: usize,
}

/// Health check response
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Error response
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// List all active sessions
pub async fn list_sessions(
    State(state): State<ApiState>,
) -> Result<Json<SessionListResponse>, (StatusCode, Json<ErrorResponse>)> {
    let sessions = state.session_manager.list_sessions().await;
    let mut session_infos = Vec::new();

    for session in sessions {
        let metadata = session.metadata();
        let client_count = session.client_count().await;
        let last_activity = session.last_activity().await;

        // Get terminal info
        let pty_session = session.pty_session();
        let pty = pty_session.lock().await;
        let (cols, rows) = pty.dimensions();
        let pid = pty.child_pid();

        session_infos.push(SessionInfo {
            session_id: metadata.session_id.clone(),
            mode: metadata.mode.to_string(),
            clients: client_count,
            created_at: format_instant(metadata.created_at),
            last_activity: format_instant(last_activity),
            terminal: TerminalInfo {
                cols,
                rows,
                pid: pid.as_raw(),
            },
        });
    }

    let total = session_infos.len();

    Ok(Json(SessionListResponse {
        sessions: session_infos,
        total,
    }))
}

/// Get information about a specific session
pub async fn get_session(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionInfo>, (StatusCode, Json<ErrorResponse>)> {
    let session = state
        .session_manager
        .get_session(&session_id)
        .await
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Session not found: {}", session_id),
                }),
            )
        })?;

    let metadata = session.metadata();
    let client_count = session.client_count().await;
    let last_activity = session.last_activity().await;

    // Get terminal info
    let pty_session = session.pty_session();
    let pty = pty_session.lock().await;
    let (cols, rows) = pty.dimensions();
    let pid = pty.child_pid();

    Ok(Json(SessionInfo {
        session_id: metadata.session_id.clone(),
        mode: metadata.mode.to_string(),
        clients: client_count,
        created_at: format_instant(metadata.created_at),
        last_activity: format_instant(last_activity),
        terminal: TerminalInfo {
            cols,
            rows,
            pid: pid.as_raw(),
        },
    }))
}

/// Delete/terminate a session
pub async fn delete_session(
    State(state): State<ApiState>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let removed = state.session_manager.remove_session(&session_id).await;

    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: format!("Session not found: {}", session_id),
            }),
        ))
    }
}

/// Get server statistics
pub async fn get_stats(
    State(state): State<ApiState>,
) -> Result<Json<StatsResponse>, (StatusCode, Json<ErrorResponse>)> {
    let stats = state.session_manager.stats().await;

    Ok(Json(StatsResponse {
        total_sessions: stats.total_sessions,
        isolated_sessions: stats.isolated_sessions,
        shared_sessions: stats.shared_sessions,
        total_clients: stats.total_clients,
    }))
}

/// Health check endpoint
pub async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Format Instant as ISO 8601 string (relative to now)
fn format_instant(instant: std::time::Instant) -> String {
    let now = std::time::Instant::now();
    let duration = if now > instant {
        now.duration_since(instant)
    } else {
        std::time::Duration::from_secs(0)
    };

    let secs = duration.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_instant() {
        let now = std::time::Instant::now();
        let result = format_instant(now);
        assert!(result.ends_with("ago"));
    }
}
