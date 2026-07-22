//! Adapters that let [`sw_econ::Ledger`] and [`sw_econ::Market`] run against the
//! SQLite [`Db`].
//!
//! Each store borrows the database, so a fresh store is created per transaction
//! and the `Db` remains owned by the server for its other tables.

use sw_econ::{LedgerStore, MarketStore, RecordedTrade, RecordedTxn, Trade, Txn};
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

/// A [`MarketStore`] backed by the persistent database.
pub struct DbMarketStore<'a> {
    db: &'a Db,
    now_ms: i64,
}

impl<'a> DbMarketStore<'a> {
    pub fn new(db: &'a Db, now_ms: i64) -> DbMarketStore<'a> {
        DbMarketStore { db, now_ms }
    }
}

impl MarketStore for DbMarketStore<'_> {
    type Error = sw_persist::Error;

    fn state(&self, port: u32, item: u32) -> Result<(i64, i64), Self::Error> {
        Ok(self.db.market_state(port, item)?.unwrap_or((0, 0)))
    }

    fn lookup_trade(&self, txn_id: u64) -> Result<Option<RecordedTrade>, Self::Error> {
        Ok(self
            .db
            .lookup_trade(txn_id)?
            .map(|(port_id, item_id, stock, price)| RecordedTrade {
                port_id,
                item_id,
                stock,
                price,
            }))
    }

    fn commit(&mut self, trade: &Trade, new_stock: i64, new_price: i64) -> Result<(), Self::Error> {
        self.db.commit_trade(
            trade.txn_id,
            trade.port_id,
            trade.item_id,
            new_stock,
            new_price,
            self.now_ms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sw_econ::{Ledger, Market};

    #[test]
    fn market_over_db_is_idempotent_and_persistent() {
        let db = Db::open_in_memory().unwrap();

        let trade = Trade {
            txn_id: 1,
            port_id: 10,
            item_id: 5,
            qty: 40,
            unit_price: 100,
        };

        // First apply commits the shared stock/price.
        let ack = {
            let mut market = Market::new(DbMarketStore::new(&db, 1));
            market.apply(&trade).unwrap()
        };
        assert!(ack.accepted);
        assert_eq!(ack.stock, 40);
        assert_eq!(ack.price, 100);
        assert_eq!(db.market_state(10, 5).unwrap(), Some((40, 100)));

        // Replay via a fresh store is idempotent — no double-apply.
        let ack2 = {
            let mut market = Market::new(DbMarketStore::new(&db, 2));
            market.apply(&trade).unwrap()
        };
        assert!(ack2.accepted);
        assert_eq!(ack2.stock, 40);
        assert_eq!(db.market_state(10, 5).unwrap(), Some((40, 100)));
    }

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
