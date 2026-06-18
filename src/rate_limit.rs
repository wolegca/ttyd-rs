/// Rate limiting module for preventing brute force attacks
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Rate limiter for tracking request rates per client
#[derive(Clone)]
pub struct RateLimiter {
    /// Storage for rate limit data
    store: Arc<RwLock<HashMap<String, RateLimitEntry>>>,
    /// Maximum requests allowed
    max_requests: u32,
    /// Time window for rate limiting
    window: Duration,
}

#[derive(Debug, Clone)]
struct RateLimitEntry {
    /// Number of requests in current window
    count: u32,
    /// Start of current window
    window_start: Instant,
    /// When the client was first blocked (if blocked)
    blocked_until: Option<Instant>,
}

impl RateLimiter {
    /// Create a new rate limiter
    ///
    /// # Arguments
    /// * `max_requests` - Maximum number of requests allowed in the time window
    /// * `window_secs` - Time window in seconds
    pub fn new(max_requests: u32, window_secs: u64) -> Self {
        Self {
            store: Arc::new(RwLock::new(HashMap::new())),
            max_requests,
            window: Duration::from_secs(window_secs),
        }
    }

    /// Check if a client should be allowed to proceed
    ///
    /// # Arguments
    /// * `client_id` - Unique identifier for the client (e.g., IP address)
    ///
    /// # Returns
    /// * `Ok(())` if the request is allowed
    /// * `Err(Duration)` if rate limited, with time until unblocked
    pub async fn check(&self, client_id: &str) -> Result<(), Duration> {
        let mut store = self.store.write().await;
        let now = Instant::now();

        let entry = store
            .entry(client_id.to_string())
            .or_insert(RateLimitEntry {
                count: 0,
                window_start: now,
                blocked_until: None,
            });

        // Check if currently blocked
        if let Some(blocked_until) = entry.blocked_until {
            if now < blocked_until {
                return Err(blocked_until.duration_since(now));
            } else {
                // Unblock and reset
                entry.blocked_until = None;
                entry.count = 0;
                entry.window_start = now;
            }
        }

        // Check if window has expired
        if now.duration_since(entry.window_start) >= self.window {
            // Reset window
            entry.count = 0;
            entry.window_start = now;
        }

        // Increment counter
        entry.count += 1;

        // Check if limit exceeded
        if entry.count > self.max_requests {
            // Block for the remainder of the window plus an additional window
            let blocked_duration = self.window
                + self
                    .window
                    .saturating_sub(now.duration_since(entry.window_start));
            entry.blocked_until = Some(now + blocked_duration);
            return Err(blocked_duration);
        }

        Ok(())
    }

    /// Manually reset rate limit for a client (e.g., after successful auth)
    pub async fn reset(&self, client_id: &str) {
        let mut store = self.store.write().await;
        store.remove(client_id);
    }

    /// Clean up expired entries (should be called periodically)
    pub async fn cleanup(&self) {
        let mut store = self.store.write().await;
        let now = Instant::now();

        store.retain(|_, entry| {
            // Keep entries that are still in their window or blocked
            if let Some(blocked_until) = entry.blocked_until {
                now < blocked_until
            } else {
                now.duration_since(entry.window_start) < self.window * 2
            }
        });
    }

    /// Get current stats (for monitoring)
    #[allow(dead_code)]
    pub async fn stats(&self) -> RateLimiterStats {
        let store = self.store.read().await;
        let now = Instant::now();

        let mut active_clients = 0;
        let mut blocked_clients = 0;

        for entry in store.values() {
            if let Some(blocked_until) = entry.blocked_until {
                if now < blocked_until {
                    blocked_clients += 1;
                }
            } else if now.duration_since(entry.window_start) < self.window {
                active_clients += 1;
            }
        }

        RateLimiterStats {
            active_clients,
            blocked_clients,
            total_tracked: store.len(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RateLimiterStats {
    pub active_clients: usize,
    pub blocked_clients: usize,
    pub total_tracked: usize,
}

impl Default for RateLimiter {
    fn default() -> Self {
        // Default: 10 auth attempts per 60 seconds
        Self::new(10, 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_rate_limiter_allows_within_limit() {
        let limiter = RateLimiter::new(5, 10);

        for _ in 0..5 {
            assert!(limiter.check("client1").await.is_ok());
        }
    }

    #[tokio::test]
    async fn test_rate_limiter_blocks_over_limit() {
        let limiter = RateLimiter::new(3, 10);

        for _ in 0..3 {
            assert!(limiter.check("client1").await.is_ok());
        }

        // 4th request should be blocked
        assert!(limiter.check("client1").await.is_err());
    }

    #[tokio::test]
    async fn test_rate_limiter_different_clients() {
        let limiter = RateLimiter::new(2, 10);

        assert!(limiter.check("client1").await.is_ok());
        assert!(limiter.check("client2").await.is_ok());
        assert!(limiter.check("client1").await.is_ok());
        assert!(limiter.check("client2").await.is_ok());

        // Both clients should now be at limit
        assert!(limiter.check("client1").await.is_err());
        assert!(limiter.check("client2").await.is_err());
    }

    #[tokio::test]
    async fn test_rate_limiter_reset() {
        let limiter = RateLimiter::new(2, 10);

        assert!(limiter.check("client1").await.is_ok());
        assert!(limiter.check("client1").await.is_ok());
        assert!(limiter.check("client1").await.is_err());

        // Reset the client
        limiter.reset("client1").await;

        // Should be able to make requests again
        assert!(limiter.check("client1").await.is_ok());
    }

    #[tokio::test]
    async fn test_rate_limiter_window_expiry() {
        let limiter = RateLimiter::new(2, 1); // 1 second window

        assert!(limiter.check("client1").await.is_ok());
        assert!(limiter.check("client1").await.is_ok());
        assert!(limiter.check("client1").await.is_err());

        // Wait for blocking period to expire (2 windows = 2 seconds)
        sleep(Duration::from_millis(2100)).await;

        // Should be able to make requests again
        assert!(limiter.check("client1").await.is_ok());
    }

    #[tokio::test]
    async fn test_rate_limiter_stats() {
        let limiter = RateLimiter::new(5, 10);

        limiter.check("client1").await.ok();
        limiter.check("client2").await.ok();

        let stats = limiter.stats().await;
        assert_eq!(stats.active_clients, 2);
        assert_eq!(stats.blocked_clients, 0);
    }
}
