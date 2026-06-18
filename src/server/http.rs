/// HTTP server implementation using axum
use crate::assets::Assets;
use crate::audit::AuditLogger;
use crate::config::Config;
use crate::rate_limit::RateLimiter;
use crate::server::api::ApiState;
use crate::server::websocket::AppState;
use crate::session::{SessionManager, SessionMode};
use crate::validation::ValidationConfig;
use axum::{
    Router,
    body::Body,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

/// Start the HTTP/WebSocket server
pub async fn start_server(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let audit_logger = AuditLogger::new(config.audit.log_file.clone(), config.audit.enabled);
    let validation = ValidationConfig::from_config(&config);
    let rate_limiter = RateLimiter::new(
        config.rate_limit.max_requests,
        config.rate_limit.window_seconds,
    );

    // Parse session mode
    let session_mode: SessionMode = config
        .session
        .mode
        .parse()
        .map_err(|e| format!("Invalid session mode: {}", e))?;

    // Create session manager
    let session_manager = Arc::new(SessionManager::new(
        Duration::from_secs(config.session.timeout),
        session_mode,
    ));

    let app_state = AppState {
        config: Arc::new(config.clone()),
        audit_logger: Arc::new(audit_logger),
        validation: Arc::new(validation),
        rate_limiter: Arc::new(rate_limiter.clone()),
        session_manager: session_manager.clone(),
    };

    let api_state = ApiState {
        session_manager: session_manager.clone(),
    };

    // Spawn cleanup task for rate limiter
    let cleanup_limiter = rate_limiter.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300)); // Cleanup every 5 minutes
        loop {
            interval.tick().await;
            cleanup_limiter.cleanup().await;
        }
    });

    // Spawn cleanup task for sessions
    let cleanup_manager = session_manager.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60)); // Check every minute
        loop {
            interval.tick().await;
            let cleaned = cleanup_manager.cleanup_inactive().await;
            if cleaned > 0 {
                info!("Cleaned up {} inactive sessions", cleaned);
            }
        }
    });

    let app = create_router(&config, app_state, api_state)?;
    let addr = config.bind;

    info!("Starting server on {}", addr);
    info!("WebSocket endpoint: ws://{}/ws", addr);
    info!(
        "Authentication: {}",
        if config.auth.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );
    info!(
        "Audit logging: {}",
        if config.audit.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    info!(
        "Rate limiting: enabled ({} requests per {} seconds)",
        config.rate_limit.max_requests, config.rate_limit.window_seconds
    );
    info!("Session mode: {}", config.session.mode);
    info!("Session timeout: {}s", config.session.timeout);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Create the axum router with all routes
fn create_router(
    _config: &Config,
    app_state: AppState,
    api_state: ApiState,
) -> Result<Router, Box<dyn std::error::Error>> {
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(super::websocket::websocket_handler))
        // API routes
        .route("/api/health", get(super::api::health_check))
        .route(
            "/api/sessions",
            get(super::api::list_sessions).with_state(api_state.clone()),
        )
        .route(
            "/api/sessions/{id}",
            get(super::api::get_session)
                .delete(super::api::delete_session)
                .with_state(api_state.clone()),
        )
        .route(
            "/api/stats",
            get(super::api::get_stats).with_state(api_state),
        )
        .fallback(static_handler)
        .with_state(app_state);

    Ok(app)
}

/// Handler for the index page
async fn index_handler() -> impl IntoResponse {
    static_handler(Uri::from_static("/index.html")).await
}

/// Handler for embedded static files
async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');

    // Default to index.html for root
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(content.data))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("404 Not Found"))
            .unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_router_creation() {
        use crate::audit::AuditLogger;
        use crate::rate_limit::RateLimiter;
        use crate::session::SessionManager;
        use crate::validation::ValidationConfig;

        let config = Config::default();
        let audit_logger = AuditLogger::new(None, false);
        let validation = ValidationConfig::default();
        let rate_limiter = RateLimiter::default();
        let session_manager = Arc::new(SessionManager::new(
            Duration::from_secs(3600),
            SessionMode::Isolated,
        ));

        let app_state = AppState {
            config: Arc::new(config.clone()),
            audit_logger: Arc::new(audit_logger),
            validation: Arc::new(validation),
            rate_limiter: Arc::new(rate_limiter),
            session_manager: session_manager.clone(),
        };

        let api_state = ApiState { session_manager };

        let result = create_router(&config, app_state, api_state);
        assert!(result.is_ok());
    }
}
