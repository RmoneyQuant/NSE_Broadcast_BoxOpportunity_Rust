//! Shared value types for box-spread pricing -- `BoxPairId`, `FourLegs`, and
//! the output row shape (`OpportunityRecord`), per §05 of the Rust HLD.
//! These are owned here (not in `nse_state`) because `nse_state`'s
//! dependency index and scheduler (Phase 4) need them, but the actual
//! pricing/tax math (Phase 5) is a separate, externally-gated phase blocked
//! on real formulas -- splitting the crate this way means P4 doesn't have to
//! wait on P5, and P5 fills in real math behind the same `PricingFn`/`TaxFn`
//! boundary without moving these types anywhere.
//!
//! As of Phase 5, `box_pricer.rs`/`interest_calc.rs` are direct ports of the
//! real box-pricing and annualized-return formulas, wired up in
//! `pricing_fn.rs`'s `real_pricing_fn()`. `cost.rs` is the real round-trip
//! tax model -- a buy-side and sell-side multiplier, applied once each to
//! the respective leg price -- wired up in `tax_fn.rs`'s `real_tax_fn()`.
//! Phase 6 adds `filter.rs`'s `OpportunityFilter` (threshold/liquidity/
//! staleness) and `DropEvent`/`QualifyingSide` below, for `nse_state`'s
//! scheduler to emit when a previously-qualifying side stops qualifying.

pub mod box_pricer;
pub mod cost;
pub mod filter;
pub mod interest_calc;
mod pricing_fn;
mod tax_fn;

pub use cost::CostConfig;
pub use filter::OpportunityFilter;
pub use interest_calc::{days_to_expiry, expiry_date};
pub use pricing_fn::real_pricing_fn;
pub use tax_fn::real_tax_fn;

use nse_adapt::{QuoteUpdate, Source};
use serde::Serialize;

/// Identifies one box-spread strike pair within one expiry. `k1`/`k2` are
/// raw integer strikes (paise, `ContractInfo::strike_raw`'s scale) rather
/// than `f64`, since this type has to be `Hash`/`Eq` to key a `HashMap`.
/// `expiry_id` is the contract's raw NSE expiry epoch (`ContractInfo::expiry_epoch`) --
/// the HLD sketch uses `u32` for this field, but the raw epoch value doesn't
/// reliably fit `u32`, so this port uses `i64` instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BoxPairId {
    pub expiry_id: i64,
    pub k1: i64,
    pub k2: i64,
}

impl BoxPairId {
    /// Always orders `k1 < k2` so `(23900, 24000)` and `(24000, 23900)`
    /// hash identically -- the index only ever needs one canonical id per pair.
    pub fn new(expiry_id: i64, strike_a: i64, strike_b: i64) -> Self {
        let (k1, k2) = if strike_a <= strike_b {
            (strike_a, strike_b)
        } else {
            (strike_b, strike_a)
        };
        Self { expiry_id, k1, k2 }
    }
}

/// Borrowed view of the four legs a box spread needs at one strike pair:
/// call and put at the lower strike (`k1`), call and put at the upper strike
/// (`k2`). Borrowing from the state store instead of copying means this can
/// never outlive the store it points into -- the compiler enforces "don't
/// price against stale, already-overwritten memory" instead of it being a
/// convention to remember.
pub struct FourLegs<'a> {
    pub call_k1: Option<&'a QuoteUpdate>,
    pub call_k2: Option<&'a QuoteUpdate>,
    pub put_k1: Option<&'a QuoteUpdate>,
    pub put_k2: Option<&'a QuoteUpdate>,
}

impl<'a> FourLegs<'a> {
    pub fn complete(&self) -> bool {
        self.call_k1.is_some()
            && self.call_k2.is_some()
            && self.put_k1.is_some()
            && self.put_k2.is_some()
    }
}

/// The four raw leg prices a box formula actually consumed for one
/// direction, labeled by strike + role (bid/ask) rather than call/put --
/// long and short boxes use opposite instruments at each strike (buying
/// the call and selling the put at `k1`, or the reverse), so which
/// instrument plays "the k1 bid" versus "the k1 ask" differs by direction.
/// This is whichever one `box_pricer::box_long_price`/`box_sell_price`
/// actually used for that side -- the same four numbers those functions
/// take as arguments, just named instead of positional.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct LegPrices {
    pub k1_bid: f64,
    pub k1_ask: f64,
    pub k2_bid: f64,
    pub k2_ask: f64,
}

/// Real pricing/rate output, from `box_pricer.py`/`interest_calc.py`.
/// `long_spread`/`short_spread` are `None` when a required leg price was
/// zero (see `box_pricer::box_sell_price`/`box_long_price`); the interest
/// rates are `None` for the same reason `interest_calc.py`'s
/// `interest_calculation` is -- no time left to annualize over, or a zero
/// box price. `long_legs`/`short_legs` are `Some` whenever all four legs
/// were structurally present, even if the corresponding spread ended up
/// `None` from a zero price -- seeing which leg was zero is exactly what
/// explains the `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BoxPriceResult {
    pub long_spread: Option<f64>,
    pub short_spread: Option<f64>,
    pub long_interest_rate: Option<f64>,
    pub short_interest_rate: Option<f64>,
    pub long_period_rate: Option<f64>,
    pub short_period_rate: Option<f64>,
    pub long_legs: Option<LegPrices>,
    pub short_legs: Option<LegPrices>,
    pub days_to_expiry: i64,
}

/// The real, tax-adjusted box price for each direction -- `long_spread`/
/// `short_spread` (`BoxPriceResult`) with each leg's own price scaled once
/// by `CostConfig`'s buy/sell multiplier before combining (see `cost.rs`).
/// This is the actual cash you'd deploy (long) or generate (short) after
/// STT/exchange charges/SEBI fee/stamp duty/GST/brokerage, in rupees per
/// share -- not an additive cost on top of `long_spread`/`short_spread`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BoxCostResult {
    pub long_spread_tax: f64,
    pub short_spread_tax: f64,
}

/// A `PricingFn`/`TaxFn`-style pluggable closure, matching the HLD's
/// `Box<dyn Fn(...)>` boundary -- swappable at construction time, no I/O.
/// `PricingFn` takes the pair id alongside the legs because the interest
/// calculation needs the strike difference, which isn't otherwise derivable
/// from `FourLegs` (it only carries quotes) -- a deliberate deviation from
/// the HLD's original `Fn(&FourLegs, i32)` sketch, written before the real
/// formula's inputs were known. `TaxFn` takes `&FourLegs` rather than the
/// HLD's `&BoxPriceResult` for the same reason: the buy/sell tax
/// multipliers apply per leg, before combining into a net box price, not
/// to the net spread itself -- see `cost.rs`.
pub type PricingFn = Box<dyn Fn(&FourLegs, BoxPairId, i32) -> BoxPriceResult + Send>;
pub type TaxFn = Box<dyn Fn(&FourLegs, &CostConfig) -> BoxCostResult + Send>;

/// Which box direction a qualification transition applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum QualifyingSide {
    Long,
    Short,
}

/// Emitted by `nse_state`'s scheduler when a strike pair's `side` stops
/// qualifying (per `OpportunityFilter`) on this tick, having qualified on
/// the previous one -- the "qualifying-diff" the roadmap's §06 calls for,
/// so a downstream sink/dashboard can explicitly retract an opportunity
/// instead of just silently seeing no further updates for it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct DropEvent {
    pub expiry: i64,
    pub strike_1: i64,
    pub strike_2: i64,
    pub side: QualifyingSide,
    pub timestamp_ns: u64,
}

/// The pipeline's output row -- one box-spread opportunity, priced and
/// costed, per §05's "10+2 field" table (adapted: `interest_rate` /
/// `annualized_interest_rate` split into a `long_`/`short_` pair each,
/// matching how `interest_calc.py` is actually called -- once per box
/// direction, since long and short box prices imply different returns).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct OpportunityRecord {
    pub expiry: i64,
    pub strike_1: i64,
    pub strike_2: i64,
    pub long_spread: Option<f64>,
    pub short_spread: Option<f64>,
    pub long_spread_tax: f64,
    pub short_spread_tax: f64,
    pub long_interest_rate: Option<f64>,
    pub short_interest_rate: Option<f64>,
    pub long_period_rate: Option<f64>,
    pub short_period_rate: Option<f64>,
    pub long_legs: Option<LegPrices>,
    pub short_legs: Option<LegPrices>,
    pub days_to_expiry: i64,
    pub timestamp_ns: u64,
    pub source: Source,
    pub complete: bool,
}

impl OpportunityRecord {
    pub fn build(
        pid: BoxPairId,
        priced: BoxPriceResult,
        costed: BoxCostResult,
        timestamp_ns: u64,
        source: Source,
    ) -> Self {
        Self {
            expiry: pid.expiry_id,
            strike_1: pid.k1,
            strike_2: pid.k2,
            long_spread: priced.long_spread,
            short_spread: priced.short_spread,
            long_spread_tax: costed.long_spread_tax,
            short_spread_tax: costed.short_spread_tax,
            long_interest_rate: priced.long_interest_rate,
            short_interest_rate: priced.short_interest_rate,
            long_period_rate: priced.long_period_rate,
            short_period_rate: priced.short_period_rate,
            long_legs: priced.long_legs,
            short_legs: priced.short_legs,
            days_to_expiry: priced.days_to_expiry,
            timestamp_ns,
            source,
            complete: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_pair_id_canonicalizes_strike_order() {
        assert_eq!(
            BoxPairId::new(1, 24000, 23900),
            BoxPairId::new(1, 23900, 24000)
        );
    }

    #[test]
    fn four_legs_complete_requires_all_four() {
        let q = QuoteUpdate::from_enhanced_mbp(1, &[], 0);
        let mut legs = FourLegs {
            call_k1: Some(&q),
            call_k2: Some(&q),
            put_k1: Some(&q),
            put_k2: None,
        };
        assert!(!legs.complete());
        legs.put_k2 = Some(&q);
        assert!(legs.complete());
    }
}
