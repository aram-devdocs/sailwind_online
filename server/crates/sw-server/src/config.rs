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
            || self.cell_size_m <= 0.0
            || self.cell_size_m > MAX_CELL_SIZE_M
        {
            return Err(anyhow::anyhow!(
                "cell_size_m must be a finite value in (0, {MAX_CELL_SIZE_M}]"
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

    /// Number of ticks between snapshot broadcasts.
    pub fn ticks_per_snapshot(&self) -> u64 {
        (self.tick_hz / self.snapshot_hz).max(1) as u64
    }

    /// Number of ticks between standalone world-clock broadcasts.
    pub fn ticks_per_clock_broadcast(&self) -> u64 {
        (self.tick_hz * self.clock_broadcast_secs).max(1) as u64
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
