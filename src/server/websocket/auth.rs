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
enum AuthMethod {
    Basic {
        validator: BasicAuth,
        username: String,
    },
    Token {
        validator: TokenAuth,
    },
}

impl AuthMethod {
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
pub(crate) async fn authenticate(
    state: &AppState,
    ws_sender: &WsSender,
    ws_receiver: &mut SplitStream<WebSocket>,
    remote_addr: &str,
    client_id: &str,
) -> Result<AuthResult, Box<dyn std::error::Error + Send + Sync>> {
    let Some(auth_config) = &state.config.auth else {
        return Ok(AuthResult::Success(None));
    };

    // Build the auth method from config, or reject if misconfigured
    let auth_method = match auth_config.method.as_str() {
        "basic"
            if let (Some(username), Some(password)) =
                (&auth_config.username, &auth_config.password) =>
        {
            AuthMethod::Basic {
                validator: BasicAuth::new(username.clone(), password.clone()),
                username: username.clone(),
            }
        }
        "token" if let Some(token) = &auth_config.token => AuthMethod::Token {
            validator: TokenAuth::new(token.clone()),
        },
        _ => {
            error!(
                "Auth method '{}' is misconfigured — missing credentials",
                auth_config.method
            );
            send_auth_fail(ws_sender, "Server authentication misconfigured").await?;
            return Ok(AuthResult::Close(CloseReason::AuthFailed));
        }
    };

    perform_auth(
        state,
        ws_sender,
        ws_receiver,
        remote_addr,
        client_id,
        &auth_method,
    )
    .await
}

/// Common authentication flow shared by all auth methods.
async fn perform_auth(
    state: &AppState,
    ws_sender: &WsSender,
    ws_receiver: &mut SplitStream<WebSocket>,
    remote_addr: &str,
    client_id: &str,
    auth_method: &AuthMethod,
) -> Result<AuthResult, Box<dyn std::error::Error + Send + Sync>> {
    // 1. Check rate limit before processing auth
    if let Err(duration) = state.rate_limiter.check(remote_addr).await {
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

    // 4. Validate credentials format
    if let Err(e) = auth_method.validate_format(&state.validation, &auth_data.credentials) {
        warn!("Invalid credentials format: {}", e);
        state
            .audit_logger
            .log_auth_attempt(remote_addr, auth_method.audit_name(), false, client_id)
            .await;
        send_auth_fail(ws_sender, "Invalid credentials format").await?;
        return Ok(AuthResult::Close(CloseReason::AuthFailed));
    }

    // 5. Validate credentials (only accept the expected method)
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

    // 6. Success — log, reset rate limit, send AuthOk
    state
        .audit_logger
        .log_auth_attempt(remote_addr, auth_method.audit_name(), true, client_id)
        .await;
    state.rate_limiter.reset(remote_addr).await;
    send_auth_ok(ws_sender, client_id).await?;

    Ok(AuthResult::Success(auth_method.success_username()))
}
