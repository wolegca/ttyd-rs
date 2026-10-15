/// PTY I/O tasks: reading from PTY, subscribing to output, and heartbeat monitoring.
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{Sink, SinkExt};
use tokio::sync::broadcast;
use tracing::{debug, error, warn};

use crate::protocol::{DisconnectData, Message};
use crate::session::Session;

use super::utils::send_ws_error;

/// Interval between server-initiated WebSocket protocol ping frames.
pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Silence budget before a client is considered dead (three missed pings).
pub(crate) const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(90);

/// Duplicate the PTY master fd for writing.
///
/// The caller gets a `tokio::fs::File` that can be used for async writes.
/// The read task does its own `dup` to avoid sharing a file descriptor.
pub(crate) async fn create_pty_writer(
    pty_session: &Arc<tokio::sync::Mutex<crate::pty::PtySession>>,
) -> Result<tokio::fs::File, String> {
    use std::os::fd::BorrowedFd;

    let pty_guard = pty_session.lock().await;
    let master_fd = pty_guard.master_fd();
    let borrowed_fd = unsafe { BorrowedFd::borrow_raw(master_fd) };
    let dup_fd = nix::unistd::dup(borrowed_fd)
        .map_err(|e| format!("Failed to duplicate PTY fd for write: {}", e))?;
    let pty_file = std::fs::File::from(dup_fd);
    Ok(tokio::fs::File::from_std(pty_file))
}

/// Spawn a task to read from the PTY and broadcast output to all subscribers.
///
/// When the PTY reaches EOF (or EIO, which the kernel reports when the shell
/// closes the slave side), the task calls [`Session::mark_pty_exited`] so
/// each subscriber can send an ordered "shell exited" disconnect after its
/// final output burst.
///
/// Returns a `JoinHandle` that should be aborted when the session ends.
pub(crate) fn spawn_pty_reader(
    pty_session: Arc<tokio::sync::Mutex<crate::pty::PtySession>>,
    session: Arc<Session>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use std::os::fd::BorrowedFd;

        let pty_guard = pty_session.lock().await;
        let master_fd = pty_guard.master_fd();

        // Duplicate the file descriptor so we have our own independent fd
        // This prevents double-close issues when the File is dropped
        let borrowed_fd = unsafe { BorrowedFd::borrow_raw(master_fd) };
        let dup_fd = match nix::unistd::dup(borrowed_fd) {
            Ok(fd) => fd,
            Err(e) => {
                error!("Failed to duplicate PTY fd: {}", e);
                return;
            }
        };

        drop(pty_guard); // Release lock before async operations

        // Set the duplicated fd non-blocking and drive reads through AsyncFd.
        // tokio::fs::File runs a synchronous read() on a spawn_blocking thread
        // that abort() cannot interrupt; combined with a shell that outlives
        // the connection, that thread blocks forever and hangs runtime
        // shutdown. AsyncFd makes the read truly async and cancellable.
        let flags = match nix::fcntl::fcntl(&dup_fd, nix::fcntl::FcntlArg::F_GETFL) {
            Ok(f) => f,
            Err(e) => {
                error!("Failed to get PTY fd flags: {}", e);
                return;
            }
        };
        if let Err(e) = nix::fcntl::fcntl(
            &dup_fd,
            nix::fcntl::FcntlArg::F_SETFL(
                nix::fcntl::OFlag::from_bits_truncate(flags) | nix::fcntl::OFlag::O_NONBLOCK,
            ),
        ) {
            error!("Failed to set PTY fd non-blocking: {}", e);
            return;
        }

        // SAFETY: `dup_fd` was just duplicated from the PTY master and the
        // fcntl calls above confirmed it is a valid, open descriptor; it stays
        // open and points at the same file description until the `AsyncFd`
        // (which owns it) is dropped at the end of this task.
        let async_fd = match unsafe { tokio::io::unix::AsyncFd::register(dup_fd) } {
            Ok(fd) => fd,
            Err(e) => {
                error!("Failed to register PTY fd with the reactor: {}", e);
                return;
            }
        };
        let mut tmp = [0u8; 16 * 1024];
        // Cap a single coalesced burst so a runaway process cannot grow one
        // WebSocket frame unboundedly; the loop then drains more on the next
        // readiness wakeup.
        const MAX_BURST: usize = 256 * 1024;

        loop {
            let mut guard = match async_fd.readable().await {
                Ok(guard) => guard,
                Err(e) => {
                    error!("Error waiting for PTY readability: {}", e);
                    break;
                }
            };

            // Drain everything currently available and broadcast it as a
            // single message. Without this, bursty output (cat, builds, ...)
            // costs one epoll wakeup, one channel send, one JSON
            // serialization, and one WebSocket write per 4 KiB chunk.
            let mut burst: Vec<u8> = Vec::with_capacity(4096);
            let mut terminated = false;
            loop {
                match guard.try_io(|inner| {
                    nix::unistd::read(inner.get_ref(), &mut tmp).map_err(std::io::Error::from)
                }) {
                    Ok(Ok(0)) => {
                        debug!("PTY EOF");
                        terminated = true;
                        break;
                    }
                    Ok(Ok(n)) => {
                        burst.extend_from_slice(&tmp[..n]);
                        if burst.len() >= MAX_BURST {
                            break;
                        }
                    }
                    Ok(Err(e)) => {
                        // EIO is expected when the shell exits (Ctrl-D closes the
                        // slave side of the PTY). Treat it as a normal EOF.
                        if e.raw_os_error() == Some(libc::EIO) {
                            debug!("PTY EIO (shell exited)");
                        } else {
                            error!("Error reading from PTY: {}", e);
                        }
                        terminated = true;
                        break;
                    }
                    Err(_would_block) => break,
                }
            }

            if !burst.is_empty() {
                // Broadcast PTY output to all connected clients
                session.broadcast_output(burst);
            }
            if terminated {
                break;
            }
        }

        // Notify subscribers that the shell exited. Dropping the broadcast
        // sender (rather than sending the disconnect directly here) lets each
        // subscriber emit its own disconnect *after* its final output burst —
        // a direct send from this task could race the subscribers' pending
        // sends and reach clients before their last output.
        session.mark_pty_exited();
    })
}

/// Spawn a task to receive broadcast output and forward to this client's WebSocket.
///
/// Returns a `JoinHandle` that should be aborted when the session ends.
pub(crate) fn spawn_output_subscriber<S>(
    ws_sender: Arc<tokio::sync::Mutex<S>>,
    mut output_rx: broadcast::Receiver<Vec<u8>>,
) -> tokio::task::JoinHandle<()>
where
    S: Sink<axum::extract::ws::Message> + Unpin + Send + 'static,
    S::Error: std::error::Error + 'static,
{
    tokio::spawn(async move {
        let mut last_lag_notify: Option<Instant> = None;
        const LAG_NOTIFY_COOLDOWN: Duration = Duration::from_secs(1);
        loop {
            match output_rx.recv().await {
                Ok(data) => {
                    // Fast path: build the Output JSON without serde (see
                    // Message::output_json). Byte-identical to the old
                    // from_utf8_lossy + Message::Output + to_json path.
                    let json = Message::output_json(&data);
                    if ws_sender
                        .lock()
                        .await
                        .send(axum::extract::ws::Message::Text(json.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    let now = Instant::now();
                    let should_notify = match last_lag_notify {
                        Some(last) => now.duration_since(last) >= LAG_NOTIFY_COOLDOWN,
                        None => true,
                    };
                    if should_notify {
                        last_lag_notify = Some(now);
                        warn!("Client lagged by {} messages, notifying", n);
                        if send_ws_error(
                            &ws_sender,
                            "OUTPUT_LAGGED",
                            format!("{} output messages dropped due to slow client", n),
                            false,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // The PTY process exited (or the session was cleaned up):
                    // the session dropped its broadcast sender after all
                    // queued output was delivered. Send the disconnect here,
                    // in this task, so it is ordered after this client's
                    // final output burst.
                    let disconnect = Message::Disconnect(DisconnectData {
                        reason: "Shell exited".to_string(),
                        code: 0,
                    });
                    if let Ok(json) = disconnect.to_json() {
                        let _ = ws_sender
                            .lock()
                            .await
                            .send(axum::extract::ws::Message::Text(json.into()))
                            .await;
                    }
                    break;
                }
            }
        }
    })
}

/// Spawn a heartbeat monitor task.
///
/// The server periodically sends a **WebSocket protocol-level ping frame**.
/// Per the WebSocket spec the client must answer with a pong frame, and
/// browsers do this in their network stack — *independently of page JS* — so
/// the connection stays alive even when the tab is backgrounded and its
/// `setInterval` timers are throttled. This is what fixes the "idle tab drops
/// after a while" problem.
///
/// Liveness is measured by `last_ping_time`, which is refreshed by *either*
/// a protocol pong (handled in the main message loop) or an app-level JSON
/// ping (kept for backward compatibility). If neither arrives within
/// `heartbeat_timeout`, the client is considered dead and the handle
/// resolves to `false`.
///
/// Returns a `JoinHandle` that resolves to `false` on timeout, or is aborted
/// by the caller.
pub(crate) fn spawn_heartbeat_monitor<S>(
    ws_sender: Arc<tokio::sync::Mutex<S>>,
    last_ping_time: Arc<tokio::sync::Mutex<Instant>>,
    interval: Duration,
    heartbeat_timeout: Duration,
) -> tokio::task::JoinHandle<bool>
where
    S: Sink<axum::extract::ws::Message> + Unpin + Send + 'static,
    S::Error: std::error::Error + 'static,
{
    tokio::spawn(async move {
        // `interval`/`heartbeat_timeout` are parameters (rather than hardcoded)
        // so tests can drive the monitor with short durations.
        let mut interval = tokio::time::interval(interval);
        // The first tick of a tokio interval fires immediately. Consume it so
        // the first ping is actually delayed by a full interval — sending a
        // ping at t=0 would race the client's initial reads (e.g. the
        // handshake/ready exchange) and surface as a spurious non-text frame.
        interval.tick().await;
        loop {
            interval.tick().await;

            // Actively probe the client. A send failure means the socket is
            // already gone — treat it as a timeout so we clean up promptly.
            if ws_sender
                .lock()
                .await
                .send(axum::extract::ws::Message::Ping(Default::default()))
                .await
                .is_err()
            {
                warn!("Heartbeat ping send failed, closing connection");
                return false;
            }

            let last = *last_ping_time.lock().await;
            if last.elapsed() > heartbeat_timeout {
                warn!(
                    "Client heartbeat timeout (no ping/pong for {:?})",
                    heartbeat_timeout
                );
                return false; // Timeout occurred
            }
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::protocol::OutputData;
    use crate::session::SessionMode;
    use axum::extract::ws::Message as WsMessage;
    use futures::StreamExt;
    use futures::channel::mpsc;
    use tokio::time::timeout;

    type MockSender = Arc<tokio::sync::Mutex<mpsc::UnboundedSender<WsMessage>>>;

    /// A mock WebSocket sink backed by an unbounded channel, so the spawned
    /// tasks can be driven and observed deterministically.
    fn mock_sender() -> (MockSender, mpsc::UnboundedReceiver<WsMessage>) {
        let (tx, rx) = mpsc::unbounded();
        (Arc::new(tokio::sync::Mutex::new(tx)), rx)
    }

    /// Parse an outbound text frame back into a protocol message.
    fn as_message(msg: WsMessage) -> Message {
        match msg {
            WsMessage::Text(t) => Message::from_json(&t).expect("outbound frame is valid JSON"),
            other => panic!("expected a Text frame, got {other:?}"),
        }
    }

    // ── spawn_output_subscriber ─────────────────────────────────────

    #[tokio::test]
    async fn test_output_subscriber_forwards_output() {
        let (data_tx, _) = broadcast::channel::<Vec<u8>>(16);
        let (sender, mut sink_rx) = mock_sender();
        let handle = spawn_output_subscriber(sender, data_tx.subscribe());

        data_tx.send(b"hello".to_vec()).unwrap();

        let msg = timeout(Duration::from_secs(2), sink_rx.next())
            .await
            .unwrap()
            .unwrap();
        match as_message(msg) {
            Message::Output(OutputData { payload }) => assert_eq!(payload, "hello"),
            other => panic!("expected Output, got {other:?}"),
        }
        handle.abort();
    }

    #[tokio::test]
    async fn test_output_subscriber_reports_lag_once() {
        // Capacity 2, but five messages are queued before the subscriber reads
        // any, so its first observation must be a Lagged notification, which
        // it surfaces as a single OUTPUT_LAGGED error.
        let (data_tx, _) = broadcast::channel::<Vec<u8>>(2);
        let rx = data_tx.subscribe();
        for i in 0..5u8 {
            data_tx.send(vec![i]).unwrap();
        }

        let (sender, mut sink_rx) = mock_sender();
        let handle = spawn_output_subscriber(sender, rx);

        let msg = timeout(Duration::from_secs(2), sink_rx.next())
            .await
            .unwrap()
            .unwrap();
        match as_message(msg) {
            Message::Error(data) => {
                assert_eq!(data.code, "OUTPUT_LAGGED");
                assert!(!data.fatal);
            }
            other => panic!("expected OUTPUT_LAGGED error, got {other:?}"),
        }
        handle.abort();
    }

    #[tokio::test]
    async fn test_output_subscriber_sends_disconnect_after_final_output() {
        let (data_tx, _) = broadcast::channel::<Vec<u8>>(16);
        let (sender, mut sink_rx) = mock_sender();
        let handle = spawn_output_subscriber(sender, data_tx.subscribe());

        data_tx.send(b"final".to_vec()).unwrap();
        // Dropping the last sender closes the channel after queued data drains.
        drop(data_tx);

        // The final output arrives first...
        let first = timeout(Duration::from_secs(2), sink_rx.next())
            .await
            .unwrap()
            .unwrap();
        match as_message(first) {
            Message::Output(OutputData { payload }) => assert_eq!(payload, "final"),
            other => panic!("expected Output first, got {other:?}"),
        }

        // ...and only then the "shell exited" disconnect, preserving order.
        let second = timeout(Duration::from_secs(2), sink_rx.next())
            .await
            .unwrap()
            .unwrap();
        match as_message(second) {
            Message::Disconnect(DisconnectData { reason, .. }) => {
                assert_eq!(reason, "Shell exited");
            }
            other => panic!("expected Disconnect second, got {other:?}"),
        }
        handle.abort();
    }

    // ── spawn_heartbeat_monitor ─────────────────────────────────────

    #[tokio::test]
    async fn test_heartbeat_monitor_sends_protocol_ping() {
        let (sender, mut sink_rx) = mock_sender();
        let last = Arc::new(tokio::sync::Mutex::new(Instant::now()));
        let handle = spawn_heartbeat_monitor(
            sender,
            last,
            Duration::from_millis(20),
            Duration::from_secs(30),
        );

        let msg = timeout(Duration::from_secs(2), sink_rx.next())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(msg, WsMessage::Ping(_)),
            "expected a protocol ping"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn test_heartbeat_monitor_returns_false_on_timeout() {
        // Keep the receiver alive so the periodic ping send succeeds; the
        // timestamp is never refreshed, so the monitor times out quickly.
        let (sender, _rx) = mock_sender();
        let last = Arc::new(tokio::sync::Mutex::new(Instant::now()));
        let handle = spawn_heartbeat_monitor(
            sender,
            last,
            Duration::from_millis(20),
            Duration::from_millis(5),
        );

        let result = timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
        assert!(!result, "monitor must report a timeout");
    }

    #[tokio::test]
    async fn test_heartbeat_monitor_returns_false_when_send_fails() {
        let (sender, rx) = mock_sender();
        // Dropping the receiver makes every ping send fail.
        drop(rx);
        let last = Arc::new(tokio::sync::Mutex::new(Instant::now()));
        let handle = spawn_heartbeat_monitor(
            sender,
            last,
            Duration::from_millis(20),
            Duration::from_secs(30),
        );

        let result = timeout(Duration::from_secs(2), handle)
            .await
            .unwrap()
            .unwrap();
        assert!(!result, "monitor must stop when the socket is gone");
    }

    // ── spawn_pty_reader ────────────────────────────────────────────

    #[tokio::test]
    async fn test_pty_reader_broadcasts_output_and_closes_on_exit() {
        let session = Arc::new(
            Session::new(
                "pty-reader-test".to_string(),
                SessionMode::Isolated,
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf hello".to_string(),
                ],
                None,
                80,
                24,
            )
            .unwrap(),
        );
        let mut rx = session.subscribe_output().unwrap();
        let handle = spawn_pty_reader(session.pty_session(), session.clone());

        // Drain until the reader closes the channel after the shell exits.
        let collected = timeout(Duration::from_secs(5), async {
            let mut buf = String::new();
            loop {
                match rx.recv().await {
                    Ok(bytes) => buf.push_str(&String::from_utf8_lossy(&bytes)),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return buf,
                }
            }
        })
        .await
        .unwrap();

        assert!(collected.contains("hello"), "collected: {collected:?}");
        // After EOF the broadcast slot is gone, so no new subscriber is allowed.
        assert!(session.subscribe_output().is_none());
        handle.await.unwrap();
    }
}
