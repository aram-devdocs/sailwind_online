//! Adapter that lets [`sw_econ::Ledger`] run against the SQLite [`Db`].
//!
//! The store borrows the database, so a fresh [`DbLedgerStore`] is created per
//! transaction and the `Db` remains owned by the server for its other tables.

use sw_econ::{LedgerStore, RecordedTxn, Txn};
use sw_persist::Db;

/// A [`LedgerStore`] backed by the persistent database.
pub struct DbLedgerStore<'a> {
    db: &'a Db,
    now_ms: i64,
}

impl<'a> DbLedgerStore<'a> {
    pub fn new(db: &'a Db, now_ms: i64) -> DbLedgerStore<'a> {
        DbLedgerStore { db, now_ms }
    }
}

impl LedgerStore for DbLedgerStore<'_> {
    type Error = sw_persist::Error;

    fn balance(&self, player: u64) -> Result<i64, Self::Error> {
        self.db.player_balance(player as i64)
    }

    fn lookup_txn(&self, txn_id: u64) -> Result<Option<RecordedTxn>, Self::Error> {
        Ok(self
            .db
            .lookup_txn(txn_id as i64)?
            .map(|(player, new_balance)| RecordedTxn {
                player: player as u64,
                new_balance,
            }))
    }

    fn commit(&mut self, player: u64, txn: &Txn, new_balance: i64) -> Result<(), Self::Error> {
        self.db.commit_txn(
            txn.txn_id as i64,
            player as i64,
            txn.amount_gold,
            txn.kind,
            &txn.note,
            self.now_ms,
            new_balance,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sw_econ::Ledger;

    #[test]
    fn ledger_over_db_is_idempotent_and_persistent() {
        let db = Db::open_in_memory().unwrap();
        let player = db.upsert_player_by_token("tok", "P", 0).unwrap();

        let txn = Txn {
            txn_id: 1,
            amount_gold: 100,
            kind: 0,
            note: "seed".into(),
        };

        // First apply commits.
        let ack = {
            let mut ledger = Ledger::new(DbLedgerStore::new(&db, 1));
            ledger.apply(player.id as u64, &txn).unwrap()
        };
        assert!(ack.accepted);
        assert_eq!(ack.new_balance, 100);
        assert_eq!(db.player_balance(player.id).unwrap(), 100);

        // Replay via a fresh store is idempotent.
        let ack2 = {
            let mut ledger = Ledger::new(DbLedgerStore::new(&db, 2));
            ledger.apply(player.id as u64, &txn).unwrap()
        };
        assert!(ack2.accepted);
        assert_eq!(ack2.new_balance, 100);
        assert_eq!(db.player_balance(player.id).unwrap(), 100);
    }
}
