//! Server configuration: TOML file plus optional CLI/env overrides.

use serde::Deserialize;
use std::path::Path;

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
        Ok(())
    }

    /// Number of ticks between snapshot broadcasts.
    pub fn ticks_per_snapshot(&self) -> u64 {
        (self.tick_hz / self.snapshot_hz).max(1) as u64
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
    fn rejects_bad_snapshot_rate() {
        let cfg = Config {
            snapshot_hz: 100,
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
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
