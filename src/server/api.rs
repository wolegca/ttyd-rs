/// REST API endpoints for session management
use crate::config::{AuthConfig, Config};
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

/// Client config response
#[derive(Debug, Serialize)]
pub struct ConfigResponse {
    pub auth_method: Option<String>,
}

/// Get client-facing configuration
pub async fn get_config(State(state): State<ApiState>) -> Json<ConfigResponse> {
    Json(ConfigResponse {
        auth_method: state.config.auth.as_ref().map(|a| a.method.clone()),
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

/// State for the API auth middleware
#[derive(Clone)]
pub(crate) struct ApiAuthState {
    pub auth_config: AuthConfig,
}

/// Middleware: validate Authorization header against configured credentials.
///
/// Supports:
/// - Basic auth: `Authorization: Basic <base64(user:pass)>`
/// - Token auth: `Authorization: Bearer <token>`
pub(crate) async fn api_auth_middleware(
    State(auth_state): State<ApiAuthState>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let authorized = match auth_header {
        Some(header) => match auth_state.auth_config.method.as_str() {
            "basic" => {
                let credentials = header.strip_prefix("Basic ").map(String::from);
                match (
                    credentials,
                    &auth_state.auth_config.username,
                    &auth_state.auth_config.password,
                ) {
                    (Some(creds), Some(username), Some(password)) => {
                        let authenticator =
                            crate::auth::BasicAuth::new(username.clone(), password.clone());
                        authenticator.validate(&creds)
                    }
                    _ => false,
                }
            }
            "token" => {
                let credentials = header.strip_prefix("Bearer ").map(String::from);
                match (credentials, &auth_state.auth_config.token) {
                    (Some(creds), Some(token)) => {
                        let authenticator = crate::auth::TokenAuth::new(token.clone());
                        authenticator.validate(&creds)
                    }
                    _ => false,
                }
            }
            _ => false,
        },
        None => false,
    };

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
    fn test_format_instant() {
        let now = std::time::Instant::now();
        let result = format_instant(now);
        assert!(result.ends_with("ago"));
    }

    #[test]
    fn test_format_instant_seconds() {
        let past = std::time::Instant::now() - Duration::from_secs(30);
        let result = format_instant(past);
        assert!(result.ends_with("s ago"));
    }

    #[test]
    fn test_format_instant_minutes() {
        let past = std::time::Instant::now() - Duration::from_secs(120);
        let result = format_instant(past);
        assert!(result.ends_with("m ago"));
    }

    #[test]
    fn test_format_instant_hours() {
        let past = std::time::Instant::now() - Duration::from_secs(7200);
        let result = format_instant(past);
        assert!(result.ends_with("h ago"));
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
        let mut cfg = Config::default();
        cfg.auth = Some(crate::config::AuthConfig {
            method: "basic".to_string(),
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
            token: None,
            audit_enabled: false,
        });
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
