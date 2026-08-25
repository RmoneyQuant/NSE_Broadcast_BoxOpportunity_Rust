//! Appends one CSV row per RMTrade send **attempt** -- exactly the data
//! that goes into the `add_box_spread` request, nothing else. Deliberately
//! *not* here: the table's display-only columns (bid/ask, gross, net,
//! rate, ...) and the response/outcome (ok, strgy_id, error) -- this is a
//! record of what we sent, not what RMTrade did with it or how the row
//! looked on screen. `api_key` is also deliberately excluded -- it's a
//! secret, and this file is plain text on disk.

use crate::rmtrade_gateway::{self, BoxLegs, BoxSpreadParams};
use chrono::Local;
use csv::Writer;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

const HEADER: &[&str] = &[
    "timestamp",
    "action",
    "client_ref",
    "exchange",
    "k1_ce_token",
    "k2_ce_token",
    "k1_pe_token",
    "k2_pe_token",
    "qty",
    "max_buy_lot",
    "max_sell_lot",
    "n_lot",
    "pro",
    "client_code",
    "sell_spread",
    "buy_spread",
    "profit",
    "jump",
    "bid_time",
    "delta",
    "lot_threshold",
];

pub struct OrderLog {
    writer: Writer<File>,
}

impl OrderLog {
    /// Opens (creating if needed) `path` in append mode, like `nse_sink`'s
    /// log sinks -- writes the header row only the first time the file is
    /// created, so re-running the app keeps appending to the same file
    /// instead of duplicating headers.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let needs_header = !path.exists();
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let mut writer = Writer::from_writer(file);
        if needs_header {
            writer.write_record(HEADER)?;
            writer.flush()?;
        }
        Ok(Self { writer })
    }

    /// One row per send attempt -- logged from the exact same `legs`/
    /// `client_ref`/`params` that `send_add_box_spread` turns into the
    /// request, independent of whether that send actually succeeds (a
    /// dropped connection doesn't change what we *attempted* to send).
    /// Flushed immediately: a rare, deliberate, real-money action per row,
    /// unlike the pricing pipeline's own batched-and-flushed-at-shutdown logs.
    pub fn log_send(&mut self, legs: &BoxLegs, client_ref: &str, params: &BoxSpreadParams) -> io::Result<()> {
        let fields: Vec<String> = vec![
            Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            "add_box_spread".to_string(),
            client_ref.to_string(),
            rmtrade_gateway::EXCHANGE.to_string(),
            legs.k1_ce.to_string(),
            legs.k2_ce.to_string(),
            legs.k1_pe.to_string(),
            legs.k2_pe.to_string(),
            params.qty.to_string(),
            params.max_buy_lot.to_string(),
            params.max_sell_lot.to_string(),
            params.n_lot.to_string(),
            rmtrade_gateway::PRO_DEFAULT.to_string(),
            rmtrade_gateway::CLIENT_CODE_DEFAULT.to_string(),
            rmtrade_gateway::SELL_SPREAD_DEFAULT.to_string(),
            rmtrade_gateway::BUY_SPREAD_DEFAULT.to_string(),
            params.profit.to_string(),
            params.jump.to_string(),
            params.bid_time.to_string(),
            params.delta.to_string(),
            params.lot_threshold.to_string(),
        ];

        self.writer.write_record(&fields)?;
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_path() -> std::path::PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("order_log_test_{id}.csv"))
    }

    fn sample_params() -> BoxSpreadParams {
        BoxSpreadParams { qty: 1.0, max_buy_lot: 5.0, max_sell_lot: 5.0, n_lot: 5.0, profit: 0.0, jump: 0.0, bid_time: 0, delta: 0.0, lot_threshold: 0 }
    }

    #[test]
    fn writes_header_once_and_appends_rows_across_reopens() {
        let path = temp_path();
        let legs = BoxLegs { k1_ce: 1, k1_pe: 2, k2_ce: 3, k2_pe: 4 };

        {
            let mut log = OrderLog::open(&path).unwrap();
            log.log_send(&legs, "box_scanner-1-23000-26000", &sample_params()).unwrap();
        }
        {
            // Reopening an existing file must not duplicate the header --
            // matches every real run appending to the same log across
            // restarts.
            let mut log = OrderLog::open(&path).unwrap();
            log.log_send(&legs, "box_scanner-4-24000-25000", &sample_params()).unwrap();
        }

        let content = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).ok();

        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3, "expected 1 header + 2 data rows, got:\n{content}");
        assert_eq!(lines[0], HEADER.join(","));
        assert!(lines[1].starts_with(char::is_numeric), "row should start with a timestamp: {}", lines[1]);
        assert!(lines[1].contains("box_scanner-1-23000-26000"));
        assert!(lines[1].contains("EXCHG_NSE_FO"));
        assert!(!lines[1].contains("api_key"), "the secret api key must never be logged");
        assert!(lines[2].contains("box_scanner-4-24000-25000"));
    }
}
