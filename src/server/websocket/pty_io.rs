/// PTY I/O tasks: reading from PTY, subscribing to output, and heartbeat monitoring.
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::SinkExt;
use tokio::sync::broadcast;
use tracing::{debug, error, warn};

use crate::protocol::{DisconnectData, Message};
use crate::session::Session;

use super::WsSender;
use super::utils::send_ws_error;

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

        let async_fd = match tokio::io::unix::AsyncFd::new(dup_fd) {
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
pub(crate) fn spawn_output_subscriber(
    ws_sender: WsSender,
    mut output_rx: broadcast::Receiver<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
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
/// Returns a `JoinHandle` that resolves to `false` if the heartbeat times out,
/// or is aborted by the caller. The shared `last_ping_time` is updated by the
/// main message loop when a Ping is received.
pub(crate) fn spawn_heartbeat_monitor(
    last_ping_time: Arc<tokio::sync::Mutex<Instant>>,
) -> tokio::task::JoinHandle<bool> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        let heartbeat_timeout = Duration::from_secs(90); // 90 seconds without ping = timeout
        loop {
            interval.tick().await;
            let last = *last_ping_time.lock().await;
            if last.elapsed() > heartbeat_timeout {
                warn!(
                    "Client heartbeat timeout (no ping for {:?})",
                    heartbeat_timeout
                );
                return false; // Timeout occurred
            }
        }
    })
}
