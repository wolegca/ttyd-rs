/// REST API endpoints for session management
use crate::config::Config;
use crate::session::SessionManager;
use axum::{
    Json,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use serde::Serialize;
use std::sync::Arc;

/// Shared API state
#[derive(Clone)]
pub struct ApiState {
    pub session_manager: Arc<SessionManager>,
    pub config: Arc<Config>,
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
    /// Unix timestamp (seconds since epoch) when the session was created.
    pub created_at: u64,
    /// Unix timestamp (seconds since epoch) of the last activity.
    pub last_activity: u64,
    pub terminal: TerminalInfo,
}

#[derive(Debug, Serialize)]
pub struct TerminalInfo {
    pub cols: u16,
    pub rows: u16,
    // NOTE: the child PID is intentionally NOT exposed — it leaks host
    // process information to any authenticated API caller.
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

        session_infos.push(SessionInfo {
            session_id: metadata.session_id.clone(),
            mode: metadata.mode.to_string(),
            clients: client_count,
            created_at: system_time_to_unix(metadata.created_at),
            last_activity: instant_to_unix(last_activity),
            terminal: TerminalInfo { cols, rows },
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

    Ok(Json(SessionInfo {
        session_id: metadata.session_id.clone(),
        mode: metadata.mode.to_string(),
        clients: client_count,
        created_at: system_time_to_unix(metadata.created_at),
        last_activity: instant_to_unix(last_activity),
        terminal: TerminalInfo { cols, rows },
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

/// Client config response
#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub auth_method: Option<String>,
    /// Exposed only when auth is not configured (unauthenticated deployments).
    /// Authenticated clients learn about file-transfer capabilities via the
    /// `auth_ok` WebSocket message instead, so there is nothing to leak here
    /// before they log in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_upload_size: Option<usize>,
    pub file_transfer_enabled: bool,
}

/// Get client-facing configuration.
///
/// Always exposes `auth_method` so the frontend can decide whether to show
/// the login overlay. When auth is disabled, `file_transfer_enabled` and
/// `max_upload_size` are also exposed so the frontend can initialise its
/// file panel in one round-trip. When auth is enabled those fields are
/// omitted — the client learns about them from the `auth_ok` WebSocket
/// message after a successful login, which avoids leaking deployment
/// details to unauthenticated callers.
pub async fn get_config(State(state): State<ApiState>) -> Json<ConfigResponse> {
    let expose_details = state.config.auth.is_none();
    Json(ConfigResponse {
        auth_method: state.config.auth.as_ref().map(|a| a.method.clone()),
        max_upload_size: if expose_details && state.config.file_transfer.enabled {
            Some(state.config.file_transfer.max_upload_size)
        } else {
            None
        },
        file_transfer_enabled: expose_details && state.config.file_transfer.enabled,
    })
}

/// Convert a `SystemTime` to a Unix timestamp (seconds since epoch).
///
/// Returns 0 on the rare case `duration_since(UNIX_EPOCH)` fails (clock
/// before epoch), which is preferable to panicking or returning garbage.
fn system_time_to_unix(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Convert an `Instant` (last-activity) to an approximate Unix timestamp.
///
/// `Instant` has no wall-clock anchor, so we derive one by subtracting how
/// long ago the instant was from the current `SystemTime`. The result is
/// approximate (millisecond-level drift is fine for "last seen" display).
fn instant_to_unix(instant: std::time::Instant) -> u64 {
    let elapsed = instant.elapsed();
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|now_secs| now_secs.saturating_sub(elapsed).as_secs())
        .unwrap_or(0)
}

/// Pre-built authenticator for the API auth middleware.
///
/// Holds an `Arc<dyn Authenticator>` built once at router startup so that
/// Argon2 password hashing (~100 ms) never runs per request. The concrete
/// type (`BasicAuth` or `TokenAuth`) is behind the trait so this module has
/// no direct dependency on the `websocket::auth::AuthMethod` enum.
#[derive(Clone)]
pub(crate) struct ApiAuthState {
    /// Pre-built authenticator; `None` means auth is misconfigured — deny all.
    pub auth: Option<Arc<dyn crate::auth::Authenticator>>,
}

/// Middleware: validate Authorization header against configured credentials.
pub(crate) async fn api_auth_middleware(
    State(auth_state): State<ApiAuthState>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let authorized = auth_header.is_some_and(|header| {
        auth_state
            .auth
            .as_ref()
            .is_some_and(|auth| auth.validate_header(header))
    });

    if authorized {
        Ok(next.run(request).await)
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Unauthorized".to_string(),
            }),
        ))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::session::SessionManager;
    use std::sync::Arc;
    use std::time::Duration;

    fn test_api_state() -> (ApiState, Arc<SessionManager>) {
        let config = Config::default();
        let session_manager = Arc::new(SessionManager::new(
            Duration::from_secs(3600),
            crate::session::SessionMode::Isolated,
        ));
        let api_state = ApiState {
            session_manager: session_manager.clone(),
            config: Arc::new(config),
        };
        (api_state, session_manager)
    }

    #[test]
    fn test_system_time_to_unix() {
        let t = std::time::SystemTime::now();
        let unix = system_time_to_unix(t);
        // Should be within a few seconds of now
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(now_secs.abs_diff(unix) < 5);
    }

    #[test]
    fn test_instant_to_unix() {
        let t = std::time::Instant::now();
        let unix = instant_to_unix(t);
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(now_secs.abs_diff(unix) < 5);
    }

    #[tokio::test]
    async fn test_health_check() {
        let response = health_check().await;
        assert_eq!(response.status, "ok");
        assert!(!response.version.is_empty());
    }

    #[tokio::test]
    async fn test_get_config_no_auth() {
        let (api_state, _) = test_api_state();
        let Json(config) = get_config(axum::extract::State(api_state)).await;
        assert!(config.auth_method.is_none());
    }

    #[tokio::test]
    async fn test_get_config_with_auth() {
        let cfg = Config {
            auth: Some(crate::config::AuthConfig {
                method: "basic".to_string(),
                username: Some("admin".to_string()),
                password: Some("secret".to_string()),
                token: None,
            }),
            ..Default::default()
        };
        let session_manager = Arc::new(SessionManager::new(
            Duration::from_secs(3600),
            crate::session::SessionMode::Isolated,
        ));
        let api_state = ApiState {
            session_manager,
            config: Arc::new(cfg),
        };
        let Json(config) = get_config(axum::extract::State(api_state)).await;
        assert_eq!(config.auth_method.unwrap(), "basic");
    }

    #[tokio::test]
    async fn test_list_sessions_empty() {
        let (api_state, _) = test_api_state();
        let result = list_sessions(axum::extract::State(api_state)).await;
        assert!(result.is_ok());
        let Json(resp) = result.unwrap();
        assert_eq!(resp.total, 0);
        assert!(resp.sessions.is_empty());
    }

    #[tokio::test]
    async fn test_get_session_not_found() {
        let (api_state, _) = test_api_state();
        let result = get_session(
            axum::extract::State(api_state),
            axum::extract::Path("nonexistent".to_string()),
        )
        .await;
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_session_not_found() {
        let (api_state, _) = test_api_state();
        let result = delete_session(
            axum::extract::State(api_state),
            axum::extract::Path("nonexistent".to_string()),
        )
        .await;
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_stats_empty() {
        let (api_state, _) = test_api_state();
        let result = get_stats(axum::extract::State(api_state)).await;
        assert!(result.is_ok());
        let Json(stats) = result.unwrap();
        assert_eq!(stats.total_sessions, 0);
        assert_eq!(stats.isolated_sessions, 0);
        assert_eq!(stats.shared_sessions, 0);
        assert_eq!(stats.total_clients, 0);
    }

    #[tokio::test]
    async fn test_list_sessions_after_create() {
        let (api_state, sm) = test_api_state();
        sm.create_session(
            "test-s1".to_string(),
            &["true".to_string()],
            None,
            80,
            24,
            None,
        )
        .await
        .unwrap();

        let Json(resp) = list_sessions(axum::extract::State(api_state))
            .await
            .unwrap();
        assert_eq!(resp.total, 1);
        assert_eq!(resp.sessions[0].session_id, "test-s1");
        assert_eq!(resp.sessions[0].terminal.cols, 80);
        assert_eq!(resp.sessions[0].terminal.rows, 24);
    }

    #[tokio::test]
    async fn test_get_session_found() {
        let (api_state, sm) = test_api_state();
        sm.create_session(
            "test-s2".to_string(),
            &["true".to_string()],
            None,
            120,
            40,
            None,
        )
        .await
        .unwrap();

        let result = get_session(
            axum::extract::State(api_state),
            axum::extract::Path("test-s2".to_string()),
        )
        .await;
        assert!(result.is_ok());
        let Json(info) = result.unwrap();
        assert_eq!(info.session_id, "test-s2");
        assert_eq!(info.terminal.cols, 120);
        assert_eq!(info.terminal.rows, 40);
    }

    #[tokio::test]
    async fn test_delete_session_found() {
        let (api_state, sm) = test_api_state();
        sm.create_session(
            "test-s3".to_string(),
            &["true".to_string()],
            None,
            80,
            24,
            None,
        )
        .await
        .unwrap();

        let result = delete_session(
            axum::extract::State(api_state),
            axum::extract::Path("test-s3".to_string()),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn test_get_stats_after_create() {
        let (api_state, sm) = test_api_state();
        sm.create_session(
            "iso1".to_string(),
            &["true".to_string()],
            None,
            80,
            24,
            Some(crate::session::SessionMode::Isolated),
        )
        .await
        .unwrap();
        sm.create_session(
            "shared1".to_string(),
            &["true".to_string()],
            None,
            80,
            24,
            Some(crate::session::SessionMode::SharedReadWrite),
        )
        .await
        .unwrap();

        let result = get_stats(axum::extract::State(api_state)).await;
        assert!(result.is_ok());
        let Json(stats) = result.unwrap();
        assert_eq!(stats.total_sessions, 2);
        assert_eq!(stats.isolated_sessions, 1);
        assert_eq!(stats.shared_sessions, 1);
    }
}
