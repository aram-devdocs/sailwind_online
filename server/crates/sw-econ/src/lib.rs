//! `sw-econ` — the authoritative gold ledger.
//!
//! [`Ledger::apply`] is the single write path for balances. It is:
//! - **idempotent** by `txn_id` — replaying a transaction returns the original
//!   result and never double-applies it;
//! - **non-negative** — a transaction that would drive a balance below zero is
//!   rejected, leaving the balance untouched;
//! - **append-only** — accepted transactions are committed to the store's log.
//!
//! Storage is abstracted behind [`LedgerStore`] so the same logic runs against
//! an in-memory map in tests and against SQLite in the server.

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
