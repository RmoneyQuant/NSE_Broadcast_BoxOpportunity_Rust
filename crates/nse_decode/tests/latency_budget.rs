//! Phase 9 hardening: a plain `#[test]` latency gate -- unlike
//! `benches/decode_packet.rs`'s criterion benchmark, this runs under
//! `cargo test` and so already executes on every CI run via the existing
//! `cargo test --workspace` step, with no extra baseline-persistence
//! infrastructure needed. Criterion's own regression detection compares
//! against a saved local baseline, which this repo's (ephemeral) CI
//! doesn't currently keep between runs -- this test is the actual ongoing
//! gate; the benchmark is the human-facing profiling tool.
//!
//! Budget, from the C++ HLD's §02 latency table: "socket read -> decoded
//! struct" < 50 µs. The CI threshold below is 10x that (500 µs) -- shared
//! CI hardware is noisier than the dedicated machine the budget was
//! written for, and the goal is catching a real regression (an
//! accidentally reintroduced subprocess spawn, an O(n^2) parse loop), not
//! chasing single-digit-percent noise.

use nse_decode::PacketDecoder;
use std::time::{Duration, Instant};

const ITERATIONS: u32 = 2_000;
const WARMUP: u32 = 200;
const CI_THRESHOLD: Duration = Duration::from_micros(500); // 10x the <50µs design budget

const FIXTURES: &str = include_str!("../../../fixtures/sample_packets.hex");

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn decode_packet_stays_within_the_latency_budget() {
    let decoder = PacketDecoder::new();
    let lines: Vec<Vec<u8>> = FIXTURES
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(hex_to_bytes)
        .collect();
    let packet = &lines[5]; // synthetic 7208 MBP -- exercises real per-message field parsing

    for _ in 0..WARMUP {
        std::hint::black_box(decoder.decode_packet(packet));
    }

    let start = Instant::now();
    for _ in 0..ITERATIONS {
        std::hint::black_box(decoder.decode_packet(packet));
    }
    let average = start.elapsed() / ITERATIONS;

    assert!(
        average < CI_THRESHOLD,
        "decode_packet averaged {average:?} per call over {ITERATIONS} iterations -- over the \
         {CI_THRESHOLD:?} CI gate. Check for an accidental hot-path regression (e.g. a new \
         allocation or syscall)."
    );
}
