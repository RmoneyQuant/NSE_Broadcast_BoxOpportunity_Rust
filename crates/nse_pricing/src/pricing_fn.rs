//! Wires `box_pricer`/`interest_calc`'s real formulas up to the
//! `PricingFn` boundary `nse_state::PricingSession` calls through.

use crate::{BoxPairId, BoxPriceResult, FourLegs, LegPrices, PricingFn, box_pricer, interest_calc};
use nse_adapt::QuoteUpdate;

/// Best bid/ask at level 1, converted from the wire's ×100 integer scale to
/// rupee floats -- the unit `box_pricer.py`/`interest_calc.py` expect.
fn bid_ask(q: &QuoteUpdate) -> (f64, f64) {
    (q.bid_price[0] as f64 / 100.0, q.ask_price[0] as f64 / 100.0)
}

/// The real `PricingFn`: prices both box directions from `legs`, then
/// annualizes each side's implied return against `pid`'s strike difference
/// and the caller-supplied days-to-expiry. Returns an all-`None` result
/// (rather than panicking) if called with incomplete legs -- callers are
/// expected to have already checked `FourLegs::complete()`, same as
/// `nse_state`'s scheduler does, but this stays safe either way.
pub fn real_pricing_fn() -> PricingFn {
    Box::new(|legs: &FourLegs, pid: BoxPairId, days: i32| price_pair(legs, pid, days))
}

fn price_pair(legs: &FourLegs, pid: BoxPairId, days: i32) -> BoxPriceResult {
    let days_to_expiry = days as i64;
    let (Some(c1), Some(c2), Some(p1), Some(p2)) =
        (legs.call_k1, legs.call_k2, legs.put_k1, legs.put_k2)
    else {
        return BoxPriceResult {
            days_to_expiry,
            ..Default::default()
        };
    };

    let (c1_bid, c1_ask) = bid_ask(c1);
    let (c2_bid, c2_ask) = bid_ask(c2);
    let (p1_bid, p1_ask) = bid_ask(p1);
    let (p2_bid, p2_ask) = bid_ask(p2);

    // pid.k1/k2 are raw ×100 strikes; the formulas operate in rupees.
    let strike_difference = (pid.k2 - pid.k1) as f64 / 100.0;

    let short_spread = box_pricer::box_sell_price(c1_bid, p1_ask, c2_ask, p2_bid);
    let long_spread = box_pricer::box_long_price(c1_ask, p1_bid, c2_bid, p2_ask);

    let long_interest_rate = long_spread.and_then(|price| {
        interest_calc::annualized_interest_rate(price, strike_difference, days_to_expiry)
    });
    let short_interest_rate = short_spread.and_then(|price| {
        interest_calc::annualized_interest_rate(price, strike_difference, days_to_expiry)
    });
    let long_period_rate = long_spread.and_then(|price| interest_calc::interest_rate(price, strike_difference));
    let short_period_rate = short_spread.and_then(|price| interest_calc::interest_rate(price, strike_difference));

    // Legs are structurally present here (the early return above already
    // ruled out a missing leg), so these are always Some, independent of
    // long_spread/short_spread possibly being None from a zero price --
    // that's exactly the case where seeing the raw leg prices matters most.
    let long_legs = Some(LegPrices { k1_bid: p1_bid, k1_ask: c1_ask, k2_bid: c2_bid, k2_ask: p2_ask });
    let short_legs = Some(LegPrices { k1_bid: c1_bid, k1_ask: p1_ask, k2_bid: p2_bid, k2_ask: c2_ask });

    BoxPriceResult {
        long_spread,
        short_spread,
        long_interest_rate,
        short_interest_rate,
        long_period_rate,
        short_period_rate,
        long_legs,
        short_legs,
        days_to_expiry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nse_adapt::Source;

    fn quote(token: i32, bid: i64, ask: i64) -> QuoteUpdate {
        let mut q = QuoteUpdate::from_enhanced_mbp(token, &[], 0);
        q.bid_price[0] = bid;
        q.ask_price[0] = ask;
        q.source = Source::Broadcast;
        q
    }

    #[test]
    fn prices_both_directions_and_annualizes_against_the_pair_strike_difference() {
        // strikes 23900/24000 (raw *100: 2390000/2400000), 30 days out.
        let c1 = quote(1, 12000, 12500); // call K1: bid 120.00, ask 125.00
        let p1 = quote(2, 3000, 3200); // put K1: bid 30.00, ask 32.00
        let c2 = quote(3, 4000, 4200); // call K2: bid 40.00, ask 42.00
        let p2 = quote(4, 6000, 6200); // put K2: bid 60.00, ask 62.00

        let legs = FourLegs {
            call_k1: Some(&c1),
            call_k2: Some(&c2),
            put_k1: Some(&p1),
            put_k2: Some(&p2),
        };
        let pid = BoxPairId::new(1, 2390000, 2400000);

        let result = price_pair(&legs, pid, 30);

        // short_spread = c1_bid - p1_ask - c2_ask + p2_bid = 120 - 32 - 42 + 60 = 106
        assert_eq!(result.short_spread, Some(106.0));
        // long_spread = c1_ask - p1_bid - c2_bid + p2_ask = 125 - 30 - 40 + 62 = 117
        assert_eq!(result.long_spread, Some(117.0));
        assert_eq!(result.days_to_expiry, 30);
        assert!(result.long_interest_rate.is_some());
        assert!(result.short_interest_rate.is_some());
    }

    #[test]
    fn incomplete_legs_yield_an_all_none_result_instead_of_panicking() {
        let c1 = quote(1, 12000, 12500);
        let legs = FourLegs {
            call_k1: Some(&c1),
            call_k2: None,
            put_k1: None,
            put_k2: None,
        };
        let result = price_pair(&legs, BoxPairId::new(1, 2390000, 2400000), 30);

        assert_eq!(result.long_spread, None);
        assert_eq!(result.short_spread, None);
    }
}
