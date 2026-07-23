//! `sw-persist` — the SQLite persistence layer.
//!
//! A thin [`Db`] wrapper over `rusqlite` (bundled SQLite): WAL journal mode,
//! `user_version`-numbered migrations applied at boot from embedded SQL, and
//! typed accessors for the schema tables (`players`, `moorings`, `ledger`,
//! `world` in v1; the shared `market` + `market_trades` in v2). Migrations are
//! forward-only and additive — a later version never drops or rewrites an
//! earlier table. Synchronous by design — it is driven directly from the
//! server's single-threaded tick loop.

use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};

pub use rusqlite::{Error, Result};

/// The latest schema version this build knows how to produce.
pub const SCHEMA_VERSION: i64 = 2;

/// Embedded schema for `user_version = 1`. Applied once, in a transaction.
const MIGRATION_V1: &str = r#"
CREATE TABLE players (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    token_hash TEXT    NOT NULL UNIQUE,
    gold       INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    last_seen  INTEGER NOT NULL
);

CREATE TABLE moorings (
    boat_id    INTEGER PRIMARY KEY,
    owner      INTEGER NOT NULL,
    cell_x     INTEGER NOT NULL,
    cell_z     INTEGER NOT NULL,
    pos_x      REAL    NOT NULL,
    pos_y      REAL    NOT NULL,
    pos_z      REAL    NOT NULL,
    rot_x      REAL    NOT NULL,
    rot_y      REAL    NOT NULL,
    rot_z      REAL    NOT NULL,
    rot_w      REAL    NOT NULL,
    name       TEXT    NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_moorings_cell ON moorings (cell_x, cell_z);

CREATE TABLE ledger (
    txn_id     INTEGER PRIMARY KEY,
    player_id  INTEGER NOT NULL,
    amount     INTEGER NOT NULL,
    kind       INTEGER NOT NULL,
    note       TEXT    NOT NULL,
    applied_at INTEGER NOT NULL
);
CREATE INDEX idx_ledger_player ON ledger (player_id);

CREATE TABLE world (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// Embedded schema for `user_version = 2`. Additive over v1: the shared,
/// authoritative per-port market. `market` holds the current `(stock, price)`
/// keyed by `(port_id, item_id)`; `market_trades` is the txn_id dedup log that
/// makes a replayed trade idempotent, exactly as `ledger` does for balances.
/// Modelled on the game `SaveContainer`'s port-demand / market-supply concepts
/// (see `docs/04-reference/game-facts.md`), by concept only — never by reading
/// the game's bytes.
const MIGRATION_V2: &str = r#"
CREATE TABLE market (
    port_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    stock   INTEGER NOT NULL DEFAULT 0,
    price   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (port_id, item_id)
);

CREATE TABLE market_trades (
    txn_id     INTEGER PRIMARY KEY,
    port_id    INTEGER NOT NULL,
    item_id    INTEGER NOT NULL,
    stock      INTEGER NOT NULL,
    price      INTEGER NOT NULL,
    applied_at INTEGER NOT NULL
);
"#;

/// A row of the `players` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerRow {
    pub id: i64,
    pub name: String,
    pub token_hash: String,
    pub gold: i64,
    pub created_at: i64,
    pub last_seen: i64,
}

/// Result of atomically admitting a player identity against the configured
/// persistent-row ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerAdmission {
    Admitted(PlayerRow),
    CapacityReached,
}

/// A row of the `moorings` table (a persisted boat mooring).
#[derive(Debug, Clone, PartialEq)]
pub struct MooringRow {
    pub boat_id: i64,
    pub owner: i64,
    pub cell_x: i32,
    pub cell_z: i32,
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub name: String,
    pub created_at: i64,
}

/// The persistence handle.
pub struct Db {
    conn: Connection,
}

impl Db {
    /// Open (creating if needed) a database file and apply migrations.
    pub fn open(path: &str) -> Result<Db> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Open an in-memory database and apply migrations (for tests).
    pub fn open_in_memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Db> {
        // WAL for concurrent readers + durability; a no-op ("memory") for
        // in-memory databases, which is harmless.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Db { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let mut version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 1 {
            self.conn.execute_batch(&format!(
                "BEGIN; {MIGRATION_V1} PRAGMA user_version = 1; COMMIT;"
            ))?;
            version = 1;
        }
        if version < 2 {
            self.conn.execute_batch(&format!(
                "BEGIN; {MIGRATION_V2} PRAGMA user_version = 2; COMMIT;"
            ))?;
            version = 2;
        }
        debug_assert_eq!(version, SCHEMA_VERSION);
        Ok(())
    }

    /// The database's current `user_version`.
    pub fn schema_version(&self) -> Result<i64> {
        self.conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
    }

    /// The reported journal mode (e.g. `"wal"` for a file, `"memory"` for `:memory:`).
    pub fn journal_mode(&self) -> Result<String> {
        self.conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
    }

    // ----- players -------------------------------------------------------

    /// Create-or-fetch a player identified by `token_hash`. On an existing
    /// player the display name and `last_seen` are refreshed; the balance is
    /// preserved. New players start at zero gold.
    pub fn upsert_player_by_token(
        &self,
        token_hash: &str,
        name: &str,
        now: i64,
    ) -> Result<PlayerRow> {
        self.conn.query_row(
            "INSERT INTO players (name, token_hash, gold, created_at, last_seen)
             VALUES (?1, ?2, 0, ?3, ?3)
             ON CONFLICT(token_hash) DO UPDATE SET name = excluded.name, last_seen = excluded.last_seen
             RETURNING id, name, token_hash, gold, created_at, last_seen",
            params![name, token_hash, now],
            player_from_row,
        )
    }

    /// Create or refresh a player while atomically enforcing `max_rows`.
    ///
    /// An immediate transaction serializes the existence check, count, and
    /// insert across SQLite connections. Existing identities remain admissible
    /// after the table reaches capacity; a refused new identity makes no change.
    pub fn admit_player_by_token(
        &mut self,
        token_hash: &str,
        name: &str,
        now: i64,
        max_rows: u32,
    ) -> Result<PlayerAdmission> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing_id = tx
            .query_row(
                "SELECT id FROM players WHERE token_hash = ?1",
                params![token_hash],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;

        if let Some(id) = existing_id {
            tx.execute(
                "UPDATE players SET name = ?2, last_seen = ?3 WHERE id = ?1",
                params![id, name, now],
            )?;
            let player = tx.query_row(
                "SELECT id, name, token_hash, gold, created_at, last_seen
                 FROM players WHERE id = ?1",
                params![id],
                player_from_row,
            )?;
            tx.commit()?;
            return Ok(PlayerAdmission::Admitted(player));
        }

        let count = tx.query_row("SELECT COUNT(*) FROM players", [], |row| {
            row.get::<_, u64>(0)
        })?;
        if count >= u64::from(max_rows) {
            tx.commit()?;
            return Ok(PlayerAdmission::CapacityReached);
        }

        let player = tx.query_row(
            "INSERT INTO players (name, token_hash, gold, created_at, last_seen)
             VALUES (?1, ?2, 0, ?3, ?3)
             RETURNING id, name, token_hash, gold, created_at, last_seen",
            params![name, token_hash, now],
            player_from_row,
        )?;
        tx.commit()?;
        Ok(PlayerAdmission::Admitted(player))
    }

    /// Fetch a player by id.
    pub fn player(&self, id: i64) -> Result<Option<PlayerRow>> {
        self.conn
            .query_row(
                "SELECT id, name, token_hash, gold, created_at, last_seen FROM players WHERE id = ?1",
                params![id],
                player_from_row,
            )
            .optional()
    }

    /// A player's current gold balance (0 if the player does not exist).
    pub fn player_balance(&self, id: i64) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT gold FROM players WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0))
    }

    /// Update a player's `last_seen` timestamp.
    pub fn touch_last_seen(&self, id: i64, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE players SET last_seen = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    // ----- ledger --------------------------------------------------------

    /// Look up a committed transaction: `(player_id, resulting_balance)`.
    pub fn lookup_txn(&self, txn_id: i64) -> Result<Option<(i64, i64)>> {
        self.conn
            .query_row(
                "SELECT l.player_id, p.gold
                 FROM ledger l JOIN players p ON p.id = l.player_id
                 WHERE l.txn_id = ?1",
                params![txn_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }

    /// Atomically append a ledger row and set the player's new balance.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_txn(
        &self,
        txn_id: i64,
        player_id: i64,
        amount: i64,
        kind: u8,
        note: &str,
        applied_at: i64,
        new_balance: i64,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO ledger (txn_id, player_id, amount, kind, note, applied_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![txn_id, player_id, amount, kind as i64, note, applied_at],
        )?;
        tx.execute(
            "UPDATE players SET gold = ?2 WHERE id = ?1",
            params![player_id, new_balance],
        )?;
        tx.commit()
    }

    // ----- market --------------------------------------------------------

    /// Current shared `(stock, price)` for a `(port, item)`, or `None` if no
    /// trade has ever touched it.
    pub fn market_state(&self, port_id: u32, item_id: u32) -> Result<Option<(i64, i64)>> {
        self.conn
            .query_row(
                "SELECT stock, price FROM market WHERE port_id = ?1 AND item_id = ?2",
                params![port_id, item_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
    }

    /// Look up a committed trade by its idempotency key:
    /// `(port_id, item_id, resulting_stock, resulting_price)`.
    pub fn lookup_trade(&self, txn_id: u64) -> Result<Option<(u32, u32, i64, i64)>> {
        self.conn
            .query_row(
                "SELECT port_id, item_id, stock, price FROM market_trades WHERE txn_id = ?1",
                params![txn_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
    }

    /// Atomically record a trade in the dedup log and upsert the shared
    /// `(port, item)` stock/price to its post-trade values.
    pub fn commit_trade(
        &self,
        txn_id: u64,
        port_id: u32,
        item_id: u32,
        new_stock: i64,
        new_price: i64,
        applied_at: i64,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO market_trades (txn_id, port_id, item_id, stock, price, applied_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![txn_id, port_id, item_id, new_stock, new_price, applied_at],
        )?;
        tx.execute(
            "INSERT INTO market (port_id, item_id, stock, price)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(port_id, item_id) DO UPDATE SET
                stock = excluded.stock, price = excluded.price",
            params![port_id, item_id, new_stock, new_price],
        )?;
        tx.commit()
    }

    // ----- moorings ------------------------------------------------------

    /// Insert or replace a mooring keyed by `boat_id`.
    pub fn upsert_mooring(&self, m: &MooringRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO moorings
                (boat_id, owner, cell_x, cell_z, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, rot_w, name, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(boat_id) DO UPDATE SET
                owner=excluded.owner, cell_x=excluded.cell_x, cell_z=excluded.cell_z,
                pos_x=excluded.pos_x, pos_y=excluded.pos_y, pos_z=excluded.pos_z,
                rot_x=excluded.rot_x, rot_y=excluded.rot_y, rot_z=excluded.rot_z, rot_w=excluded.rot_w,
                name=excluded.name",
            params![
                m.boat_id, m.owner, m.cell_x, m.cell_z,
                m.pos[0] as f64, m.pos[1] as f64, m.pos[2] as f64,
                m.rot[0] as f64, m.rot[1] as f64, m.rot[2] as f64, m.rot[3] as f64,
                m.name, m.created_at,
            ],
        )?;
        Ok(())
    }

    /// All moorings in a given grid cell.
    pub fn moorings_in_cell(&self, cell_x: i32, cell_z: i32) -> Result<Vec<MooringRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT boat_id, owner, cell_x, cell_z, pos_x, pos_y, pos_z, rot_x, rot_y, rot_z, rot_w, name, created_at
             FROM moorings WHERE cell_x = ?1 AND cell_z = ?2 ORDER BY boat_id",
        )?;
        let rows = stmt.query_map(params![cell_x, cell_z], mooring_from_row)?;
        rows.collect()
    }

    /// Count of all moorings (used in tests / diagnostics).
    pub fn mooring_count(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM moorings", [], |r| r.get(0))
    }

    // ----- world key/value ----------------------------------------------

    /// Read a `world` value.
    pub fn world_get(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM world WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
    }

    /// Write a `world` value (insert or replace).
    pub fn world_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO world (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Get an existing `world` value or initialise it with `default` and return it.
    pub fn world_get_or_init(&self, key: &str, default: &str) -> Result<String> {
        if let Some(v) = self.world_get(key)? {
            return Ok(v);
        }
        self.world_set(key, default)?;
        Ok(default.to_string())
    }
}

fn player_from_row(r: &Row<'_>) -> Result<PlayerRow> {
    Ok(PlayerRow {
        id: r.get(0)?,
        name: r.get(1)?,
        token_hash: r.get(2)?,
        gold: r.get(3)?,
        created_at: r.get(4)?,
        last_seen: r.get(5)?,
    })
}

fn mooring_from_row(r: &Row<'_>) -> Result<MooringRow> {
    Ok(MooringRow {
        boat_id: r.get(0)?,
        owner: r.get(1)?,
        cell_x: r.get(2)?,
        cell_z: r.get(3)?,
        pos: [
            r.get::<_, f64>(4)? as f32,
            r.get::<_, f64>(5)? as f32,
            r.get::<_, f64>(6)? as f32,
        ],
        rot: [
            r.get::<_, f64>(7)? as f32,
            r.get::<_, f64>(8)? as f32,
            r.get::<_, f64>(9)? as f32,
            r.get::<_, f64>(10)? as f32,
        ],
        name: r.get(11)?,
        created_at: r.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_mooring(boat_id: i64, owner: i64) -> MooringRow {
        MooringRow {
            boat_id,
            owner,
            cell_x: 3,
            cell_z: -2,
            pos: [10.0, 1.5, -4.0],
            rot: [0.0, 0.0, 0.0, 1.0],
            name: "Dawn Treader".to_string(),
            created_at: 1000,
        }
    }

    #[test]
    fn opens_and_migrates_to_latest() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(SCHEMA_VERSION, 2);
    }

    #[test]
    fn market_state_defaults_and_roundtrips() {
        let db = Db::open_in_memory().unwrap();
        // Untouched (port, item) reads as (0, 0) — no row yet.
        assert_eq!(db.market_state(10, 5).unwrap(), None);
        // A committed trade upserts the shared state and records the dedup key.
        assert_eq!(db.lookup_trade(1).unwrap(), None);
        db.commit_trade(1, 10, 5, 30, 100, 1000).unwrap();
        assert_eq!(db.market_state(10, 5).unwrap(), Some((30, 100)));
        assert_eq!(db.lookup_trade(1).unwrap(), Some((10, 5, 30, 100)));

        // A second trade to the same (port, item) replaces stock/price in place.
        db.commit_trade(2, 10, 5, 20, 110, 1001).unwrap();
        assert_eq!(db.market_state(10, 5).unwrap(), Some((20, 110)));
        // Distinct (port, item) is independent.
        db.commit_trade(3, 11, 5, 50, 300, 1002).unwrap();
        assert_eq!(db.market_state(11, 5).unwrap(), Some((50, 300)));
        assert_eq!(db.market_state(10, 5).unwrap(), Some((20, 110)));
    }

    #[test]
    fn migration_v1_to_v2_preserves_v1_tables_and_data() {
        // A fresh v1 database, opened by a v2 build, must forward-migrate to v2
        // (adding the market tables) while preserving every v1 row.
        let mut path = std::env::temp_dir();
        let unique = format!(
            "sw-persist-migrate-{}-{:?}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        path.push(unique);
        let path_str = path.to_str().unwrap().to_string();

        // Stamp a genuine v1 database: only the v1 schema, user_version = 1.
        {
            let conn = Connection::open(&path_str).unwrap();
            conn.execute_batch(&format!(
                "BEGIN; {MIGRATION_V1} PRAGMA user_version = 1; COMMIT;"
            ))
            .unwrap();
            conn.execute(
                "INSERT INTO players (name, token_hash, gold, created_at, last_seen)
                 VALUES ('Skipper', 'v1hash', 250, 1, 2)",
                [],
            )
            .unwrap();
        }

        // Reopen with the current build: it migrates v1 -> v2.
        {
            let db = Db::open(&path_str).unwrap();
            assert_eq!(db.schema_version().unwrap(), 2);
            // v1 data survived the migration.
            let p = db.upsert_player_by_token("v1hash", "Skipper", 3).unwrap();
            assert_eq!(p.gold, 250);
            // The v2 market table exists and starts empty.
            assert_eq!(db.market_state(1, 1).unwrap(), None);
            db.commit_trade(1, 1, 1, 5, 7, 4).unwrap();
            assert_eq!(db.market_state(1, 1).unwrap(), Some((5, 7)));
        }

        let _ = std::fs::remove_file(&path_str);
        let _ = std::fs::remove_file(format!("{path_str}-wal"));
        let _ = std::fs::remove_file(format!("{path_str}-shm"));
    }

    #[test]
    fn player_upsert_is_idempotent_by_token() {
        let db = Db::open_in_memory().unwrap();
        let a = db
            .upsert_player_by_token("hash-abc", "Skipper", 100)
            .unwrap();
        assert_eq!(a.gold, 0);

        // Same token, new name + later timestamp -> same id, balance preserved.
        db.commit_txn(1, a.id, 250, 0, "reward", 110, 250).unwrap();
        let b = db
            .upsert_player_by_token("hash-abc", "Captain", 200)
            .unwrap();
        assert_eq!(b.id, a.id);
        assert_eq!(b.name, "Captain");
        assert_eq!(b.gold, 250);
        assert_eq!(b.last_seen, 200);
        assert_eq!(b.created_at, a.created_at);

        // Different token -> different player.
        let c = db.upsert_player_by_token("hash-xyz", "Other", 300).unwrap();
        assert_ne!(c.id, a.id);
    }

    #[test]
    fn player_admission_enforces_capacity_without_blocking_existing_identity() {
        let mut db = Db::open_in_memory().unwrap();

        let first = match db
            .admit_player_by_token("hash-one", "First", 100, 2)
            .unwrap()
        {
            PlayerAdmission::Admitted(player) => player,
            PlayerAdmission::CapacityReached => panic!("first player must fit"),
        };
        let second = match db
            .admit_player_by_token("hash-two", "Second", 100, 2)
            .unwrap()
        {
            PlayerAdmission::Admitted(player) => player,
            PlayerAdmission::CapacityReached => panic!("second player must fit"),
        };
        assert_ne!(first.id, second.id);

        assert_eq!(
            db.admit_player_by_token("hash-three", "Third", 200, 2)
                .unwrap(),
            PlayerAdmission::CapacityReached
        );
        assert!(
            db.player(second.id + 1).unwrap().is_none(),
            "capacity refusal must not insert a player row"
        );

        let reconnected = match db
            .admit_player_by_token("hash-one", "Renamed", 300, 2)
            .unwrap()
        {
            PlayerAdmission::Admitted(player) => player,
            PlayerAdmission::CapacityReached => {
                panic!("an existing identity must reconnect at capacity")
            }
        };
        assert_eq!(reconnected.id, first.id);
        assert_eq!(reconnected.name, "Renamed");
        assert_eq!(reconnected.created_at, first.created_at);
        assert_eq!(reconnected.last_seen, 300);
    }

    #[test]
    fn ledger_commit_and_lookup_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let p = db.upsert_player_by_token("t", "P", 0).unwrap();
        assert_eq!(db.lookup_txn(42).unwrap(), None);
        db.commit_txn(42, p.id, 100, 1, "sale", 5, 100).unwrap();
        assert_eq!(db.player_balance(p.id).unwrap(), 100);
        assert_eq!(db.lookup_txn(42).unwrap(), Some((p.id, 100)));
    }

    #[test]
    fn mooring_roundtrip_and_cell_query() {
        let db = Db::open_in_memory().unwrap();
        let m = sample_mooring(1, 7);
        db.upsert_mooring(&m).unwrap();
        let got = db.moorings_in_cell(3, -2).unwrap();
        assert_eq!(got, vec![m.clone()]);
        assert!(db.moorings_in_cell(0, 0).unwrap().is_empty());

        // Upsert replaces in place (still one row).
        let mut m2 = m.clone();
        m2.name = "Renamed".to_string();
        db.upsert_mooring(&m2).unwrap();
        assert_eq!(db.mooring_count().unwrap(), 1);
        assert_eq!(db.moorings_in_cell(3, -2).unwrap()[0].name, "Renamed");
    }

    #[test]
    fn mooring_survives_reopen_and_is_found_by_cell() {
        // Mirrors the restart-persistence conformance check: a mooring written
        // before a process restart must be recoverable, in its cell, from a
        // fresh Db opened on the same file.
        let mut path = std::env::temp_dir();
        let unique = format!(
            "sw-persist-mooring-{}-{:?}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        path.push(unique);
        let path_str = path.to_str().unwrap().to_string();

        let m = sample_mooring(1, 7);
        {
            let db = Db::open(&path_str).unwrap();
            db.upsert_mooring(&m).unwrap();
        }
        {
            // Reopen the same file (as the server does on restart): the mooring
            // is still indexed in its cell.
            let db = Db::open(&path_str).unwrap();
            assert_eq!(db.moorings_in_cell(m.cell_x, m.cell_z).unwrap(), vec![m]);
        }

        let _ = std::fs::remove_file(&path_str);
        let _ = std::fs::remove_file(format!("{path_str}-wal"));
        let _ = std::fs::remove_file(format!("{path_str}-shm"));
    }

    #[test]
    fn world_kv_roundtrip_and_init() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.world_get("seed").unwrap(), None);
        let v = db.world_get_or_init("seed", "12345").unwrap();
        assert_eq!(v, "12345");
        // Second call returns the stored value, not the new default.
        assert_eq!(db.world_get_or_init("seed", "99999").unwrap(), "12345");
        db.world_set("seed", "777").unwrap();
        assert_eq!(db.world_get("seed").unwrap().as_deref(), Some("777"));
    }

    #[test]
    fn file_database_uses_wal_and_persists_across_reopen() {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "sw-persist-test-{}-{:?}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        path.push(unique);
        let path_str = path.to_str().unwrap().to_string();

        {
            let db = Db::open(&path_str).unwrap();
            assert_eq!(db.journal_mode().unwrap(), "wal");
            let p = db.upsert_player_by_token("persist", "Ish", 1).unwrap();
            db.commit_txn(1, p.id, 500, 0, "grant", 2, 500).unwrap();
        }
        {
            // Reopen the same file: data survives.
            let db = Db::open(&path_str).unwrap();
            let p = db.upsert_player_by_token("persist", "Ish", 3).unwrap();
            assert_eq!(p.gold, 500);
        }

        // Best-effort cleanup of the db + WAL/SHM sidecars.
        let _ = std::fs::remove_file(&path_str);
        let _ = std::fs::remove_file(format!("{path_str}-wal"));
        let _ = std::fs::remove_file(format!("{path_str}-shm"));
    }
}
