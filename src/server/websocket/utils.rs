/// Utility functions for WebSocket handler: IP extraction, error sending, and message helpers.
use crate::protocol::{ErrorData, Message};
use axum::extract::ws::Message as WsMessage;
use futures::{Sink, SinkExt};
use std::sync::Arc;
use tokio::sync::Mutex;

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
/// Silently ignores serialization/send errors — use [`send_ws_error`] when
/// the caller needs to know about failures.
///
/// Generic over the sink type (see [`send_ws_error`]) so it can be unit-tested
/// with a mock sender.
pub(crate) async fn send_message<S>(sender: &Arc<Mutex<S>>, msg: &Message)
where
    S: Sink<WsMessage> + Unpin,
    S::Error: std::error::Error + 'static,
{
    if let Ok(json) = msg.to_json() {
        let _ = sender.lock().await.send(WsMessage::Text(json.into())).await;
    }
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
}
