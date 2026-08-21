//! Wires `cost.rs`'s real round-trip tax multipliers up to the `TaxFn`
//! boundary `nse_state::PricingSession` calls through.

use crate::{BoxCostResult, CostConfig, FourLegs, TaxFn, cost};

/// The real `TaxFn`: the real, tax-adjusted box price for both directions
/// from `legs` -- what `nse_pricing::OpportunityRecord::long_spread_tax`/
/// `short_spread_tax` actually hold from here on (the field names predate
/// this multiplier model but keep their meaning: "the spread, after tax").
/// `None` from either side (a missing leg) becomes `0.0` -- callers are
/// expected to have already checked `FourLegs::complete()`, same as
/// `real_pricing_fn` assumes.
pub fn real_tax_fn() -> TaxFn {
    Box::new(|legs: &FourLegs, cfg: &CostConfig| BoxCostResult {
        long_spread_tax: cost::long_box_taxed_price(legs, cfg).unwrap_or(0.0),
        short_spread_tax: cost::short_box_taxed_price(legs, cfg).unwrap_or(0.0),
    })
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
    fn real_tax_fn_matches_the_cost_module_directly() {
        let cfg = CostConfig::default();
        let call_k1 = quote(12000, 12500);
        let put_k1 = quote(3000, 3200);
        let call_k2 = quote(4000, 4200);
        let put_k2 = quote(6000, 6200);
        let legs = FourLegs { call_k1: Some(&call_k1), call_k2: Some(&call_k2), put_k1: Some(&put_k1), put_k2: Some(&put_k2) };

        let tax_fn = real_tax_fn();
        let costed = tax_fn(&legs, &cfg);

        assert_eq!(costed.long_spread_tax, cost::long_box_taxed_price(&legs, &cfg).unwrap());
        assert_eq!(costed.short_spread_tax, cost::short_box_taxed_price(&legs, &cfg).unwrap());
    }
}
