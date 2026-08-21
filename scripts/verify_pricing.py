"""
Cross-checks the Rust port (crates/nse_pricing) against the real
box_pricer.py / interest_calc.py by running both over the same test
vectors in fixtures/pricing_test_vectors.json, printing one JSON line per
vector in a shape meant to be diffed against:

    cargo run -p nse_pricing --example verify_pricing

Run from anywhere; paths are resolved relative to this file, not the cwd.

Usage:
    python scripts/verify_pricing.py
"""

import json
import os
import sys
from datetime import date, timedelta

REPO_ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
sys.path.insert(0, REPO_ROOT)

from box_pricer import calculate_box_sell_price, calculate_box_long_price  # noqa: E402
from interest_calc import calculate_interest_rate  # noqa: E402

FIXTURE = os.path.join(REPO_ROOT, "fixtures", "pricing_test_vectors.json")


def main():
    with open(FIXTURE, encoding="utf-8") as f:
        vectors = json.load(f)

    for v in vectors:
        k1, k2 = v["k1"], v["k2"]
        days = v["days_to_expiry"]

        # calculate_interest_rate reads date.today() internally, so build an
        # expiry string that lands exactly `days` days out *right now* --
        # keeps this deterministic without touching interest_calc.py.
        expiry = (date.today() + timedelta(days=days)).strftime("%d%b%Y").upper()

        quotes = {
            (k1, "CE"): {"bid": v["call_k1"]["bid"], "ask": v["call_k1"]["ask"], "display_name": f"CE{k1}"},
            (k1, "PE"): {"bid": v["put_k1"]["bid"], "ask": v["put_k1"]["ask"], "display_name": f"PE{k1}"},
            (k2, "CE"): {"bid": v["call_k2"]["bid"], "ask": v["call_k2"]["ask"], "display_name": f"CE{k2}"},
            (k2, "PE"): {"bid": v["put_k2"]["bid"], "ask": v["put_k2"]["ask"], "display_name": f"PE{k2}"},
        }

        row = {"name": v["name"], "k1": k1, "k2": k2}

        try:
            short_spread, _ = calculate_box_sell_price(quotes, k1, k2)
            row["short_spread"] = short_spread
            row["short_interest_rate"] = calculate_interest_rate(short_spread, k1, k2, expiry)["interest_calculation"]
        except ValueError:
            row["short_spread"] = None
            row["short_interest_rate"] = None

        try:
            long_spread, _ = calculate_box_long_price(quotes, k1, k2)
            row["long_spread"] = long_spread
            row["long_interest_rate"] = calculate_interest_rate(long_spread, k1, k2, expiry)["interest_calculation"]
        except ValueError:
            row["long_spread"] = None
            row["long_interest_rate"] = None

        print(json.dumps(row))


if __name__ == "__main__":
    main()
