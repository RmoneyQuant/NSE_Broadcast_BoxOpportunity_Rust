//! Owned live-market state (Phase 3) -- replaces the `FEEDS` module-level
//! dict in `nse_fo.py`. A `LiveMarketStateStore` is meant to be owned by a
//! single thread (the pricing/state thread in the target architecture);
//! nothing here reaches for a `Mutex`, because Rust's ownership rules
//! already make "no other thread can touch this map" the default, not a
//! rule someone has to remember.

use nse_adapt::QuoteUpdate;
use nse_refdata::InstrumentMaster;
use std::collections::HashMap;

#[derive(Default)]
pub struct LiveMarketStateStore {
    book: HashMap<i32, QuoteUpdate>,
}

impl LiveMarketStateStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, q: QuoteUpdate) {
        self.book.insert(q.token, q);
    }

    pub fn find(&self, token: i32) -> Option<&QuoteUpdate> {
        self.book.get(&token)
    }

    /// True if `token` has a stored quote no older than `max_age_ns` as of
    /// `now_ns`. A token never seen returns `false`.
    pub fn is_fresh(&self, token: i32, now_ns: u64, max_age_ns: u64) -> bool {
        self.book
            .get(&token)
            .is_some_and(|q| now_ns.saturating_sub(q.recv_time_ns) <= max_age_ns)
    }

    pub fn len(&self) -> usize {
        self.book.len()
    }

    pub fn is_empty(&self) -> bool {
        self.book.is_empty()
    }
}

/// Owns the contract universe and the live quote state together, constructed
/// once per live run or replay run -- the direct replacement for
/// `nse_fo.py`'s pair of module-level globals (`CONTRACTS`, `FEEDS`).
pub struct Session {
    pub instruments: InstrumentMaster,
    pub state: LiveMarketStateStore,
}

impl Session {
    pub fn new(instruments: InstrumentMaster) -> Self {
        Self {
            instruments,
            state: LiveMarketStateStore::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nse_adapt::Source;

    fn quote(token: i32, ltp: i64, recv_time_ns: u64) -> QuoteUpdate {
        QuoteUpdate {
            token,
            bid_price: [0; 5],
            bid_qty: [0; 5],
            bid_orders: [0; 5],
            ask_price: [0; 5],
            ask_qty: [0; 5],
            ask_orders: [0; 5],
            ltp,
            ltq: 0,
            ltt: 0,
            open: 0,
            high: 0,
            low: 0,
            close: 0,
            atp: 0,
            total_traded_qty: 0,
            tbq: 0,
            tsq: 0,
            open_interest: 0,
            day_high_oi: 0,
            day_low_oi: 0,
            exchange_time_ns: 0,
            recv_time_ns,
            source: Source::Broadcast,
        }
    }

    #[test]
    fn a_quote_applied_is_retrievable_by_token() {
        let mut store = LiveMarketStateStore::new();
        assert!(store.find(35084).is_none());

        store.apply(quote(35084, 12550, 1_000));

        let found = store
            .find(35084)
            .expect("quote should be retrievable by token");
        assert_eq!(found.ltp, 12550);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn applying_the_same_token_again_overwrites_not_duplicates() {
        let mut store = LiveMarketStateStore::new();
        store.apply(quote(35084, 12550, 1_000));
        store.apply(quote(35084, 12600, 2_000));

        assert_eq!(store.len(), 1);
        assert_eq!(store.find(35084).unwrap().ltp, 12600);
    }

    #[test]
    fn freshness_respects_max_age() {
        let mut store = LiveMarketStateStore::new();
        store.apply(quote(1, 100, 1_000_000_000)); // 1s in ns

        assert!(store.is_fresh(1, 1_500_000_000, 1_000_000_000)); // 0.5s old, within 1s budget
        assert!(!store.is_fresh(1, 3_000_000_000, 1_000_000_000)); // 2s old, over budget
        assert!(!store.is_fresh(2, 1_500_000_000, 1_000_000_000)); // never seen
    }
}
