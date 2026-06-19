/// HTTP server implementation using axum
use crate::assets::Assets;
use crate::audit::AuditLogger;
use crate::config::Config;
use crate::rate_limit::RateLimiter;
use crate::server::api::ApiState;
use crate::server::websocket::AppState;
use crate::session::{SessionManager, SessionMode};
use axum::{
    Router,
    body::Body,
    http::{StatusCode, Uri, header},
    middleware,
    response::{IntoResponse, Response},
    routing::get,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// Start the HTTP/WebSocket server
pub async fn start_server(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let audit_logger = AuditLogger::new(config.audit.log_file.clone(), config.audit.enabled);
    let validation = config.validation.clone();
    let rate_limiter = RateLimiter::new(
        config.rate_limit.max_requests,
        config.rate_limit.window_seconds,
    );

    // Parse session mode
    let session_mode: SessionMode = config.session.mode.parse()?;

    // Create session manager
    let session_manager = Arc::new(
        SessionManager::new(Duration::from_secs(config.session.timeout), session_mode)
            .with_reconnect_window(Duration::from_secs(config.session.reconnect_window)),
    );

    let shutdown_token = CancellationToken::new();

    let app_state = AppState {
        config: Arc::new(config.clone()),
        audit_logger: Arc::new(audit_logger),
        validation: Arc::new(validation),
        rate_limiter: Arc::new(rate_limiter.clone()),
        session_manager: session_manager.clone(),
        shutdown_token: shutdown_token.clone(),
        active_connections: Arc::new(AtomicUsize::new(0)),
    };

    let api_state = ApiState {
        session_manager: session_manager.clone(),
        config: Arc::new(config.clone()),
    };

    // Spawn cleanup task for rate limiter
    let cleanup_limiter = rate_limiter.clone();
    let limiter_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300)); // Cleanup every 5 minutes
        loop {
            interval.tick().await;
            cleanup_limiter.cleanup().await;
        }
    });

    // Spawn cleanup task for sessions
    let cleanup_manager = session_manager.clone();
    let session_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            let cleaned = cleanup_manager.cleanup_inactive().await;
            if cleaned > 0 {
                info!("Cleaned up {} inactive sessions", cleaned);
            }
        }
    });

    let app = create_router(&config, app_state, api_state);
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

    // Spawn task to cancel token when shutdown signal is received.
    // This must happen before with_graceful_shutdown so that WebSocket handlers
    // can break out of their message loops and complete.
    let token_for_signal = shutdown_token.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        token_for_signal.cancel();
    });

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { shutdown_token.cancelled().await })
    .await?;

    info!("Server stopped, cleaning up sessions...");
    session_manager.shutdown().await;

    // Abort background tasks so the tokio runtime can shut down cleanly
    limiter_task.abort();
    session_task.abort();

    info!("Shutdown complete");

    Ok(())
}

/// Wait for a shutdown signal (SIGINT or SIGTERM)
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!("Failed to listen for Ctrl+C: {}", e);
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(e) => {
                error!("Failed to listen for SIGTERM: {}", e);
                // Block forever so this branch never resolves
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("Received SIGINT (Ctrl+C), shutting down..."),
        _ = terminate => info!("Received SIGTERM, shutting down..."),
    }
}

/// Create the axum router with all routes
fn create_router(config: &Config, app_state: AppState, api_state: ApiState) -> Router {
    // Public API routes (no auth required)
    let public_api = Router::new()
        .route("/api/health", get(super::api::health_check))
        .route(
            "/api/config",
            get(super::api::get_config).with_state(api_state.clone()),
        );

    // Protected API routes (auth required when configured)
    let protected_api = Router::new()
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
        );

    // Apply auth middleware to protected routes when auth is configured
    let protected_api = if let Some(ref auth_config) = config.auth {
        let auth_state = super::api::ApiAuthState {
            auth_config: auth_config.clone(),
        };
        protected_api.layer(middleware::from_fn_with_state(
            auth_state,
            super::api::api_auth_middleware,
        ))
    } else {
        protected_api
    };

    Router::new()
        .route("/", get(index_handler))
        .route("/ws", get(super::websocket::websocket_handler))
        .merge(public_api)
        .merge(protected_api)
        .fallback(static_handler)
        .with_state(app_state)
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
                .ok()
                .unwrap_or_else(|| {
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::empty())
                        .unwrap_or_default()
                })
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("404 Not Found"))
            .ok()
            .unwrap_or_else(|| {
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::empty())
                    .unwrap_or_default()
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditLogger;
    use crate::config::ValidationConfig;
    use crate::rate_limit::RateLimiter;
    use crate::session::SessionManager;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Build test AppState and ApiState with default config
    fn test_state() -> (AppState, ApiState) {
        let config = Config::default();
        let session_manager = Arc::new(SessionManager::new(
            Duration::from_secs(3600),
            SessionMode::Isolated,
        ));
        let config_arc = Arc::new(config);

        let app_state = AppState {
            config: config_arc.clone(),
            audit_logger: Arc::new(AuditLogger::new(None, false)),
            validation: Arc::new(ValidationConfig::default()),
            rate_limiter: Arc::new(RateLimiter::default()),
            session_manager: session_manager.clone(),
            shutdown_token: CancellationToken::new(),
            active_connections: Arc::new(AtomicUsize::new(0)),
        };

        let api_state = ApiState {
            session_manager,
            config: config_arc,
        };
        (app_state, api_state)
    }

    #[test]
    fn test_router_creation() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let _app = create_router(&config, app_state, api_state);
    }

    // ── HTTP API integration tests ──────────────────────────────────

    #[tokio::test]
    async fn test_api_health_check() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert!(json["version"].is_string());
    }

    #[tokio::test]
    async fn test_api_list_sessions_empty() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/sessions")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total"], 0);
        assert!(json["sessions"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_api_get_session_not_found() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/sessions/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_api_delete_session_not_found() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/sessions/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_api_stats() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/stats")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total_sessions"], 0);
        assert_eq!(json["total_clients"], 0);
    }

    #[tokio::test]
    async fn test_no_cors_headers_by_default() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .method("OPTIONS")
            .uri("/api/health")
            .header("origin", "https://example.com")
            .header("access-control-request-method", "GET")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        // Without a CORS layer, no access-control-allow-origin header is set
        assert!(!resp.headers().contains_key("access-control-allow-origin"));
    }

    #[tokio::test]
    async fn test_api_config_no_auth() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["auth_method"].is_null());
    }

    #[tokio::test]
    async fn test_api_config_with_auth() {
        let mut config = Config::default();
        config.auth = Some(crate::config::AuthConfig {
            method: "basic".to_string(),
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
            token: None,
            audit_enabled: false,
        });
        let session_manager = Arc::new(SessionManager::new(
            Duration::from_secs(3600),
            SessionMode::Isolated,
        ));
        let config_arc = Arc::new(config.clone());
        let app_state = AppState {
            config: config_arc.clone(),
            audit_logger: Arc::new(AuditLogger::new(None, false)),
            validation: Arc::new(ValidationConfig::default()),
            rate_limiter: Arc::new(RateLimiter::default()),
            session_manager: session_manager.clone(),
            shutdown_token: CancellationToken::new(),
            active_connections: Arc::new(AtomicUsize::new(0)),
        };
        let api_state = ApiState {
            session_manager,
            config: config_arc,
        };
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["auth_method"], "basic");
    }

    #[tokio::test]
    async fn test_static_not_found() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/nonexistent/file.txt")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── API auth middleware tests ───────────────────────────────────

    /// Build test AppState and ApiState with basic auth configured
    fn test_state_with_basic_auth() -> (Config, AppState, ApiState) {
        let mut config = Config::default();
        config.auth = Some(crate::config::AuthConfig {
            method: "basic".to_string(),
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
            token: None,
            audit_enabled: false,
        });
        let session_manager = Arc::new(SessionManager::new(
            Duration::from_secs(3600),
            SessionMode::Isolated,
        ));
        let config_arc = Arc::new(config.clone());

        let app_state = AppState {
            config: config_arc.clone(),
            audit_logger: Arc::new(AuditLogger::new(None, false)),
            validation: Arc::new(ValidationConfig::default()),
            rate_limiter: Arc::new(RateLimiter::default()),
            session_manager: session_manager.clone(),
            shutdown_token: CancellationToken::new(),
            active_connections: Arc::new(AtomicUsize::new(0)),
        };

        let api_state = ApiState {
            session_manager,
            config: config_arc,
        };
        (config, app_state, api_state)
    }

    /// Build test AppState and ApiState with token auth configured
    fn test_state_with_token_auth() -> (Config, AppState, ApiState) {
        let mut config = Config::default();
        config.auth = Some(crate::config::AuthConfig {
            method: "token".to_string(),
            username: None,
            password: None,
            token: Some("test-secret-token".to_string()),
            audit_enabled: false,
        });
        let session_manager = Arc::new(SessionManager::new(
            Duration::from_secs(3600),
            SessionMode::Isolated,
        ));
        let config_arc = Arc::new(config.clone());

        let app_state = AppState {
            config: config_arc.clone(),
            audit_logger: Arc::new(AuditLogger::new(None, false)),
            validation: Arc::new(ValidationConfig::default()),
            rate_limiter: Arc::new(RateLimiter::default()),
            session_manager: session_manager.clone(),
            shutdown_token: CancellationToken::new(),
            active_connections: Arc::new(AtomicUsize::new(0)),
        };

        let api_state = ApiState {
            session_manager,
            config: config_arc,
        };
        (config, app_state, api_state)
    }

    #[tokio::test]
    async fn test_api_auth_basic_sessions_401_without_credentials() {
        let (config, app_state, api_state) = test_state_with_basic_auth();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/sessions")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_api_auth_basic_sessions_401_with_wrong_credentials() {
        use base64::Engine as _;

        let (config, app_state, api_state) = test_state_with_basic_auth();
        let app = create_router(&config, app_state, api_state);

        let creds = base64::engine::general_purpose::STANDARD.encode("admin:wrong");
        let req = Request::builder()
            .uri("/api/sessions")
            .header("authorization", format!("Basic {}", creds))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_api_auth_basic_sessions_ok_with_correct_credentials() {
        use base64::Engine as _;

        let (config, app_state, api_state) = test_state_with_basic_auth();
        let app = create_router(&config, app_state, api_state);

        let creds = base64::engine::general_purpose::STANDARD.encode("admin:secret");
        let req = Request::builder()
            .uri("/api/sessions")
            .header("authorization", format!("Basic {}", creds))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_auth_basic_stats_401_without_credentials() {
        let (config, app_state, api_state) = test_state_with_basic_auth();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/stats")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_api_auth_basic_delete_session_401_without_credentials() {
        let (config, app_state, api_state) = test_state_with_basic_auth();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/sessions/some-id")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_api_auth_token_sessions_401_without_credentials() {
        let (config, app_state, api_state) = test_state_with_token_auth();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/sessions")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_api_auth_token_sessions_401_with_wrong_token() {
        let (config, app_state, api_state) = test_state_with_token_auth();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/sessions")
            .header("authorization", "Bearer wrong-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_api_auth_token_sessions_ok_with_correct_token() {
        let (config, app_state, api_state) = test_state_with_token_auth();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/sessions")
            .header("authorization", "Bearer test-secret-token")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_auth_health_public_with_auth_configured() {
        let (config, app_state, api_state) = test_state_with_basic_auth();
        let app = create_router(&config, app_state, api_state);

        // /api/health should be accessible without credentials
        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_api_auth_config_public_with_auth_configured() {
        let (config, app_state, api_state) = test_state_with_basic_auth();
        let app = create_router(&config, app_state, api_state);

        // /api/config should be accessible without credentials
        let req = Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["auth_method"], "basic");
    }

    #[tokio::test]
    async fn test_api_auth_no_auth_config_all_endpoints_open() {
        // When no auth is configured, all API endpoints should be accessible
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        for uri in ["/api/sessions", "/api/stats", "/api/health", "/api/config"] {
            let req = Request::builder().uri(uri).body(Body::empty()).unwrap();

            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "Expected 200 for {}", uri);
        }
    }

    // ── WebSocket integration tests ─────────────────────────────────

    #[tokio::test]
    async fn test_max_connections_rejected_at_zero() {
        let mut config = Config::default();
        config.max_connections = 0; // Set limit to 0 to force rejection
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let result = tokio_tungstenite::connect_async(&url).await;

        // Connection should be rejected with 503
        assert!(result.is_err(), "Expected connection to be rejected");
    }

    #[tokio::test]
    async fn test_max_connections_allowed_under_limit() {
        let mut config = Config::default();
        config.max_connections = 10;
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Connection should succeed, send resize to get ready
        let resize = serde_json::json!({
            "type": "resize",
            "data": { "cols": 80, "rows": 24 }
        });
        send_ws_msg(&mut ws, &resize).await;
        let ready = read_ws_msg(&mut ws).await;
        assert_eq!(ready["type"], "ready");

        ws.close(None).await.unwrap();
    }

    #[tokio::test]
    async fn test_max_connections_rejected_at_limit() {
        let mut config = Config::default();
        config.max_connections = 1;
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);

        // First connection should succeed
        let (mut ws1, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let resize = serde_json::json!({
            "type": "resize",
            "data": { "cols": 80, "rows": 24 }
        });
        send_ws_msg(&mut ws1, &resize).await;
        let ready = read_ws_msg(&mut ws1).await;
        assert_eq!(ready["type"], "ready");

        // Second connection should be rejected
        let result = tokio_tungstenite::connect_async(&url).await;
        assert!(result.is_err(), "Expected second connection to be rejected");

        ws1.close(None).await.unwrap();
    }

    #[tokio::test]
    async fn test_max_connections_reopens_after_close() {
        let mut config = Config::default();
        config.max_connections = 1;
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);

        // First connection
        let (mut ws1, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let resize = serde_json::json!({
            "type": "resize",
            "data": { "cols": 80, "rows": 24 }
        });
        send_ws_msg(&mut ws1, &resize).await;
        let ready = read_ws_msg(&mut ws1).await;
        assert_eq!(ready["type"], "ready");

        // Second connection should be rejected while first is open
        let result = tokio_tungstenite::connect_async(&url).await;
        assert!(result.is_err(), "Expected second connection to be rejected");

        // Close first connection
        ws1.close(None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // New connection should succeed after close
        let (mut ws2, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let resize = serde_json::json!({
            "type": "resize",
            "data": { "cols": 80, "rows": 24 }
        });
        send_ws_msg(&mut ws2, &resize).await;
        let ready = read_ws_msg(&mut ws2).await;
        assert_eq!(ready["type"], "ready");

        ws2.close(None).await.unwrap();
    }

    /// Helper: start server on a random port, return the bound address
    async fn start_test_server(config: Config) -> SocketAddr {
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
        let shutdown_token = CancellationToken::new();

        let app_state = AppState {
            config: Arc::new(config.clone()),
            audit_logger: Arc::new(audit_logger),
            validation: Arc::new(validation),
            rate_limiter: Arc::new(rate_limiter),
            session_manager: session_manager.clone(),
            shutdown_token: shutdown_token.clone(),
            active_connections: Arc::new(AtomicUsize::new(0)),
        };
        let api_state = ApiState {
            session_manager,
            config: Arc::new(config.clone()),
        };

        let app = create_router(&config, app_state, api_state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move { shutdown_token.cancelled().await })
            .await
            .unwrap();
        });

        // Give the server a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;
        addr
    }

    /// Helper: read one WebSocket text message and parse it as JSON
    async fn read_ws_msg(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> serde_json::Value {
        use futures::StreamExt;
        let msg = ws.next().await.unwrap().unwrap();
        let text = msg.into_text().unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// Helper: send a JSON message over WebSocket
    async fn send_ws_msg(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        msg: &serde_json::Value,
    ) {
        use futures::SinkExt;
        ws.send(tokio_tungstenite::tungstenite::Message::Text(
            msg.to_string().into(),
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_websocket_no_auth_flow() {
        let config = Config::default();
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Send resize
        let resize = serde_json::json!({
            "type": "resize",
            "data": { "cols": 80, "rows": 24 }
        });
        send_ws_msg(&mut ws, &resize).await;

        // Receive ready
        let ready = read_ws_msg(&mut ws).await;
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["data"]["cols"], 80);
        assert_eq!(ready["data"]["rows"], 24);
        assert!(!ready["data"]["readonly"].as_bool().unwrap());
        let _session_id = ready["data"]["session_id"].as_str().unwrap().to_string();

        // Send input
        let input = serde_json::json!({
            "type": "input",
            "data": { "payload": "echo hello\n" }
        });
        send_ws_msg(&mut ws, &input).await;

        // Collect output until we see "hello" or timeout
        let found = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let msg = read_ws_msg(&mut ws).await;
                if msg["type"] == "output" {
                    let payload = msg["data"]["payload"].as_str().unwrap();
                    if payload.contains("hello") {
                        return true;
                    }
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(found, "Expected output containing 'hello'");

        // Ping/pong
        let ping = serde_json::json!({
            "type": "ping",
            "data": { "timestamp": 12345 }
        });
        send_ws_msg(&mut ws, &ping).await;

        let pong = read_ws_msg(&mut ws).await;
        assert_eq!(pong["type"], "pong");
        assert_eq!(pong["data"]["timestamp"], 12345);

        ws.close(None).await.unwrap();
    }

    #[tokio::test]
    async fn test_websocket_basic_auth_success() {
        use base64::Engine as _;

        let mut config = Config::default();
        config.auth = Some(crate::config::AuthConfig {
            method: "basic".to_string(),
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
            token: None,
            audit_enabled: false,
        });
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Send auth
        let creds = base64::engine::general_purpose::STANDARD.encode("admin:secret");
        let auth = serde_json::json!({
            "type": "auth",
            "data": { "method": "basic", "credentials": creds }
        });
        send_ws_msg(&mut ws, &auth).await;

        // Receive auth_ok
        let auth_ok = read_ws_msg(&mut ws).await;
        assert_eq!(auth_ok["type"], "auth_ok");
        assert!(!auth_ok["data"]["readonly"].as_bool().unwrap());

        // Continue with resize → ready
        let resize = serde_json::json!({
            "type": "resize",
            "data": { "cols": 80, "rows": 24 }
        });
        send_ws_msg(&mut ws, &resize).await;

        let ready = read_ws_msg(&mut ws).await;
        assert_eq!(ready["type"], "ready");

        ws.close(None).await.unwrap();
    }

    #[tokio::test]
    async fn test_websocket_basic_auth_failure() {
        use base64::Engine as _;

        let mut config = Config::default();
        config.auth = Some(crate::config::AuthConfig {
            method: "basic".to_string(),
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
            token: None,
            audit_enabled: false,
        });
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Send auth with wrong password
        let creds = base64::engine::general_purpose::STANDARD.encode("admin:wrong");
        let auth = serde_json::json!({
            "type": "auth",
            "data": { "method": "basic", "credentials": creds }
        });
        send_ws_msg(&mut ws, &auth).await;

        // Should receive auth_fail
        let auth_fail = read_ws_msg(&mut ws).await;
        assert_eq!(auth_fail["type"], "auth_fail");
        assert!(
            auth_fail["data"]["reason"]
                .as_str()
                .unwrap()
                .contains("Invalid")
        );

        // Connection should close after auth failure
        use futures::StreamExt;
        let next = ws.next().await;
        assert!(next.is_none() || next.unwrap().is_err());
    }

    #[tokio::test]
    async fn test_websocket_token_auth_success() {
        // Token must be base64-compatible (alphanumeric + /+=) since
        // validate_credentials checks the format before comparing.
        let token = "dGVzdHNlY3JldDEyMzQ1";

        let mut config = Config::default();
        config.auth = Some(crate::config::AuthConfig {
            method: "token".to_string(),
            username: None,
            password: None,
            token: Some(token.to_string()),
            audit_enabled: false,
        });
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Send auth with valid token
        let auth = serde_json::json!({
            "type": "auth",
            "data": { "method": "token", "credentials": token }
        });
        send_ws_msg(&mut ws, &auth).await;

        // Should receive auth_ok
        let auth_ok = read_ws_msg(&mut ws).await;
        assert_eq!(auth_ok["type"], "auth_ok");

        // Continue with resize → ready
        let resize = serde_json::json!({
            "type": "resize",
            "data": { "cols": 120, "rows": 40 }
        });
        send_ws_msg(&mut ws, &resize).await;

        let ready = read_ws_msg(&mut ws).await;
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["data"]["cols"], 120);
        assert_eq!(ready["data"]["rows"], 40);

        ws.close(None).await.unwrap();
    }
}
