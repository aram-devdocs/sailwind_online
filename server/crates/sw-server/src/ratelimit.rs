//! Per-(player, port) market-trade rate limiter.
//!
//! A minimum-interval throttle: a given player may have at most one *new*
//! accepted trade per port within [`RateLimiter::min_interval_ms`]. It is
//! deliberately separate from the idempotency dedup in [`sw_econ::Market`] —
//! the caller checks dedup first, so an idempotent replay of an already-applied
//! trade is never throttled; only genuinely new trades are.
//!
//! The interval is bounded/saturating at construction time (see
//! [`crate::config::Config::trade_min_interval_ms_i64`]), so the window math
//! never overflows.

use std::collections::HashMap;

/// Tracks the last accepted trade time per `(player, port)` and admits a new
/// trade only once the configured interval has elapsed.
#[derive(Debug)]
pub struct RateLimiter {
    min_interval_ms: i64,
    last: HashMap<(u64, u32), i64>,
}

impl RateLimiter {
    /// Build a limiter with the given per-(player, port) minimum interval in
    /// milliseconds. A non-positive interval disables throttling.
    pub fn new(min_interval_ms: i64) -> RateLimiter {
        RateLimiter {
            min_interval_ms,
            last: HashMap::new(),
        }
    }

    /// Try to admit a new trade by `player` at `port` at time `now_ms`.
    ///
    /// Returns `true` and records `now_ms` when the trade is allowed; returns
    /// `false` without recording when it falls inside the throttle window, so a
    /// rejected attempt never extends the cooldown.
    pub fn allow(&mut self, player: u64, port: u32, now_ms: i64) -> bool {
        if let Some(&last) = self.last.get(&(player, port)) {
            if now_ms.saturating_sub(last) < self.min_interval_ms {
                return false;
            }
        }
        self.last.insert((player, port), now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_trade_is_always_allowed() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 10, 1000));
    }

    #[test]
    fn a_second_trade_inside_the_window_is_rejected() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 10, 1000));
        // 100ms later, still inside the 250ms window -> throttled.
        assert!(!rl.allow(1, 10, 1100));
        // A rejected attempt must not push the cooldown out: once the original
        // window elapses the next trade is admitted.
        assert!(rl.allow(1, 10, 1250));
    }

    #[test]
    fn trade_after_the_window_is_allowed() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 10, 1000));
        assert!(rl.allow(1, 10, 1300));
    }

    #[test]
    fn limits_are_independent_per_player_and_per_port() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 10, 1000));
        // Same player, different port: independent bucket.
        assert!(rl.allow(1, 11, 1000));
        // Different player, same port: independent bucket.
        assert!(rl.allow(2, 10, 1000));
        // The original bucket is still throttled.
        assert!(!rl.allow(1, 10, 1050));
    }

    #[test]
    fn zero_interval_never_throttles() {
        let mut rl = RateLimiter::new(0);
        assert!(rl.allow(1, 10, 1000));
        assert!(rl.allow(1, 10, 1000));
    }
}
