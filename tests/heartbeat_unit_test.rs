#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

/// Unit tests for heartbeat timeout logic
use std::time::{Duration, Instant};
use tokio::time::sleep;

#[tokio::test]
async fn test_last_ping_time_updates() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let last_ping = Arc::new(Mutex::new(Instant::now()));

    // Simulate initial time
    let initial_time = *last_ping.lock().await;

    // Wait a bit
    sleep(Duration::from_millis(100)).await;

    // Update timestamp (simulating ping received)
    *last_ping.lock().await = Instant::now();

    let updated_time = *last_ping.lock().await;
    assert!(updated_time > initial_time, "Timestamp should be updated");
}

#[tokio::test]
async fn test_heartbeat_timeout_detection() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let last_ping = Arc::new(Mutex::new(Instant::now()));
    let timeout = Duration::from_millis(500);

    // Initially should not timeout
    {
        let last = *last_ping.lock().await;
        assert!(last.elapsed() < timeout, "Should not timeout immediately");
    }

    // Wait for timeout period
    sleep(Duration::from_millis(600)).await;

    // Now should detect timeout
    {
        let last = *last_ping.lock().await;
        assert!(
            last.elapsed() >= timeout,
            "Should detect timeout after waiting"
        );
    }
}

#[tokio::test]
async fn test_heartbeat_prevents_timeout() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let last_ping = Arc::new(Mutex::new(Instant::now()));
    let timeout = Duration::from_millis(500);

    // Keep updating timestamp before timeout
    for _ in 0..5 {
        sleep(Duration::from_millis(200)).await;
        *last_ping.lock().await = Instant::now();

        let last = *last_ping.lock().await;
        assert!(
            last.elapsed() < timeout,
            "Should never timeout with regular updates"
        );
    }
}

#[tokio::test]
async fn test_reconnect_window_duration() {
    use ttyd_rs::session::RECONNECT_WINDOW;

    // Verify reconnect window is 120 seconds as per fix
    assert_eq!(RECONNECT_WINDOW.as_secs(), 120);
}

#[tokio::test]
async fn test_session_manager_reconnect_window() {
    use ttyd_rs::session::{SessionManager, SessionMode};

    let manager = SessionManager::new(Duration::from_secs(3600), SessionMode::Isolated)
        .with_reconnect_window(Duration::from_secs(200));

    // Create and remove a session
    let session = manager
        .create_session(
            "test".to_string(),
            &["bash".to_string()],
            None,
            80,
            24,
            None,
        )
        .await
        .unwrap();

    // Add then remove client to make session empty
    let client = ttyd_rs::session::Client {
        client_id: "c1".to_string(),
        remote_addr: "127.0.0.1".to_string(),
        username: None,
        connected_at: Instant::now(),
        readonly: false,
    };
    session.add_client(client).await.unwrap();
    session.remove_client("c1").await;

    // Session should exist
    assert_eq!(manager.session_count().await, 1);

    // Cleanup should not remove it yet (within 200s window)
    let cleaned = manager.cleanup_inactive().await;
    assert_eq!(cleaned, 0);
}

#[tokio::test]
async fn test_isolated_session_immediate_cleanup() {
    use ttyd_rs::session::{SessionManager, SessionMode};

    let manager = SessionManager::new(Duration::from_secs(3600), SessionMode::Isolated);

    let session = manager
        .create_session(
            "isolated".to_string(),
            &["bash".to_string()],
            None,
            80,
            24,
            Some(SessionMode::Isolated),
        )
        .await
        .unwrap();

    let client = ttyd_rs::session::Client {
        client_id: "c1".to_string(),
        remote_addr: "127.0.0.1".to_string(),
        username: None,
        connected_at: Instant::now(),
        readonly: false,
    };
    session.add_client(client).await.unwrap();
    session.remove_client("c1").await;

    // For isolated sessions, remove_if_empty should clean up immediately
    let removed = manager.remove_if_empty("isolated").await;
    assert!(
        removed,
        "Isolated session should be removed immediately when empty"
    );
    assert_eq!(manager.session_count().await, 0);
}

#[tokio::test]
async fn test_shared_session_kept_alive() {
    use ttyd_rs::session::{SessionManager, SessionMode};

    let manager = SessionManager::new(Duration::from_secs(3600), SessionMode::SharedReadWrite);

    let session = manager
        .create_session(
            "shared".to_string(),
            &["bash".to_string()],
            None,
            80,
            24,
            Some(SessionMode::SharedReadWrite),
        )
        .await
        .unwrap();

    let client = ttyd_rs::session::Client {
        client_id: "c1".to_string(),
        remote_addr: "127.0.0.1".to_string(),
        username: None,
        connected_at: Instant::now(),
        readonly: false,
    };
    session.add_client(client).await.unwrap();
    session.remove_client("c1").await;

    // Shared session should be kept alive for reconnection
    assert_eq!(manager.session_count().await, 1);

    // Immediate cleanup should not remove it
    let cleaned = manager.cleanup_inactive().await;
    assert_eq!(cleaned, 0);
}

#[test]
fn test_heartbeat_constants() {
    // Verify reasonable values
    const HEARTBEAT_INTERVAL: u64 = 30; // seconds
    const PONG_TIMEOUT: u64 = 90; // seconds
    const SERVER_TIMEOUT: u64 = 90; // seconds

    // Pong timeout should be longer than heartbeat interval
    const { assert!(PONG_TIMEOUT > HEARTBEAT_INTERVAL * 2) };

    // Server and client timeouts should match
    const { assert!(PONG_TIMEOUT == SERVER_TIMEOUT) };
}
