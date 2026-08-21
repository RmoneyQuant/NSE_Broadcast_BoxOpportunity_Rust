//! Per-side pricing view types -- originally `OpportunityRecord`'s wire
//! encoding for the (now-removed) web dashboard's WebSocket protocol, kept
//! because `box_scanner_live`'s native Win32 desktop window needs the
//! exact same shape for its table rows: `OpportunityRecord` always carries
//! both directions' numbers together (per §05's "10+2 field" design), but a
//! long/short table row needs just one side's numbers plus its own
//! qualify/drop lifecycle. Deliberate deviations from the original web
//! spec, noted where they matter below:
//!
//! - `k1`/`k2` are sent as rupees (e.g. `24400`), converted down from
//!   `BoxPairId`'s raw ×100 scale -- matches the spec's own example values.
//! - `long_spread`/`short_spread` are sent as this pipeline's real per-share
//!   values, *not* multiplied by a lot size. The spec's example numbers
//!   (`73218.40`) are lot-scaled (`(k2-k1) * LOT`, `LOT = 25` in the
//!   mockup's fake data) -- but lot size isn't tracked anywhere in this
//!   pipeline's real data (see `nse_refdata::ContractInfo`), and guessing
//!   one to multiply by would present a fabricated-looking number as if it
//!   were real. Sending genuine per-share figures instead.

use nse_pricing::{BoxPairId, OpportunityRecord, QualifyingSide};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Long,
    Short,
}

impl From<QualifyingSide> for Side {
    fn from(s: QualifyingSide) -> Self {
        match s {
            QualifyingSide::Long => Side::Long,
            QualifyingSide::Short => Side::Short,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct WirePairId {
    pub expiry_id: i64,
    pub k1: i64,
    pub k2: i64,
}

impl From<BoxPairId> for WirePairId {
    fn from(pid: BoxPairId) -> Self {
        Self {
            expiry_id: pid.expiry_id,
            k1: pid.k1 / 100,
            k2: pid.k2 / 100,
        }
    }
}

/// `OpportunityRecord` as it appears on the wire, scoped to one `side` --
/// unlike the Rust-side record (which always carries both directions'
/// numbers together, per §05's "10+2 field" design), the wire protocol
/// tracks long/short as independently qualifying, independently cached
/// entities (spec §02), so each side gets its own wire record with only
/// that side's numbers.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WireOpportunityRecord {
    pub pair_id: WirePairId,
    /// ISO date (`"2026-08-28"`), derived from `OpportunityRecord::expiry`
    /// via `nse_pricing::expiry_date`. `None` (rendered as `null`) only if
    /// the raw expiry epoch somehow doesn't convert -- shouldn't happen for
    /// real contract data.
    pub expiry: Option<chrono::NaiveDate>,
    pub days_to_expiry: i64,
    /// Raw pre-tax box price (`box_pricer.rs`, unchanged from `box_pricer.py`).
    pub spread: f64,
    /// Real, tax-adjusted box price -- the actual cash to deploy (long) or
    /// generated (short), after the buy/sell round-trip tax multipliers
    /// (`nse_pricing::cost`). Not an additive cost on top of `spread`.
    pub spread_tax: f64,
    /// Guaranteed payoff at expiry (`strike_difference`) minus what a long
    /// box costs, or minus what a short box costs the other way --
    /// `strike_difference - spread` (long) / `spread - strike_difference`
    /// (short) -- computed off the raw pre-tax `spread`, not `spread_tax`.
    pub profit: f64,
    /// The un-annualized return over the trade's life --
    /// `interest_calc::interest_rate`, distinct from `annualized_interest_rate`.
    pub interest_rate: f64,
    pub annualized_interest_rate: f64,
    /// The four raw leg prices `spread` was actually computed from, labeled
    /// by strike + role -- see `nse_pricing::LegPrices`.
    pub k1_bid: f64,
    pub k1_ask: f64,
    pub k2_bid: f64,
    pub k2_ask: f64,
    pub timestamp_ns: u64,
    pub source: String,
    pub complete: bool,
}

impl WireOpportunityRecord {
    /// `None` if `side`'s rate/spread isn't actually available on `record`
    /// (e.g. a zero-leg-priced side) -- callers should treat that as "don't
    /// send this side," same as the filter would.
    pub fn from_record(record: &OpportunityRecord, side: Side) -> Option<Self> {
        let (spread, annualized_rate, period_rate, legs) = match side {
            Side::Long => (record.long_spread?, record.long_interest_rate?, record.long_period_rate?, record.long_legs?),
            Side::Short => (record.short_spread?, record.short_interest_rate?, record.short_period_rate?, record.short_legs?),
        };
        let spread_tax = match side {
            Side::Long => record.long_spread_tax,
            Side::Short => record.short_spread_tax,
        };

        let strike_difference = (record.strike_2 - record.strike_1) as f64 / 100.0;
        let profit = match side {
            Side::Long => strike_difference - spread,
            Side::Short => spread - strike_difference,
        };

        Some(Self {
            pair_id: WirePairId {
                expiry_id: record.expiry,
                k1: record.strike_1 / 100,
                k2: record.strike_2 / 100,
            },
            expiry: nse_pricing::expiry_date(record.expiry),
            days_to_expiry: record.days_to_expiry,
            spread,
            spread_tax,
            profit,
            interest_rate: period_rate,
            annualized_interest_rate: annualized_rate,
            k1_bid: legs.k1_bid,
            k1_ask: legs.k1_ask,
            k2_bid: legs.k2_bid,
            k2_ask: legs.k2_ask,
            timestamp_ns: record.timestamp_ns,
            source: format!("{:?}", record.source).to_uppercase(),
            complete: record.complete,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nse_adapt::Source;
    use nse_pricing::LegPrices;

    fn sample_record() -> OpportunityRecord {
        OpportunityRecord {
            expiry: 1_450_000_000,
            strike_1: 2_440_000,
            strike_2: 2_470_000,
            long_spread: Some(73218.40),
            short_spread: Some(70115.90),
            long_spread_tax: 41.25,
            short_spread_tax: 36.00,
            long_interest_rate: Some(7.42),
            short_interest_rate: Some(6.21),
            long_period_rate: Some(0.74),
            short_period_rate: Some(0.62),
            long_legs: Some(LegPrices { k1_bid: 10.0, k1_ask: 10.5, k2_bid: 5.0, k2_ask: 5.5 }),
            short_legs: Some(LegPrices { k1_bid: 9.5, k1_ask: 10.0, k2_bid: 4.5, k2_ask: 5.0 }),
            days_to_expiry: 10,
            timestamp_ns: 1_755_500_000_010_000_000,
            source: Source::Broadcast,
            complete: true,
        }
    }

    #[test]
    fn wire_pair_id_converts_from_paise_scale_to_rupees() {
        let pid = BoxPairId::new(1, 2_440_000, 2_470_000);
        let wire: WirePairId = pid.into();
        assert_eq!(wire.k1, 24_400);
        assert_eq!(wire.k2, 24_700);
    }

    #[test]
    fn from_record_selects_the_right_side_numbers() {
        let record = sample_record();

        let long = WireOpportunityRecord::from_record(&record, Side::Long).unwrap();
        assert_eq!(long.spread, 73218.40);
        assert_eq!(long.spread_tax, 41.25);
        assert_eq!(long.interest_rate, 0.74);
        assert_eq!(long.annualized_interest_rate, 7.42);
        assert_eq!(long.days_to_expiry, 10);
        assert_eq!(long.k1_bid, 10.0);
        assert_eq!(long.k1_ask, 10.5);
        assert_eq!(long.k2_bid, 5.0);
        assert_eq!(long.k2_ask, 5.5);
        assert_eq!(long.profit, 24_700.0 - 24_400.0 - 73218.40);
        assert_eq!(long.pair_id.k1, 24_400);

        let short = WireOpportunityRecord::from_record(&record, Side::Short).unwrap();
        assert_eq!(short.spread, 70115.90);
        assert_eq!(short.interest_rate, 0.62);
        assert_eq!(short.annualized_interest_rate, 6.21);
        assert_eq!(short.profit, 70115.90 - (24_700.0 - 24_400.0));
    }

    #[test]
    fn from_record_is_none_when_that_side_never_priced() {
        let mut record = sample_record();
        record.long_spread = None;
        assert!(WireOpportunityRecord::from_record(&record, Side::Long).is_none());
        assert!(WireOpportunityRecord::from_record(&record, Side::Short).is_some());
    }

    #[test]
    fn source_is_uppercased_for_the_wire() {
        let record = sample_record();
        let wire = WireOpportunityRecord::from_record(&record, Side::Long).unwrap();
        assert_eq!(wire.source, "BROADCAST");
    }

}
