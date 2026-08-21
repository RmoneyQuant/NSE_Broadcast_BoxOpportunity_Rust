//! Real NSE F&O round-trip transaction-cost model: a single multiplicative
//! factor per side (buy/sell), already covering STT, exchange transaction
//! charges, SEBI turnover fee, stamp duty, GST, and brokerage combined.
//! User-supplied, same standing as `box_pricer.py`/`interest_calc.py` --
//! not self-derived, no rate-verification caveat needed.
//!
//! Applied once, directly to each leg's own price: the ask (what you pay to
//! buy that leg) is scaled by `CostConfig::buy_tax_multiplier`; the bid
//! (what you receive selling that leg) is scaled by
//! `CostConfig::sell_tax_multiplier`. This holds regardless of whether the
//! leg is part of a long or short box -- "the ask price" always means "cost
//! to buy that leg" and "the bid price" always means "proceeds from selling
//! that leg," independent of which combination of legs makes up the box, so
//! there's no separate buy/sell-side bookkeeping needed per box direction
//! the way the old itemized STT/stamp-duty stack needed (STT only on sells,
//! stamp duty only on buys -- that asymmetry is already baked into these
//! two multipliers).

use crate::{FourLegs, box_pricer};

/// Round-trip tax multipliers for one leg's price, per side.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CostConfig {
    /// Multiplies an ask price to get the real cost of buying that leg,
    /// after STT/exchange charges/SEBI fee/stamp duty/GST/brokerage.
    pub buy_tax_multiplier: f64,
    /// Multiplies a bid price to get the real proceeds from selling that
    /// leg, net of STT/exchange charges/SEBI fee/stamp duty/GST/brokerage.
    pub sell_tax_multiplier: f64,
}

impl Default for CostConfig {
    fn default() -> Self {
        Self { buy_tax_multiplier: 1.001515, sell_tax_multiplier: 0.998485 }
    }
}

fn bid_ask(q: &nse_adapt::QuoteUpdate) -> (f64, f64) {
    (q.bid_price[0] as f64 / 100.0, q.ask_price[0] as f64 / 100.0)
}

/// Real, tax-adjusted LONG box price -- the actual cash you'd have to
/// deploy: buy Call(K1), sell Put(K1), sell Call(K2), buy Put(K2), each
/// leg's own price scaled once by the multiplier matching how it executes.
/// `None` if any leg is missing or -- same as `box_pricer::box_long_price`
/// -- any required leg price is zero.
pub fn long_box_taxed_price(legs: &FourLegs, cfg: &CostConfig) -> Option<f64> {
    let (_, c1_ask) = bid_ask(legs.call_k1?);
    let (p1_bid, _) = bid_ask(legs.put_k1?);
    let (c2_bid, _) = bid_ask(legs.call_k2?);
    let (_, p2_ask) = bid_ask(legs.put_k2?);

    box_pricer::box_long_price(c1_ask * cfg.buy_tax_multiplier, p1_bid * cfg.sell_tax_multiplier, c2_bid * cfg.sell_tax_multiplier, p2_ask * cfg.buy_tax_multiplier)
}

/// Real, tax-adjusted SHORT box price -- the actual cash you'd generate:
/// sell Call(K1), buy Put(K1), buy Call(K2), sell Put(K2).
pub fn short_box_taxed_price(legs: &FourLegs, cfg: &CostConfig) -> Option<f64> {
    let (c1_bid, _) = bid_ask(legs.call_k1?);
    let (_, p1_ask) = bid_ask(legs.put_k1?);
    let (_, c2_ask) = bid_ask(legs.call_k2?);
    let (p2_bid, _) = bid_ask(legs.put_k2?);

    box_pricer::box_sell_price(c1_bid * cfg.sell_tax_multiplier, p1_ask * cfg.buy_tax_multiplier, c2_ask * cfg.buy_tax_multiplier, p2_bid * cfg.sell_tax_multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nse_adapt::QuoteUpdate;

    fn quote(bid: i64, ask: i64) -> QuoteUpdate {
        let mut q = QuoteUpdate::from_enhanced_mbp(1, &[], 0);
        q.bid_price[0] = bid;
        q.ask_price[0] = ask;
        q
    }

    #[test]
    fn taxed_price_matches_multiplying_each_leg_once_then_summing() {
        let cfg = CostConfig::default();
        let call_k1 = quote(12000, 12500); // 120.00 / 125.00
        let put_k1 = quote(3000, 3200); // 30.00 / 32.00
        let call_k2 = quote(4000, 4200); // 40.00 / 42.00
        let put_k2 = quote(6000, 6200); // 60.00 / 62.00
        let legs = FourLegs { call_k1: Some(&call_k1), call_k2: Some(&call_k2), put_k1: Some(&put_k1), put_k2: Some(&put_k2) };

        let long = long_box_taxed_price(&legs, &cfg).unwrap();
        let short = short_box_taxed_price(&legs, &cfg).unwrap();

        let expected_long = 125.0 * cfg.buy_tax_multiplier - 30.0 * cfg.sell_tax_multiplier - 40.0 * cfg.sell_tax_multiplier + 62.0 * cfg.buy_tax_multiplier;
        let expected_short = 120.0 * cfg.sell_tax_multiplier - 32.0 * cfg.buy_tax_multiplier - 42.0 * cfg.buy_tax_multiplier + 60.0 * cfg.sell_tax_multiplier;

        assert!((long - expected_long).abs() < 1e-9);
        assert!((short - expected_short).abs() < 1e-9);
    }

    #[test]
    fn buy_multiplier_raises_cost_and_sell_multiplier_lowers_proceeds() {
        // Buying costs more after tax (multiplier > 1); selling nets less
        // (multiplier < 1) -- so the taxed long box (net cash OUT) should
        // come out higher than the untaxed raw price, and the taxed short
        // box (net cash IN) should come out lower.
        let cfg = CostConfig::default();
        let call_k1 = quote(12000, 12500);
        let put_k1 = quote(3000, 3200);
        let call_k2 = quote(4000, 4200);
        let put_k2 = quote(6000, 6200);
        let legs = FourLegs { call_k1: Some(&call_k1), call_k2: Some(&call_k2), put_k1: Some(&put_k1), put_k2: Some(&put_k2) };

        let raw_long = box_pricer::box_long_price(125.0, 30.0, 40.0, 62.0).unwrap();
        let raw_short = box_pricer::box_sell_price(120.0, 32.0, 42.0, 60.0).unwrap();

        let taxed_long = long_box_taxed_price(&legs, &cfg).unwrap();
        let taxed_short = short_box_taxed_price(&legs, &cfg).unwrap();

        assert!(taxed_long > raw_long);
        assert!(taxed_short < raw_short);
    }

    #[test]
    fn missing_leg_yields_none() {
        let cfg = CostConfig::default();
        let call_k1 = quote(12000, 12500);
        let legs = FourLegs { call_k1: Some(&call_k1), call_k2: None, put_k1: None, put_k2: None };
        assert_eq!(long_box_taxed_price(&legs, &cfg), None);
        assert_eq!(short_box_taxed_price(&legs, &cfg), None);
    }
}
