//! Direct port of `box_pricer.py`. Inputs are rupee floats (best bid/ask,
//! already divided down from the wire's paise-scaled integers), matching
//! this codebase's `nse_fo.py`-derived convention -- see `bid_ask()` in
//! `pricing_fn.rs` for where that conversion happens.

/// Box sell (short box): `-Call(K1) + Put(K1) + Call(K2) - Put(K2)`.
///
/// Execution logic, same as the Python docstring:
/// - `-Call(K1)` -- sell Call(K1) -> use bid
/// - `+Put(K1)`  -- buy Put(K1)   -> use ask
/// - `+Call(K2)` -- buy Call(K2)  -> use ask
/// - `-Put(K2)`  -- sell Put(K2)  -> use bid
///
/// `None` if any required leg price is zero -- `box_pricer.py` raises
/// `ValueError` for this; here it's a skip instead, matching the scheduler's
/// existing "can't price this tick, try again next update" tolerance for
/// incomplete legs.
pub fn box_sell_price(c1_bid: f64, p1_ask: f64, c2_ask: f64, p2_bid: f64) -> Option<f64> {
    if c1_bid == 0.0 || p1_ask == 0.0 || c2_ask == 0.0 || p2_bid == 0.0 {
        return None;
    }
    Some(c1_bid - p1_ask - c2_ask + p2_bid)
}

/// Box long (buy box): `+Call(K1) - Put(K1) - Call(K2) + Put(K2)`.
///
/// Executable price: `C(K1)_ask - P(K1)_bid - C(K2)_bid + P(K2)_ask`.
pub fn box_long_price(c1_ask: f64, p1_bid: f64, c2_bid: f64, p2_ask: f64) -> Option<f64> {
    if c1_ask == 0.0 || p1_bid == 0.0 || c2_bid == 0.0 || p2_ask == 0.0 {
        return None;
    }
    Some(c1_ask - p1_bid - c2_bid + p2_ask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_sell_price_matches_the_formula() {
        // -Call(K1) + Put(K1) + Call(K2) - Put(K2), executable as
        // c1_bid - p1_ask - c2_ask + p2_bid
        let price = box_sell_price(120.0, 30.0, 40.0, 60.0).unwrap();
        assert_eq!(price, 120.0 - 30.0 - 40.0 + 60.0);
    }

    #[test]
    fn box_long_price_matches_the_formula() {
        let price = box_long_price(125.0, 28.0, 38.0, 65.0).unwrap();
        assert_eq!(price, 125.0 - 28.0 - 38.0 + 65.0);
    }

    #[test]
    fn zero_leg_price_is_skipped_not_computed() {
        assert_eq!(box_sell_price(0.0, 30.0, 40.0, 60.0), None);
        assert_eq!(box_sell_price(120.0, 0.0, 40.0, 60.0), None);
        assert_eq!(box_long_price(125.0, 28.0, 0.0, 65.0), None);
        assert_eq!(box_long_price(125.0, 28.0, 38.0, 0.0), None);
    }
}
