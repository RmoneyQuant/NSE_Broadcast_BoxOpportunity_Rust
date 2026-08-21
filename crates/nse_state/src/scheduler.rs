//! Incremental recompute scheduler -- Phase 4 (dirty-set mechanism) and
//! Phase 6 (qualifying-diff) of the roadmap. A quote update dirties only
//! the strike pairs that use the changed token as a leg; `drain_dirty`
//! recomputes exactly those, decides per-side qualification via
//! `OpportunityFilter`, and emits an explicit `Dropped` event whenever a
//! side that qualified on the previous tick stops qualifying on this one --
//! the "qualifying-diff" §06 calls for, so a downstream sink doesn't have
//! to infer a drop from an opportunity simply going quiet.

use crate::index::StrikePairDependencyIndex;
use crate::store::LiveMarketStateStore;
use nse_adapt::{QuoteUpdate, Source};
use nse_pricing::{
    BoxCostResult, BoxPairId, BoxPriceResult, CostConfig, DropEvent, FourLegs, OpportunityFilter,
    OpportunityRecord, PricingFn, QualifyingSide, TaxFn,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// One `drain_dirty` output: either a priced opportunity (at least one side
/// still qualifies) or a notice that a previously-qualifying side no longer
/// does. A single dirtied pair can emit both in the same call -- e.g. the
/// long side collapses while the short side is still live.
///
/// `Opportunity` carries `long_qualifies`/`short_qualifies` alongside the
/// record (which always has both sides' numbers, per §05's "10+2 field"
/// design) so a per-side consumer -- the dashboard sink, in particular --
/// knows which side(s) to actually surface without re-deriving the filter
/// decision itself.
#[derive(Clone, Debug, PartialEq)]
pub enum SchedulerEvent {
    Opportunity {
        // Boxed: `OpportunityRecord` grew past clippy's large-enum-variant
        // threshold once it started carrying both sides' leg prices
        // (`LegPrices` x2) alongside the rate/spread fields, and `Dropped`
        // shouldn't pay for that in every `Vec<SchedulerEvent>` slot.
        record: Box<OpportunityRecord>,
        long_qualifies: bool,
        short_qualifies: bool,
    },
    Dropped(DropEvent),
}

pub struct PricingSession {
    state: LiveMarketStateStore,
    index: Arc<StrikePairDependencyIndex>,
    dirty: HashSet<BoxPairId>,
    pricing_fn: PricingFn,
    tax_fn: TaxFn,
    cost_cfg: CostConfig,
    filter: OpportunityFilter,
    /// (long_qualified, short_qualified) as of the last tick each pair was
    /// recomputed -- the only state the qualifying-diff needs.
    last_qualifying: HashMap<BoxPairId, (bool, bool)>,
}

impl PricingSession {
    pub fn new(
        index: Arc<StrikePairDependencyIndex>,
        pricing_fn: PricingFn,
        tax_fn: TaxFn,
        cost_cfg: CostConfig,
    ) -> Self {
        Self {
            state: LiveMarketStateStore::new(),
            index,
            dirty: HashSet::new(),
            pricing_fn,
            tax_fn,
            cost_cfg,
            filter: OpportunityFilter::permissive(),
            last_qualifying: HashMap::new(),
        }
    }

    /// Overrides the default permissive filter (everything with a complete,
    /// priced pair qualifies) with real thresholds.
    pub fn with_filter(mut self, filter: OpportunityFilter) -> Self {
        self.filter = filter;
        self
    }

    /// A no-op `PricingFn`/`TaxFn` pair -- Phase 4's stand-in for Phase 5's
    /// real formulas. Both return fixed, non-zero values so a `drain_dirty`
    /// test can assert a record was actually produced, not just that a
    /// default-initialized one was. Superseded by `with_real_pricing` for
    /// the pricing side now that real formulas exist; kept for tests that
    /// only care about the dirty-set mechanism, not real numbers.
    pub fn with_stub_pricing(index: Arc<StrikePairDependencyIndex>) -> Self {
        let pricing_fn: PricingFn = Box::new(
            |_legs: &FourLegs, _pid: BoxPairId, _days_to_expiry: i32| BoxPriceResult {
                long_spread: Some(1.0),
                short_spread: Some(1.0),
                // populated (not left at the Default None) so the permissive
                // filter's rate check has something to pass -- see OpportunityFilter::qualifies_long/short.
                long_interest_rate: Some(1.0),
                short_interest_rate: Some(1.0),
                ..Default::default()
            },
        );
        Self::new(index, pricing_fn, stub_tax_fn(), CostConfig::default())
    }

    /// Real box-pricing, annualized-return, and transaction-cost formulas --
    /// `nse_pricing::real_pricing_fn`/`real_tax_fn`. The cost side uses
    /// `CostConfig::default()`'s standard NSE rates; see `nse_pricing::cost`'s
    /// module docs before trusting those defaults for real capital.
    pub fn with_real_pricing(index: Arc<StrikePairDependencyIndex>) -> Self {
        Self::new(
            index,
            nse_pricing::real_pricing_fn(),
            nse_pricing::real_tax_fn(),
            CostConfig::default(),
        )
    }

    pub fn state(&self) -> &LiveMarketStateStore {
        &self.state
    }

    pub fn dirty_len(&self) -> usize {
        self.dirty.len()
    }

    pub fn on_quote(&mut self, q: QuoteUpdate) {
        let token = q.token;
        self.state.apply(q);
        for pid in self.index.pairs_for(token) {
            self.dirty.insert(*pid);
        }
    }

    fn legs_for(&self, pid: BoxPairId) -> Option<FourLegs<'_>> {
        let legs = self.index.legs_of(pid)?;
        Some(FourLegs {
            call_k1: self.state.find(legs.call_k1),
            call_k2: self.state.find(legs.call_k2),
            put_k1: self.state.find(legs.put_k1),
            put_k2: self.state.find(legs.put_k2),
        })
    }

    /// Recomputes every dirtied pair, skipping any still missing a leg's
    /// quote (partial data -- same tolerance as the HLD sketch); clears the
    /// dirty set regardless of how many pairs actually priced. For each
    /// pair, emits `Dropped` for any side that qualified last tick and
    /// doesn't now, then `Opportunity` if at least one side still qualifies.
    pub fn drain_dirty(&mut self, out: &mut Vec<SchedulerEvent>) {
        let pending: Vec<BoxPairId> = self.dirty.drain().collect();

        for pid in pending {
            let Some(legs) = self.legs_for(pid) else {
                continue;
            };
            if !legs.complete() {
                continue;
            }

            let today = chrono::Local::now().date_naive();
            let days = nse_pricing::days_to_expiry(pid.expiry_id, today) as i32;
            let priced = (self.pricing_fn)(&legs, pid, days);
            let costed = (self.tax_fn)(&legs, &self.cost_cfg);

            let timestamp_ns = [legs.call_k1, legs.call_k2, legs.put_k1, legs.put_k2]
                .into_iter()
                .flatten()
                .map(|q| q.recv_time_ns)
                .max()
                .unwrap_or(0);
            let source = legs.call_k1.map(|q| q.source).unwrap_or(Source::Broadcast);

            let long_ok = self.filter.qualifies_long(&priced, &legs, timestamp_ns);
            let short_ok = self.filter.qualifies_short(&priced, &legs, timestamp_ns);
            let (prev_long, prev_short) = self
                .last_qualifying
                .get(&pid)
                .copied()
                .unwrap_or((false, false));

            if prev_long && !long_ok {
                out.push(SchedulerEvent::Dropped(DropEvent {
                    expiry: pid.expiry_id,
                    strike_1: pid.k1,
                    strike_2: pid.k2,
                    side: QualifyingSide::Long,
                    timestamp_ns,
                }));
            }
            if prev_short && !short_ok {
                out.push(SchedulerEvent::Dropped(DropEvent {
                    expiry: pid.expiry_id,
                    strike_1: pid.k1,
                    strike_2: pid.k2,
                    side: QualifyingSide::Short,
                    timestamp_ns,
                }));
            }

            if long_ok || short_ok {
                let record = OpportunityRecord::build(pid, priced, costed, timestamp_ns, source);
                out.push(SchedulerEvent::Opportunity {
                    record: Box::new(record),
                    long_qualifies: long_ok,
                    short_qualifies: short_ok,
                });
            }

            self.last_qualifying.insert(pid, (long_ok, short_ok));
        }
    }
}

/// Always-zero `TaxFn` -- kept for tests that only care about the dirty-set
/// mechanism, not real cost numbers. `with_real_pricing` uses
/// `nse_pricing::real_tax_fn()` instead.
fn stub_tax_fn() -> TaxFn {
    Box::new(|_legs: &FourLegs, _cfg: &CostConfig| BoxCostResult {
        long_spread_tax: 0.0,
        short_spread_tax: 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::StrikePairDependencyIndex;
    use nse_refdata::InstrumentMaster;
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A raw NSE expiry epoch (`ContractInfo::expiry_epoch` scale, i.e.
    /// pre-`NSE_EPOCH_OFFSET`) landing `days_ahead` days from whenever the
    /// test actually runs -- real-formula tests need a genuinely future
    /// expiry, since `interest_calc::annualized_interest_rate` returns
    /// `None` once `days_to_expiry <= 0`, which a hardcoded past date would
    /// eventually become.
    fn future_expiry_epoch(days_ahead: i64) -> i64 {
        let expiry_date = chrono::Local::now().date_naive() + chrono::Duration::days(days_ahead);
        expiry_date
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp()
            - 315_532_800
    }

    fn worked_example_index() -> StrikePairDependencyIndex {
        let expiry = future_expiry_epoch(30);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("nse_sched_test_{id}.csv"));
        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, "C,OPTIDX,1,OPTIDX,NIFTY,{expiry},2390000,CE").unwrap();
        writeln!(file, "C,OPTIDX,2,OPTIDX,NIFTY,{expiry},2390000,PE").unwrap();
        writeln!(file, "C,OPTIDX,3,OPTIDX,NIFTY,{expiry},2400000,CE").unwrap();
        writeln!(file, "C,OPTIDX,4,OPTIDX,NIFTY,{expiry},2400000,PE").unwrap();
        writeln!(file, "C,OPTIDX,5,OPTIDX,NIFTY,{expiry},2410000,CE").unwrap();
        writeln!(file, "C,OPTIDX,6,OPTIDX,NIFTY,{expiry},2410000,PE").unwrap();
        let instruments = InstrumentMaster::load(&path, &["NIFTY"], &["OPTIDX"]);
        fs::remove_file(&path).ok();
        StrikePairDependencyIndex::build(&instruments)
    }

    fn find_opportunity(out: &[SchedulerEvent], k1: i64, k2: i64) -> Option<&OpportunityRecord> {
        out.iter().find_map(|e| match e {
            SchedulerEvent::Opportunity { record, .. }
                if record.strike_1 == k1 && record.strike_2 == k2 =>
            {
                Some(record.as_ref())
            }
            _ => None,
        })
    }

    fn quote(token: i32, recv_time_ns: u64) -> QuoteUpdate {
        QuoteUpdate {
            token,
            bid_price: [0; 5],
            bid_qty: [0; 5],
            bid_orders: [0; 5],
            ask_price: [0; 5],
            ask_qty: [0; 5],
            ask_orders: [0; 5],
            ltp: 100,
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
    fn on_quote_dirties_exactly_the_pairs_that_use_the_token_as_a_leg() {
        let index = Arc::new(worked_example_index());
        let mut session = PricingSession::with_stub_pricing(index);

        // token 3 = 24000 CE -- a leg of two pairs in this 3-strike universe:
        // (23900,24000) and (24000,24100).
        session.on_quote(quote(3, 1_000));
        assert_eq!(session.dirty_len(), 2);
    }

    #[test]
    fn dirtying_the_put_at_an_already_dirty_strike_does_not_grow_the_set() {
        // With 3 strikes (23900/24000/24100), token 1 (23900 CE) and token 2
        // (23900 PE) are legs of the exact same two pairs -- (23900,24000)
        // and (23900,24100) -- so the dirty *set* should stay a union, not
        // grow by 2 more entries on the second update.
        let index = Arc::new(worked_example_index());
        let mut session = PricingSession::with_stub_pricing(index);

        session.on_quote(quote(1, 1_000)); // 23900 CE -> 2 pairs
        let after_first = session.dirty_len();
        session.on_quote(quote(2, 1_000)); // 23900 PE -> same 2 pairs (union, not +2)

        assert_eq!(
            session.dirty_len(),
            after_first,
            "PE at the same strike as an already-dirty CE shouldn't add new pairs"
        );
    }

    #[test]
    fn drain_dirty_skips_incomplete_pairs_and_clears_the_dirty_set() {
        let index = Arc::new(worked_example_index());
        let mut session = PricingSession::with_stub_pricing(index);

        // Only 3 of the 4 legs for (23900,24000) have a quote -- put_k2 (token 4) never arrives.
        session.on_quote(quote(1, 1_000)); // 23900 CE
        session.on_quote(quote(2, 1_000)); // 23900 PE
        session.on_quote(quote(3, 1_000)); // 24000 CE

        let mut out = Vec::new();
        session.drain_dirty(&mut out);

        assert!(
            out.is_empty(),
            "no pair has all four legs yet, so nothing should price"
        );
        assert_eq!(
            session.dirty_len(),
            0,
            "drain_dirty must clear the dirty set even when nothing priced"
        );
    }

    #[test]
    fn drain_dirty_prices_a_pair_once_all_four_legs_are_present() {
        let index = Arc::new(worked_example_index());
        let mut session = PricingSession::with_stub_pricing(index);

        session.on_quote(quote(1, 1_000)); // 23900 CE
        session.on_quote(quote(2, 2_000)); // 23900 PE
        session.on_quote(quote(3, 3_000)); // 24000 CE
        session.on_quote(quote(4, 4_000)); // 24000 PE -- completes (23900,24000)

        let mut out = Vec::new();
        session.drain_dirty(&mut out);

        let record = find_opportunity(&out, 2390000, 2400000)
            .expect("the now-complete pair should have priced");
        assert_eq!(record.long_spread, Some(1.0)); // from the stub PricingFn
        assert_eq!(record.timestamp_ns, 4_000); // latest of the four legs' recv_time_ns
        assert!(record.complete);
    }

    fn quote_with_book(token: i32, bid: i64, ask: i64, recv_time_ns: u64) -> QuoteUpdate {
        let mut q = quote(token, recv_time_ns);
        q.bid_price[0] = bid;
        q.ask_price[0] = ask;
        q
    }

    #[test]
    fn with_real_pricing_produces_the_real_box_sell_and_box_long_formulas() {
        let index = Arc::new(worked_example_index());
        let mut session = PricingSession::with_real_pricing(index);

        // strike pair (23900,24000): call K1 bid/ask 120.00/125.00,
        // put K1 30.00/32.00, call K2 40.00/42.00, put K2 60.00/62.00.
        session.on_quote(quote_with_book(1, 12000, 12500, 1_000)); // 23900 CE
        session.on_quote(quote_with_book(2, 3000, 3200, 2_000)); // 23900 PE
        session.on_quote(quote_with_book(3, 4000, 4200, 3_000)); // 24000 CE
        session.on_quote(quote_with_book(4, 6000, 6200, 4_000)); // 24000 PE

        let mut out = Vec::new();
        session.drain_dirty(&mut out);

        let record = find_opportunity(&out, 2390000, 2400000)
            .expect("the now-complete pair should have priced");
        // short_spread = c1_bid - p1_ask - c2_ask + p2_bid = 120 - 32 - 42 + 60 = 106
        assert_eq!(record.short_spread, Some(106.0));
        // long_spread = c1_ask - p1_bid - c2_bid + p2_ask = 125 - 30 - 40 + 62 = 117
        assert_eq!(record.long_spread, Some(117.0));
        // real tax-adjusted price now, via nse_pricing::real_tax_fn() --
        // exact value is covered by nse_pricing's own tests; here just
        // confirm it's wired through and produces a real (positive) number,
        // not the old stub zero.
        assert!(record.long_spread_tax > 0.0);
        assert!(record.short_spread_tax > 0.0);
    }

    /// P6's exit criterion: replaying a captured session produces the
    /// expected drop event when a pair's edge collapses mid-replay.
    /// Simulated here as two successive `on_quote`/`drain_dirty` ticks on
    /// the same pair, rather than via a separate `replay_harness` binary --
    /// the roadmap describes that as a real captured-session tool, which is
    /// future work; this exercises the identical scheduler code path.
    #[test]
    fn an_edge_collapsing_mid_replay_emits_a_drop_event_for_just_that_side() {
        let index = Arc::new(worked_example_index());
        let filter = OpportunityFilter {
            min_long_rate: Some(0.0),
            min_short_rate: Some(0.0),
            ..OpportunityFilter::permissive()
        };
        let mut session = PricingSession::with_real_pricing(index).with_filter(filter);

        // Tick 1, strike pair (23900,24000), strike_difference=100:
        // long_spread = c1_ask(25) - p1_bid(5) - c2_bid(5) + p2_ask(25) = 40 -- well under 100, positive rate, qualifies.
        // short_spread = c1_bid(24) - p1_ask(6) - c2_ask(6) + p2_bid(24) = 36 -- also qualifies.
        session.on_quote(quote_with_book(1, 2400, 2500, 1_000)); // 23900 CE bid=24.00 ask=25.00
        session.on_quote(quote_with_book(2, 500, 600, 1_000)); // 23900 PE bid=5.00 ask=6.00
        session.on_quote(quote_with_book(3, 500, 600, 1_000)); // 24000 CE bid=5.00 ask=6.00
        session.on_quote(quote_with_book(4, 2400, 2500, 1_000)); // 24000 PE bid=24.00 ask=25.00

        let mut tick1 = Vec::new();
        session.drain_dirty(&mut tick1);

        assert!(
            find_opportunity(&tick1, 2390000, 2400000).is_some(),
            "first tick should surface an opportunity"
        );
        assert!(
            !tick1
                .iter()
                .any(|e| matches!(e, SchedulerEvent::Dropped(_))),
            "nothing qualified before tick 1, so nothing can drop yet"
        );

        // Tick 2: 23900 CE's ask jumps to 90.00 -- long_spread becomes
        // 90-5-5+25=105 (over strike_difference), long stops qualifying.
        // Its bid is unchanged, so short_spread (which uses c1_bid, not
        // c1_ask) is untouched -- short must still qualify.
        session.on_quote(quote_with_book(1, 2400, 9000, 2_000));

        let mut tick2 = Vec::new();
        session.drain_dirty(&mut tick2);

        let dropped_long = tick2
            .iter()
            .any(|e| matches!(e, SchedulerEvent::Dropped(d) if d.side == QualifyingSide::Long && d.strike_1 == 2390000 && d.strike_2 == 2400000));
        assert!(
            dropped_long,
            "long side's edge collapsed, so it must emit a Dropped event"
        );

        let dropped_short = tick2
            .iter()
            .any(|e| matches!(e, SchedulerEvent::Dropped(d) if d.side == QualifyingSide::Short));
        assert!(
            !dropped_short,
            "short side's inputs never changed, so it must not drop"
        );

        assert!(
            find_opportunity(&tick2, 2390000, 2400000).is_some(),
            "short side still qualifies, so the pair must still surface an opportunity"
        );
    }
}
