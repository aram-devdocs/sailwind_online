//! Server configuration: TOML file plus optional CLI/env overrides.

use serde::Deserialize;
use std::path::Path;

/// Upper bound on the AoI radius, in cells. A radius drives a `(2r+1)^2` block
/// allocation ([`sw_world::cells_in_radius`]); bounding it here keeps that math
/// finite and rejects the unbounded-multiply class of bug before it can reach
/// the grid. A radius-16 block is a 33x33 = 1089-cell neighbourhood — already
/// far larger than any sane interest set.
pub const MAX_AOI_RADIUS_CELLS: u32 = 16;

/// Upper bound on the grid cell size, in metres. Sanity ceiling only; the world
/// only needs `> 0`, but an absurd value is a misconfiguration.
const MAX_CELL_SIZE_M: f32 = 1_000_000.0;

/// Lower bound on the grid cell size, in metres. A sub-metre cell is nonsensical
/// for the 1024 m-default sailing world, and a pathological *operator* value
/// below 1 m shrinks the `(x / cell_size)` divisor enough to saturate the
/// `.floor() as i32` cast in [`sw_world::Grid::cell_of`] — the same #19-class
/// overflow the hostile-client position path is already guarded against. The
/// per-coordinate [`crate::validate::MAX_WORLD_COORD_M`] clamp only bounds the
/// numerator, so the divisor needs its own floor here.
const MIN_CELL_SIZE_M: f32 = 1.0;

/// Upper bound on any aggregate player message-class min-interval, in
/// milliseconds (market trade, client-state, chat, econ, moor). Bounds the
/// rate-limit knobs so a misconfiguration cannot wedge a message class behind an
/// absurd cooldown, and so the saturating accessors have a finite ceiling. One
/// hour is already far beyond any sane throttle.
pub const MAX_TRADE_MIN_INTERVAL_MS: u32 = 3_600_000;

/// Upper bound on the pre-authentication hello throttle. It matches the
/// client's fixed retry interval, so the first retry after a lost response is
/// eligible for admission.
pub const MAX_HELLO_MIN_INTERVAL_MS: u32 = 250;

/// Lower bound on the pre-authentication hello throttle. Zero would disable the
/// limiter and expose validation, persistence, and response generation to an
/// unthrottled flood.
const MIN_HELLO_MIN_INTERVAL_MS: u32 = 1;

/// Upper bound on the process-wide admission interval for new sessions. One
/// second is long enough to enforce a bounded retry exchange while keeping an
/// operator misconfiguration from wedging fresh identities indefinitely.
pub const MAX_NEW_SESSION_MIN_INTERVAL_MS: u32 = 1_000;

/// Highest configurable live transport-peer ceiling.
pub const MAX_TRANSPORT_PEERS: u32 = 65_535;

/// Highest configurable persistent player-row ceiling. The server still uses
/// the operator's lower configured value; this only prevents an accidental
/// effectively-unbounded cap.
pub const MAX_PLAYER_ROWS: u32 = 1_000_000;

/// Upper bound on the per-message wire-string length cap, in bytes. Bounds the
/// [`Config::max_wire_string_len`] knob so a misconfiguration cannot admit an
/// unbounded string, and so the saturating accessor has a finite ceiling. The
/// datagram itself is already capped near the MTU by the transport, so this is
/// a defense-in-depth ceiling on individual string fields.
pub const MAX_WIRE_STRING_LEN: u32 = 4_096;

/// Runtime configuration for the server (see `config.example.toml`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// UDP address to bind.
    pub bind: String,
    /// SQLite database path.
    pub db_path: String,
    /// Fixed tick / network pump rate (Hz).
    pub tick_hz: u32,
    /// Snapshot broadcast rate (Hz), advertised to clients.
    pub snapshot_hz: u32,
    /// Interval (seconds) between standalone world-clock broadcasts.
    pub clock_broadcast_secs: u32,
    /// AoI interest radius, in grid cells (advertised to clients). Bounded by
    /// [`MAX_AOI_RADIUS_CELLS`].
    pub aoi_radius_cells: u32,
    /// Grid cell size, in metres (advertised to clients).
    pub cell_size_m: f32,
    /// Minimum interval, in milliseconds, between two processed `ClientHello`
    /// messages from the same peer. A flood beyond this rate is dropped before
    /// validation, persistence, or response generation. The 250 ms default
    /// matches the client's handshake retry cadence. Source-IP session
    /// admission uses a derived interval strictly longer than the global
    /// new-session interval. Bounded to `1..=`[`MAX_HELLO_MIN_INTERVAL_MS`].
    pub hello_min_interval_ms: u32,
    /// Process-wide minimum interval, in milliseconds, between database
    /// admissions for identities without an active session. Active-identity
    /// reconnects use a separate per-player gate, so they cannot consume every
    /// new-identity window. Bounded to
    /// `1..=`[`MAX_NEW_SESSION_MIN_INTERVAL_MS`].
    pub new_session_min_interval_ms: u32,
    /// Hard ceiling on all live transport peers, including pre-authentication
    /// peers. Bounded to `1..=`[`MAX_TRANSPORT_PEERS`].
    pub max_transport_peers: u32,
    /// Hard ceiling on live transport peers sharing a source IP. Must not
    /// exceed [`Config::max_transport_peers`].
    pub max_transport_peers_per_ip: u32,
    /// Hard ceiling on persistent rows in the `players` table. Existing
    /// identities may reconnect at capacity; new identities are refused.
    /// Bounded to `1..=`[`MAX_PLAYER_ROWS`].
    pub max_player_rows: u32,
    /// Minimum interval, in milliseconds, between two accepted market trades by
    /// the same player (an aggregate per-player throttle, independent of which
    /// port the request names). A new trade inside this window is rejected; an
    /// idempotent replay of an already-applied trade is not throttled. Bounded
    /// by [`MAX_TRADE_MIN_INTERVAL_MS`].
    pub trade_min_interval_ms: u32,
    /// Minimum interval, in milliseconds, between two processed `ClientState`
    /// updates from the same player (an aggregate per-player throttle). A flood
    /// beyond this rate is dropped before it can drive the grid/AoI recompute.
    /// Bounded by [`MAX_TRADE_MIN_INTERVAL_MS`]; 0 disables the throttle.
    pub client_state_min_interval_ms: u32,
    /// Minimum interval, in milliseconds, between two processed chat messages
    /// from the same player (an aggregate per-player throttle). Bounded by
    /// [`MAX_TRADE_MIN_INTERVAL_MS`]; 0 disables the throttle.
    pub chat_min_interval_ms: u32,
    /// Minimum interval, in milliseconds, between two *new* econ transactions
    /// from the same player (an aggregate per-player throttle). An idempotent
    /// replay of an already-applied txn is not throttled, exactly like trades.
    /// Bounded by [`MAX_TRADE_MIN_INTERVAL_MS`]; 0 disables the throttle.
    pub econ_min_interval_ms: u32,
    /// Minimum interval, in milliseconds, between two accepted mooring requests
    /// from the same player (an aggregate per-player throttle). A `MoorRequest`
    /// commits a persistent moorage record, so a new moor beyond this rate is
    /// rejected before the write to bound the persistent-state churn. Bounded by
    /// [`MAX_TRADE_MIN_INTERVAL_MS`]; 0 disables the throttle.
    pub moor_min_interval_ms: u32,
    /// Maximum length, in bytes, of any inbound wire string (chat text, econ
    /// note, mooring name). A message carrying a longer string is rejected.
    /// Bounded by [`MAX_WIRE_STRING_LEN`].
    pub max_wire_string_len: u32,
    /// Human-readable server name in ServerHello.
    pub server_name: String,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            bind: "0.0.0.0:38455".to_string(),
            db_path: "sailwind.db".to_string(),
            tick_hz: 30,
            snapshot_hz: 4,
            clock_broadcast_secs: 10,
            aoi_radius_cells: sw_world::AOI_RADIUS_CELLS as u32,
            cell_size_m: sw_world::Grid::DEFAULT_CELL_SIZE_M,
            hello_min_interval_ms: 250,
            new_session_min_interval_ms: 30,
            max_transport_peers: sw_net::DEFAULT_MAX_PEERS as u32,
            max_transport_peers_per_ip: sw_net::DEFAULT_MAX_PEERS_PER_IP as u32,
            max_player_rows: 10_000,
            trade_min_interval_ms: 250,
            client_state_min_interval_ms: 20,
            chat_min_interval_ms: 500,
            econ_min_interval_ms: 100,
            moor_min_interval_ms: 250,
            max_wire_string_len: 512,
            server_name: "Sailwind Online (dev)".to_string(),
        }
    }
}

impl Config {
    /// Load configuration from a TOML file.
    pub fn from_file(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parsing config {}: {e}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Resolve configuration from process arguments and environment.
    ///
    /// Precedence (low to high): built-in defaults, `--config <file>`,
    /// `SWO_BIND` / `SWO_DB` env vars, then `--bind` / `--db` flags.
    pub fn resolve() -> anyhow::Result<Config> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut config_path: Option<String> = None;
        let mut bind_override: Option<String> = None;
        let mut db_override: Option<String> = None;

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--config" => {
                    config_path = Some(expect_value(&args, &mut i, "--config")?);
                }
                "--bind" => {
                    bind_override = Some(expect_value(&args, &mut i, "--bind")?);
                }
                "--db" => {
                    db_override = Some(expect_value(&args, &mut i, "--db")?);
                }
                other => return Err(anyhow::anyhow!("unknown argument: {other}")),
            }
            i += 1;
        }

        let mut cfg = match config_path {
            Some(p) => Config::from_file(Path::new(&p))?,
            None => Config::default(),
        };

        if let Ok(bind) = std::env::var("SWO_BIND") {
            cfg.bind = bind;
        }
        if let Ok(db) = std::env::var("SWO_DB") {
            cfg.db_path = db;
        }
        if let Some(bind) = bind_override {
            cfg.bind = bind;
        }
        if let Some(db) = db_override {
            cfg.db_path = db;
        }

        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.tick_hz == 0 {
            return Err(anyhow::anyhow!("tick_hz must be > 0"));
        }
        if self.snapshot_hz == 0 || self.snapshot_hz > self.tick_hz {
            return Err(anyhow::anyhow!("snapshot_hz must be in 1..=tick_hz"));
        }
        if self.aoi_radius_cells == 0 || self.aoi_radius_cells > MAX_AOI_RADIUS_CELLS {
            return Err(anyhow::anyhow!(
                "aoi_radius_cells must be in 1..={MAX_AOI_RADIUS_CELLS}"
            ));
        }
        if !self.cell_size_m.is_finite()
            || self.cell_size_m < MIN_CELL_SIZE_M
            || self.cell_size_m > MAX_CELL_SIZE_M
        {
            return Err(anyhow::anyhow!(
                "cell_size_m must be a finite value in [{MIN_CELL_SIZE_M}, {MAX_CELL_SIZE_M}]"
            ));
        }
        if !(MIN_HELLO_MIN_INTERVAL_MS..=MAX_HELLO_MIN_INTERVAL_MS)
            .contains(&self.hello_min_interval_ms)
        {
            return Err(anyhow::anyhow!(
                "hello_min_interval_ms must be in {MIN_HELLO_MIN_INTERVAL_MS}..={MAX_HELLO_MIN_INTERVAL_MS}"
            ));
        }
        if !(MIN_HELLO_MIN_INTERVAL_MS..=MAX_NEW_SESSION_MIN_INTERVAL_MS)
            .contains(&self.new_session_min_interval_ms)
        {
            return Err(anyhow::anyhow!(
                "new_session_min_interval_ms must be in {MIN_HELLO_MIN_INTERVAL_MS}..={MAX_NEW_SESSION_MIN_INTERVAL_MS}"
            ));
        }
        if self.max_transport_peers == 0 || self.max_transport_peers > MAX_TRANSPORT_PEERS {
            return Err(anyhow::anyhow!(
                "max_transport_peers must be in 1..={MAX_TRANSPORT_PEERS}"
            ));
        }
        if self.max_transport_peers_per_ip == 0
            || self.max_transport_peers_per_ip > self.max_transport_peers
        {
            return Err(anyhow::anyhow!(
                "max_transport_peers_per_ip must be in 1..=max_transport_peers"
            ));
        }
        if self.max_player_rows == 0 || self.max_player_rows > MAX_PLAYER_ROWS {
            return Err(anyhow::anyhow!(
                "max_player_rows must be in 1..={MAX_PLAYER_ROWS}"
            ));
        }
        for (name, value) in [
            ("trade_min_interval_ms", self.trade_min_interval_ms),
            (
                "client_state_min_interval_ms",
                self.client_state_min_interval_ms,
            ),
            ("chat_min_interval_ms", self.chat_min_interval_ms),
            ("econ_min_interval_ms", self.econ_min_interval_ms),
            ("moor_min_interval_ms", self.moor_min_interval_ms),
        ] {
            if value > MAX_TRADE_MIN_INTERVAL_MS {
                return Err(anyhow::anyhow!(
                    "{name} must be in 0..={MAX_TRADE_MIN_INTERVAL_MS}"
                ));
            }
        }
        if self.max_wire_string_len == 0 || self.max_wire_string_len > MAX_WIRE_STRING_LEN {
            return Err(anyhow::anyhow!(
                "max_wire_string_len must be in 1..={MAX_WIRE_STRING_LEN}"
            ));
        }
        Ok(())
    }

    /// AoI radius as a bounded `i32` for the `sw_world` grid and subscription
    /// APIs. Saturates at [`MAX_AOI_RADIUS_CELLS`], so the `(2r+1)^2` block math
    /// can never overflow even if a caller bypasses [`Config::validate`].
    pub fn aoi_radius_i32(&self) -> i32 {
        self.aoi_radius_cells.min(MAX_AOI_RADIUS_CELLS) as i32
    }

    /// Market trade min-interval as a bounded `i64` of milliseconds for the
    /// rate limiter. Saturates at [`MAX_TRADE_MIN_INTERVAL_MS`], so the
    /// limiter's interval math stays finite even if a caller bypasses
    /// [`Config::validate`].
    pub fn trade_min_interval_ms_i64(&self) -> i64 {
        self.trade_min_interval_ms.min(MAX_TRADE_MIN_INTERVAL_MS) as i64
    }

    /// Client-hello throttle min-interval as a bounded `i64` of milliseconds.
    /// Clamps to the security- and liveness-safe hello-specific range even if a
    /// caller bypasses [`Config::validate`].
    pub fn hello_min_interval_ms_i64(&self) -> i64 {
        self.hello_min_interval_ms
            .clamp(MIN_HELLO_MIN_INTERVAL_MS, MAX_HELLO_MIN_INTERVAL_MS) as i64
    }

    /// Process-wide new-session admission interval as bounded milliseconds.
    pub fn new_session_min_interval_ms_i64(&self) -> i64 {
        self.new_session_min_interval_ms
            .clamp(MIN_HELLO_MIN_INTERVAL_MS, MAX_NEW_SESSION_MIN_INTERVAL_MS) as i64
    }

    /// Per-source session-admission interval in bounded milliseconds.
    ///
    /// It is strictly greater than the global new-session interval, so the
    /// source that consumed one global slot is ineligible at the next slot.
    /// Another source therefore gets an uncontested admission opportunity for
    /// every accepted configuration, including a 250/1000 hello/global pair.
    pub fn source_session_min_interval_ms_i64(&self) -> i64 {
        self.hello_min_interval_ms_i64()
            .max(self.new_session_min_interval_ms_i64().saturating_add(1))
    }

    /// Global live-peer ceiling with a defense-in-depth clamp.
    pub fn max_transport_peers_usize(&self) -> usize {
        self.max_transport_peers.clamp(1, MAX_TRANSPORT_PEERS) as usize
    }

    /// Per-source-IP live-peer ceiling with a defense-in-depth clamp.
    pub fn max_transport_peers_per_ip_usize(&self) -> usize {
        self.max_transport_peers_per_ip
            .clamp(1, self.max_transport_peers.clamp(1, MAX_TRANSPORT_PEERS)) as usize
    }

    /// Persistent player-row ceiling with a defense-in-depth clamp.
    pub fn max_player_rows_u32(&self) -> u32 {
        self.max_player_rows.clamp(1, MAX_PLAYER_ROWS)
    }

    /// Client-state throttle min-interval as a bounded `i64` of milliseconds.
    /// Saturates at [`MAX_TRADE_MIN_INTERVAL_MS`] so the limiter math stays
    /// finite even if a caller bypasses [`Config::validate`].
    pub fn client_state_min_interval_ms_i64(&self) -> i64 {
        self.client_state_min_interval_ms
            .min(MAX_TRADE_MIN_INTERVAL_MS) as i64
    }

    /// Chat throttle min-interval as a bounded `i64` of milliseconds. Saturates
    /// at [`MAX_TRADE_MIN_INTERVAL_MS`].
    pub fn chat_min_interval_ms_i64(&self) -> i64 {
        self.chat_min_interval_ms.min(MAX_TRADE_MIN_INTERVAL_MS) as i64
    }

    /// Econ throttle min-interval as a bounded `i64` of milliseconds. Saturates
    /// at [`MAX_TRADE_MIN_INTERVAL_MS`].
    pub fn econ_min_interval_ms_i64(&self) -> i64 {
        self.econ_min_interval_ms.min(MAX_TRADE_MIN_INTERVAL_MS) as i64
    }

    /// Moor throttle min-interval as a bounded `i64` of milliseconds. Saturates
    /// at [`MAX_TRADE_MIN_INTERVAL_MS`].
    pub fn moor_min_interval_ms_i64(&self) -> i64 {
        self.moor_min_interval_ms.min(MAX_TRADE_MIN_INTERVAL_MS) as i64
    }

    /// Wire-string length cap as a bounded `usize` of bytes. Saturates at
    /// [`MAX_WIRE_STRING_LEN`] so the guard has a finite ceiling even if a
    /// caller bypasses [`Config::validate`].
    pub fn max_wire_string_len_usize(&self) -> usize {
        self.max_wire_string_len.min(MAX_WIRE_STRING_LEN) as usize
    }

    /// Number of ticks between snapshot broadcasts.
    pub fn ticks_per_snapshot(&self) -> u64 {
        (self.tick_hz / self.snapshot_hz).max(1) as u64
    }

    /// Number of ticks between standalone world-clock broadcasts. Uses a
    /// saturating multiply for parity with the other cadence accessors, so the
    /// `tick_hz * clock_broadcast_secs` product can never overflow even if a
    /// caller bypasses [`Config::validate`] or supplies a hostile pair.
    pub fn ticks_per_clock_broadcast(&self) -> u64 {
        self.tick_hz
            .saturating_mul(self.clock_broadcast_secs)
            .max(1) as u64
    }
}

fn expect_value(args: &[String], i: &mut usize, flag: &str) -> anyhow::Result<String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let cfg = Config::default();
        cfg.validate().unwrap();
        assert_eq!(cfg.bind, "0.0.0.0:38455");
        assert_eq!(cfg.ticks_per_snapshot(), 30 / 4);
    }

    #[test]
    fn clock_broadcast_cadence() {
        let cfg = Config::default();
        assert_eq!(cfg.clock_broadcast_secs, 10);
        assert_eq!(cfg.ticks_per_clock_broadcast(), 30 * 10);

        let fast = Config {
            tick_hz: 20,
            clock_broadcast_secs: 3,
            ..Config::default()
        };
        assert_eq!(fast.ticks_per_clock_broadcast(), 60);
    }

    #[test]
    fn ticks_per_clock_broadcast_saturates_and_never_overflows() {
        // Parity with the other cadence accessors: `tick_hz * clock_broadcast_secs`
        // is an unbounded `u32` multiply, so a hostile/misconfigured pair would
        // overflow (panic in debug, wrap in release) before this fix. The
        // accessor must saturate instead, so the tick cadence math can never
        // overflow no matter how the values were supplied (#17 nit).
        let cfg = Config {
            tick_hz: u32::MAX,
            clock_broadcast_secs: u32::MAX,
            ..Config::default()
        };
        assert_eq!(cfg.ticks_per_clock_broadcast(), u32::MAX as u64);
    }

    #[test]
    fn rejects_bad_snapshot_rate() {
        let cfg = Config {
            snapshot_hz: 100,
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn aoi_defaults_are_valid() {
        let cfg = Config::default();
        cfg.validate().unwrap();
        assert_eq!(cfg.aoi_radius_cells, 2);
        assert_eq!(cfg.cell_size_m, 1024.0);
        assert_eq!(cfg.aoi_radius_i32(), 2);
    }

    #[test]
    fn rejects_nonpositive_or_nonfinite_cell_size() {
        for bad in [0.0f32, -1.0, f32::NAN, f32::INFINITY] {
            let cfg = Config {
                cell_size_m: bad,
                ..Config::default()
            };
            assert!(cfg.validate().is_err(), "cell_size {bad} must be rejected");
        }
    }

    #[test]
    fn rejects_sub_metre_cell_size() {
        // A sub-metre cell is nonsensical for the 1024 m-default sailing world and
        // would let a pathological *operator* value shrink the `(x / cell_size)`
        // divisor enough to saturate the `.floor() as i32` cast in
        // `sw_world::Grid::cell_of` — the same #19-class overflow the client path
        // is already guarded against. The validator must floor the cell at 1.0 m.
        for bad in [0.5f32, 0.999, f32::MIN_POSITIVE] {
            let cfg = Config {
                cell_size_m: bad,
                ..Config::default()
            };
            assert!(
                cfg.validate().is_err(),
                "sub-metre cell_size {bad} must be rejected"
            );
        }
        // The lower bound is inclusive: exactly 1.0 m is the smallest sane cell.
        let ok = Config {
            cell_size_m: MIN_CELL_SIZE_M,
            ..Config::default()
        };
        ok.validate().unwrap();
    }

    #[test]
    fn rejects_out_of_range_aoi_radius() {
        let zero = Config {
            aoi_radius_cells: 0,
            ..Config::default()
        };
        assert!(zero.validate().is_err(), "radius 0 sees no neighbours");
        let huge = Config {
            aoi_radius_cells: MAX_AOI_RADIUS_CELLS + 1,
            ..Config::default()
        };
        assert!(
            huge.validate().is_err(),
            "radius past the bound is rejected"
        );
    }

    #[test]
    fn aoi_radius_i32_saturates_and_never_overflows() {
        // Even a pathological radius that would blow up the (2r+1)^2 block
        // allocation is clamped to the bound, so the world/subscription math
        // stays finite no matter how the value was supplied. This is the
        // saturating guard against the unbounded-multiply class of bug.
        let cfg = Config {
            aoi_radius_cells: u32::MAX,
            ..Config::default()
        };
        let r = cfg.aoi_radius_i32();
        assert_eq!(r, MAX_AOI_RADIUS_CELLS as i32);
        let side = 2i32
            .checked_mul(r)
            .and_then(|v| v.checked_add(1))
            .expect("2r+1 must not overflow i32");
        assert!(side.checked_mul(side).is_some(), "block area stays finite");
    }

    #[test]
    fn parses_aoi_keys() {
        let toml_text = r#"
            aoi_radius_cells = 4
            cell_size_m = 512.0
        "#;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.aoi_radius_cells, 4);
        assert_eq!(cfg.cell_size_m, 512.0);
        cfg.validate().unwrap();
        assert_eq!(cfg.aoi_radius_i32(), 4);
    }

    #[test]
    fn trade_rate_limit_default_is_valid_and_bounded() {
        let cfg = Config::default();
        cfg.validate().unwrap();
        assert!(cfg.trade_min_interval_ms > 0);
        assert!(cfg.trade_min_interval_ms <= MAX_TRADE_MIN_INTERVAL_MS);
        // The bounded accessor mirrors the config value for a sane default.
        assert_eq!(
            cfg.trade_min_interval_ms_i64(),
            cfg.trade_min_interval_ms as i64
        );
    }

    #[test]
    fn rejects_out_of_range_trade_interval() {
        let huge = Config {
            trade_min_interval_ms: MAX_TRADE_MIN_INTERVAL_MS + 1,
            ..Config::default()
        };
        assert!(
            huge.validate().is_err(),
            "an interval past the bound is rejected"
        );
    }

    #[test]
    fn trade_interval_accessor_saturates_at_the_bound() {
        // Even a pathological value is clamped by the saturating accessor, so
        // the rate-limiter's min-interval math can never be handed a value the
        // validator would have rejected.
        let cfg = Config {
            trade_min_interval_ms: u32::MAX,
            ..Config::default()
        };
        assert_eq!(
            cfg.trade_min_interval_ms_i64(),
            MAX_TRADE_MIN_INTERVAL_MS as i64
        );
    }

    #[test]
    fn parses_trade_interval_key() {
        let toml_text = r#"
            trade_min_interval_ms = 500
        "#;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.trade_min_interval_ms, 500);
        cfg.validate().unwrap();
    }

    #[test]
    fn parses_hello_interval_key() {
        let toml_text = r#"
            hello_min_interval_ms = 249
        "#;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.hello_min_interval_ms, 249);
        cfg.validate().unwrap();
    }

    #[test]
    fn hello_interval_enforces_security_and_client_retry_liveness() {
        for value in [0, MAX_HELLO_MIN_INTERVAL_MS + 1, 3_600_000] {
            let cfg = Config {
                hello_min_interval_ms: value,
                ..Config::default()
            };
            assert!(
                cfg.validate().is_err(),
                "hello interval {value} must be rejected"
            );
        }

        let disabled = Config {
            hello_min_interval_ms: 0,
            ..Config::default()
        };
        assert_eq!(
            disabled.hello_min_interval_ms_i64(),
            i64::from(MIN_HELLO_MIN_INTERVAL_MS)
        );

        let boundary = Config {
            hello_min_interval_ms: MAX_HELLO_MIN_INTERVAL_MS,
            ..Config::default()
        };
        boundary.validate().unwrap();
        assert_eq!(
            boundary.hello_min_interval_ms_i64(),
            i64::from(MAX_HELLO_MIN_INTERVAL_MS)
        );
    }

    #[test]
    fn message_rate_limit_defaults_are_valid_and_bounded() {
        let cfg = Config::default();
        cfg.validate().unwrap();
        // A permissive-but-finite default for each per-class throttle.
        assert_eq!(cfg.hello_min_interval_ms, 250);
        assert!(cfg.hello_min_interval_ms <= MAX_HELLO_MIN_INTERVAL_MS);
        assert!(cfg.client_state_min_interval_ms <= MAX_TRADE_MIN_INTERVAL_MS);
        assert!(cfg.chat_min_interval_ms <= MAX_TRADE_MIN_INTERVAL_MS);
        assert!(cfg.econ_min_interval_ms <= MAX_TRADE_MIN_INTERVAL_MS);
        assert!(cfg.moor_min_interval_ms <= MAX_TRADE_MIN_INTERVAL_MS);
        // The client-state cap must not throttle a client sending at snapshot_hz
        // (a full snapshot period is far longer than the min-interval).
        let snapshot_period_ms = 1000 / cfg.snapshot_hz;
        assert!(cfg.client_state_min_interval_ms < snapshot_period_ms);
        // The bounded accessors mirror the config values for sane defaults.
        assert_eq!(
            cfg.hello_min_interval_ms_i64(),
            cfg.hello_min_interval_ms as i64
        );
        assert_eq!(
            cfg.client_state_min_interval_ms_i64(),
            cfg.client_state_min_interval_ms as i64
        );
        assert_eq!(
            cfg.chat_min_interval_ms_i64(),
            cfg.chat_min_interval_ms as i64
        );
        assert_eq!(
            cfg.econ_min_interval_ms_i64(),
            cfg.econ_min_interval_ms as i64
        );
        assert_eq!(
            cfg.moor_min_interval_ms_i64(),
            cfg.moor_min_interval_ms as i64
        );
    }

    #[test]
    fn rejects_out_of_range_message_intervals() {
        for mutate in [
            |c: &mut Config| c.client_state_min_interval_ms = MAX_TRADE_MIN_INTERVAL_MS + 1,
            |c: &mut Config| c.chat_min_interval_ms = MAX_TRADE_MIN_INTERVAL_MS + 1,
            |c: &mut Config| c.econ_min_interval_ms = MAX_TRADE_MIN_INTERVAL_MS + 1,
            |c: &mut Config| c.moor_min_interval_ms = MAX_TRADE_MIN_INTERVAL_MS + 1,
        ] {
            let mut cfg = Config::default();
            mutate(&mut cfg);
            assert!(
                cfg.validate().is_err(),
                "an interval past the bound must be rejected"
            );
        }
    }

    #[test]
    fn message_interval_accessors_saturate_at_the_bound() {
        let cfg = Config {
            hello_min_interval_ms: u32::MAX,
            client_state_min_interval_ms: u32::MAX,
            chat_min_interval_ms: u32::MAX,
            econ_min_interval_ms: u32::MAX,
            moor_min_interval_ms: u32::MAX,
            ..Config::default()
        };
        assert_eq!(
            cfg.hello_min_interval_ms_i64(),
            MAX_HELLO_MIN_INTERVAL_MS as i64
        );
        assert_eq!(
            cfg.client_state_min_interval_ms_i64(),
            MAX_TRADE_MIN_INTERVAL_MS as i64
        );
        assert_eq!(
            cfg.chat_min_interval_ms_i64(),
            MAX_TRADE_MIN_INTERVAL_MS as i64
        );
        assert_eq!(
            cfg.econ_min_interval_ms_i64(),
            MAX_TRADE_MIN_INTERVAL_MS as i64
        );
        assert_eq!(
            cfg.moor_min_interval_ms_i64(),
            MAX_TRADE_MIN_INTERVAL_MS as i64
        );
    }

    #[test]
    fn wire_string_len_default_is_valid_and_accessor_saturates() {
        let cfg = Config::default();
        cfg.validate().unwrap();
        assert!(cfg.max_wire_string_len > 0);
        assert!(cfg.max_wire_string_len <= MAX_WIRE_STRING_LEN);
        assert_eq!(
            cfg.max_wire_string_len_usize(),
            cfg.max_wire_string_len as usize
        );
        let huge = Config {
            max_wire_string_len: u32::MAX,
            ..Config::default()
        };
        assert_eq!(
            huge.max_wire_string_len_usize(),
            MAX_WIRE_STRING_LEN as usize
        );
    }

    #[test]
    fn rejects_out_of_range_wire_string_len() {
        let zero = Config {
            max_wire_string_len: 0,
            ..Config::default()
        };
        assert!(zero.validate().is_err(), "a zero-length cap is rejected");
        let huge = Config {
            max_wire_string_len: MAX_WIRE_STRING_LEN + 1,
            ..Config::default()
        };
        assert!(huge.validate().is_err(), "a cap past the bound is rejected");
    }

    #[test]
    fn parses_new_hardening_keys() {
        let toml_text = r#"
            new_session_min_interval_ms = 125
            max_transport_peers = 2048
            max_transport_peers_per_ip = 24
            max_player_rows = 5000
            client_state_min_interval_ms = 33
            chat_min_interval_ms = 750
            econ_min_interval_ms = 200
            moor_min_interval_ms = 400
            max_wire_string_len = 256
        "#;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.new_session_min_interval_ms, 125);
        assert_eq!(cfg.max_transport_peers, 2048);
        assert_eq!(cfg.max_transport_peers_per_ip, 24);
        assert_eq!(cfg.max_player_rows, 5000);
        assert_eq!(cfg.client_state_min_interval_ms, 33);
        assert_eq!(cfg.chat_min_interval_ms, 750);
        assert_eq!(cfg.econ_min_interval_ms, 200);
        assert_eq!(cfg.moor_min_interval_ms, 400);
        assert_eq!(cfg.max_wire_string_len, 256);
        cfg.validate().unwrap();
    }

    #[test]
    fn new_session_budget_defaults_and_bounds_are_safe() {
        let cfg = Config::default();
        cfg.validate().unwrap();
        assert_eq!(cfg.new_session_min_interval_ms, 30);
        assert_eq!(cfg.max_transport_peers, sw_net::DEFAULT_MAX_PEERS as u32);
        assert_eq!(
            cfg.max_transport_peers_per_ip,
            sw_net::DEFAULT_MAX_PEERS_PER_IP as u32
        );
        assert_eq!(cfg.max_player_rows, 10_000);
        assert_eq!(
            cfg.new_session_min_interval_ms_i64(),
            i64::from(cfg.new_session_min_interval_ms)
        );
        assert_eq!(cfg.max_player_rows_u32(), cfg.max_player_rows);

        for value in [0, MAX_NEW_SESSION_MIN_INTERVAL_MS + 1, u32::MAX] {
            let invalid = Config {
                new_session_min_interval_ms: value,
                ..Config::default()
            };
            assert!(
                invalid.validate().is_err(),
                "new-session interval {value} must be rejected"
            );
        }
        for value in [0, MAX_PLAYER_ROWS + 1, u32::MAX] {
            let invalid = Config {
                max_player_rows: value,
                ..Config::default()
            };
            assert!(
                invalid.validate().is_err(),
                "player-row capacity {value} must be rejected"
            );
        }
        for value in [0, MAX_TRANSPORT_PEERS + 1, u32::MAX] {
            let invalid = Config {
                max_transport_peers: value,
                ..Config::default()
            };
            assert!(
                invalid.validate().is_err(),
                "transport-peer capacity {value} must be rejected"
            );
        }
        for value in [0, Config::default().max_transport_peers + 1, u32::MAX] {
            let invalid = Config {
                max_transport_peers_per_ip: value,
                ..Config::default()
            };
            assert!(
                invalid.validate().is_err(),
                "per-IP transport-peer capacity {value} must be rejected"
            );
        }
    }

    #[test]
    fn source_admission_window_is_strictly_longer_than_every_global_window() {
        for hello_ms in MIN_HELLO_MIN_INTERVAL_MS..=MAX_HELLO_MIN_INTERVAL_MS {
            for new_session_ms in MIN_HELLO_MIN_INTERVAL_MS..=MAX_NEW_SESSION_MIN_INTERVAL_MS {
                let cfg = Config {
                    hello_min_interval_ms: hello_ms,
                    new_session_min_interval_ms: new_session_ms,
                    ..Config::default()
                };
                cfg.validate().unwrap();
                assert!(
                    cfg.source_session_min_interval_ms_i64()
                        > cfg.new_session_min_interval_ms_i64(),
                    "one source must never be eligible for two consecutive global slots"
                );
            }
        }
    }

    #[test]
    fn new_session_budget_accessors_clamp_bypassed_validation() {
        let invalid = Config {
            new_session_min_interval_ms: u32::MAX,
            max_transport_peers: u32::MAX,
            max_transport_peers_per_ip: u32::MAX,
            max_player_rows: u32::MAX,
            ..Config::default()
        };
        assert_eq!(
            invalid.new_session_min_interval_ms_i64(),
            i64::from(MAX_NEW_SESSION_MIN_INTERVAL_MS)
        );
        assert_eq!(invalid.max_player_rows_u32(), MAX_PLAYER_ROWS);
        assert_eq!(
            invalid.max_transport_peers_usize(),
            MAX_TRANSPORT_PEERS as usize
        );
        assert_eq!(
            invalid.max_transport_peers_per_ip_usize(),
            MAX_TRANSPORT_PEERS as usize
        );
    }

    #[test]
    fn parses_example_shape() {
        let toml_text = r#"
            bind = "127.0.0.1:1234"
            db_path = "x.db"
            tick_hz = 20
            snapshot_hz = 5
            server_name = "T"
        "#;
        let cfg: Config = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.bind, "127.0.0.1:1234");
        assert_eq!(cfg.tick_hz, 20);
        assert_eq!(cfg.ticks_per_snapshot(), 4);
    }
}
