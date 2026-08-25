/// Authentication handling for WebSocket connections.
///
/// Unifies the `basic` and `token` auth flows into a single common path,
/// eliminating the ~250 lines of duplicated code that previously existed as
/// near-identical `match` arms in the session handler.
use crate::auth::{BasicAuth, TokenAuth};
use crate::protocol::*;
use crate::validation::ValidationError;
use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::stream::SplitStream;
use futures::{SinkExt, StreamExt};
use tracing::{error, warn};

use super::{AppState, CloseReason, WsSender};

/// Outcome of the authentication process.
pub(crate) enum AuthResult {
    /// Authentication succeeded; contains the optional username
    /// (`Some` for basic auth, `None` for token auth).
    Success(Option<String>),
    /// Connection should be closed (auth failed, client disconnected,
    /// or auth is misconfigured); carries the reason for the
    /// `ConnectionClosed` audit event.
    Close(CloseReason),
}

/// The configured authentication method and its validator.
///
/// Built **once at startup** (see [`AuthMethod::build`]) and shared via
/// `Arc` in `AppState`: Argon2 password hashing is expensive (~100 ms), so
/// it must never run per connection. This also removes a DoS amplification
/// vector where unauthenticated clients could force the server to re-hash
/// the configured password on every login attempt.
#[derive(Clone)]
pub(crate) enum AuthMethod {
    Basic {
        validator: BasicAuth,
        username: String,
    },
    Token {
        validator: TokenAuth,
    },
}

impl AuthMethod {
    /// Build the validator from configuration.
    ///
    /// Returns `Err` with a reason when the method is unknown, credentials
    /// are missing, or password hashing fails. Called once at startup.
    pub(crate) fn build(config: &crate::config::AuthConfig) -> Result<Self, String> {
        match config.method.as_str() {
            "basic" => {
                let (Some(username), Some(password)) = (&config.username, &config.password) else {
                    return Err("basic auth requires both username and password".to_string());
                };
                let validator = BasicAuth::new(username.clone(), password.clone())
                    .map_err(|e| format!("failed to hash password: {e}"))?;
                Ok(Self::Basic {
                    validator,
                    username: username.clone(),
                })
            }
            "token" => {
                let Some(token) = &config.token else {
                    return Err("token auth requires a token".to_string());
                };
                Ok(Self::Token {
                    validator: TokenAuth::new(token.clone()),
                })
            }
            other => Err(format!("unknown auth method '{other}'")),
        }
    }

    /// Validate credentials against the stored auth method.
    fn validate(&self, credentials: &str) -> bool {
        match self {
            AuthMethod::Basic { validator, .. } => validator.validate(credentials),
            AuthMethod::Token { validator } => validator.validate(credentials),
        }
    }

    /// Validate the format of credentials (length and charset checks).
    fn validate_format(
        &self,
        config: &crate::config::ValidationConfig,
        credentials: &str,
    ) -> Result<(), ValidationError> {
        match self {
            AuthMethod::Basic { .. } => config.validate_credentials(credentials),
            AuthMethod::Token { .. } => config.validate_token_credentials(credentials),
        }
    }

    /// The username used for audit logging.
    fn audit_name(&self) -> &str {
        match self {
            AuthMethod::Basic { username, .. } => username,
            AuthMethod::Token { .. } => "token-user",
        }
    }

    /// The expected auth method string (`"basic"` or `"token"`).
    fn method_name(&self) -> &str {
        match self {
            AuthMethod::Basic { .. } => "basic",
            AuthMethod::Token { .. } => "token",
        }
    }

    /// The username to return on success (`Some` for basic, `None` for token).
    fn success_username(&self) -> Option<String> {
        match self {
            AuthMethod::Basic { username, .. } => Some(username.clone()),
            AuthMethod::Token { .. } => None,
        }
    }

    /// Error message for invalid credentials.
    fn invalid_message(&self) -> &'static str {
        match self {
            AuthMethod::Basic { .. } => "Invalid credentials",
            AuthMethod::Token { .. } => "Invalid token",
        }
    }
}

/// Send an `AuthFail` message to the client.
async fn send_auth_fail(
    ws_sender: &WsSender,
    reason: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let msg = Message::AuthFail(AuthFailData {
        reason: reason.to_string(),
    });
    ws_sender
        .lock()
        .await
        .send(WsMessage::Text(msg.to_json()?.into()))
        .await?;
    Ok(())
}

/// Send an `AuthOk` message to the client.
async fn send_auth_ok(
    ws_sender: &WsSender,
    client_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let msg = Message::AuthOk(AuthOkData {
        client_id: client_id.to_string(),
        readonly: false,
    });
    ws_sender
        .lock()
        .await
        .send(WsMessage::Text(msg.to_json()?.into()))
        .await?;
    Ok(())
}

/// Handle authentication for a WebSocket connection.
///
/// - If auth is not configured, returns `Success(None)`.
/// - If auth is configured but the client fails authentication (or disconnects),
///   returns `Close` — the caller ends the session with the given reason.
/// - On success, returns `Success(Some(username))` for basic auth or
///   `Success(None)` for token auth.
///
/// The authenticator was built once at startup and lives in
/// [`AppState::auth_method`]; misconfigured auth (build failure at startup)
/// is represented by `Some(config)` present but `auth_method == None`, which
/// fails closed here.
pub(crate) async fn authenticate(
    state: &AppState,
    ws_sender: &WsSender,
    ws_receiver: &mut SplitStream<WebSocket>,
    remote_addr: &str,
    client_id: &str,
    rate_limit_key: &str,
) -> Result<AuthResult, Box<dyn std::error::Error + Send + Sync>> {
    let Some(auth_method) = state.auth_method.as_ref() else {
        if state.config.auth.is_some() {
            // Auth is configured but could not be built at startup — fail
            // closed rather than letting connections through unauthenticated.
            error!(
                "Auth is configured but the authenticator failed to build; rejecting connection"
            );
            send_auth_fail(ws_sender, "Server authentication misconfigured").await?;
            return Ok(AuthResult::Close(CloseReason::AuthFailed));
        }
        return Ok(AuthResult::Success(None));
    };

    perform_auth(
        state,
        ws_sender,
        ws_receiver,
        remote_addr,
        client_id,
        rate_limit_key,
        auth_method,
    )
    .await
}

/// Common authentication flow shared by all auth methods.
///
/// Uses the pre-built authenticator from startup; no per-connection hashing.
async fn perform_auth(
    state: &AppState,
    ws_sender: &WsSender,
    ws_receiver: &mut SplitStream<WebSocket>,
    remote_addr: &str,
    client_id: &str,
    rate_limit_key: &str,
    auth_method: &AuthMethod,
) -> Result<AuthResult, Box<dyn std::error::Error + Send + Sync>> {
    // 1. Check rate limit before processing auth.
    // The key combines the (possibly header-derived) IP with the real socket
    // address when trust_proxy is on, so spoofed headers cannot evade it.
    if let Err(duration) = state.rate_limiter.check(rate_limit_key).await {
        warn!("Rate limit exceeded for {}", remote_addr);
        send_auth_fail(
            ws_sender,
            &format!(
                "Rate limit exceeded. Try again in {} seconds",
                duration.as_secs()
            ),
        )
        .await?;
        return Ok(AuthResult::Close(CloseReason::AuthFailed));
    }

    // 2. Wait for auth message from client
    let auth_data = match ws_receiver.next().await {
        Some(Ok(WsMessage::Text(text))) => match Message::from_json(&text)? {
            Message::Auth(auth_data) => auth_data,
            _ => {
                send_auth_fail(ws_sender, "Expected auth message").await?;
                return Ok(AuthResult::Close(CloseReason::AuthFailed));
            }
        },
        _ => {
            // Connection closed or non-text message
            return Ok(AuthResult::Close(CloseReason::ClientClosed));
        }
    };

    // 3. Validate auth method
    if let Err(e) = state.validation.validate_auth_method(&auth_data.method) {
        warn!("Invalid auth method: {}", e);
        send_auth_fail(ws_sender, &format!("Invalid authentication method: {}", e)).await?;
        return Ok(AuthResult::Close(CloseReason::AuthFailed));
    }

    // 4. The authenticator was pre-built at startup (see `authenticate`);
    //    no per-connection hashing happens here.

    // 5. Validate credentials format
    if let Err(e) = auth_method.validate_format(&state.validation, &auth_data.credentials) {
        warn!("Invalid credentials format: {}", e);
        state
            .audit_logger
            .log_auth_attempt(remote_addr, auth_method.audit_name(), false, client_id)
            .await;
        send_auth_fail(ws_sender, "Invalid credentials format").await?;
        return Ok(AuthResult::Close(CloseReason::AuthFailed));
    }

    // 6. Validate credentials (only accept the expected method)
    let valid = if auth_data.method == auth_method.method_name() {
        auth_method.validate(&auth_data.credentials)
    } else {
        false
    };

    if !valid {
        state
            .audit_logger
            .log_auth_attempt(remote_addr, auth_method.audit_name(), false, client_id)
            .await;
        send_auth_fail(ws_sender, auth_method.invalid_message()).await?;
        return Ok(AuthResult::Close(CloseReason::AuthFailed));
    }

    // 7. Success — log, reset rate limit, send AuthOk
    state
        .audit_logger
        .log_auth_attempt(remote_addr, auth_method.audit_name(), true, client_id)
        .await;
    state.rate_limiter.reset(rate_limit_key).await;
    send_auth_ok(ws_sender, client_id).await?;

    Ok(AuthResult::Success(auth_method.success_username()))
}
