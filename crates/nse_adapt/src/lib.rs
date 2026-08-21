//! Normalizes `nse_decode::Message` into the source-agnostic `QuoteUpdate`
//! shape that `nse_state` stores. By design, `nse_adapt` is the only crate
//! downstream of `nse_state` allowed to depend on `nse_decode`'s
//! broadcast-specific wire types -- `nse_state` and everything above it only
//! ever sees `QuoteUpdate`, so a second feed source (TBT, snapshot) plugs in
//! by adding another `from_*` constructor here, not by touching the state
//! store.

use nse_decode::{BookLevel, MbpMessage, OiTickerMessage, Side};

const DEPTH: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Source {
    Broadcast,
    Tbt,
    Snapshot,
}

/// One token's normalized market state at a point in time. `Copy` and
/// allocation-free so it can move through a channel by value once threading
/// is wired up (Phase 1's `crossbeam-channel` plumbing).
#[derive(Clone, Copy, Debug)]
pub struct QuoteUpdate {
    pub token: i32,

    pub bid_price: [i64; DEPTH],
    pub bid_qty: [i64; DEPTH],
    pub bid_orders: [i64; DEPTH],
    pub ask_price: [i64; DEPTH],
    pub ask_qty: [i64; DEPTH],
    pub ask_orders: [i64; DEPTH],

    pub ltp: i64,
    pub ltq: i64,
    pub ltt: i64,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub atp: i64,
    pub total_traded_qty: i64,
    pub tbq: i64,
    pub tsq: i64,

    pub open_interest: i64,
    pub day_high_oi: i64,
    pub day_low_oi: i64,

    /// Feed-supplied timestamp, in nanoseconds since the Unix epoch, when
    /// the source carries one. Zero when not available (neither 7208 nor
    /// 17208 give one beyond `ltt`, which is itself frequently zero).
    pub exchange_time_ns: u64,
    /// Local capture time, in nanoseconds since the Unix epoch -- always set.
    pub recv_time_ns: u64,
    pub source: Source,
}

impl QuoteUpdate {
    fn empty(token: i32, recv_time_ns: u64, source: Source) -> Self {
        Self {
            token,
            bid_price: [0; DEPTH],
            bid_qty: [0; DEPTH],
            bid_orders: [0; DEPTH],
            ask_price: [0; DEPTH],
            ask_qty: [0; DEPTH],
            ask_orders: [0; DEPTH],
            ltp: 0,
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
            source,
        }
    }

    fn apply_book(&mut self, entries: &[BookLevel]) {
        for entry in entries {
            if entry.level == 0 || entry.level as usize > DEPTH {
                continue;
            }
            let idx = entry.level as usize - 1;
            match entry.side {
                Side::Bid => {
                    self.bid_price[idx] = entry.price as i64;
                    self.bid_qty[idx] = entry.qty as i64;
                    self.bid_orders[idx] = entry.orders as i64;
                }
                Side::Ask => {
                    self.ask_price[idx] = entry.price as i64;
                    self.ask_qty[idx] = entry.qty as i64;
                    self.ask_orders[idx] = entry.orders as i64;
                }
            }
        }
    }

    /// 7208 MBP -- full best-5 book plus LTP/OHLC/ATP/volume.
    pub fn from_mbp(m: &MbpMessage, recv_time_ns: u64) -> Self {
        let mut q = Self::empty(m.token, recv_time_ns, Source::Broadcast);
        q.apply_book(&m.entries);
        q.ltp = m.ltp as i64;
        q.ltq = m.ltq as i64;
        q.ltt = m.ltt as i64;
        q.open = m.open as i64;
        q.high = m.high as i64;
        q.low = m.low as i64;
        q.close = m.close as i64;
        q.atp = m.atp as i64;
        q.total_traded_qty = m.total_traded_qty as i64;
        q.tbq = m.tbq;
        q.tsq = m.tsq;
        q
    }

    /// 17208 enhanced MBP -- book only. LTP/OHLC/ATP/TBQ/TSQ/volume aren't
    /// decoded for this transaction code (see `nse_decode::EnhancedMbpMessage`
    /// docs), so they're left at zero rather than guessed.
    pub fn from_enhanced_mbp(token: i32, entries: &[BookLevel], recv_time_ns: u64) -> Self {
        let mut q = Self::empty(token, recv_time_ns, Source::Broadcast);
        q.apply_book(entries);
        q
    }

    /// 7202 OI ticker -- updates only the OI fields, carrying forward
    /// whatever book/LTP/OHLC state is already known for this token. Mirrors
    /// `nse_fo.py`'s `handle_oi_ticker()`, which mutates the existing
    /// `FeedState` in place rather than replacing it. `prev` is `None` the
    /// first time a token is seen (an OI ticker arriving before any MBP for
    /// that token) -- yields an all-zero book with just OI populated.
    pub fn merge_oi(prev: Option<&QuoteUpdate>, m: &OiTickerMessage, recv_time_ns: u64) -> Self {
        let mut q = prev
            .copied()
            .unwrap_or_else(|| Self::empty(m.token, recv_time_ns, Source::Broadcast));
        q.token = m.token;
        q.open_interest = m.open_interest as i64;
        q.day_high_oi = m.day_high_oi as i64;
        q.day_low_oi = m.day_low_oi as i64;
        q.recv_time_ns = recv_time_ns;
        q
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nse_decode::{BookLevel as DecBookLevel, Side as DecSide};

    fn mbp(token: i32) -> MbpMessage {
        MbpMessage {
            token,
            sequence: 1,
            ltp: 12550,
            atp: 12480,
            close: 12000,
            ltq: 50,
            ltt: 0,
            open: 11800,
            high: 12800,
            low: 11500,
            tbq: 15000,
            tsq: 12500,
            total_traded_qty: 987650,
            entries: vec![
                DecBookLevel {
                    side: DecSide::Bid,
                    level: 1,
                    price: 12525,
                    qty: 600,
                    orders: 12,
                },
                DecBookLevel {
                    side: DecSide::Ask,
                    level: 1,
                    price: 12575,
                    qty: 550,
                    orders: 11,
                },
            ],
        }
    }

    #[test]
    fn from_mbp_populates_book_and_scalars() {
        let q = QuoteUpdate::from_mbp(&mbp(35084), 1_000);
        assert_eq!(q.token, 35084);
        assert_eq!(q.ltp, 12550);
        assert_eq!(q.total_traded_qty, 987650);
        assert_eq!(q.bid_price[0], 12525);
        assert_eq!(q.bid_qty[0], 600);
        assert_eq!(q.ask_price[0], 12575);
        assert_eq!(q.recv_time_ns, 1_000);
        assert_eq!(q.source, Source::Broadcast);
    }

    #[test]
    fn merge_oi_preserves_book_and_updates_oi_fields_only() {
        let base = QuoteUpdate::from_mbp(&mbp(35084), 1_000);
        let oi = OiTickerMessage {
            token: 35084,
            market_type: 0,
            fill_price: 0,
            fill_volume: 0,
            open_interest: 4200,
            day_high_oi: 4500,
            day_low_oi: 3900,
        };

        let merged = QuoteUpdate::merge_oi(Some(&base), &oi, 2_000);

        assert_eq!(merged.open_interest, 4200);
        assert_eq!(merged.day_high_oi, 4500);
        assert_eq!(merged.day_low_oi, 3900);
        assert_eq!(merged.recv_time_ns, 2_000);
        // book and LTP carried forward untouched
        assert_eq!(merged.ltp, base.ltp);
        assert_eq!(merged.bid_price[0], base.bid_price[0]);
    }

    #[test]
    fn merge_oi_with_no_prior_state_yields_zeroed_book() {
        let oi = OiTickerMessage {
            token: 99,
            market_type: 0,
            fill_price: 0,
            fill_volume: 0,
            open_interest: 10,
            day_high_oi: 12,
            day_low_oi: 8,
        };
        let q = QuoteUpdate::merge_oi(None, &oi, 500);
        assert_eq!(q.token, 99);
        assert_eq!(q.open_interest, 10);
        assert_eq!(q.ltp, 0);
        assert_eq!(q.bid_price[0], 0);
    }
}
