//! Reference data -- the contract/instrument universe. Ports `CONTRACTS`,
//! `load_contract_csv`, `contract_name`, and the `ALLOWED_SYMBOLS`/
//! `ALLOWED_INSTRUMENTS` filtering from `nse_fo.py` into an owned struct
//! instead of module-level globals -- Phase 3 ("Refdata & session") of the
//! Box Spread Pipeline roadmap.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// NSE epoch timestamps (contract expiry) are seconds since 1980-01-01, not
/// the standard Unix epoch (1970-01-01) -- add this before treating one as a
/// normal Unix timestamp.
const NSE_EPOCH_OFFSET: i64 = 315_532_800;

#[derive(Clone, Debug, PartialEq)]
pub struct ContractInfo {
    pub token: i32,
    pub instrument_type: String,
    pub symbol: String,
    pub expiry_epoch: i64,
    pub expiry_date: String,
    /// Numeric strike (rupees), for display and math. `None` for instruments
    /// with no strike (e.g. futures).
    pub strike: Option<f64>,
    /// The same strike as an integer, scaled by 100 (paise) exactly as it
    /// appears on the wire -- unlike `strike`, this is `Hash`/`Eq`, so it's
    /// what `BoxPairId` keys pairing on. Zero when `strike` is `None`.
    pub strike_raw: i64,
    pub strike_display: String,
    pub option_type: String,
}

/// Owns the full contract universe loaded from NSE's contract-stream CSV,
/// plus the derived allowed-token set. Constructed once per session; there
/// is no mutable global equivalent to `nse_fo.py`'s `CONTRACTS`/`ALLOWED_TOKENS`.
pub struct InstrumentMaster {
    contracts: HashMap<i32, ContractInfo>,
    allowed_tokens: HashSet<i32>,
}

impl InstrumentMaster {
    /// Parses NSE's raw (headerless) contract-stream CSV, one row per contract:
    ///   `C,<series>,<token>,<instrument_type>,<symbol>,<expiry_epoch>,<strike*100>,<option_type>`
    /// then restricts `allowed_tokens()` to rows matching both `allowed_symbols`
    /// and `allowed_instruments` -- the same two-filter behavior as
    /// `nse_fo.py`'s `ALLOWED_SYMBOLS`/`ALLOWED_INSTRUMENTS`. An empty
    /// `allowed_symbols` means "no symbol filter", matching the Python's
    /// `not ALLOWED_SYMBOLS` check. A missing or unreadable file yields an
    /// empty `InstrumentMaster` rather than an error, matching the Python's
    /// `FileNotFoundError` handling (log and continue with zero contracts).
    pub fn load(path: &Path, allowed_symbols: &[&str], allowed_instruments: &[&str]) -> Self {
        let mut contracts = HashMap::new();

        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                let row: Vec<&str> = line.split(',').collect();
                if row.len() < 8 || row[0] != "C" {
                    continue;
                }

                let (Ok(token), Ok(expiry_epoch), Ok(strike_raw)) = (
                    row[2].parse::<i32>(),
                    row[5].parse::<i64>(),
                    row[6].parse::<i64>(),
                ) else {
                    continue;
                };

                let expiry_date = if expiry_epoch != 0 {
                    chrono::DateTime::from_timestamp(expiry_epoch + NSE_EPOCH_OFFSET, 0)
                        .map(|dt| dt.format("%d-%b-%Y").to_string())
                        .unwrap_or_default()
                } else {
                    String::new()
                };

                let (strike, strike_display) = if strike_raw != 0 {
                    let value = strike_raw as f64 / 100.0;
                    (Some(value), format!("{value:.2}"))
                } else {
                    (None, String::new())
                };

                contracts.insert(
                    token,
                    ContractInfo {
                        token,
                        instrument_type: row[3].to_string(),
                        symbol: row[4].to_string(),
                        expiry_epoch,
                        expiry_date,
                        strike,
                        strike_raw,
                        strike_display,
                        option_type: row[7].to_string(),
                    },
                );
            }
        }

        let allowed_tokens = contracts
            .iter()
            .filter(|(_, info)| {
                (allowed_symbols.is_empty() || allowed_symbols.contains(&info.symbol.as_str()))
                    && allowed_instruments.contains(&info.instrument_type.as_str())
            })
            .map(|(token, _)| *token)
            .collect();

        Self {
            contracts,
            allowed_tokens,
        }
    }

    pub fn get(&self, token: i32) -> Option<&ContractInfo> {
        self.contracts.get(&token)
    }

    pub fn is_allowed(&self, token: i32) -> bool {
        self.allowed_tokens.contains(&token)
    }

    pub fn allowed_tokens(&self) -> &HashSet<i32> {
        &self.allowed_tokens
    }

    /// Every loaded contract, regardless of the load-time symbol/instrument
    /// filter -- `contracts` itself is never filtered (only
    /// `allowed_tokens` is), so this reaches things the constructor's
    /// `allowed_instruments` excluded, e.g. finding a NIFTY future to use
    /// as a spot-price reference when `allowed_instruments` was `["OPTIDX"]`.
    pub fn all_contracts(&self) -> impl Iterator<Item = &ContractInfo> {
        self.contracts.values()
    }

    /// Narrows `allowed_tokens()` down to the intersection with `keep` --
    /// for a caller that wants a specific handful of contracts (e.g. a
    /// small set of strikes to manually verify against the exchange
    /// screen), not a general symbol/instrument-type filter. `contracts`
    /// itself is untouched, so `get()` still resolves anything by token;
    /// only `is_allowed()`/`allowed_tokens()` shrink.
    pub fn restrict_tokens(&mut self, keep: &HashSet<i32>) {
        self.allowed_tokens.retain(|t| keep.contains(t));
    }

    pub fn len(&self) -> usize {
        self.contracts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.contracts.is_empty()
    }

    /// Human-readable contract label, e.g. `"NIFTY OPTIDX 29-Sep-2026 29150.00 CE"`.
    /// Mirrors `contract_name()` in `nse_fo.py` field for field.
    pub fn contract_name(&self, token: i32) -> String {
        let Some(info) = self.contracts.get(&token) else {
            return "UNKNOWN CONTRACT".to_string();
        };

        let mut parts = vec![
            info.symbol.clone(),
            info.instrument_type.clone(),
            info.expiry_date.clone(),
        ];

        if !info.strike_display.is_empty() {
            parts.push(info.strike_display.clone());
        }
        if !info.option_type.is_empty() && info.option_type != "XX" {
            parts.push(info.option_type.clone());
        }

        parts
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Writes `rows` (already comma-joined) to a fresh temp file and returns
    /// its path -- avoids a `tempfile` dependency for what's otherwise a
    /// zero-dependency test.
    fn write_csv(rows: &[&str]) -> std::path::PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("nse_refdata_test_{id}.csv"));
        let mut file = fs::File::create(&path).unwrap();
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
        path
    }

    #[test]
    fn loads_and_filters_by_symbol_and_instrument() {
        let path = write_csv(&[
            "1700000000,3,", // summary row, skipped (doesn't start with "C")
            "C,OPTIDX,101,OPTIDX,NIFTY,1450000000,2915000,CE",
            "C,OPTIDX,102,OPTIDX,NIFTY,1450000000,2915000,PE",
            "C,OPTIDX,103,OPTIDX,BANKNIFTY,1450000000,4500000,CE", // wrong symbol
            "C,FUTIDX,104,FUTIDX,NIFTY,1450000000,0,XX",           // wrong instrument, no strike
        ]);

        let master = InstrumentMaster::load(&path, &["NIFTY"], &["OPTIDX"]);
        fs::remove_file(&path).ok();

        assert_eq!(master.len(), 4);
        assert_eq!(master.allowed_tokens().len(), 2);
        // all_contracts() sees the FUTIDX/BANKNIFTY rows too -- allowed_instruments
        // only narrows allowed_tokens(), never the underlying contract set.
        assert_eq!(master.all_contracts().count(), 4);
        assert!(master.all_contracts().any(|c| c.instrument_type == "FUTIDX"));
        assert!(master.is_allowed(101));
        assert!(master.is_allowed(102));
        assert!(!master.is_allowed(103), "wrong symbol must be filtered out");
        assert!(
            !master.is_allowed(104),
            "wrong instrument type must be filtered out"
        );

        let info = master.get(101).unwrap();
        assert_eq!(info.symbol, "NIFTY");
        assert_eq!(info.strike, Some(29150.0));
        assert_eq!(info.option_type, "CE");
    }

    #[test]
    fn restrict_tokens_intersects_and_leaves_contracts_untouched() {
        let path = write_csv(&[
            "C,OPTIDX,101,OPTIDX,NIFTY,1450000000,2915000,CE",
            "C,OPTIDX,102,OPTIDX,NIFTY,1450000000,2915000,PE",
            "C,OPTIDX,103,OPTIDX,NIFTY,1450000000,2920000,CE",
        ]);
        let mut master = InstrumentMaster::load(&path, &["NIFTY"], &["OPTIDX"]);
        fs::remove_file(&path).ok();

        master.restrict_tokens(&[101].into_iter().collect());

        assert_eq!(master.allowed_tokens().len(), 1);
        assert!(master.is_allowed(101));
        assert!(!master.is_allowed(102));
        assert!(!master.is_allowed(103));
        // contracts() lookup is untouched -- only the allowed set shrinks.
        assert!(master.get(102).is_some());
        assert_eq!(master.len(), 3);
    }

    #[test]
    fn contract_name_matches_python_format() {
        let path = write_csv(&["C,OPTIDX,101,OPTIDX,NIFTY,1450000000,2915000,CE"]);
        let master = InstrumentMaster::load(&path, &[], &["OPTIDX"]);
        fs::remove_file(&path).ok();

        assert_eq!(
            master.contract_name(101),
            "NIFTY OPTIDX 12-Dec-2025 29150.00 CE"
        );
        assert_eq!(master.contract_name(999), "UNKNOWN CONTRACT");
    }

    #[test]
    fn empty_symbol_filter_means_no_symbol_filter() {
        let path = write_csv(&["C,OPTIDX,101,OPTIDX,BANKNIFTY,1450000000,4500000,PE"]);
        let master = InstrumentMaster::load(&path, &[], &["OPTIDX"]);
        fs::remove_file(&path).ok();

        assert!(master.is_allowed(101));
    }

    #[test]
    fn missing_file_yields_empty_master_not_a_panic() {
        let master =
            InstrumentMaster::load(Path::new("does/not/exist.csv"), &["NIFTY"], &["OPTIDX"]);
        assert_eq!(master.len(), 0);
        assert!(master.is_empty());
        assert!(master.allowed_tokens().is_empty());
    }
}
