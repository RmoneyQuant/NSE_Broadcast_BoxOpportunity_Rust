//! Opportunity filter -- Phase 6 of the roadmap. Decides whether a priced
//! box pair is worth surfacing: a minimum implied annualized rate per side,
//! plus mechanical liquidity (quoted size) and staleness (quote age)
//! checks that apply to both sides equally.
//!
//! Long and short qualify independently -- a pair can have a good long-box
//! opportunity and no short-box opportunity (or vice versa) at the same
//! tick, which is exactly why `nse_state`'s scheduler tracks qualification
//! per side, not per pair.

use crate::{BoxPriceResult, FourLegs};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpportunityFilter {
    /// Minimum annualized implied rate (percent) to qualify. `None` disables
    /// the rate check for that side entirely.
    pub min_long_rate: Option<f64>,
    pub min_short_rate: Option<f64>,
    /// Minimum quoted size required on *both* the bid and ask of every leg.
    /// `0` disables the liquidity check.
    pub min_leg_qty: i64,
    /// Maximum age, in nanoseconds, a leg's quote may be (relative to the
    /// pair's own latest-leg timestamp -- see `nse_state`'s scheduler) and
    /// still count as fresh. `u64::MAX` disables the staleness check.
    pub max_quote_age_ns: u64,
}

impl OpportunityFilter {
    /// Every check disabled -- everything with a complete, priced set of
    /// legs qualifies. A reasonable starting point before real thresholds
    /// are chosen; tighten field by field once you know what you're looking for.
    pub fn permissive() -> Self {
        Self {
            min_long_rate: None,
            min_short_rate: None,
            min_leg_qty: 0,
            max_quote_age_ns: u64::MAX,
        }
    }

    pub fn qualifies_long(&self, priced: &BoxPriceResult, legs: &FourLegs, as_of_ns: u64) -> bool {
        let Some(rate) = priced.long_interest_rate else {
            return false;
        };
        if self.min_long_rate.is_some_and(|min| rate < min) {
            return false;
        }
        self.legs_liquid_and_fresh(legs, as_of_ns)
    }

    pub fn qualifies_short(&self, priced: &BoxPriceResult, legs: &FourLegs, as_of_ns: u64) -> bool {
        let Some(rate) = priced.short_interest_rate else {
            return false;
        };
        if self.min_short_rate.is_some_and(|min| rate < min) {
            return false;
        }
        self.legs_liquid_and_fresh(legs, as_of_ns)
    }

    fn legs_liquid_and_fresh(&self, legs: &FourLegs, as_of_ns: u64) -> bool {
        [legs.call_k1, legs.call_k2, legs.put_k1, legs.put_k2]
            .into_iter()
            .all(|leg| {
                let Some(q) = leg else { return false };
                let fresh = as_of_ns.saturating_sub(q.recv_time_ns) <= self.max_quote_age_ns;
                let liquid = q.bid_qty[0] >= self.min_leg_qty && q.ask_qty[0] >= self.min_leg_qty;
                fresh && liquid
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nse_adapt::QuoteUpdate;

    fn quote(bid_qty: i64, ask_qty: i64, recv_time_ns: u64) -> QuoteUpdate {
        let mut q = QuoteUpdate::from_enhanced_mbp(1, &[], recv_time_ns);
        q.bid_qty[0] = bid_qty;
        q.ask_qty[0] = ask_qty;
        q
    }

    fn priced(long_rate: Option<f64>, short_rate: Option<f64>) -> BoxPriceResult {
        BoxPriceResult {
            long_interest_rate: long_rate,
            short_interest_rate: short_rate,
            ..Default::default()
        }
    }

    #[test]
    fn permissive_filter_accepts_any_complete_priced_pair() {
        let q = quote(1, 1, 0);
        let legs = FourLegs {
            call_k1: Some(&q),
            call_k2: Some(&q),
            put_k1: Some(&q),
            put_k2: Some(&q),
        };
        let filter = OpportunityFilter::permissive();

        assert!(filter.qualifies_long(&priced(Some(-500.0), None), &legs, 0));
        assert!(filter.qualifies_short(&priced(None, Some(0.0)), &legs, 0));
    }

    #[test]
    fn missing_rate_never_qualifies_regardless_of_threshold() {
        let q = quote(1, 1, 0);
        let legs = FourLegs {
            call_k1: Some(&q),
            call_k2: Some(&q),
            put_k1: Some(&q),
            put_k2: Some(&q),
        };
        let filter = OpportunityFilter::permissive();
        assert!(!filter.qualifies_long(&priced(None, None), &legs, 0));
    }

    #[test]
    fn rate_below_minimum_does_not_qualify() {
        let q = quote(1, 1, 0);
        let legs = FourLegs {
            call_k1: Some(&q),
            call_k2: Some(&q),
            put_k1: Some(&q),
            put_k2: Some(&q),
        };
        let filter = OpportunityFilter {
            min_long_rate: Some(10.0),
            ..OpportunityFilter::permissive()
        };

        assert!(!filter.qualifies_long(&priced(Some(5.0), None), &legs, 0));
        assert!(filter.qualifies_long(&priced(Some(15.0), None), &legs, 0));
    }

    #[test]
    fn thin_leg_fails_the_liquidity_check() {
        let thin = quote(0, 1, 0); // zero bid_qty
        let legs = FourLegs {
            call_k1: Some(&thin),
            call_k2: Some(&thin),
            put_k1: Some(&thin),
            put_k2: Some(&thin),
        };
        let filter = OpportunityFilter {
            min_leg_qty: 1,
            ..OpportunityFilter::permissive()
        };

        assert!(!filter.qualifies_long(&priced(Some(50.0), None), &legs, 0));
    }

    #[test]
    fn stale_leg_fails_the_staleness_check() {
        let stale = quote(1, 1, 0); // recv_time_ns = 0
        let legs = FourLegs {
            call_k1: Some(&stale),
            call_k2: Some(&stale),
            put_k1: Some(&stale),
            put_k2: Some(&stale),
        };
        let filter = OpportunityFilter {
            max_quote_age_ns: 1_000,
            ..OpportunityFilter::permissive()
        };

        assert!(!filter.qualifies_long(&priced(Some(50.0), None), &legs, 5_000)); // 5000ns old, over budget
        assert!(filter.qualifies_long(&priced(Some(50.0), None), &legs, 500)); // 500ns old, within budget
    }

    #[test]
    fn missing_leg_never_qualifies() {
        let q = quote(1, 1, 0);
        let legs = FourLegs {
            call_k1: Some(&q),
            call_k2: None,
            put_k1: Some(&q),
            put_k2: Some(&q),
        };
        let filter = OpportunityFilter::permissive();
        assert!(!filter.qualifies_long(&priced(Some(50.0), None), &legs, 0));
    }
}
