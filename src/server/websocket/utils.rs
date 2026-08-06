/// Utility functions for WebSocket handler: IP extraction, error sending, and message helpers.
use crate::protocol::{ErrorData, Message};
use axum::extract::ws::Message as WsMessage;
use futures::SinkExt;

use super::WsSender;

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
pub(crate) async fn send_ws_error(
    sender: &WsSender,
    code: &str,
    message: String,
    fatal: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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
pub(crate) async fn send_message(sender: &WsSender, msg: &Message) {
    if let Ok(json) = msg.to_json() {
        let _ = sender.lock().await.send(WsMessage::Text(json.into())).await;
    }
}

// WsSender is used via the type alias from mod.rs; this import ensures the
// Arc reference in the type alias is resolved.
#[allow(unused_imports)]
use futures::stream::SplitSink;
#[allow(unused_imports)]
use tokio::sync::Mutex;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
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
}
