//! Direct port of `interest_calc.py`.

/// NSE epoch timestamps (contract expiry) are seconds since 1980-01-01, not
/// the standard Unix epoch (1970-01-01) -- same offset `nse_refdata` applies
/// when formatting `expiry_date` for display. `BoxPairId::expiry_id` carries
/// the *raw* (pre-offset) epoch, so this is where the pair id's expiry
/// finally gets turned into a real calendar date.
const NSE_EPOCH_OFFSET: i64 = 315_532_800;

/// Calendar days from `today` to the expiry named by `expiry_id`
/// (a `BoxPairId::expiry_id` / raw NSE epoch). Mirrors `interest_calc.py`'s
/// `(expiry_date - date.today()).days` -- `today` is a parameter rather than
/// reading the system clock internally, so this stays a pure, deterministic
/// function to test.
pub fn days_to_expiry(expiry_id: i64, today: chrono::NaiveDate) -> i64 {
    let Some(date) = expiry_date(expiry_id) else {
        return 0;
    };
    (date - today).num_days()
}

/// Converts a `BoxPairId::expiry_id` (raw NSE epoch) into a calendar date --
/// the same offset `days_to_expiry` applies, exposed standalone for callers
/// that need the date itself rather than a day count (e.g. the dashboard
/// sink's wire encoding, which sends an ISO date string).
pub fn expiry_date(expiry_id: i64) -> Option<chrono::NaiveDate> {
    chrono::DateTime::from_timestamp(expiry_id + NSE_EPOCH_OFFSET, 0).map(|dt| dt.date_naive())
}

/// Annualized return implied by paying/receiving `box_price` now for a
/// guaranteed `strike_difference` payoff at expiry, `days_to_expiry` days
/// out. `strike_difference` and `box_price` are both rupees (not the raw
/// ×100 wire scale). Mirrors `calculate_interest_rate`'s
/// `interest_calculation` field exactly, including its two "leave it blank"
/// cases: no time left to annualize over (`days_to_expiry <= 0`), or a zero
/// box price (no meaningful return).
///
/// Rounds to 2 decimal places like the Python's `round(x, 2)`, via
/// round-half-away-from-zero rather than Python's round-half-to-even --
/// the two only disagree exactly on a `.xx5` boundary, which real bid/ask
/// derived prices essentially never land on.
pub fn annualized_interest_rate(
    box_price: f64,
    strike_difference: f64,
    days_to_expiry: i64,
) -> Option<f64> {
    if days_to_expiry <= 0 || box_price == 0.0 {
        return None;
    }
    let rate =
        ((strike_difference - box_price) / box_price) / days_to_expiry as f64 * 365.0 * 100.0;
    Some((rate * 100.0).round() / 100.0)
}

/// The un-annualized return implied by paying/receiving `box_price` now for
/// a guaranteed `strike_difference` payoff at expiry -- same numerator/
/// denominator as `annualized_interest_rate`, just not scaled by
/// `365/days_to_expiry`. `interest_calc.py` itself defines a different,
/// unrelated "ratio" (`box_price / strike_difference`, cost as a fraction
/// of payoff) under `ratio_calculation` that was never ported here; this is
/// not that -- it's the period version of the same return `annualized_interest_rate`
/// annualizes, so the two numbers are directly comparable (one is the other
/// divided by `days_to_expiry` and multiplied by 365).
pub fn interest_rate(box_price: f64, strike_difference: f64) -> Option<f64> {
    if box_price == 0.0 {
        return None;
    }
    let rate = ((strike_difference - box_price) / box_price) * 100.0;
    Some((rate * 100.0).round() / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn days_to_expiry_matches_calendar_difference() {
        // expiry_id chosen so expiry_id + NSE_EPOCH_OFFSET lands on 2026-09-29.
        let expiry_id = NaiveDate::from_ymd_opt(2026, 9, 29)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp()
            - NSE_EPOCH_OFFSET;
        let today = NaiveDate::from_ymd_opt(2026, 8, 19).unwrap();
        assert_eq!(days_to_expiry(expiry_id, today), 41);
    }

    #[test]
    fn annualized_interest_rate_matches_the_formula() {
        // box_price=95, strike_difference=100, 30 days out.
        let rate = annualized_interest_rate(95.0, 100.0, 30).unwrap();
        let expected: f64 =
            (((100.0 - 95.0) / 95.0) / 30.0 * 365.0 * 100.0 * 100.0_f64).round() / 100.0;
        assert_eq!(rate, expected);
    }

    #[test]
    fn zero_or_negative_days_to_expiry_yields_none() {
        assert_eq!(annualized_interest_rate(95.0, 100.0, 0), None);
        assert_eq!(annualized_interest_rate(95.0, 100.0, -1), None);
    }

    #[test]
    fn zero_box_price_yields_none() {
        assert_eq!(annualized_interest_rate(0.0, 100.0, 30), None);
    }

    #[test]
    fn interest_rate_matches_the_formula_without_day_scaling() {
        let rate = interest_rate(95.0, 100.0).unwrap();
        let expected: f64 = (((100.0 - 95.0) / 95.0) * 100.0 * 100.0_f64).round() / 100.0;
        assert_eq!(rate, expected);
    }

    #[test]
    fn interest_rate_is_the_annualized_rate_divided_by_365_over_days() {
        // interest_rate * (365 / days_to_expiry) == annualized_interest_rate,
        // since annualized just scales the same numerator/denominator --
        // loose tolerance since both sides independently round to 2dp
        // *before* this comparison, and the 365/30 scale-up amplifies that.
        let period = interest_rate(95.0, 100.0).unwrap();
        let annualized = annualized_interest_rate(95.0, 100.0, 30).unwrap();
        let reannualized = period * 365.0 / 30.0;
        assert!((reannualized - annualized).abs() < 0.1);
    }

    #[test]
    fn interest_rate_zero_box_price_yields_none() {
        assert_eq!(interest_rate(0.0, 100.0), None);
    }
}
