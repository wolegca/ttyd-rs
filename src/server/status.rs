/// Server status quick-view endpoint (`GET /api/status`).
///
/// Aggregates application state (version, uptime, connection/session
/// counters, the ttyd-rs process's own footprint) with host-level metrics
/// read directly from `/proc`. The project is Linux-only, so no portable
/// crate (e.g. `sysinfo`) is pulled in — the files we need are cheap to
/// parse:
///
/// - `/proc/self/status`  → `VmRSS` (application resident memory, kB)
/// - `/proc/self/stat`    → `utime`/`stime` (application CPU ticks, sampled)
/// - `/proc/meminfo`      → `MemTotal`/`MemAvailable` (host memory)
/// - `/proc/stat`         → aggregate `cpu` line (host CPU ticks, sampled)
/// - `/proc/loadavg`      → 1/5/15-minute load averages
/// - `/proc/uptime`       → host uptime
/// - `/proc/sys/kernel/osrelease` → kernel release
///
/// CPU usage cannot be derived from a single read, so a background sampler
/// (see [`spawn_cpu_sampler`]) recomputes both the process and host figures
/// every few seconds and stores the results as fixed-point milli-percent in
/// `AtomicU32`s; requests just read the latest values.
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

use axum::Json;
use axum::extract::State;
use serde::Serialize;

use super::api::ApiState;

/// How often the CPU sampler wakes up to take new `/proc/self/stat` and
/// `/proc/stat` readings.
const CPU_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Clock ticks per second used by the kernel for `utime`/`stime`.
///
/// Linux user-space ABI fixes this at 100 (`USER_HZ`); reading it via
/// `sysconf(_SC_CLK_TCK)` would only ever confirm the constant.
const CLK_TCK: f64 = 100.0;

/// Shared state backing the status endpoint.
#[derive(Clone)]
pub struct StatusState {
    /// Same counter instance as `AppState::active_connections` (live
    /// WebSocket connections), shared so the numbers cannot diverge.
    pub active_connections: Arc<AtomicUsize>,
    /// Captured when the server starts; used for the uptime field.
    pub started_at: Instant,
    /// Latest process CPU usage as percent × 1000 (milli-percent),
    /// maintained by the sampler task. 0 until the second sample lands.
    pub cpu_percent_milli: Arc<AtomicU32>,
    /// Latest host-wide CPU usage as percent × 1000 (milli-percent),
    /// maintained by the same sampler task. 0 until the second sample lands.
    pub host_cpu_percent_milli: Arc<AtomicU32>,
}

impl Default for StatusState {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusState {
    /// Fresh state with its own (zeroed) counters — used by tests and any
    /// caller that does not need to share `AppState`'s connection counter.
    pub fn new() -> Self {
        Self {
            active_connections: Arc::new(AtomicUsize::new(0)),
            started_at: Instant::now(),
            cpu_percent_milli: Arc::new(AtomicU32::new(0)),
            host_cpu_percent_milli: Arc::new(AtomicU32::new(0)),
        }
    }
}

/// Response for `GET /api/status`.
///
/// Metrics are split in two groups so callers can tell the ttyd-rs
/// process's own footprint apart from the host it runs on:
/// [`ApplicationStatus`] vs [`ServerStatus`].
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub version: String,
    /// How long the ttyd-rs process has been running.
    pub uptime_seconds: u64,
    pub sessions: SessionsStatus,
    pub connections: ConnectionsStatus,
    /// ttyd-rs process metrics.
    pub application: ApplicationStatus,
    /// Host-level metrics; individual fields are omitted when `/proc` cannot
    /// be read (never fails the whole request).
    pub server: ServerStatus,
}

#[derive(Debug, Serialize)]
pub struct SessionsStatus {
    pub total: usize,
    pub isolated: usize,
    pub shared: usize,
    pub clients: usize,
}

#[derive(Debug, Serialize)]
pub struct ConnectionsStatus {
    pub active: usize,
    pub max: usize,
}

/// ttyd-rs process metrics.
#[derive(Debug, Serialize)]
pub struct ApplicationStatus {
    /// Resident set size of the ttyd-rs process, in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
    /// Process CPU usage in percent (can exceed 100 on multi-core).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
}

/// Host-level metrics.
#[derive(Debug, Serialize)]
pub struct ServerStatus {
    /// Host-wide CPU usage in percent (0..=100 across all cores).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<f64>,
    /// 1/5/15-minute system load averages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_average: Option<[f64; 3]>,
    /// Host memory: total, used, and available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryStatus>,
    /// How long the host has been up, in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_seconds: Option<u64>,
    /// Kernel release (e.g. `6.8.0-45-generic`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
}

/// Host memory snapshot, in bytes.
#[derive(Debug, Serialize)]
pub struct MemoryStatus {
    pub total_bytes: u64,
    /// `total - available`, matching what `free(1)` reports as "used" for
    /// the application-visible figure (buffers/cache excluded).
    pub used_bytes: u64,
    pub available_bytes: u64,
}

/// Serve the server status snapshot.
pub async fn get_status(State(state): State<ApiState>) -> Json<StatusResponse> {
    let stats = state.session_manager.stats().await;

    Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_seconds: state.status.started_at.elapsed().as_secs(),
        sessions: SessionsStatus {
            total: stats.total_sessions,
            isolated: stats.isolated_sessions,
            shared: stats.shared_sessions,
            clients: stats.total_clients,
        },
        connections: ConnectionsStatus {
            active: state.status.active_connections.load(Ordering::Relaxed),
            max: state.config.max_connections,
        },
        application: ApplicationStatus {
            rss_bytes: read_rss_bytes(),
            cpu_percent: Some(
                state.status.cpu_percent_milli.load(Ordering::Relaxed) as f64 / 1000.0,
            ),
        },
        server: ServerStatus {
            cpu_percent: Some(
                state.status.host_cpu_percent_milli.load(Ordering::Relaxed) as f64 / 1000.0,
            ),
            load_average: read_loadavg(),
            memory: read_memory_status(),
            uptime_seconds: read_host_uptime_seconds(),
            kernel: read_kernel_release(),
        },
    })
}

/// Spawn the background CPU sampler for both the process and the host.
///
/// Takes a first reading immediately, then every `CPU_SAMPLE_INTERVAL`
/// computes the busy-ticks delta and stores it as milli-percent:
///
/// - process: `utime`+`stime` delta from `/proc/self/stat` divided by wall
///   time (can exceed 100% on multi-core);
/// - host: busy-ticks delta / total-ticks delta from the aggregate `cpu`
///   line of `/proc/stat` (bounded to 100%).
///
/// The returned handle should be aborted on server shutdown (same pattern
/// as the rate-limiter cleanup task in `http.rs`).
pub fn spawn_cpu_sampler(
    process_cpu_percent_milli: Arc<AtomicU32>,
    host_cpu_percent_milli: Arc<AtomicU32>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(CPU_SAMPLE_INTERVAL);
        let mut previous_process: Option<(u64, Instant)> = None;
        let mut previous_host: Option<(u64, u64)> = None; // (busy, total) ticks

        loop {
            interval.tick().await;

            if let Some(ticks) = read_process_cpu_ticks() {
                let now = Instant::now();
                if let Some((prev_ticks, prev_at)) = previous_process {
                    let busy = ticks.saturating_sub(prev_ticks) as f64 / CLK_TCK;
                    let elapsed = now.saturating_duration_since(prev_at).as_secs_f64();
                    if elapsed > 0.0 {
                        let percent = busy / elapsed * 100.0;
                        process_cpu_percent_milli.store(
                            (percent * 1000.0).clamp(0.0, u32::MAX as f64) as u32,
                            Ordering::Relaxed,
                        );
                    }
                }
                previous_process = Some((ticks, now));
            }
            // else: unreadable /proc (container stripped?) — keep the last
            // value rather than zeroing out, and retry next round.

            if let Some((busy, total)) = read_host_cpu_ticks() {
                if let Some((prev_busy, prev_total)) = previous_host {
                    let busy_delta = busy.saturating_sub(prev_busy) as f64;
                    let total_delta = total.saturating_sub(prev_total) as f64;
                    if total_delta > 0.0 {
                        let percent = busy_delta / total_delta * 100.0;
                        host_cpu_percent_milli.store(
                            (percent * 1000.0).clamp(0.0, u32::MAX as f64) as u32,
                            Ordering::Relaxed,
                        );
                    }
                }
                previous_host = Some((busy, total));
            }
        }
    })
}

/// Read this process's resident memory from `/proc/self/status`.
///
/// Returns bytes; `None` when the file or `VmRSS` line is missing or
/// malformed.
fn read_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_rss_bytes(&status)
}

/// Extract `VmRSS` (reported in kB) from the contents of `/proc/self/status`.
fn parse_rss_bytes(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    // Format: "VmRSS:\t  12345 kB"
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb.saturating_mul(1024))
}

/// Read host memory totals from `/proc/meminfo`.
///
/// Returns bytes; `None` when the file or the `MemTotal`/`MemAvailable`
/// lines are missing or malformed.
fn read_memory_status() -> Option<MemoryStatus> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_memory_status(&content)
}

/// Extract `MemTotal`/`MemAvailable` (reported in kB) from `/proc/meminfo`
/// contents and derive `used = total - available`.
fn parse_memory_status(content: &str) -> Option<MemoryStatus> {
    let mut total_kb: Option<u64> = None;
    let mut available_kb: Option<u64> = None;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = rest.split_whitespace().next()?.parse().ok();
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available_kb = rest.split_whitespace().next()?.parse().ok();
        }
    }

    let total = total_kb?.saturating_mul(1024);
    let available = available_kb?.saturating_mul(1024);
    Some(MemoryStatus {
        used_bytes: total.saturating_sub(available),
        total_bytes: total,
        available_bytes: available,
    })
}

/// Read host-wide cumulative CPU ticks from the aggregate `cpu` line of
/// `/proc/stat`.
///
/// Returns `(busy, total)` ticks; `None` on parse failure.
fn read_host_cpu_ticks() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    parse_host_cpu_ticks(&stat)
}

/// Parse `(busy, total)` from the aggregate `cpu` line of `/proc/stat`.
///
/// Fields: `user nice system idle iowait irq softirq steal [guest ...]`.
/// `guest` is already accounted inside `user` on modern kernels, so it is
/// excluded to avoid double counting.
fn parse_host_cpu_ticks(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let ticks: Vec<u64> = fields
        .map(|f| f.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    if ticks.len() < 8 {
        return None;
    }
    let busy = ticks[0]
        .saturating_add(ticks[1])
        .saturating_add(ticks[2])
        .saturating_add(ticks[5])
        .saturating_add(ticks[6])
        .saturating_add(ticks[7]);
    let idle = ticks[3].saturating_add(ticks[4]);
    Some((busy, busy.saturating_add(idle)))
}

/// Read host uptime in seconds from `/proc/uptime` (first field).
fn read_host_uptime_seconds() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/uptime").ok()?;
    parse_uptime_seconds(&content)
}

/// Extract the uptime in whole seconds from `/proc/uptime` contents.
fn parse_uptime_seconds(content: &str) -> Option<u64> {
    let uptime: f64 = content.split_whitespace().next()?.parse().ok()?;
    Some(uptime.max(0.0) as u64)
}

/// Read the kernel release from `/proc/sys/kernel/osrelease`.
fn read_kernel_release() -> Option<String> {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    let trimmed = release.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Read the 1/5/15-minute load averages from `/proc/loadavg`.
fn read_loadavg() -> Option<[f64; 3]> {
    let content = std::fs::read_to_string("/proc/loadavg").ok()?;
    parse_loadavg(&content)
}

/// Extract the first three load averages from `/proc/loadavg` contents.
fn parse_loadavg(content: &str) -> Option<[f64; 3]> {
    let mut parts = content.split_whitespace();
    let one: f64 = parts.next()?.parse().ok()?;
    let five: f64 = parts.next()?.parse().ok()?;
    let fifteen: f64 = parts.next()?.parse().ok()?;
    Some([one, five, fifteen])
}

/// Read this process's cumulative CPU time from `/proc/self/stat`.
///
/// Returns `(utime, stime)` in clock ticks; `None` on parse failure.
fn read_process_cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    parse_process_cpu_ticks(&stat)
}

/// Parse `(utime, stime)` from a `/proc/self/stat` line.
///
/// The second field (`comm`) is parenthesised and may itself contain
/// spaces and parentheses, so parsing starts after the *last* `)`.
/// Fields 14 (utime) and 15 (stime) are then the 12th/13th tokens of
/// the remainder (field 3 is the first).
fn parse_process_cpu_ticks(stat: &str) -> Option<u64> {
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    // Skip state..ppid.. (fields 3..13): indices 0..10 of the remainder.
    for _ in 0..11 {
        fields.next()?;
    }
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime.saturating_add(stime))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rss_bytes() {
        let status = "Name:\ttyd-rs\nVmPeak:\t 100 kB\nVmRSS:\t  12345 kB\n";
        assert_eq!(parse_rss_bytes(status), Some(12345 * 1024));
        // Tolerates processes whose comm leaks into weird formats.
        assert_eq!(
            parse_rss_bytes("VmRSS:\t1 kB\n"),
            Some(1024),
            "minimal single-kB line"
        );
    }

    #[test]
    fn test_parse_rss_bytes_missing() {
        assert_eq!(parse_rss_bytes("Name:\ttyd-rs\n"), None);
        assert_eq!(parse_rss_bytes("VmRSS:\tnot-a-number kB\n"), None);
    }

    #[test]
    fn test_parse_loadavg() {
        assert_eq!(
            parse_loadavg("0.10 0.20 0.30 1/500 12345\n"),
            Some([0.10, 0.20, 0.30])
        );
        assert_eq!(parse_loadavg("1.00 2.00"), None);
        assert_eq!(parse_loadavg(""), None);
    }

    #[test]
    fn test_parse_memory_status() {
        let meminfo = "MemTotal:       16384000 kB\nMemFree:         1000000 kB\nMemAvailable:   12000000 kB\nBuffers:          200000 kB\n";
        let mem = parse_memory_status(meminfo).unwrap();
        assert_eq!(mem.total_bytes, 16384000 * 1024);
        assert_eq!(mem.available_bytes, 12000000 * 1024);
        assert_eq!(mem.used_bytes, (16384000 - 12000000) * 1024);
    }

    #[test]
    fn test_parse_memory_status_missing() {
        assert!(parse_memory_status("MemTotal: 1024 kB\n").is_none());
        assert!(parse_memory_status("MemAvailable: 512 kB\n").is_none());
        assert!(parse_memory_status("").is_none());
        // Non-numeric values are rejected, not zeroed.
        assert!(parse_memory_status("MemTotal: n/a kB\nMemAvailable: 512 kB\n").is_none());
    }

    #[test]
    fn test_parse_host_cpu_ticks() {
        // user nice system idle iowait irq softirq steal
        let stat = "cpu  100 0 50 800 10 5 5 0\ncpu0 50 0 25 400 5 2 2 0\n";
        // busy = 100+0+50+5+5+0 = 160, idle = 800+10 = 810, total = 970
        assert_eq!(parse_host_cpu_ticks(stat), Some((160, 970)));
    }

    #[test]
    fn test_parse_host_cpu_ticks_missing() {
        assert_eq!(
            parse_host_cpu_ticks("cpu  1 2 3 4\n"),
            None,
            "too few fields"
        );
        assert_eq!(
            parse_host_cpu_ticks("intr 1 2 3\n"),
            None,
            "not the cpu line"
        );
        assert_eq!(parse_host_cpu_ticks(""), None);
    }

    #[test]
    fn test_parse_host_cpu_ticks_real_file() {
        let (busy, total) = read_host_cpu_ticks().unwrap();
        assert!(busy > 0);
        assert!(total >= busy);
    }

    #[test]
    fn test_parse_uptime_seconds() {
        assert_eq!(parse_uptime_seconds("123456.78 234567.89\n"), Some(123456));
        assert_eq!(parse_uptime_seconds("0.42 0.00\n"), Some(0));
        assert_eq!(parse_uptime_seconds(""), None);
    }

    #[test]
    fn test_read_kernel_release_real_file() {
        // The current test host must expose its kernel release.
        assert!(read_kernel_release().is_some());
    }

    #[test]
    fn test_parse_process_cpu_ticks_with_spaces_in_comm() {
        // comm containing spaces and a parenthesis (field 2), then the
        // usual numeric fields. Fields 3..13 are placeholders here.
        let stat = "42 (some (weird) name) S 1 0 0 0 0 0 0 0 0 0 100 200 0 0";
        assert_eq!(parse_process_cpu_ticks(stat), Some(300));
    }

    #[test]
    fn test_parse_process_cpu_ticks_real_file() {
        // The current test process must have a parseable /proc/self/stat.
        let ticks = read_process_cpu_ticks();
        assert!(ticks.is_some(), "/proc/self/stat should parse");
    }

    #[test]
    fn test_status_state_new() {
        let state = StatusState::new();
        assert_eq!(state.active_connections.load(Ordering::Relaxed), 0);
        assert_eq!(state.cpu_percent_milli.load(Ordering::Relaxed), 0);
        assert_eq!(state.host_cpu_percent_milli.load(Ordering::Relaxed), 0);
    }
}
