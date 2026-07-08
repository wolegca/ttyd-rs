#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

/// Integration tests for heartbeat and reconnection mechanisms
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Helper to start a test server instance
async fn start_test_server() -> (tokio::task::JoinHandle<()>, u16) {
    use ttyd_rs::config::Config;
    use ttyd_rs::server::http::start_server;

    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        command: vec!["bash".to_string()],
        ..Default::default()
    };

    let port = config.bind.port();
    let handle = tokio::spawn(async move {
        let _ = start_server(config).await;
    });

    // Give server time to start
    sleep(Duration::from_millis(500)).await;

    (handle, port)
}

#[tokio::test]
#[ignore] // Run with `cargo test -- --ignored`
async fn test_heartbeat_ping_pong() {
    let (_server, port) = start_test_server().await;
    let url = format!("ws://127.0.0.1:{}/ws", port);

    let (ws_stream, _) = connect_async(&url).await.expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();

    // Send resize first (required by server)
    let resize_msg = serde_json::json!({
        "type": "resize",
        "data": { "cols": 80, "rows": 24 }
    });
    write
        .send(Message::Text(resize_msg.to_string().into()))
        .await
        .unwrap();

    // Wait for ready message
    let ready = timeout(Duration::from_secs(2), read.next()).await;
    assert!(ready.is_ok(), "Should receive ready message");

    // Send ping
    let ping_msg = serde_json::json!({
        "type": "ping",
        "data": { "timestamp": 12345 }
    });
    write
        .send(Message::Text(ping_msg.to_string().into()))
        .await
        .unwrap();

    // Should receive pong within 1 second
    let pong = timeout(Duration::from_secs(1), read.next()).await;
    assert!(pong.is_ok(), "Should receive pong response");

    if let Ok(Some(Ok(Message::Text(text)))) = pong {
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["type"], "pong");
        assert_eq!(parsed["data"]["timestamp"], 12345);
    }
}

#[tokio::test]
#[ignore]
async fn test_multiple_pings() {
    let (_server, port) = start_test_server().await;
    let url = format!("ws://127.0.0.1:{}/ws", port);

    let (ws_stream, _) = connect_async(&url).await.expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();

    // Send resize
    let resize_msg = serde_json::json!({
        "type": "resize",
        "data": { "cols": 80, "rows": 24 }
    });
    write
        .send(Message::Text(resize_msg.to_string().into()))
        .await
        .unwrap();

    // Wait for ready
    let _ = timeout(Duration::from_secs(2), read.next()).await;

    // Send multiple pings
    for i in 0..5 {
        let ping_msg = serde_json::json!({
            "type": "ping",
            "data": { "timestamp": i }
        });
        write
            .send(Message::Text(ping_msg.to_string().into()))
            .await
            .unwrap();

        // Should receive corresponding pong
        let pong = timeout(Duration::from_millis(500), read.next()).await;
        assert!(pong.is_ok(), "Should receive pong for ping {}", i);
    }
}

#[tokio::test]
#[ignore]
async fn test_heartbeat_timeout_not_triggered_with_regular_pings() {
    let (_server, port) = start_test_server().await;
    let url = format!("ws://127.0.0.1:{}/ws", port);

    let (ws_stream, _) = connect_async(&url).await.expect("Failed to connect");
    let (mut write, mut read) = ws_stream.split();

    // Send resize
    let resize_msg = serde_json::json!({
        "type": "resize",
        "data": { "cols": 80, "rows": 24 }
    });
    write
        .send(Message::Text(resize_msg.to_string().into()))
        .await
        .unwrap();

    // Wait for ready
    let _ = timeout(Duration::from_secs(2), read.next()).await;

    // Send pings every 20 seconds for 60 seconds total
    // This should prevent timeout (timeout is 90 seconds)
    for i in 0..3 {
        sleep(Duration::from_secs(20)).await;

        let ping_msg = serde_json::json!({
            "type": "ping",
            "data": { "timestamp": i }
        });
        write
            .send(Message::Text(ping_msg.to_string().into()))
            .await
            .unwrap();

        // Verify we get pong back
        let pong = timeout(Duration::from_secs(1), read.next()).await;
        assert!(pong.is_ok(), "Should still be connected at iteration {}", i);
    }
}

#[tokio::test]
#[ignore]
async fn test_session_cleanup_after_reconnect_window() {
    use std::sync::Arc;
    use ttyd_rs::session::{SessionManager, SessionMode};

    // Create session manager with short reconnect window for testing
    let manager = Arc::new(
        SessionManager::new(Duration::from_secs(300), SessionMode::Isolated)
            .with_reconnect_window(Duration::from_secs(2)),
    );

    // Create a session
    let session = manager
        .create_session(
            "test-session".to_string(),
            &["bash".to_string()],
            None,
            80,
            24,
            None,
        )
        .await
        .unwrap();

    // Add a client
    let client = ttyd_rs::session::Client {
        client_id: "client1".to_string(),
        remote_addr: "127.0.0.1".to_string(),
        username: None,
        connected_at: std::time::Instant::now(),
        readonly: false,
    };
    session.add_client(client).await.unwrap();

    // Remove client (session becomes empty)
    session.remove_client("client1").await;
    assert!(session.is_empty().await);

    // Session should still exist within reconnect window
    assert_eq!(manager.session_count().await, 1);

    // Wait for reconnect window to expire
    sleep(Duration::from_secs(3)).await;

    // Run cleanup
    let cleaned = manager.cleanup_inactive().await;
    assert_eq!(cleaned, 1, "Should clean up 1 session");
    assert_eq!(
        manager.session_count().await,
        0,
        "No sessions should remain"
    );
}

#[tokio::test]
#[ignore]
async fn test_reconnect_within_window_reuses_session() {
    use std::sync::Arc;
    use ttyd_rs::session::{Client, SessionManager, SessionMode};

    let manager = Arc::new(
        SessionManager::new(Duration::from_secs(300), SessionMode::SharedReadWrite)
            .with_reconnect_window(Duration::from_secs(10)),
    );

    // Create initial session
    let session_id = "reconnect-test".to_string();
    let session = manager
        .create_session(
            session_id.clone(),
            &["bash".to_string()],
            None,
            80,
            24,
            Some(SessionMode::SharedReadWrite),
        )
        .await
        .unwrap();

    // Add first client
    let client1 = Client {
        client_id: "client1".to_string(),
        remote_addr: "127.0.0.1".to_string(),
        username: None,
        connected_at: std::time::Instant::now(),
        readonly: false,
    };
    session.add_client(client1).await.unwrap();

    // Client disconnects
    session.remove_client("client1").await;

    // Wait a bit (but less than reconnect window)
    sleep(Duration::from_secs(2)).await;

    // Cleanup should not remove the session yet
    let cleaned = manager.cleanup_inactive().await;
    assert_eq!(cleaned, 0);

    // Get session again (simulating reconnect)
    let session_again = manager.get_session(&session_id).await;
    assert!(
        session_again.is_some(),
        "Session should still exist for reconnection"
    );

    // Add client back
    let client2 = Client {
        client_id: "client2".to_string(),
        remote_addr: "127.0.0.1".to_string(),
        username: None,
        connected_at: std::time::Instant::now(),
        readonly: false,
    };
    session_again.unwrap().add_client(client2).await.unwrap();
}
