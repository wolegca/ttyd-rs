/// HTTP server implementation using axum
use crate::assets::Assets;
use crate::audit::AuditLogger;
use crate::config::Config;
use crate::rate_limit::RateLimiter;
use crate::server::api::ApiState;
use crate::server::websocket::AppState;
use crate::session::{SessionManager, SessionMode};
use axum::middleware::Next;
use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{StatusCode, Uri, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Start the HTTP/WebSocket server
pub async fn start_server(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let audit_logger = AuditLogger::new(config.audit.log_file.clone(), config.audit.enabled);
    if config.audit.enabled {
        audit_logger.prepare().await.map_err(|e| {
            format!("audit logging is enabled but its log file cannot be prepared: {e}")
        })?;
    }
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

    // Build the WebSocket authenticator once at startup: Argon2 password
    // hashing is expensive (~100 ms) and must never run per connection.
    // A build failure is logged here; `AppState.auth_method` stays `None`
    // and the WS auth path fails closed (rejects every connection).
    let ws_auth_method = config.auth.as_ref().and_then(|auth_config| {
        match crate::server::websocket::AuthMethod::build(auth_config) {
            Ok(method) => Some(Arc::new(method)),
            Err(reason) => {
                error!("Failed to build WebSocket authenticator: {}", reason);
                None
            }
        }
    });

    // Dedicated limiter for file endpoints (see AppState::file_rate_limiter).
    let file_rate_limiter = Arc::new(RateLimiter::new(
        config.rate_limit.max_requests,
        config.rate_limit.window_seconds,
    ));

    let app_state = AppState {
        config: Arc::new(config.clone()),
        audit_logger: Arc::new(audit_logger),
        validation: Arc::new(validation),
        rate_limiter: Arc::new(rate_limiter.clone()),
        session_manager: session_manager.clone(),
        shutdown_token: shutdown_token.clone(),
        active_connections: Arc::new(AtomicUsize::new(0)),
        auth_method: ws_auth_method,
        file_rate_limiter,
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
    if config.compression.enabled {
        info!(
            "Compression: gzip enabled (level {}, static assets only)",
            config.compression.level
        );
    } else {
        info!("Compression: disabled");
    }

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

/// Middleware: per-client-IP rate limiting for the file transfer endpoints.
///
/// Uses a dedicated [`RateLimiter`] (`AppState::file_rate_limiter`) so file
/// browsing cannot exhaust the WebSocket auth budget (and vice versa). The
/// client IP honors `trust_proxy` via [`extract_real_ip`], preventing limit
/// bypass through spoofed headers.
async fn file_rate_limit_middleware(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, axum::Json<super::api::ErrorResponse>)> {
    let client_ip =
        super::websocket::extract_real_ip(&headers, addr.ip(), state.config.trust_proxy);

    if let Err(retry_after) = state.file_rate_limiter.check(&client_ip).await {
        warn!(
            "Rate limit exceeded for {} on file transfer endpoint (retry after {}s)",
            client_ip,
            retry_after.as_secs()
        );
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            axum::Json(super::api::ErrorResponse {
                error: format!(
                    "Rate limit exceeded. Try again in {} seconds",
                    retry_after.as_secs()
                ),
            }),
        ));
    }

    Ok(next.run(request).await)
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
            get(super::api::get_stats).with_state(api_state.clone()),
        );

    // File transfer routes (conditionally enabled)
    let protected_api = if config.file_transfer.enabled {
        let file_state = super::files::FileTransferState {
            config: Arc::new(config.file_transfer.clone()),
            session_manager: app_state.session_manager.clone(),
        };
        // The upload route needs a larger body limit than axum's 2MB default.
        // We set it to max_upload_size since we do our own incremental checking.
        let upload_router = Router::new()
            .route(
                "/api/files/upload",
                post(super::files::upload_file).with_state(file_state.clone()),
            )
            .layer(axum::extract::DefaultBodyLimit::max(
                config.file_transfer.max_upload_size,
            ));
        let other_file_routes = Router::new()
            .route(
                "/api/files/download",
                get(super::files::download_file).with_state(file_state.clone()),
            )
            .route(
                "/api/files/list",
                get(super::files::list_files).with_state(file_state),
            );
        // Rate-limit file endpoints per client IP. Without this, an
        // authenticated (or, when auth is disabled, unauthenticated) client
        // could hammer downloads or repeatedly upload large files to
        // exhaust bandwidth and disk. Uses the same limiter and real-IP
        // extraction as the WebSocket auth path.
        let rate_limit_state = app_state.clone();
        let file_router =
            upload_router
                .merge(other_file_routes)
                .layer(middleware::from_fn_with_state(
                    rate_limit_state,
                    file_rate_limit_middleware,
                ));
        protected_api.merge(file_router)
    } else {
        protected_api
    };

    // Apply auth middleware to protected routes when auth is configured.
    // The authenticator (including the expensive Argon2 hashing) is built
    // once here rather than per request.
    let protected_api = if let Some(ref auth_config) = config.auth {
        let auth_state = super::api::ApiAuthState {
            auth: super::api::ApiAuth::from_config(auth_config),
        };
        protected_api.layer(middleware::from_fn_with_state(
            auth_state,
            super::api::api_auth_middleware,
        ))
    } else {
        protected_api
    };

    // Static asset routes (index page + embedded fallback files).
    // Gzip compression is applied only to these routes — API and WebSocket
    // responses are intentionally left uncompressed.
    let mut static_router = Router::new()
        .route("/", get(index_handler))
        .fallback(static_handler);

    if config.compression.enabled {
        // Skip compression for already-compressed formats (fonts, icons) —
        // gzipping them wastes CPU and can even increase size.
        static_router = static_router.layer(
            tower_http::compression::CompressionLayer::new()
                .quality(tower_http::compression::CompressionLevel::Precise(
                    config.compression.level as i32,
                ))
                .compress_when(
                    |_status: axum::http::StatusCode,
                     _version: axum::http::Version,
                     headers: &axum::http::HeaderMap,
                     _extensions: &axum::http::Extensions| {
                        let content_type = headers
                            .get(header::CONTENT_TYPE)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("");
                        !content_type.starts_with("font/") && content_type != "image/x-icon"
                    },
                ),
        );
    }

    Router::new()
        .route("/ws", get(super::websocket::websocket_handler))
        .merge(public_api)
        .merge(protected_api)
        .merge(static_router)
        .with_state(app_state)
}

/// Handler for the index page
async fn index_handler() -> impl IntoResponse {
    static_handler(Uri::from_static("/index.html")).await
}

/// Content-Security-Policy for the embedded frontend.
///
/// The page only loads same-origin resources (xterm.js, fonts, CSS) and makes
/// same-origin WebSocket/fetch/XHR calls, so a strict policy is safe. Inline
/// `<script>` and `<style>` blocks in index.html require `'unsafe-inline'`.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'";

/// Handler for embedded static files
async fn static_handler(uri: Uri) -> impl IntoResponse {
    let raw_path = uri.path().trim_start_matches('/');

    // Default to index.html for root
    let path = if raw_path.is_empty() {
        "index.html"
    } else {
        raw_path
    };

    // Reject path traversal attempts explicitly. rust_embed does a map lookup
    // so this cannot escape the embedded set, but being explicit keeps intent
    // clear and avoids serving anything unexpected.
    if path.split('/').any(|segment| segment == "..") {
        return not_found_response();
    }

    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();

            // Vendor assets are embedded at compile time and only change when
            // the binary is rebuilt, so they can be cached aggressively. The
            // index page is revalidated on every load so clients pick up a new
            // entry point after an upgrade.
            let is_vendor = path.starts_with("vendor/");
            let cache_control = if is_vendor {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .header(header::CACHE_CONTROL, cache_control)
                .header(header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY)
                .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
                .header(header::REFERRER_POLICY, "no-referrer")
                .body(Body::from(content.data))
                .ok()
                .unwrap_or_else(|| {
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::empty())
                        .unwrap_or_default()
                })
        }
        None => not_found_response(),
    }
}

/// Build a 404 response with an explicit text/plain content type.
fn not_found_response() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from("404 Not Found"))
        .ok()
        .unwrap_or_else(|| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap_or_default()
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
            auth_method: None,
            file_rate_limiter: Arc::new(RateLimiter::new(10, 60)),
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
            auth_method: None,
            file_rate_limiter: Arc::new(RateLimiter::new(10, 60)),
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

    // ── Compression tests ───────────────────────────────────────────

    #[tokio::test]
    async fn test_static_index_gzip_when_requested() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/")
            .header("accept-encoding", "gzip")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );
        let vary = resp
            .headers()
            .get("vary")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            vary.to_ascii_lowercase().contains("accept-encoding"),
            "Vary header should include Accept-Encoding, got: {vary}"
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!body.is_empty());

        // Compressed body must be smaller than the embedded original
        let original = crate::assets::Assets::get("index.html")
            .map(|f| f.data.len())
            .unwrap_or(0);
        assert!(
            body.len() < original,
            "compressed body ({}) should be smaller than original ({})",
            body.len(),
            original
        );
    }

    #[tokio::test]
    async fn test_static_no_gzip_when_not_requested() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/vendor/scripts/xterm.css")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("content-encoding").is_none());
    }

    #[tokio::test]
    async fn test_static_gzip_body_is_valid() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/vendor/scripts/xterm.js")
            .header("accept-encoding", "gzip")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("gzip")
        );

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();

        // Decompress and verify it matches the embedded original byte-for-byte
        let mut decoder = flate2::read::GzDecoder::new(&body[..]);
        let mut decompressed = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decompressed).unwrap();

        let original = crate::assets::Assets::get("vendor/scripts/xterm.js")
            .map(|f| f.data)
            .unwrap_or_default();
        assert_eq!(original, decompressed);
    }

    #[tokio::test]
    async fn test_compression_disabled_in_config() {
        let mut config = Config::default();
        config.compression.enabled = false;
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/")
            .header("accept-encoding", "gzip")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get("content-encoding").is_none());
    }

    #[tokio::test]
    async fn test_api_not_compressed() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/api/health")
            .header("accept-encoding", "gzip")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get("content-encoding").is_none(),
            "API responses must not be compressed"
        );
    }

    // ── Static asset header tests ───────────────────────────────────

    #[tokio::test]
    async fn test_static_font_not_gzipped() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/vendor/fonts/0xProtoNerdFontMono-Regular.woff2")
            .header("accept-encoding", "gzip")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("font/woff2")
        );
        // Already-compressed fonts must NOT be gzipped again.
        assert!(
            resp.headers().get("content-encoding").is_none(),
            "woff2 fonts must not be compressed, got: {:?}",
            resp.headers().get("content-encoding")
        );
    }

    #[tokio::test]
    async fn test_static_cache_headers() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        // Vendor assets are immutable (embedded at compile time).
        let req = Request::builder()
            .uri("/vendor/scripts/xterm.js")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=31536000, immutable")
        );

        // The index page is revalidated on every load.
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-cache")
        );
    }

    #[tokio::test]
    async fn test_static_security_headers() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let csp = resp
            .headers()
            .get("content-security-policy")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            csp.contains("default-src 'self'"),
            "CSP should restrict to same-origin, got: {csp}"
        );
        assert!(
            csp.contains("connect-src 'self'"),
            "CSP should allow same-origin connections, got: {csp}"
        );
        assert_eq!(
            resp.headers()
                .get("x-content-type-options")
                .and_then(|v| v.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            resp.headers()
                .get("referrer-policy")
                .and_then(|v| v.to_str().ok()),
            Some("no-referrer")
        );
    }

    #[tokio::test]
    async fn test_static_path_traversal_rejected() {
        let config = Config::default();
        let (app_state, api_state) = test_state();
        let app = create_router(&config, app_state, api_state);

        let req = Request::builder()
            .uri("/../etc/passwd")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/plain; charset=utf-8")
        );
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
            auth_method: None,
            file_rate_limiter: Arc::new(RateLimiter::new(10, 60)),
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
            auth_method: None,
            file_rate_limiter: Arc::new(RateLimiter::new(10, 60)),
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
        let config = Config {
            max_connections: 0, // Set limit to 0 to force rejection
            ..Default::default()
        };
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let result = tokio_tungstenite::connect_async(&url).await;

        // Connection should be rejected with 503
        assert!(result.is_err(), "Expected connection to be rejected");
    }

    #[tokio::test]
    async fn test_max_connections_allowed_under_limit() {
        let config = Config {
            max_connections: 10,
            ..Default::default()
        };
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
        let config = Config {
            max_connections: 1,
            ..Default::default()
        };
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
        let config = Config {
            max_connections: 1,
            ..Default::default()
        };
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

        // Build the WebSocket authenticator the same way start_server does.
        let ws_auth_method = config.auth.as_ref().and_then(|auth_config| {
            crate::server::websocket::AuthMethod::build(auth_config)
                .ok()
                .map(Arc::new)
        });

        let app_state = AppState {
            config: Arc::new(config.clone()),
            audit_logger: Arc::new(audit_logger),
            validation: Arc::new(validation),
            rate_limiter: Arc::new(rate_limiter),
            file_rate_limiter: Arc::new(RateLimiter::new(10, 60)),
            session_manager: session_manager.clone(),
            shutdown_token: shutdown_token.clone(),
            active_connections: Arc::new(AtomicUsize::new(0)),
            auth_method: ws_auth_method,
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
        // Token may contain arbitrary characters (e.g. -, _, .) since
        // validate_token_credentials only checks length, not charset.
        let token = "my-secret_token.v2";

        let mut config = Config::default();
        config.auth = Some(crate::config::AuthConfig {
            method: "token".to_string(),
            username: None,
            password: None,
            token: Some(token.to_string()),
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

    /// PTY output bursts are coalesced into fewer messages; the key
    /// property is that every byte still arrives intact and in order.
    ///
    /// Uses `seq -s ,` (comma-separated, no newlines) so the output is
    /// deterministic and unaffected by the PTY line discipline (OPOST/ONLCR
    /// would rewrite `\n` to `\r\n` for a non-raw-mode child).
    #[tokio::test]
    #[allow(clippy::panic)]
    async fn test_output_coalescing_delivers_all_data() {
        let config = Config {
            command: vec![
                "seq".to_string(),
                "-s".to_string(),
                ",".to_string(),
                "1".to_string(),
                "1000".to_string(),
            ],
            ..Config::default()
        };
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        let resize = serde_json::json!({
            "type": "resize",
            "data": { "cols": 120, "rows": 40 }
        });
        send_ws_msg(&mut ws, &resize).await;

        let numbers: String = (1..=1000)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut got = String::new();
        loop {
            let msg = read_ws_msg(&mut ws).await;
            let msg_type = msg["type"].as_str().unwrap();
            match msg_type {
                "ready" => {}
                "output" => {
                    got.push_str(msg["data"]["payload"].as_str().unwrap());
                    if got.contains("1000") {
                        break;
                    }
                }
                "disconnect" => break,
                other => panic!("unexpected message: {other}"),
            }
        }

        // Coalescing must not lose or reorder any output byte. (seq emits a
        // trailing newline which the PTY line discipline may rewrite to
        // \r\n, so assert on the number sequence prefix.)
        assert!(
            got.starts_with(&numbers),
            "expected the full number sequence at the start of {} bytes of output",
            got.len()
        );
    }

    /// A normal session must also log `connection_closed`, with the
    /// client-closed reason (previously this event was hardcoded to
    /// "normal closure" even on timeout/shutdown/error).
    #[tokio::test]
    async fn test_audit_logs_connection_closed_on_normal_close() {
        let dir = std::env::temp_dir().join("ttyd-rs-audit-normal-ws");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("audit.log");

        let mut config = Config::default();
        config.audit = crate::config::AuditConfig {
            enabled: true,
            log_file: Some(log_path.clone()),
        };
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // resize → ready, then close the session normally
        let resize = serde_json::json!({
            "type": "resize",
            "data": { "cols": 120, "rows": 40 }
        });
        send_ws_msg(&mut ws, &resize).await;
        let ready = read_ws_msg(&mut ws).await;
        assert_eq!(ready["type"], "ready");

        ws.close(None).await.unwrap();

        // Let the audit write flush before reading the file
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let content = std::fs::read_to_string(&log_path).unwrap();

        assert!(content.contains("connection_opened"));
        assert!(content.contains("session_started"));
        assert!(content.contains("connection_closed"));
        assert!(content.contains("client closed the connection"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The audit log must record a `connection_closed` event even when
    /// authentication fails (previously the close event was only logged on
    /// the happy path), carrying the actual reason.
    #[tokio::test]
    async fn test_audit_logs_connection_closed_on_auth_failure() {
        let dir = std::env::temp_dir().join("ttyd-rs-audit-authfail-ws");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("audit.log");

        let mut config = Config::default();
        config.auth = Some(crate::config::AuthConfig {
            method: "basic".to_string(),
            username: Some("admin".to_string()),
            password: Some("secret".to_string()),
            token: None,
        });
        config.audit = crate::config::AuditConfig {
            enabled: true,
            log_file: Some(log_path.clone()),
        };
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Send auth with a wrong password → auth_fail, then the server closes
        use base64::Engine as _;
        let creds = base64::engine::general_purpose::STANDARD.encode("admin:wrong-password");
        let auth = serde_json::json!({
            "type": "auth",
            "data": { "method": "basic", "credentials": creds }
        });
        send_ws_msg(&mut ws, &auth).await;
        let auth_fail = read_ws_msg(&mut ws).await;
        assert_eq!(auth_fail["type"], "auth_fail");

        // Let the audit write flush before reading the file
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let content = std::fs::read_to_string(&log_path).unwrap();

        assert!(content.contains("connection_opened"));
        assert!(content.contains("auth_failure"));
        assert!(content.contains("connection_closed"));
        assert!(content.contains("authentication failed"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The audit log must also record `connection_closed` (with the
    /// handshake-failed reason) when the client sends an invalid terminal
    /// size during the handshake.
    #[tokio::test]
    async fn test_audit_logs_connection_closed_on_handshake_failure() {
        let dir = std::env::temp_dir().join("ttyd-rs-audit-handshake-ws");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("audit.log");

        let mut config = Config::default();
        config.audit = crate::config::AuditConfig {
            enabled: true,
            log_file: Some(log_path.clone()),
        };
        let addr = start_test_server(config).await;

        let url = format!("ws://{}/ws", addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Below the minimum allowed size (default min_cols = 10)
        let resize = serde_json::json!({
            "type": "resize",
            "data": { "cols": 5, "rows": 24 }
        });
        send_ws_msg(&mut ws, &resize).await;
        let err = read_ws_msg(&mut ws).await;
        assert_eq!(err["type"], "error");
        assert_eq!(err["data"]["code"], "INVALID_SIZE");

        // Let the audit write flush before reading the file
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let content = std::fs::read_to_string(&log_path).unwrap();

        assert!(content.contains("connection_opened"));
        assert!(content.contains("connection_closed"));
        assert!(content.contains("handshake failed"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
