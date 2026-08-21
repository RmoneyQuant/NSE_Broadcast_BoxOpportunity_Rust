//! Alert sink -- Phase 8c of the roadmap. Threshold-crossing notification:
//! fires once when a pair/side's annualized rate crosses *into*
//! alert-worthy territory (edge-triggered), and again when it later drops
//! back out -- `StructuredLogSink`/`DashboardWsSink` already report every
//! tick a pair is qualifying (level-triggered), so this exists specifically
//! for "an opportunity just became worth a human looking at, right now,"
//! not a duplicate of either of those.
//!
//! Unlike P8a/P8b (TBT/Snapshot decoders, both blocked on an external wire
//! spec that hasn't been supplied), this needed nothing but P6's qualifying
//! records, which already exist -- so it's the one P8 item actually built
//! here. `AlertSink` only decides *when* to fire and logs to a JSONL file;
//! it doesn't send anything anywhere (email/Slack/webhook) -- that's a
//! `check()`/`on_drop()` caller's job, keeping this testable without a real
//! notification channel.

use nse_pricing::{BoxPairId, DropEvent, OpportunityRecord, QualifyingSide};
use serde::Serialize;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlertThresholds {
    /// Minimum annualized long-box rate (percent) to alert on. `None`
    /// disables long-side alerts entirely.
    pub min_long_rate: Option<f64>,
    pub min_short_rate: Option<f64>,
}

impl AlertThresholds {
    pub fn disabled() -> Self {
        Self {
            min_long_rate: None,
            min_short_rate: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AlertEvent {
    /// A pair/side just crossed above its threshold.
    Fired {
        expiry: i64,
        strike_1: i64,
        strike_2: i64,
        side: QualifyingSide,
        rate: f64,
        timestamp_ns: u64,
    },
    /// A previously-fired pair/side dropped back below threshold, or
    /// stopped qualifying at all (`on_drop`).
    Cleared {
        expiry: i64,
        strike_1: i64,
        strike_2: i64,
        side: QualifyingSide,
        timestamp_ns: u64,
    },
}

pub struct AlertSink {
    thresholds: AlertThresholds,
    /// Pairs/sides currently above threshold -- the whole point of tracking
    /// this is turning a level (`rate >= threshold`, true every tick) into
    /// an edge (`Fired` once, `Cleared` once), so a caller wiring this to a
    /// real notification channel doesn't get paged every tick a hot
    /// opportunity is merely still open.
    active: HashSet<(BoxPairId, QualifyingSide)>,
    writer: Option<BufWriter<File>>,
}

impl AlertSink {
    pub fn new(thresholds: AlertThresholds) -> Self {
        Self {
            thresholds,
            active: HashSet::new(),
            writer: None,
        }
    }

    /// Also appends every alert to `path` as JSON lines, in addition to
    /// returning them from `check`/`on_drop` for the caller to act on
    /// (print, notify, etc).
    pub fn with_log(thresholds: AlertThresholds, path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            thresholds,
            active: HashSet::new(),
            writer: Some(BufWriter::new(file)),
        })
    }

    /// Call once per `SchedulerEvent::Opportunity`. Returns a `Fired`/`Cleared`
    /// event for each side that just crossed the threshold on this call --
    /// empty if nothing changed (still below, or already alerting and still above).
    pub fn check(
        &mut self,
        record: &OpportunityRecord,
        long_qualifies: bool,
        short_qualifies: bool,
    ) -> Vec<AlertEvent> {
        let pid = BoxPairId::new(record.expiry, record.strike_1, record.strike_2);
        let mut out = Vec::new();

        self.check_side(
            pid,
            QualifyingSide::Long,
            long_qualifies,
            record.long_interest_rate,
            self.thresholds.min_long_rate,
            record.timestamp_ns,
            &mut out,
        );
        self.check_side(
            pid,
            QualifyingSide::Short,
            short_qualifies,
            record.short_interest_rate,
            self.thresholds.min_short_rate,
            record.timestamp_ns,
            &mut out,
        );

        for event in &out {
            self.log(event);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn check_side(
        &mut self,
        pid: BoxPairId,
        side: QualifyingSide,
        qualifies: bool,
        rate: Option<f64>,
        min_rate: Option<f64>,
        timestamp_ns: u64,
        out: &mut Vec<AlertEvent>,
    ) {
        let Some(rate) = rate else { return };
        let above = qualifies && min_rate.is_some_and(|min| rate >= min);
        let key = (pid, side);
        let was_active = self.active.contains(&key);

        if above && !was_active {
            self.active.insert(key);
            out.push(AlertEvent::Fired {
                expiry: pid.expiry_id,
                strike_1: pid.k1,
                strike_2: pid.k2,
                side,
                rate,
                timestamp_ns,
            });
        } else if !above && was_active {
            self.active.remove(&key);
            out.push(AlertEvent::Cleared {
                expiry: pid.expiry_id,
                strike_1: pid.k1,
                strike_2: pid.k2,
                side,
                timestamp_ns,
            });
        }
    }

    /// Call once per `SchedulerEvent::Dropped`. A `DropEvent` means the
    /// scheduler itself stopped tracking the pair/side as qualifying at
    /// all -- a stronger signal than "rate dipped below the alert
    /// threshold" -- so this always clears an active alert if there was
    /// one, regardless of what the last known rate was.
    pub fn on_drop(&mut self, drop: DropEvent) -> Option<AlertEvent> {
        let pid = BoxPairId::new(drop.expiry, drop.strike_1, drop.strike_2);
        if !self.active.remove(&(pid, drop.side)) {
            return None;
        }
        let event = AlertEvent::Cleared {
            expiry: drop.expiry,
            strike_1: drop.strike_1,
            strike_2: drop.strike_2,
            side: drop.side,
            timestamp_ns: drop.timestamp_ns,
        };
        self.log(&event);
        Some(event)
    }

    fn log(&mut self, event: &AlertEvent) {
        if let Some(writer) = &mut self.writer
            && let Ok(json) = serde_json::to_string(event)
        {
            let _ = writeln!(writer, "{json}");
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        match &mut self.writer {
            Some(writer) => writer.flush(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nse_adapt::Source;
    use nse_pricing::LegPrices;

    fn record(long_rate: Option<f64>, short_rate: Option<f64>) -> OpportunityRecord {
        OpportunityRecord {
            expiry: 1_450_000_000,
            strike_1: 2_390_000,
            strike_2: 2_400_000,
            long_spread: Some(40.0),
            short_spread: Some(36.0),
            long_spread_tax: 10.0,
            short_spread_tax: 9.0,
            long_interest_rate: long_rate,
            short_interest_rate: short_rate,
            long_period_rate: long_rate,
            short_period_rate: short_rate,
            long_legs: Some(LegPrices { k1_bid: 10.0, k1_ask: 10.5, k2_bid: 5.0, k2_ask: 5.5 }),
            short_legs: Some(LegPrices { k1_bid: 9.5, k1_ask: 10.0, k2_bid: 4.5, k2_ask: 5.0 }),
            days_to_expiry: 30,
            timestamp_ns: 1_000,
            source: Source::Broadcast,
            complete: true,
        }
    }

    #[test]
    fn crossing_above_threshold_fires_once() {
        let mut sink = AlertSink::new(AlertThresholds {
            min_long_rate: Some(100.0),
            min_short_rate: None,
        });

        let fired = sink.check(&record(Some(50.0), None), true, false);
        assert!(fired.is_empty(), "below threshold -- must not fire");

        let fired = sink.check(&record(Some(150.0), None), true, false);
        assert_eq!(fired.len(), 1);
        assert!(
            matches!(fired[0], AlertEvent::Fired { side: QualifyingSide::Long, rate, .. } if rate == 150.0)
        );

        // still above threshold on the next tick -- must NOT fire again (edge, not level).
        let fired = sink.check(&record(Some(160.0), None), true, false);
        assert!(
            fired.is_empty(),
            "still above threshold -- must not re-fire"
        );
    }

    #[test]
    fn dropping_back_below_threshold_clears() {
        let mut sink = AlertSink::new(AlertThresholds {
            min_long_rate: Some(100.0),
            min_short_rate: None,
        });
        sink.check(&record(Some(150.0), None), true, false);

        let cleared = sink.check(&record(Some(50.0), None), true, false);
        assert_eq!(cleared.len(), 1);
        assert!(matches!(
            cleared[0],
            AlertEvent::Cleared {
                side: QualifyingSide::Long,
                ..
            }
        ));

        // crossing back above should fire again -- proves the edge really reset.
        let fired = sink.check(&record(Some(150.0), None), true, false);
        assert_eq!(fired.len(), 1);
        assert!(matches!(fired[0], AlertEvent::Fired { .. }));
    }

    #[test]
    fn on_drop_clears_an_active_alert() {
        let mut sink = AlertSink::new(AlertThresholds {
            min_long_rate: Some(100.0),
            min_short_rate: None,
        });
        sink.check(&record(Some(150.0), None), true, false);

        let cleared = sink.on_drop(DropEvent {
            expiry: 1_450_000_000,
            strike_1: 2_390_000,
            strike_2: 2_400_000,
            side: QualifyingSide::Long,
            timestamp_ns: 5_000,
        });
        assert!(matches!(cleared, Some(AlertEvent::Cleared { .. })));
    }

    #[test]
    fn on_drop_for_a_pair_never_alerting_is_a_no_op() {
        let mut sink = AlertSink::new(AlertThresholds::disabled());
        let cleared = sink.on_drop(DropEvent {
            expiry: 1,
            strike_1: 1,
            strike_2: 2,
            side: QualifyingSide::Long,
            timestamp_ns: 0,
        });
        assert!(cleared.is_none());
    }

    #[test]
    fn disabled_threshold_never_fires() {
        let mut sink = AlertSink::new(AlertThresholds::disabled());
        let fired = sink.check(&record(Some(10_000.0), Some(10_000.0)), true, true);
        assert!(fired.is_empty());
    }

    #[test]
    fn long_and_short_are_independent() {
        let mut sink = AlertSink::new(AlertThresholds {
            min_long_rate: Some(100.0),
            min_short_rate: Some(200.0),
        });

        // only long crosses.
        let fired = sink.check(&record(Some(150.0), Some(50.0)), true, true);
        assert_eq!(fired.len(), 1);
        assert!(matches!(
            fired[0],
            AlertEvent::Fired {
                side: QualifyingSide::Long,
                ..
            }
        ));

        // now only short crosses -- long stays active (no duplicate/clear for it).
        let fired = sink.check(&record(Some(150.0), Some(250.0)), true, true);
        assert_eq!(fired.len(), 1);
        assert!(matches!(
            fired[0],
            AlertEvent::Fired {
                side: QualifyingSide::Short,
                ..
            }
        ));
    }

    #[test]
    fn writes_fired_and_cleared_events_to_the_log_file() {
        let path = std::env::temp_dir().join("nse_alert_test.jsonl");
        std::fs::remove_file(&path).ok();

        let mut sink = AlertSink::with_log(
            AlertThresholds {
                min_long_rate: Some(100.0),
                min_short_rate: None,
            },
            &path,
        )
        .unwrap();
        sink.check(&record(Some(150.0), None), true, false);
        sink.check(&record(Some(50.0), None), true, false);
        sink.flush().unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"kind\":\"fired\""));
        assert!(lines[1].contains("\"kind\":\"cleared\""));
    }
}
