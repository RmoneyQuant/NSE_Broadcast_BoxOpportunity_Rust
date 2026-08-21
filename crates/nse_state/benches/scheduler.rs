//! Phase 9 hardening: benchmarks `PricingSession::on_quote` and
//! `drain_dirty` against the C++ HLD's §02 latency budget table (the Rust
//! roadmap inherits this budget by reference rather than restating it):
//!
//!   decoded struct -> state store apply:   < 5 µs
//!   dependency lookup + dirty mark:        < 5 µs   (on_quote does both -- combined budget < 10 µs)
//!   pricing + tax, per dirty pair:         < 10 µs  (drain_dirty, per pair)
//!
//! Run with `cargo bench -p nse_state`. `tests/latency_budget.rs` is the
//! CI-gating counterpart (plain `#[test]`, not criterion) -- see that
//! file's docs for why both exist.

use criterion::{Criterion, criterion_group, criterion_main};
use nse_adapt::{QuoteUpdate, Source};
use nse_refdata::InstrumentMaster;
use nse_state::{PricingSession, StrikePairDependencyIndex};
use std::fs;
use std::hint::black_box;
use std::io::Write;
use std::sync::Arc;

fn worked_example_index() -> StrikePairDependencyIndex {
    let path = std::env::temp_dir().join("nse_state_bench_fixture.csv");
    let mut file = fs::File::create(&path).unwrap();
    for row in [
        "C,OPTIDX,1,OPTIDX,NIFTY,1900000000,2390000,CE",
        "C,OPTIDX,2,OPTIDX,NIFTY,1900000000,2390000,PE",
        "C,OPTIDX,3,OPTIDX,NIFTY,1900000000,2400000,CE",
        "C,OPTIDX,4,OPTIDX,NIFTY,1900000000,2400000,PE",
        "C,OPTIDX,5,OPTIDX,NIFTY,1900000000,2410000,CE",
        "C,OPTIDX,6,OPTIDX,NIFTY,1900000000,2410000,PE",
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

fn bench_on_quote(c: &mut Criterion) {
    let index = Arc::new(worked_example_index());
    let mut session = PricingSession::with_real_pricing(index);

    let mut i: u64 = 0;
    c.bench_function("on_quote", |b| {
        b.iter(|| {
            i += 1;
            session.on_quote(black_box(quote(3, 4000, 4200, i)));
        });
    });
}

fn bench_drain_dirty(c: &mut Criterion) {
    let index = Arc::new(worked_example_index());
    let mut session = PricingSession::with_real_pricing(index);
    session.on_quote(quote(1, 12000, 12500, 1_000));
    session.on_quote(quote(2, 3000, 3200, 2_000));
    session.on_quote(quote(3, 4000, 4200, 3_000));
    session.on_quote(quote(4, 6000, 6200, 4_000));

    let mut out = Vec::new();
    let mut i: u64 = 4_000;
    c.bench_function("drain_dirty (1 dirty, complete pair)", |b| {
        b.iter(|| {
            i += 1;
            session.on_quote(quote(3, 4000, 4200, i)); // re-dirty the same pair each iteration
            out.clear();
            session.drain_dirty(black_box(&mut out));
        });
    });
}

criterion_group!(benches, bench_on_quote, bench_drain_dirty);
criterion_main!(benches);
