//! Phase 9 hardening: plain `#[test]` latency gates. See
//! `nse_decode/tests/latency_budget.rs`'s docs for why these exist
//! alongside `benches/scheduler.rs`'s criterion benchmarks rather than
//! instead of them (criterion needs a persisted baseline this repo's CI
//! doesn't keep; a plain assertion already runs on every `cargo test`).
//!
//! Budgets, from the C++ HLD's §02 latency table:
//!   on_quote (state apply + dependency lookup/dirty mark, combined): < 10 µs
//!   drain_dirty, per dirty pair (pricing + tax):                     < 10 µs
//! CI thresholds below are 10x those design budgets -- see nse_decode's
//! test for the noise-margin reasoning.

use nse_adapt::{QuoteUpdate, Source};
use nse_refdata::InstrumentMaster;
use nse_state::{PricingSession, StrikePairDependencyIndex};
use std::fs;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

const ITERATIONS: u32 = 2_000;
const WARMUP: u32 = 200;
const CI_THRESHOLD: Duration = Duration::from_micros(100); // 10x the <10µs design budgets

fn worked_example_index() -> StrikePairDependencyIndex {
    let path = std::env::temp_dir().join("nse_state_latency_test_fixture.csv");
    let mut file = fs::File::create(&path).unwrap();
    for row in [
        "C,OPTIDX,1,OPTIDX,NIFTY,1900000000,2390000,CE",
        "C,OPTIDX,2,OPTIDX,NIFTY,1900000000,2390000,PE",
        "C,OPTIDX,3,OPTIDX,NIFTY,1900000000,2400000,CE",
        "C,OPTIDX,4,OPTIDX,NIFTY,1900000000,2400000,PE",
    ] {
        writeln!(file, "{row}").unwrap();
    }
    let instruments = InstrumentMaster::load(&path, &["NIFTY"], &["OPTIDX"]);
    fs::remove_file(&path).ok();
    StrikePairDependencyIndex::build(&instruments)
}

fn quote(token: i32, bid: i64, ask: i64, recv_time_ns: u64) -> QuoteUpdate {
    let mut q = QuoteUpdate::from_enhanced_mbp(token, &[], recv_time_ns);
    q.bid_price[0] = bid;
    q.ask_price[0] = ask;
    q.source = Source::Broadcast;
    q
}

#[test]
fn on_quote_stays_within_the_latency_budget() {
    let index = Arc::new(worked_example_index());
    let mut session = PricingSession::with_real_pricing(index);

    for i in 0..WARMUP {
        session.on_quote(quote(3, 4000, 4200, i as u64));
    }

    let start = Instant::now();
    for i in 0..ITERATIONS {
        session.on_quote(quote(3, 4000, 4200, i as u64));
    }
    let average = start.elapsed() / ITERATIONS;

    assert!(
        average < CI_THRESHOLD,
        "on_quote averaged {average:?} per call over {ITERATIONS} iterations -- over the \
         {CI_THRESHOLD:?} CI gate."
    );
}

#[test]
fn drain_dirty_stays_within_the_per_pair_latency_budget() {
    let index = Arc::new(worked_example_index());
    let mut session = PricingSession::with_real_pricing(index);
    session.on_quote(quote(1, 12000, 12500, 1_000));
    session.on_quote(quote(2, 3000, 3200, 2_000));
    session.on_quote(quote(3, 4000, 4200, 3_000));
    session.on_quote(quote(4, 6000, 6200, 4_000));

    let mut out = Vec::new();
    for i in 0..WARMUP {
        session.on_quote(quote(3, 4000, 4200, i as u64));
        out.clear();
        session.drain_dirty(&mut out);
    }

    let start = Instant::now();
    for i in 0..ITERATIONS {
        session.on_quote(quote(3, 4000, 4200, i as u64)); // re-dirty exactly 1 pair each iteration
        out.clear();
        session.drain_dirty(&mut out);
    }
    let average = start.elapsed() / ITERATIONS;

    assert!(
        average < CI_THRESHOLD,
        "drain_dirty (1 pair) averaged {average:?} per call over {ITERATIONS} iterations -- \
         over the {CI_THRESHOLD:?} CI gate."
    );
}
