//! Phase 9 hardening: benchmarks `PacketDecoder::decode_packet` against the
//! C++ HLD's §02 latency budget table (the Rust roadmap inherits this
//! budget by reference rather than restating it -- see that document's
//! §02 "Process & threading view" for the original):
//!
//!   socket read -> decoded struct: < 50 µs
//!
//! Run with `cargo bench -p nse_decode`. This is the human-facing profiling
//! tool; `tests/latency_budget.rs` (plain `#[test]`, not criterion) is what
//! actually gates CI, since criterion's own regression detection needs a
//! saved baseline this repo's CI doesn't currently persist across runs.

use criterion::{Criterion, criterion_group, criterion_main};
use nse_decode::PacketDecoder;
use std::hint::black_box;

const FIXTURES: &str = include_str!("../../../fixtures/sample_packets.hex");

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn bench_decode_packet(c: &mut Criterion) {
    let decoder = PacketDecoder::new();
    let lines: Vec<Vec<u8>> = FIXTURES
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(hex_to_bytes)
        .collect();

    // Line 6 (index 5) is one of the four synthetic 7208 MBP packets -- the
    // real per-message field-parsing path, not one of the five real-world
    // fixture lines that decode to zero messages (see nse_decode's own
    // tests for why those exist as a *separate*, ground-truth regression
    // check, not a representative benchmark input).
    let synthetic_mbp = &lines[5];

    c.bench_function("decode_packet (synthetic 7208 MBP)", |b| {
        b.iter(|| decoder.decode_packet(black_box(synthetic_mbp)));
    });

    c.bench_function("decode_packet (real-world empty packet)", |b| {
        b.iter(|| decoder.decode_packet(black_box(&lines[0])));
    });
}

criterion_group!(benches, bench_decode_packet);
criterion_main!(benches);
