//! Aggregate per-session-key rate limiter.
//!
//! A minimum-interval throttle keyed by a connected peer or authenticated
//! player. Callers choose a lifecycle-bounded key, then call
//! [`RateLimiter::clear`] when that peer or player disconnects. This keeps the
//! map bounded to active/session keys without scanning all entries during
//! admission.
//!
//! It is deliberately separate from the idempotency dedup in [`sw_econ::Market`]:
//! the caller checks dedup first, so an idempotent replay of an already-applied
//! trade is never throttled; only genuinely new trades are.
//!
//! The interval is bounded/saturating at construction time (see
//! [`crate::config::Config::trade_min_interval_ms_i64`]), so the window math
//! never overflows.

use std::collections::{hash_map::Entry, HashMap};

/// Tracks the last accepted message time per lifecycle-bounded key and admits a
/// new message only once the configured interval has elapsed.
#[derive(Debug)]
pub struct RateLimiter {
    min_interval_ms: i64,
    last: HashMap<u64, i64>,
}

impl RateLimiter {
    /// Build a limiter with the given aggregate per-key minimum interval in
    /// milliseconds. A non-positive interval disables throttling.
    pub fn new(min_interval_ms: i64) -> RateLimiter {
        RateLimiter {
            min_interval_ms,
            last: HashMap::new(),
        }
    }

    /// Try to admit a new message by `key` at time `now_ms`.
    ///
    /// Returns `true` and records `now_ms` when the message is allowed; returns
    /// `false` without recording when it falls inside the throttle window, so a
    /// rejected attempt never extends the cooldown. A timestamp rollback is
    /// treated as no elapsed time and cannot bypass the window.
    pub fn allow(&mut self, key: u64, now_ms: i64) -> bool {
        if self.min_interval_ms <= 0 {
            return true;
        }

        match self.last.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(now_ms);
                true
            }
            Entry::Occupied(mut entry) => {
                if now_ms.saturating_sub(*entry.get()) < self.min_interval_ms {
                    return false;
                }
                entry.insert(now_ms);
                true
            }
        }
    }

    /// Forget a key's throttle state on disconnect, keeping storage bounded to
    /// active peers or authenticated player sessions.
    pub fn clear(&mut self, key: u64) {
        self.last.remove(&key);
    }

    /// Number of live entries the limiter is tracking. Test-only: it lets the
    /// unit and dispatch tests assert the map stays bounded and is cleared on
    /// disconnect.
    #[cfg(test)]
    pub fn tracked_count(&self) -> usize {
        self.last.len()
    }

    /// Last admitted monotonic timestamp for a key.
    #[cfg(test)]
    pub fn last_accepted_ms(&self, key: u64) -> Option<i64> {
        self.last.get(&key).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_trade_is_always_allowed() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 1000));
    }

    #[test]
    fn a_second_trade_inside_the_window_is_rejected() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 1000));
        assert!(!rl.allow(1, 1100));
        assert!(rl.allow(1, 1250));
    }

    #[test]
    fn trade_after_the_window_is_allowed() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 1000));
        assert!(rl.allow(1, 1300));
    }

    #[test]
    fn the_throttle_is_aggregate_per_player_and_rotation_proof() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 1000));
        assert!(rl.allow(2, 1000));
        assert!(!rl.allow(1, 1050));
    }

    #[test]
    fn clear_keeps_storage_bounded_to_active_keys() {
        let mut rl = RateLimiter::new(250);
        for key in 0..10_000u64 {
            assert!(rl.allow(key, 1_000));
            rl.clear(key);
        }
        assert_eq!(rl.tracked_count(), 0);
    }

    #[test]
    fn admission_does_not_scan_all_tracked_keys() {
        let source = include_str!("ratelimit.rs");
        let full_map_scan = [".ret", "ain("].concat();
        assert!(
            !source.contains(&full_map_scan),
            "allow must not scan every tracked key"
        );

        let mut rl = RateLimiter::new(250);
        for key in 0..10_000 {
            assert!(rl.allow(key, 1_000));
        }
        assert_eq!(rl.tracked_count(), 10_000);
        assert!(!rl.allow(9_999, 1_001));
    }

    #[test]
    fn timestamp_rollback_does_not_bypass_the_window() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 1_000));
        assert!(!rl.allow(1, 900));
        assert!(!rl.allow(1, 1_249));
        assert!(rl.allow(1, 1_250));
    }

    #[test]
    fn clear_drops_a_players_entry() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 1000));
        assert_eq!(rl.tracked_count(), 1);
        rl.clear(1);
        assert_eq!(rl.tracked_count(), 0);
        assert!(rl.allow(1, 1050));
    }

    #[test]
    fn zero_interval_never_throttles_and_never_stores() {
        let mut rl = RateLimiter::new(0);
        assert!(rl.allow(1, 1000));
        assert!(rl.allow(1, 1000));
        assert_eq!(rl.tracked_count(), 0);
    }
}
