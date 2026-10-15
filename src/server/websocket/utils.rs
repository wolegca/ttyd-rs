/// Utility functions for WebSocket handler: IP extraction, error sending, and message helpers.
use crate::protocol::{DisconnectData, ErrorData, Message};
use axum::extract::ws::Message as WsMessage;
use futures::{Sink, SinkExt};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::warn;

use super::AppState;

/// Extract the real client IP from proxy headers.
///
/// When `trust_proxy` is enabled, checks (in order):
/// 1. `X-Real-IP` header — the canonical real IP set by nginx/Caddy
/// 2. `X-Forwarded-For` header — first entry (client IP) from the chain
///
/// Falls back to `connect_addr` if neither header is present or valid.
/// Only accepts valid IP addresses from headers to prevent spoofing with
/// arbitrary strings.
pub(crate) fn extract_real_ip(
    headers: &axum::http::HeaderMap,
    connect_addr: std::net::IpAddr,
    trust_proxy: bool,
) -> String {
    if !trust_proxy {
        return connect_addr.to_string();
    }

    // Prefer X-Real-IP (single value, set by most reverse proxies)
    if let Some(val) = headers.get("x-real-ip")
        && let Ok(s) = val.to_str()
    {
        let trimmed = s.trim();
        if let Ok(ip) = trimmed.parse::<std::net::IpAddr>() {
            return ip.to_string();
        }
    }

    // Fall back to first entry of X-Forwarded-For
    if let Some(val) = headers.get("x-forwarded-for")
        && let Ok(s) = val.to_str()
    {
        // X-Forwarded-For: client, proxy1, proxy2
        if let Some(first) = s.split(',').next() {
            let trimmed = first.trim();
            if let Ok(ip) = trimmed.parse::<std::net::IpAddr>() {
                return ip.to_string();
            }
        }
    }

    connect_addr.to_string()
}

/// Build the per-client rate-limit key shared by the WebSocket auth path and
/// the HTTP file-transfer endpoints.
///
/// `remote_addr` is the client IP as seen by the application — proxy-aware
/// when `trust_proxy` is enabled (see [`extract_real_ip`]).
///
/// When `trust_proxy` is enabled the header-derived `remote_addr` alone is
/// insufficient: a client that can reach the server directly (a misconfigured
/// proxy, or one that does not overwrite an inbound header) could forge
/// `X-Real-IP` to impersonate another client's bucket. Mixing in the
/// unforgeable socket address closes that gap while keeping distinct real
/// clients behind a proxy on distinct keys (their `X-Real-IP` differs). The
/// WebSocket auth path and the file endpoints must use this same key so their
/// policies cannot diverge.
pub(crate) fn build_rate_limit_key(
    remote_addr: &str,
    connect_addr: std::net::IpAddr,
    trust_proxy: bool,
) -> String {
    if trust_proxy {
        format!("{}|{}", remote_addr, connect_addr)
    } else {
        remote_addr.to_string()
    }
}

/// Extract the (proxy-aware) client IP together with the rate-limit key
/// derived from it, in one call.
///
/// Combines [`extract_real_ip`] and [`build_rate_limit_key`] so the WebSocket
/// upgrade path and the file-endpoint middleware cannot drift apart: both must
/// use the same key policy, and routing them through this helper makes that
/// structural rather than a convention. Returns `(remote_addr, key)`.
pub(crate) fn client_rate_limit_key(
    headers: &axum::http::HeaderMap,
    connect_addr: std::net::IpAddr,
    trust_proxy: bool,
) -> (String, String) {
    let remote_addr = extract_real_ip(headers, connect_addr, trust_proxy);
    let key = build_rate_limit_key(&remote_addr, connect_addr, trust_proxy);
    (remote_addr, key)
}

/// Send a structured error message to the client via the WebSocket sender.
///
/// Returns `Ok(())` on success, or the serialization/send error on failure.
/// Callers decide whether to propagate the error (`?`) or ignore it (`let _ =`).
///
/// Generic over the sink type so it can be unit-tested with a mock sender such
/// as `futures::channel::mpsc::UnboundedSender`. In production the inferred
/// `S` is `futures::stream::SplitSink<WebSocket, WsMessage>` (via [`WsSender`]).
pub(crate) async fn send_ws_error<S>(
    sender: &Arc<Mutex<S>>,
    code: &str,
    message: String,
    fatal: bool,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: Sink<WsMessage> + Unpin,
    S::Error: std::error::Error + 'static,
{
    let msg = Message::Error(ErrorData {
        code: code.to_string(),
        message,
        fatal,
    });
    sender
        .lock()
        .await
        .send(WsMessage::Text(msg.to_json()?.into()))
        .await?;
    Ok(())
}

/// Returns a human-readable name for a protocol message variant.
pub(crate) fn message_type_name(msg: &Message) -> &'static str {
    match msg {
        Message::Auth(_) => "auth",
        Message::Input(_) => "input",
        Message::Resize(_) => "resize",
        Message::Ping(_) => "ping",
        Message::AuthOk(_) => "auth_ok",
        Message::AuthFail(_) => "auth_fail",
        Message::Output(_) => "output",
        Message::Pong(_) => "pong",
        Message::Error(_) => "error",
        Message::Disconnect(_) => "disconnect",
        Message::Ready(_) => "ready",
        Message::Join(_) => "join",
        Message::FileList(_) => "file_list",
        Message::FileListResult(_) => "file_list_result",
    }
}

/// Send a JSON-serialized protocol message to the client.
///
/// Logs a warning on serialization failure (which would mean a bug in the
/// protocol types). Send errors are silently ignored — the connection may
/// already be closing and the caller can't act on it here. Use
/// [`send_ws_error`] when the caller needs to know about failures.
///
/// Generic over the sink type (see [`send_ws_error`]) so it can be unit-tested
/// with a mock sender.
pub(crate) async fn send_message<S>(sender: &Arc<Mutex<S>>, msg: &Message)
where
    S: Sink<WsMessage> + Unpin,
    S::Error: std::error::Error + 'static,
{
    match msg.to_json() {
        Ok(json) => {
            let _ = sender.lock().await.send(WsMessage::Text(json.into())).await;
        }
        Err(e) => {
            tracing::warn!("Failed to serialize outbound message: {}", e);
        }
    }
}

/// Serialize and send a protocol message, surfacing serialization/send errors.
///
/// Generic over the sink type (see [`send_ws_error`]). Use this when the caller
/// must react to a failure (e.g. aborting the auth flow with `?`); use
/// [`send_message`] when a failure can be ignored.
pub(crate) async fn send_message_result<S>(
    sender: &Arc<Mutex<S>>,
    msg: &Message,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: Sink<WsMessage> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    sender
        .lock()
        .await
        .send(WsMessage::Text(msg.to_json()?.into()))
        .await?;
    Ok(())
}

/// Send a `Disconnect` message with the given reason and exit code.
///
/// A send failure is ignored: the connection is already ending, so the caller
/// cannot act on it (mirrors [`send_message`]).
pub(crate) async fn send_disconnect<S>(sender: &Arc<Mutex<S>>, reason: &str, code: i32)
where
    S: Sink<WsMessage> + Unpin,
    S::Error: std::error::Error + 'static,
{
    let msg = Message::Disconnect(DisconnectData {
        reason: reason.to_string(),
        code,
    });
    send_message(sender, &msg).await;
}

/// Report a rejected inbound message to the logs, the audit trail and the
/// client in one place.
///
/// Every validation failure follows the same sequence — a `warn!`, an audit
/// `log_error`, then a protocol `error` frame — so centralising it keeps the
/// three in sync. `fatal` is supplied by the caller because it is
/// phase-dependent: a handshake failure terminates the connection (fatal),
/// whereas a rejected `input`/`resize` after `ready` does not.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn report_rejection<S>(
    state: &AppState,
    ws_sender: &Arc<Mutex<S>>,
    remote_addr: &str,
    session_id: &str,
    code: &str,
    message: String,
    fatal: bool,
) where
    S: Sink<WsMessage> + Unpin,
    S::Error: std::error::Error + 'static,
{
    warn!("{}", message);
    state
        .audit_logger
        .log_error(remote_addr, session_id, &message)
        .await;
    let _ = send_ws_error(ws_sender, code, message, fatal).await;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::protocol::*;
    use axum::http::HeaderMap;
    use futures::StreamExt;
    use futures::channel::mpsc;
    use std::net::{IpAddr, Ipv4Addr};

    fn make_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (k, v) in pairs {
            headers.insert(
                k.parse::<axum::http::header::HeaderName>().unwrap(),
                v.parse().unwrap(),
            );
        }
        headers
    }

    #[test]
    fn test_extract_real_ip_no_proxy() {
        let headers = HeaderMap::new();
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, false), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_trust_disabled_ignores_headers() {
        let headers = make_headers(&[("x-real-ip", "10.0.0.1")]);
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        // trust_proxy = false → header ignored
        assert_eq!(extract_real_ip(&headers, addr, false), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_x_real_ip() {
        let headers = make_headers(&[("x-real-ip", "10.0.0.1")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.1");
    }

    #[test]
    fn test_extract_real_ip_x_forwarded_for() {
        let headers = make_headers(&[("x-forwarded-for", "10.0.0.1, 10.0.0.2, 10.0.0.3")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.1");
    }

    #[test]
    fn test_extract_real_ip_x_real_ip_takes_priority() {
        let headers = make_headers(&[("x-real-ip", "10.0.0.1"), ("x-forwarded-for", "10.0.0.99")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.1");
    }

    #[test]
    fn test_extract_real_ip_fallback_to_connect_addr() {
        let headers = HeaderMap::new();
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        // trust_proxy = true but no headers → fallback
        assert_eq!(extract_real_ip(&headers, addr, true), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_empty_x_real_ip_falls_back() {
        let headers = make_headers(&[("x-real-ip", ""), ("x-forwarded-for", "10.0.0.5")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        // Empty X-Real-IP → try X-Forwarded-For
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.5");
    }

    #[test]
    fn test_extract_real_ip_whitespace_trimmed() {
        let headers = make_headers(&[("x-real-ip", "  10.0.0.1  ")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.1");
    }

    #[test]
    fn test_extract_real_ip_ipv6() {
        let headers = make_headers(&[("x-real-ip", "2001:db8::1")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "2001:db8::1");
    }

    #[test]
    fn test_extract_real_ip_rejects_non_ip_values() {
        // A non-IP string in the header should be rejected
        let headers = make_headers(&[("x-real-ip", "not-an-ip-address")]);
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, true), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_rejects_hostname() {
        // A hostname is not a valid IP
        let headers = make_headers(&[("x-forwarded-for", "attacker.example.com")]);
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, true), "192.168.1.100");
    }

    // ── extract_real_ip: 新增边界用例 ──────────────────────────────

    #[test]
    fn test_extract_real_ip_invalid_x_real_ip_falls_back_to_xff() {
        // 非空但非 IP 的 X-Real-IP 应被拒绝并继续尝试 X-Forwarded-For
        let headers = make_headers(&[("x-real-ip", "not-an-ip"), ("x-forwarded-for", "10.0.0.5")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.5");
    }

    #[test]
    fn test_extract_real_ip_xff_first_entry_invalid_does_not_try_second() {
        // 安全设计：XFF 第一个条目无效时直接回退 connect_addr，
        // 不遍历后续条目（防止攻击者用 "invalid, <真实IP>" 构造）。
        let headers = make_headers(&[("x-forwarded-for", "invalid, 10.0.0.2")]);
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, true), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_xff_empty_string() {
        let headers = make_headers(&[("x-forwarded-for", "")]);
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, true), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_xff_only_commas() {
        let headers = make_headers(&[("x-forwarded-for", ",")]);
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, true), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_xff_entry_with_port() {
        // 带端口的条目不是合法 IpAddr，应回退 connect_addr
        let headers = make_headers(&[("x-forwarded-for", "203.0.113.1:8080")]);
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, true), "192.168.1.100");
    }

    #[test]
    fn test_extract_real_ip_invalid_utf8_header() {
        // 非法 UTF-8 的 header 值：to_str() 返回 Err → 跳过该 header
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-real-ip",
            axum::http::HeaderValue::from_bytes(b"\xff\xfe").unwrap(),
        );
        headers.insert(
            "x-forwarded-for",
            axum::http::HeaderValue::from_bytes(b"10.0.0.5").unwrap(),
        );
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.5");
    }

    #[test]
    fn test_extract_real_ip_x_real_ip_whitespace_only() {
        // trim() 后为空 → 视为无效，继续尝试 X-Forwarded-For
        let headers = make_headers(&[("x-real-ip", "   "), ("x-forwarded-for", "10.0.0.5")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "10.0.0.5");
    }

    #[test]
    fn test_extract_real_ip_xff_first_entry_ipv6() {
        let headers = make_headers(&[("x-forwarded-for", "2001:db8::1, 10.0.0.2")]);
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(extract_real_ip(&headers, addr, true), "2001:db8::1");
    }

    // ── build_rate_limit_key ───────────────────────────────────────

    #[test]
    fn test_build_rate_limit_key_trust_disabled_ignores_socket() {
        // With trust_proxy off the remote_addr already equals the socket
        // address, so the key is just the client IP.
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(
            build_rate_limit_key("192.168.1.100", addr, false),
            "192.168.1.100"
        );
    }

    #[test]
    fn test_build_rate_limit_key_trust_enabled_combines_socket() {
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100));
        assert_eq!(
            build_rate_limit_key("10.0.0.1", addr, true),
            "10.0.0.1|192.168.1.100"
        );
    }

    #[test]
    fn test_build_rate_limit_key_forgery_cannot_reuse_victim_bucket() {
        // A direct client spoofing a victim's X-Real-IP connects from a
        // different socket than the legitimate proxy, so its key differs and
        // it cannot consume the victim's rate-limit budget.
        let proxy = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let attacker = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
        assert_ne!(
            build_rate_limit_key("198.51.100.7", proxy, true),
            build_rate_limit_key("198.51.100.7", attacker, true)
        );
    }

    #[test]
    fn test_build_rate_limit_key_rotated_header_same_socket_still_differs() {
        // Documents the boundary: combining the socket does not make a
        // directly-connected attacker's rotated headers collapse to one
        // bucket — that requires not trusting headers from untrusted peers.
        let addr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
        assert_ne!(
            build_rate_limit_key("10.0.0.1", addr, true),
            build_rate_limit_key("10.0.0.2", addr, true)
        );
    }

    // ── message_type_name: 14 个变体全覆盖 ─────────────────────────

    #[test]
    fn test_message_type_name_all_variants() {
        let cases = [
            (
                Message::Auth(AuthData {
                    method: "basic".to_string(),
                    credentials: "x".to_string(),
                }),
                "auth",
            ),
            (
                Message::Input(InputData {
                    payload: "x".to_string(),
                }),
                "input",
            ),
            (Message::Resize(ResizeData { cols: 80, rows: 24 }), "resize"),
            (Message::Ping(PingData { timestamp: 1 }), "ping"),
            (
                Message::AuthOk(AuthOkData {
                    client_id: "c".to_string(),
                    readonly: false,
                    capabilities: crate::protocol::ClientCapabilities {
                        file_transfer: false,
                        max_upload_size: None,
                    },
                }),
                "auth_ok",
            ),
            (
                Message::AuthFail(AuthFailData {
                    reason: "r".to_string(),
                }),
                "auth_fail",
            ),
            (
                Message::Output(OutputData {
                    payload: "o".to_string(),
                }),
                "output",
            ),
            (Message::Pong(PongData { timestamp: 1 }), "pong"),
            (
                Message::Error(ErrorData {
                    code: "E".to_string(),
                    message: "m".to_string(),
                    fatal: false,
                }),
                "error",
            ),
            (
                Message::Disconnect(DisconnectData {
                    reason: "d".to_string(),
                    code: 0,
                }),
                "disconnect",
            ),
            (
                Message::Ready(ReadyData {
                    session_id: "s".to_string(),
                    cols: 80,
                    rows: 24,
                    readonly: false,
                }),
                "ready",
            ),
            (
                Message::Join(JoinData {
                    session_id: "s".to_string(),
                }),
                "join",
            ),
            (
                Message::FileList(FileListData {
                    path: ".".to_string(),
                    show_hidden: false,
                }),
                "file_list",
            ),
            (
                Message::FileListResult(FileListResultData {
                    path: ".".to_string(),
                    entries: vec![],
                }),
                "file_list_result",
            ),
        ];

        for (msg, expected) in cases {
            assert_eq!(message_type_name(&msg), expected);
        }
    }

    // ── send_ws_error / send_message: 用 mpsc mock 覆盖 ────────────

    #[tokio::test]
    async fn test_send_ws_error_sends_error_message() {
        let (tx, mut rx) = mpsc::unbounded::<WsMessage>();
        let sender = Arc::new(Mutex::new(tx));

        send_ws_error(&sender, "TEST_CODE", "test message".to_string(), true)
            .await
            .unwrap();

        let received = rx.next().await.unwrap();
        match received {
            WsMessage::Text(text) => {
                let parsed = Message::from_json(&text).unwrap();
                match parsed {
                    Message::Error(data) => {
                        assert_eq!(data.code, "TEST_CODE");
                        assert_eq!(data.message, "test message");
                        assert!(data.fatal);
                    }
                    _ => panic!("expected Error message"),
                }
            }
            _ => panic!("expected Text message"),
        }
    }

    #[tokio::test]
    async fn test_send_ws_error_non_fatal() {
        let (tx, mut rx) = mpsc::unbounded::<WsMessage>();
        let sender = Arc::new(Mutex::new(tx));

        send_ws_error(&sender, "C", "m".to_string(), false)
            .await
            .unwrap();

        let received = rx.next().await.unwrap();
        match received {
            WsMessage::Text(text) => {
                let parsed = Message::from_json(&text).unwrap();
                match parsed {
                    Message::Error(data) => {
                        assert_eq!(data.code, "C");
                        assert!(!data.fatal);
                    }
                    _ => panic!("expected Error message"),
                }
            }
            _ => panic!("expected Text message"),
        }
    }

    #[tokio::test]
    async fn test_send_message_sends_serialized_message() {
        let (tx, mut rx) = mpsc::unbounded::<WsMessage>();
        let sender = Arc::new(Mutex::new(tx));

        let msg = Message::Pong(PongData { timestamp: 42 });
        send_message(&sender, &msg).await;

        let received = rx.next().await.unwrap();
        match received {
            WsMessage::Text(text) => {
                assert!(text.contains(r#""type":"pong""#));
                assert!(text.contains(r#""timestamp":42"#));
            }
            _ => panic!("expected Text message"),
        }
    }

    // ── send_message_result / send_disconnect ───────────────────────

    #[tokio::test]
    async fn test_send_message_result_sends_and_reports_ok() {
        let (tx, mut rx) = mpsc::unbounded::<WsMessage>();
        let sender = Arc::new(Mutex::new(tx));

        let msg = Message::AuthFail(AuthFailData {
            reason: "nope".to_string(),
        });
        send_message_result(&sender, &msg).await.unwrap();

        let received = rx.next().await.unwrap();
        match received {
            WsMessage::Text(text) => {
                assert!(text.contains(r#""type":"auth_fail""#));
                assert!(text.contains("nope"));
            }
            _ => panic!("expected Text message"),
        }
    }

    #[tokio::test]
    async fn test_send_disconnect_sends_disconnect_message() {
        let (tx, mut rx) = mpsc::unbounded::<WsMessage>();
        let sender = Arc::new(Mutex::new(tx));

        send_disconnect(&sender, "Shell exited", 0).await;

        let received = rx.next().await.unwrap();
        match received {
            WsMessage::Text(text) => match Message::from_json(&text).unwrap() {
                Message::Disconnect(data) => {
                    assert_eq!(data.reason, "Shell exited");
                    assert_eq!(data.code, 0);
                }
                _ => panic!("expected Disconnect message"),
            },
            _ => panic!("expected Text message"),
        }
    }

    // ── client_rate_limit_key ───────────────────────────────────────

    #[test]
    fn test_client_rate_limit_key_matches_individual_helpers() {
        let addr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        // trust_proxy disabled: the IP is the socket address, key is that IP.
        let headers = HeaderMap::new();
        let (ip, key) = client_rate_limit_key(&headers, addr, false);
        assert_eq!(ip, "127.0.0.1");
        assert_eq!(key, "127.0.0.1");

        // trust_proxy enabled with a header: the key mixes IP and socket.
        let headers = make_headers(&[("x-real-ip", "10.0.0.1")]);
        let (ip, key) = client_rate_limit_key(&headers, addr, true);
        assert_eq!(ip, "10.0.0.1");
        assert_eq!(key, "10.0.0.1|127.0.0.1");
    }
}
