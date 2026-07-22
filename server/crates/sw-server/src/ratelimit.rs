//! Aggregate per-player market-trade rate limiter.
//!
//! A minimum-interval throttle keyed by **player**: a given player may have at
//! most one *new* accepted trade within [`RateLimiter::min_interval_ms`],
//! regardless of which `port_id` the request names. Keying by player (never by
//! an attacker-chosen port) makes the throttle rotation-proof — a client cannot
//! escape the bound by sending a fresh `port_id` on every message — and bounds
//! the map to players active within the last window rather than to the 2^32
//! possible port ids.
//!
//! It is deliberately separate from the idempotency dedup in [`sw_econ::Market`]
//! — the caller checks dedup first, so an idempotent replay of an already-applied
//! trade is never throttled; only genuinely new trades are.
//!
//! Memory stays bounded two ways: every [`RateLimiter::allow`] first evicts
//! entries whose window has fully elapsed (a stale entry can never throttle
//! anything, so it is pure waste), and [`RateLimiter::clear`] drops a player's
//! entry when they disconnect.
//!
//! The interval is bounded/saturating at construction time (see
//! [`crate::config::Config::trade_min_interval_ms_i64`]), so the window math
//! never overflows.

use std::collections::HashMap;

/// Tracks the last accepted trade time per player and admits a new trade only
/// once the configured interval has elapsed, pruning stale entries as it goes.
#[derive(Debug)]
pub struct RateLimiter {
    min_interval_ms: i64,
    last: HashMap<u64, i64>,
}

impl RateLimiter {
    /// Build a limiter with the given aggregate per-player minimum interval in
    /// milliseconds. A non-positive interval disables throttling.
    pub fn new(min_interval_ms: i64) -> RateLimiter {
        RateLimiter {
            min_interval_ms,
            last: HashMap::new(),
        }
    }

    /// Try to admit a new trade by `player` at time `now_ms`.
    ///
    /// Returns `true` and records `now_ms` when the trade is allowed; returns
    /// `false` without recording when it falls inside the throttle window, so a
    /// rejected attempt never extends the cooldown. The decision is aggregate
    /// per player: the `port_id` a request names cannot open a fresh bucket.
    pub fn allow(&mut self, player: u64, now_ms: i64) -> bool {
        // A non-positive interval disables throttling. Never store, so the map
        // cannot grow at all in this mode.
        if self.min_interval_ms <= 0 {
            return true;
        }
        // Evict every entry whose window has fully elapsed. A stale entry can no
        // longer throttle anything, so keeping it is pure memory waste; pruning
        // here bounds the live map to players who traded within the last window.
        self.last
            .retain(|_, &mut last| now_ms.saturating_sub(last) < self.min_interval_ms);
        // A surviving entry means this player is still inside their window, so a
        // new trade is throttled.
        if self.last.contains_key(&player) {
            return false;
        }
        self.last.insert(player, now_ms);
        true
    }

    /// Forget a player's throttle state, e.g. on disconnect. A departed
    /// player's entry is useless and would otherwise linger until its window
    /// elapsed; dropping it keeps the map bounded to connected players.
    pub fn clear(&mut self, player: u64) {
        self.last.remove(&player);
    }

    /// Number of live entries the limiter is tracking. Test-only: it lets the
    /// unit and dispatch tests assert the map stays bounded and is cleared on
    /// disconnect.
    #[cfg(test)]
    pub fn tracked_count(&self) -> usize {
        self.last.len()
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
        // 100ms later, still inside the 250ms window -> throttled.
        assert!(!rl.allow(1, 1100));
        // A rejected attempt must not push the cooldown out: once the original
        // window elapses the next trade is admitted.
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
        // Regression for the security review's DoS finding. The pre-fix limiter
        // keyed by (player, port) and admitted the first trade per key, so a
        // client rotating port_ids got a fresh bucket every message and was
        // never throttled. The throttle is now aggregate per PLAYER: once a
        // player trades, every further trade inside the window is rejected no
        // matter what port_id (or any other request field) it names.
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 1000));
        // A different player keeps an independent bucket.
        assert!(rl.allow(2, 1000));
        // Player 1 is still throttled 50ms later — there is no per-port escape.
        assert!(!rl.allow(1, 1050));
    }

    #[test]
    fn stale_entries_are_evicted_so_the_map_stays_bounded() {
        // An attacker sending distinct requests cannot grow the map without
        // bound: the limiter is keyed by player and prunes entries whose window
        // has elapsed, so the live size is bounded by the players active within
        // one window, not by how many messages arrive over time.
        let mut rl = RateLimiter::new(250);
        for player in 0..10_000u64 {
            // Each player trades once, a full window apart, so each new arrival
            // makes the previous entry stale and it is pruned on the next check.
            assert!(rl.allow(player, (player as i64) * 250));
        }
        assert_eq!(rl.tracked_count(), 1);
    }

    #[test]
    fn clear_drops_a_players_entry() {
        let mut rl = RateLimiter::new(250);
        assert!(rl.allow(1, 1000));
        assert_eq!(rl.tracked_count(), 1);
        rl.clear(1);
        assert_eq!(rl.tracked_count(), 0);
        // After a clear the player may trade again immediately.
        assert!(rl.allow(1, 1050));
    }

    #[test]
    fn zero_interval_never_throttles_and_never_stores() {
        let mut rl = RateLimiter::new(0);
        assert!(rl.allow(1, 1000));
        assert!(rl.allow(1, 1000));
        // With throttling disabled the map never grows, so it cannot be a memory
        // sink either.
        assert_eq!(rl.tracked_count(), 0);
    }
}
