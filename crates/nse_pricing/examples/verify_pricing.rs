//! Cross-check tool: runs the same test vectors from
//! `fixtures/pricing_test_vectors.json` through the real Rust formulas
//! (`box_pricer` + `interest_calc`) and prints one JSON line per vector, in
//! the same shape `scripts/verify_pricing.py` prints its real-Python
//! results in -- run both and diff:
//!
//!     cargo run -p nse_pricing --example verify_pricing
//!     python scripts/verify_pricing.py
//!
//! Any mismatch means the Rust port has drifted from `box_pricer.py`/
//! `interest_calc.py`, not just that a hand-picked unit test happens to
//! agree with itself.
//!
//! Also prints `nse_pricing::cost`'s real, tax-adjusted box price (using
//! `CostConfig::default()`'s buy/sell round-trip multipliers) alongside the
//! raw pre-tax spread above. There's no Python side to diff this against.

use nse_adapt::QuoteUpdate;
use nse_pricing::{CostConfig, FourLegs, box_pricer, cost, interest_calc};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Leg {
    bid: f64,
    ask: f64,
}

#[derive(Deserialize)]
struct Vector {
    name: String,
    k1: f64,
    k2: f64,
    days_to_expiry: i64,
    call_k1: Leg,
    put_k1: Leg,
    call_k2: Leg,
    put_k2: Leg,
}

fn leg_quote(leg: &Leg) -> QuoteUpdate {
    let mut q = QuoteUpdate::from_enhanced_mbp(0, &[], 0);
    q.bid_price[0] = (leg.bid * 100.0).round() as i64;
    q.ask_price[0] = (leg.ask * 100.0).round() as i64;
    q
}

fn main() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/pricing_test_vectors.json"
    );
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    let vectors: Vec<Vector> =
        serde_json::from_str(&text).expect("invalid pricing_test_vectors.json");
    let cost_cfg = CostConfig::default();

    for v in vectors {
        let strike_difference = v.k2 - v.k1;

        let short_spread =
            box_pricer::box_sell_price(v.call_k1.bid, v.put_k1.ask, v.call_k2.ask, v.put_k2.bid);
        let long_spread =
            box_pricer::box_long_price(v.call_k1.ask, v.put_k1.bid, v.call_k2.bid, v.put_k2.ask);

        let short_interest_rate = short_spread.and_then(|p| {
            interest_calc::annualized_interest_rate(p, strike_difference, v.days_to_expiry)
        });
        let long_interest_rate = long_spread.and_then(|p| {
            interest_calc::annualized_interest_rate(p, strike_difference, v.days_to_expiry)
        });

        let (call_k1, put_k1, call_k2, put_k2) = (
            leg_quote(&v.call_k1),
            leg_quote(&v.put_k1),
            leg_quote(&v.call_k2),
            leg_quote(&v.put_k2),
        );
        let legs = FourLegs {
            call_k1: Some(&call_k1),
            call_k2: Some(&call_k2),
            put_k1: Some(&put_k1),
            put_k2: Some(&put_k2),
        };
        let long_spread_tax = cost::long_box_taxed_price(&legs, &cost_cfg);
        let short_spread_tax = cost::short_box_taxed_price(&legs, &cost_cfg);

        let row = json!({
            "name": v.name,
            "k1": v.k1,
            "k2": v.k2,
            "short_spread": short_spread,
            "short_interest_rate": short_interest_rate,
            "short_spread_tax": short_spread_tax,
            "long_spread": long_spread,
            "long_interest_rate": long_interest_rate,
            "long_spread_tax": long_spread_tax,
        });
        println!("{row}");
    }
}
