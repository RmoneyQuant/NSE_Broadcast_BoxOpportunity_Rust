//! Output sinks -- Phase 6's `StructuredLogSink` (append-only JSON-lines
//! file: one line per `OpportunityRecord` and one per `DropEvent`, each
//! tagged with a `"kind"` field so a downstream reader can tell them apart
//! without inspecting every field -- candidate home for the OHLC
//! persistence `nse_fo.py` already has disabled) and Phase 8c's
//! `alert::AlertSink` (edge-triggered threshold-crossing notifications).
//! P8a/P8b (TBT/Snapshot decoders) aren't here -- both are blocked on an
//! external wire-format spec that hasn't been supplied, same as P5's tax
//! formulas were until they arrived.
//!
//! The Phase 7 web dashboard (`axum`/WebSocket, browser UI) that used to
//! live here was removed and replaced with `box_scanner_live`'s native
//! Win32 desktop window (`native-windows-gui`) -- per-side pricing view
//! types (`WireOpportunityRecord` et al, in `wire.rs`) were kept and reused
//! there, since the shape a GUI table row needs is the same shape a WS wire
//! record needed, even though nothing about them is web-specific.

pub mod alert;
mod wire;

pub use alert::{AlertEvent, AlertSink, AlertThresholds};
pub use wire::{Side, WireOpportunityRecord, WirePairId};

use nse_pricing::{DropEvent, OpportunityRecord};
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

pub struct StructuredLogSink {
    writer: BufWriter<File>,
}

impl StructuredLogSink {
    /// Opens `path` for appending, creating it if it doesn't exist -- safe
    /// to point multiple runs (e.g. across restarts) at the same file.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub fn write_opportunity(&mut self, record: &OpportunityRecord) -> io::Result<()> {
        self.write_tagged("opportunity", record)
    }

    pub fn write_drop(&mut self, drop: &DropEvent) -> io::Result<()> {
        self.write_tagged("drop", drop)
    }

    fn write_tagged<T: serde::Serialize>(&mut self, kind: &str, value: &T) -> io::Result<()> {
        let mut json = serde_json::to_value(value)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let serde_json::Value::Object(map) = &mut json {
            map.insert(
                "kind".to_string(),
                serde_json::Value::String(kind.to_string()),
            );
        }
        writeln!(self.writer, "{json}")
    }

    /// Flushes buffered writes to disk. Not automatic per line -- that
    /// would defeat the point of buffering under load -- call periodically
    /// or on shutdown.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nse_adapt::Source;
    use nse_pricing::{LegPrices, QualifyingSide};
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_path() -> std::path::PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("nse_sink_test_{id}.jsonl"))
    }

    fn sample_record() -> OpportunityRecord {
        OpportunityRecord {
            expiry: 1_450_000_000,
            strike_1: 2_390_000,
            strike_2: 2_400_000,
            long_spread: Some(40.0),
            short_spread: Some(36.0),
            long_spread_tax: 94.5,
            short_spread_tax: 94.6,
            long_interest_rate: Some(1825.0),
            short_interest_rate: Some(1500.0),
            long_period_rate: Some(15.0),
            short_period_rate: Some(12.5),
            long_legs: Some(LegPrices { k1_bid: 10.0, k1_ask: 10.5, k2_bid: 5.0, k2_ask: 5.5 }),
            short_legs: Some(LegPrices { k1_bid: 9.5, k1_ask: 10.0, k2_bid: 4.5, k2_ask: 5.0 }),
            days_to_expiry: 30,
            timestamp_ns: 4_000,
            source: Source::Broadcast,
            complete: true,
        }
    }

    #[test]
    fn writes_one_jsonl_line_per_opportunity_tagged_correctly() {
        let path = temp_path();
        let mut sink = StructuredLogSink::open(&path).unwrap();
        sink.write_opportunity(&sample_record()).unwrap();
        sink.flush().unwrap();

        let text = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).ok();

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);

        let parsed: serde_json::Value =
            serde_json::from_str(lines[0]).expect("line must be valid JSON");
        assert_eq!(parsed["kind"], "opportunity");
        assert_eq!(parsed["strike_1"], 2_390_000);
        assert_eq!(parsed["strike_2"], 2_400_000);
        assert_eq!(parsed["long_spread"], 40.0);
    }

    #[test]
    fn writes_one_jsonl_line_per_drop_tagged_correctly() {
        let path = temp_path();
        let mut sink = StructuredLogSink::open(&path).unwrap();
        let drop = DropEvent {
            expiry: 1_450_000_000,
            strike_1: 2_390_000,
            strike_2: 2_400_000,
            side: QualifyingSide::Long,
            timestamp_ns: 2_000,
        };
        sink.write_drop(&drop).unwrap();
        sink.flush().unwrap();

        let text = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).ok();

        let parsed: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(parsed["kind"], "drop");
        assert_eq!(parsed["side"], "Long");
        assert_eq!(parsed["timestamp_ns"], 2_000);
    }

    #[test]
    fn reopening_the_same_path_appends_instead_of_truncating() {
        let path = temp_path();
        {
            let mut sink = StructuredLogSink::open(&path).unwrap();
            sink.write_opportunity(&sample_record()).unwrap();
            sink.flush().unwrap();
        }
        {
            let mut sink = StructuredLogSink::open(&path).unwrap();
            sink.write_opportunity(&sample_record()).unwrap();
            sink.flush().unwrap();
        }

        let text = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).ok();
        assert_eq!(
            text.lines().count(),
            2,
            "second open() must append, not truncate the first run's line"
        );
    }
}
