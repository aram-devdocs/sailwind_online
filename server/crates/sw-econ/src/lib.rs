//! `sw-econ` — the authoritative gold ledger and shared port market.
//!
//! [`Ledger::apply`] is the single write path for balances. It is:
//! - **idempotent** by `txn_id` — replaying a transaction returns the original
//!   result and never double-applies it;
//! - **non-negative** — a transaction that would drive a balance below zero is
//!   rejected, leaving the balance untouched;
//! - **append-only** — accepted transactions are committed to the store's log.
//!
//! [`Market::apply`] is the sibling write path for the *shared* per-port
//! stock/price state. It carries the same guarantees: idempotent by `txn_id`
//! (a replayed trade never double-applies), non-negative stock (a buy cannot
//! drive stock below zero), and append-only commit. It is a shared,
//! rate-limited trade against per-port state — deliberately not a dynamic-price
//! simulation; the price is authoritative state the client reads back, not a
//! server-computed curve.
//!
//! Storage is abstracted behind [`LedgerStore`] / [`MarketStore`] so the same
//! logic runs against an in-memory map in tests and against SQLite in the
//! server.

use std::collections::HashMap;

/// Player identifier (matches the persisted `players.id`).
pub type PlayerId = u64;

/// A balance-changing transaction. `amount_gold` is a signed delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Txn {
    pub txn_id: u64,
    pub amount_gold: i64,
    pub kind: u8,
    pub note: String,
}

/// The result of applying (or replaying) a transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerAck {
    pub txn_id: u64,
    pub accepted: bool,
    pub new_balance: i64,
    pub reason: String,
}

/// A previously committed transaction, returned on idempotent replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordedTxn {
    pub player: PlayerId,
    pub new_balance: i64,
}

/// Persistence backing the ledger. Implementors must make [`commit`] atomic
/// with respect to [`balance`] and [`lookup_txn`] observed afterwards.
///
/// [`commit`]: LedgerStore::commit
/// [`balance`]: LedgerStore::balance
/// [`lookup_txn`]: LedgerStore::lookup_txn
pub trait LedgerStore {
    /// Error type surfaced by the backing store.
    type Error;

    /// Current gold balance for `player` (0 if the player has no balance yet).
    fn balance(&self, player: PlayerId) -> Result<i64, Self::Error>;

    /// The record of `txn_id` if it was already committed, else `None`.
    fn lookup_txn(&self, txn_id: u64) -> Result<Option<RecordedTxn>, Self::Error>;

    /// Append `txn` to the log and set `player`'s balance to `new_balance`.
    fn commit(&mut self, player: PlayerId, txn: &Txn, new_balance: i64) -> Result<(), Self::Error>;
}

/// The ledger: a thin, well-tested policy layer over a [`LedgerStore`].
#[derive(Debug, Default)]
pub struct Ledger<S> {
    store: S,
}

impl<S: LedgerStore> Ledger<S> {
    pub fn new(store: S) -> Ledger<S> {
        Ledger { store }
    }

    /// Borrow the underlying store (e.g. to read a balance for a hello reply).
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Mutable access to the underlying store.
    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Consume the ledger, returning the store.
    pub fn into_store(self) -> S {
        self.store
    }

    /// Apply `txn` to `player`. See the module docs for the guarantees.
    pub fn apply(&mut self, player: PlayerId, txn: &Txn) -> Result<LedgerAck, S::Error> {
        if let Some(recorded) = self.store.lookup_txn(txn.txn_id)? {
            return Ok(LedgerAck {
                txn_id: txn.txn_id,
                accepted: true,
                new_balance: recorded.new_balance,
                reason: "duplicate (idempotent replay)".to_string(),
            });
        }

        let balance = self.store.balance(player)?;
        match balance.checked_add(txn.amount_gold) {
            Some(new_balance) if new_balance >= 0 => {
                self.store.commit(player, txn, new_balance)?;
                Ok(LedgerAck {
                    txn_id: txn.txn_id,
                    accepted: true,
                    new_balance,
                    reason: String::new(),
                })
            }
            Some(_) => Ok(LedgerAck {
                txn_id: txn.txn_id,
                accepted: false,
                new_balance: balance,
                reason: "insufficient funds".to_string(),
            }),
            None => Ok(LedgerAck {
                txn_id: txn.txn_id,
                accepted: false,
                new_balance: balance,
                reason: "amount out of range".to_string(),
            }),
        }
    }
}

/// A simple in-memory [`LedgerStore`] for tests and tooling.
#[derive(Debug, Default)]
pub struct MemStore {
    balances: HashMap<PlayerId, i64>,
    txns: HashMap<u64, RecordedTxn>,
    log: Vec<(PlayerId, Txn, i64)>,
}

impl MemStore {
    pub fn new() -> MemStore {
        MemStore::default()
    }

    /// The append-only committed transaction log, in commit order.
    pub fn log(&self) -> &[(PlayerId, Txn, i64)] {
        &self.log
    }
}

impl LedgerStore for MemStore {
    type Error = std::convert::Infallible;

    fn balance(&self, player: PlayerId) -> Result<i64, Self::Error> {
        Ok(self.balances.get(&player).copied().unwrap_or(0))
    }

    fn lookup_txn(&self, txn_id: u64) -> Result<Option<RecordedTxn>, Self::Error> {
        Ok(self.txns.get(&txn_id).copied())
    }

    fn commit(&mut self, player: PlayerId, txn: &Txn, new_balance: i64) -> Result<(), Self::Error> {
        self.balances.insert(player, new_balance);
        self.txns.insert(
            txn.txn_id,
            RecordedTxn {
                player,
                new_balance,
            },
        );
        self.log.push((player, txn.clone(), new_balance));
        Ok(())
    }
}

// ===================================================================
// Shared port market
// ===================================================================

/// Port identifier (a stable id for a trading port / island market).
pub type PortId = u32;

/// Commodity / item identifier traded at a port.
pub type ItemId = u32;

/// A stock-changing trade against a port's shared market. `qty` is a signed
/// delta on the port's stock: positive sells *into* the port (stock rises),
/// negative buys *out of* it (stock falls). `unit_price` is the price recorded
/// for the (port, item) as authoritative state the client reads back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trade {
    pub txn_id: u64,
    pub port_id: PortId,
    pub item_id: ItemId,
    pub qty: i64,
    pub unit_price: i64,
}

/// The result of applying (or replaying) a [`Trade`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketAck {
    pub txn_id: u64,
    pub accepted: bool,
    pub port_id: PortId,
    pub item_id: ItemId,
    pub stock: i64,
    pub price: i64,
    pub reason: String,
}

/// A previously committed trade, returned on idempotent replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordedTrade {
    pub port_id: PortId,
    pub item_id: ItemId,
    pub stock: i64,
    pub price: i64,
}

/// Persistence backing the shared market. Implementors must make [`commit`]
/// atomic with respect to [`state`] and [`lookup_trade`] observed afterwards.
///
/// [`commit`]: MarketStore::commit
/// [`state`]: MarketStore::state
/// [`lookup_trade`]: MarketStore::lookup_trade
pub trait MarketStore {
    /// Error type surfaced by the backing store.
    type Error;

    /// Current `(stock, price)` for `(port, item)` — `(0, 0)` if untouched.
    fn state(&self, port: PortId, item: ItemId) -> Result<(i64, i64), Self::Error>;

    /// The record of `txn_id` if the trade was already committed, else `None`.
    fn lookup_trade(&self, txn_id: u64) -> Result<Option<RecordedTrade>, Self::Error>;

    /// Append `trade` to the dedup log and set `(port, item)` to the new state.
    fn commit(&mut self, trade: &Trade, new_stock: i64, new_price: i64) -> Result<(), Self::Error>;
}

/// The shared port market: a thin, well-tested policy layer over a
/// [`MarketStore`]. It mirrors [`Ledger`] exactly, applied to per-port stock
/// rather than a per-player balance.
#[derive(Debug, Default)]
pub struct Market<S> {
    store: S,
}

impl<S: MarketStore> Market<S> {
    pub fn new(store: S) -> Market<S> {
        Market { store }
    }

    /// Borrow the underlying store.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Mutable access to the underlying store.
    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Consume the market, returning the store.
    pub fn into_store(self) -> S {
        self.store
    }

    /// Apply `trade` to the shared per-port stock/price. See the module docs
    /// for the guarantees (idempotent by `txn_id`, non-negative stock,
    /// bounded/saturating math).
    pub fn apply(&mut self, trade: &Trade) -> Result<MarketAck, S::Error> {
        if let Some(recorded) = self.store.lookup_trade(trade.txn_id)? {
            return Ok(MarketAck {
                txn_id: trade.txn_id,
                accepted: true,
                port_id: recorded.port_id,
                item_id: recorded.item_id,
                stock: recorded.stock,
                price: recorded.price,
                reason: "duplicate (idempotent replay)".to_string(),
            });
        }

        let (stock, price) = self.store.state(trade.port_id, trade.item_id)?;

        if trade.unit_price < 0 {
            return Ok(self.reject(trade, stock, price, "invalid price"));
        }

        match stock.checked_add(trade.qty) {
            Some(new_stock) if new_stock >= 0 => {
                let new_price = trade.unit_price;
                self.store.commit(trade, new_stock, new_price)?;
                Ok(MarketAck {
                    txn_id: trade.txn_id,
                    accepted: true,
                    port_id: trade.port_id,
                    item_id: trade.item_id,
                    stock: new_stock,
                    price: new_price,
                    reason: String::new(),
                })
            }
            Some(_) => Ok(self.reject(trade, stock, price, "insufficient stock")),
            None => Ok(self.reject(trade, stock, price, "quantity out of range")),
        }
    }

    fn reject(&self, trade: &Trade, stock: i64, price: i64, reason: &str) -> MarketAck {
        MarketAck {
            txn_id: trade.txn_id,
            accepted: false,
            port_id: trade.port_id,
            item_id: trade.item_id,
            stock,
            price,
            reason: reason.to_string(),
        }
    }
}

/// A simple in-memory [`MarketStore`] for tests and tooling.
#[derive(Debug, Default)]
pub struct MemMarketStore {
    state: HashMap<(PortId, ItemId), (i64, i64)>,
    trades: HashMap<u64, RecordedTrade>,
    log: Vec<(Trade, i64, i64)>,
}

impl MemMarketStore {
    pub fn new() -> MemMarketStore {
        MemMarketStore::default()
    }

    /// The append-only committed trade log, in commit order.
    pub fn log(&self) -> &[(Trade, i64, i64)] {
        &self.log
    }
}

impl MarketStore for MemMarketStore {
    type Error = std::convert::Infallible;

    fn state(&self, port: PortId, item: ItemId) -> Result<(i64, i64), Self::Error> {
        Ok(self.state.get(&(port, item)).copied().unwrap_or((0, 0)))
    }

    fn lookup_trade(&self, txn_id: u64) -> Result<Option<RecordedTrade>, Self::Error> {
        Ok(self.trades.get(&txn_id).copied())
    }

    fn commit(&mut self, trade: &Trade, new_stock: i64, new_price: i64) -> Result<(), Self::Error> {
        self.state
            .insert((trade.port_id, trade.item_id), (new_stock, new_price));
        self.trades.insert(
            trade.txn_id,
            RecordedTrade {
                port_id: trade.port_id,
                item_id: trade.item_id,
                stock: new_stock,
                price: new_price,
            },
        );
        self.log.push((trade.clone(), new_stock, new_price));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn txn(id: u64, amount: i64) -> Txn {
        Txn {
            txn_id: id,
            amount_gold: amount,
            kind: 0,
            note: String::new(),
        }
    }

    #[test]
    fn credit_then_balance_updates() {
        let mut ledger = Ledger::new(MemStore::new());
        let ack = ledger.apply(1, &txn(1, 100)).unwrap();
        assert!(ack.accepted);
        assert_eq!(ack.new_balance, 100);
        assert_eq!(ledger.store().balance(1).unwrap(), 100);
    }

    #[test]
    fn replaying_same_txn_id_is_idempotent() {
        let mut ledger = Ledger::new(MemStore::new());
        let first = ledger.apply(1, &txn(1, 100)).unwrap();
        let second = ledger.apply(1, &txn(1, 100)).unwrap();
        assert!(second.accepted);
        assert_eq!(second.new_balance, first.new_balance);
        // Applied exactly once.
        assert_eq!(ledger.store().balance(1).unwrap(), 100);
        assert_eq!(ledger.store().log().len(), 1);
    }

    #[test]
    fn replay_does_not_reapply_even_with_different_amount() {
        // Same txn_id wins regardless of payload — the id is the dedup key.
        let mut ledger = Ledger::new(MemStore::new());
        ledger.apply(1, &txn(7, 50)).unwrap();
        let replay = ledger.apply(1, &txn(7, 999)).unwrap();
        assert_eq!(replay.new_balance, 50);
        assert_eq!(ledger.store().balance(1).unwrap(), 50);
    }

    #[test]
    fn balance_never_goes_negative() {
        let mut ledger = Ledger::new(MemStore::new());
        ledger.apply(1, &txn(1, 30)).unwrap();
        let ack = ledger.apply(1, &txn(2, -100)).unwrap();
        assert!(!ack.accepted);
        assert_eq!(ack.reason, "insufficient funds");
        assert_eq!(ack.new_balance, 30); // unchanged
        assert_eq!(ledger.store().balance(1).unwrap(), 30);
        assert_eq!(ledger.store().log().len(), 1); // rejection not logged
    }

    #[test]
    fn overflow_is_rejected_not_wrapped() {
        let mut ledger = Ledger::new(MemStore::new());
        ledger.apply(1, &txn(1, i64::MAX)).unwrap();
        let ack = ledger.apply(1, &txn(2, 1)).unwrap();
        assert!(!ack.accepted);
        assert_eq!(ack.reason, "amount out of range");
        assert_eq!(ledger.store().balance(1).unwrap(), i64::MAX);
    }

    #[test]
    fn distinct_players_have_independent_balances() {
        let mut ledger = Ledger::new(MemStore::new());
        ledger.apply(1, &txn(1, 100)).unwrap();
        ledger.apply(2, &txn(2, 40)).unwrap();
        assert_eq!(ledger.store().balance(1).unwrap(), 100);
        assert_eq!(ledger.store().balance(2).unwrap(), 40);
    }
}

#[cfg(test)]
mod market_tests {
    use super::*;

    fn trade(txn_id: u64, port: PortId, item: ItemId, qty: i64, price: i64) -> Trade {
        Trade {
            txn_id,
            port_id: port,
            item_id: item,
            qty,
            unit_price: price,
        }
    }

    #[test]
    fn selling_to_a_port_raises_shared_stock() {
        let mut market = Market::new(MemMarketStore::new());
        let ack = market.apply(&trade(1, 10, 5, 30, 100)).unwrap();
        assert!(ack.accepted);
        assert_eq!(ack.stock, 30);
        assert_eq!(ack.price, 100);
        assert_eq!(market.store().state(10, 5).unwrap(), (30, 100));
    }

    #[test]
    fn replaying_same_txn_id_does_not_double_apply() {
        let mut market = Market::new(MemMarketStore::new());
        let first = market.apply(&trade(7, 10, 5, 40, 100)).unwrap();
        // A resend with the same txn_id (different payload) must return the
        // recorded result and leave shared stock untouched — exactly the
        // ledger's txn_id dedup, now for the shared port market.
        let replay = market.apply(&trade(7, 10, 5, 999, 999)).unwrap();
        assert!(replay.accepted);
        assert_eq!(replay.stock, first.stock);
        assert_eq!(replay.price, first.price);
        assert_eq!(market.store().state(10, 5).unwrap(), (40, 100));
        assert_eq!(market.store().log().len(), 1);
    }

    #[test]
    fn buying_more_than_stock_is_rejected_and_leaves_state() {
        let mut market = Market::new(MemMarketStore::new());
        market.apply(&trade(1, 10, 5, 20, 100)).unwrap();
        // A buy (negative qty) larger than the stock cannot drive stock below
        // zero, mirroring the ledger's non-negative guarantee.
        let ack = market.apply(&trade(2, 10, 5, -50, 100)).unwrap();
        assert!(!ack.accepted);
        assert_eq!(ack.reason, "insufficient stock");
        assert_eq!(ack.stock, 20); // unchanged
        assert_eq!(market.store().state(10, 5).unwrap(), (20, 100));
        assert_eq!(market.store().log().len(), 1); // rejection not logged
    }

    #[test]
    fn buying_reduces_stock() {
        let mut market = Market::new(MemMarketStore::new());
        market.apply(&trade(1, 10, 5, 50, 100)).unwrap();
        let ack = market.apply(&trade(2, 10, 5, -20, 110)).unwrap();
        assert!(ack.accepted);
        assert_eq!(ack.stock, 30);
        assert_eq!(ack.price, 110);
    }

    #[test]
    fn overflowing_stock_is_rejected_not_wrapped() {
        let mut market = Market::new(MemMarketStore::new());
        market.apply(&trade(1, 10, 5, i64::MAX, 100)).unwrap();
        let ack = market.apply(&trade(2, 10, 5, 1, 100)).unwrap();
        assert!(!ack.accepted);
        assert_eq!(ack.reason, "quantity out of range");
        assert_eq!(market.store().state(10, 5).unwrap(), (i64::MAX, 100));
    }

    #[test]
    fn negative_price_is_rejected() {
        let mut market = Market::new(MemMarketStore::new());
        let ack = market.apply(&trade(1, 10, 5, 10, -1)).unwrap();
        assert!(!ack.accepted);
        assert_eq!(ack.reason, "invalid price");
        // Nothing committed.
        assert_eq!(market.store().state(10, 5).unwrap(), (0, 0));
        assert_eq!(market.store().log().len(), 0);
    }

    #[test]
    fn distinct_ports_and_items_are_independent() {
        let mut market = Market::new(MemMarketStore::new());
        market.apply(&trade(1, 10, 5, 30, 100)).unwrap();
        market.apply(&trade(2, 10, 6, 40, 200)).unwrap();
        market.apply(&trade(3, 11, 5, 50, 300)).unwrap();
        assert_eq!(market.store().state(10, 5).unwrap(), (30, 100));
        assert_eq!(market.store().state(10, 6).unwrap(), (40, 200));
        assert_eq!(market.store().state(11, 5).unwrap(), (50, 300));
    }
}
