//! World clock and weather seed derivation.
//!
//! The clock is a pure function of a DB-stored epoch (Unix ms of first boot)
//! and the current wall-clock time, so it survives restarts without drift.

/// Real seconds per in-game day.
pub const DAY_LENGTH_SECS: f64 = 1200.0;

/// In-game days per lunar cycle.
pub const LUNAR_CYCLE_DAYS: u32 = 30;

/// A derived world-clock reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldClock {
    pub day: u32,
    /// Fraction of the current day in `[0, 1)`.
    pub time_of_day: f32,
    /// Lunar phase in `[0, 1)`.
    pub moon_phase: f32,
}

/// Derive the world clock from the stored epoch and the current time (both in
/// Unix milliseconds).
pub fn clock_from_epoch(epoch_ms: i64, now_ms: i64) -> WorldClock {
    let elapsed_secs = (now_ms - epoch_ms).max(0) as f64 / 1000.0;
    let total_days = elapsed_secs / DAY_LENGTH_SECS;
    let day = total_days.floor() as u32;
    let time_of_day = total_days.fract() as f32;
    let moon_phase = (day % LUNAR_CYCLE_DAYS) as f32 / LUNAR_CYCLE_DAYS as f32;
    WorldClock {
        day,
        time_of_day,
        moon_phase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_zero_at_epoch() {
        let c = clock_from_epoch(1000, 1000);
        assert_eq!(c.day, 0);
        assert!(c.time_of_day.abs() < 1e-6);
        assert!(c.moon_phase.abs() < 1e-6);
    }

    #[test]
    fn advances_by_day_length() {
        let epoch = 0;
        let now = (DAY_LENGTH_SECS * 1000.0) as i64 + (DAY_LENGTH_SECS * 500.0) as i64;
        let c = clock_from_epoch(epoch, now);
        assert_eq!(c.day, 1);
        assert!((c.time_of_day - 0.5).abs() < 1e-3);
    }

    #[test]
    fn clock_never_goes_negative_before_epoch() {
        let c = clock_from_epoch(10_000, 0);
        assert_eq!(c.day, 0);
        assert_eq!(c.time_of_day, 0.0);
    }
}
