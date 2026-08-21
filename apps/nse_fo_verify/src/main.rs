//! Rust port of `nse_fo.py`, kept as a direct line-for-line mirror of it
//! (same section banners, same functions, same env var names) so the two
//! can be read side by side. Purpose: verify `nse_fo_decoder_dyn.exe`
//! exactly the way the Python script does -- hex-encode a raw packet, shell
//! out to the exe, parse its JSON, print a formatted feed -- independent of
//! `nse_decode`'s native port, so the two can be cross-checked against each
//! other.
//!
//! Deliberately NOT ported: the Postgres OHLC-candle persistence layer
//! (`DB_CONFIG` is `None` by default in the Python too, so it's dead code
//! there for the same reason it would be here) and `NSE_FO_DUMP_HEX_FILE`
//! packet logging.
//!
//! Deliberately ADDED beyond the Python: a `replay` mode (default), since
//! there's no live NSE multicast feed reachable from this machine to test
//! against. `live` mode is still implemented for parity.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Write;
use std::net::{Ipv4Addr, UdpSocket};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

// ============================================================
// CONFIG
// ============================================================

const PRICE_DIVISOR: f64 = 100.0;
const DEPTH: usize = 5;

// NSE epoch timestamps (contract expiry) are seconds since 1980-01-01, not
// the standard Unix epoch (1970-01-01) -- add this offset before treating
// them as a normal Unix timestamp.
const NSE_EPOCH_OFFSET: i64 = 315_532_800;

const ALLOWED_SYMBOLS: &[&str] = &["NIFTY"];
const ALLOWED_INSTRUMENTS: &[&str] = &["OPTIDX"];

struct Config {
    mcast_grp: String,
    mcast_port: u16,
    local_if: String,
    decoder_exe: String,
    contract_csv: PathBuf,
    fixture: PathBuf,
}

impl Config {
    fn from_env() -> Self {
        Self {
            mcast_grp: env::var("NSE_FO_MCAST_GRP").unwrap_or_else(|_| "239.60.60.8".to_string()),
            mcast_port: env::var("NSE_FO_MCAST_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10008),
            local_if: env::var("NSE_FO_LOCAL_IF").unwrap_or_else(|_| "192.168.50.210".to_string()),
            decoder_exe: resolve_decoder_exe(
                env::var("NSE_FO_DECODER_EXE")
                    .unwrap_or_else(|_| "nse_fo_decoder_dyn.exe".to_string()),
            ),
            contract_csv: env::var("NSE_FO_CONTRACT_CSV")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("fixtures/fo_contract_stream_info.csv")),
            fixture: env::var("NSE_FO_FIXTURE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("fixtures/sample_packets.hex")),
        }
    }
}

/// `Command::new("bare_name.exe")` on Windows only searches `PATH` -- unlike
/// `cmd.exe`, it does not implicitly check the current directory. Canonicalize
/// against cwd first so a bare filename that exists here still resolves; fall
/// back to the raw string so an actual `PATH`-based name still works.
fn resolve_decoder_exe(raw: String) -> String {
    std::fs::canonicalize(&raw)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(raw)
}

// ============================================================
// FEED STATE
// ============================================================

#[derive(Default)]
struct FeedState {
    token: i64,

    buy_price: [i64; DEPTH],
    buy_qty: [i64; DEPTH],
    buy_orders: [i64; DEPTH],

    sell_price: [i64; DEPTH],
    sell_qty: [i64; DEPTH],
    sell_orders: [i64; DEPTH],

    ltp: i64,
    ltq: i64,
    ltt: i64,

    open_price: i64,
    high_price: i64,
    low_price: i64,
    close_price: i64,

    atp: i64,
    total_traded_qty: i64,

    open_interest: i64,
    day_high_oi: i64,
    day_low_oi: i64,

    tbq: i64,
    tsq: i64,

    last_update_time: String,
}

impl FeedState {
    fn new(token: i64) -> Self {
        Self {
            token,
            ..Default::default()
        }
    }

    fn price(&self, value: i64) -> f64 {
        if value != 0 {
            value as f64 / PRICE_DIVISOR
        } else {
            0.0
        }
    }

    fn clear_book(&mut self) {
        self.buy_price = [0; DEPTH];
        self.buy_qty = [0; DEPTH];
        self.buy_orders = [0; DEPTH];
        self.sell_price = [0; DEPTH];
        self.sell_qty = [0; DEPTH];
        self.sell_orders = [0; DEPTH];
    }

    fn touch(&mut self) {
        self.last_update_time = chrono::Local::now().format("%H:%M:%S%.6f").to_string();
    }

    fn print_feed(&self, contracts: &HashMap<i64, ContractInfo>) {
        println!("{}", "=".repeat(120));
        println!(
            "TOKEN: {} | {} | UPDATE: {}",
            self.token,
            contract_name(contracts, self.token),
            self.last_update_time
        );

        println!(
            "LTP={:.2} LTQ={} O={:.2} H={:.2} L={:.2} C={:.2} ATP={:.2}",
            self.price(self.ltp),
            self.ltq,
            self.price(self.open_price),
            self.price(self.high_price),
            self.price(self.low_price),
            self.price(self.close_price),
            self.price(self.atp),
        );

        println!(
            "TTQ={} OI={} DAY_HI_OI={} DAY_LO_OI={} TBQ={} TSQ={}",
            self.total_traded_qty,
            self.open_interest,
            self.day_high_oi,
            self.day_low_oi,
            self.tbq,
            self.tsq,
        );

        println!("{}", "-".repeat(120));
        println!(
            "{:<8} {:>12} {:>15} {:>10} || {:>15} {:>12} {:>10}",
            "LEVEL", "BID_QTY", "BID_PRICE", "BID_ORD", "ASK_PRICE", "ASK_QTY", "ASK_ORD"
        );
        println!("{}", "-".repeat(120));

        for i in 0..DEPTH {
            println!(
                "{:<8} {:>12} {:>15.2} {:>10} || {:>15.2} {:>12} {:>10}",
                i + 1,
                self.buy_qty[i],
                self.price(self.buy_price[i]),
                self.buy_orders[i],
                self.price(self.sell_price[i]),
                self.sell_qty[i],
                self.sell_orders[i],
            );
        }

        println!();
    }
}

struct AppState {
    contracts: HashMap<i64, ContractInfo>,
    allowed_tokens: HashSet<i64>,
    feeds: HashMap<i64, FeedState>,
}

/// Borrows only `feeds`, not the whole `AppState` -- callers that also need
/// `&state.contracts` afterward (e.g. for `print_feed`) would otherwise hit
/// a borrow-checker conflict against a `&mut self` method.
fn get_feed(feeds: &mut HashMap<i64, FeedState>, token: i64) -> &mut FeedState {
    feeds.entry(token).or_insert_with(|| FeedState::new(token))
}

// ============================================================
// SOCKET
// ============================================================

/// Mirrors `create_multicast_socket()` in nse_fo.py exactly, including the
/// SO_REUSEADDR call -- `std::net::UdpSocket::bind` alone binds and creates
/// in one step with no way to set socket options first, so this needs
/// `socket2` to build the socket, set the option, *then* bind. Without this,
/// bind() fails outright (os error 10048) the moment anything else already
/// holds the port -- e.g. a second instance of this tool, or nse_fo.py
/// itself already running against the same feed.
fn create_multicast_socket(cfg: &Config) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Socket, Type};

    let raw = Socket::new(Domain::IPV4, Type::DGRAM, None)?;
    raw.set_reuse_address(true)?;
    let addr: std::net::SocketAddr = ([0, 0, 0, 0], cfg.mcast_port).into();
    raw.bind(&addr.into())?;
    let socket: UdpSocket = raw.into();

    let grp: Ipv4Addr = cfg.mcast_grp.parse().unwrap_or_else(|_| {
        panic!(
            "NSE_FO_MCAST_GRP is not a valid IPv4 address: {}",
            cfg.mcast_grp
        )
    });
    let iface: Ipv4Addr = cfg.local_if.parse().unwrap_or_else(|_| {
        panic!(
            "NSE_FO_LOCAL_IF is not a valid IPv4 address: {}",
            cfg.local_if
        )
    });

    socket.join_multicast_v4(&grp, &iface)?;
    Ok(socket)
}

// ============================================================
// C++ DECODER BRIDGE
// ============================================================

/// Sends one raw NSE F&O broadcast UDP packet to `nse_fo_decoder_dyn.exe`,
/// exactly as `decode_using_cpp()` does in nse_fo.py: hex-encode, spawn a
/// fresh process, write the hex line to stdin, close it (so the exe's
/// `getline` loop sees EOF after one line and exits on its own), read
/// whatever it printed. Returns `None` on any of the same failure paths
/// as the Python (not found, timeout, non-zero exit, invalid JSON).
fn decode_using_cpp(exe: &str, data: &[u8]) -> Option<Value> {
    let hex_packet = hex_encode(data);

    let output = match run_decoder(exe, &hex_packet, Duration::from_secs(2)) {
        Ok(o) => o,
        Err(RunError::NotFound(e)) => {
            println!("NSE F&O decoder not found: {exe} ({e})");
            return None;
        }
        Err(RunError::Timeout) => {
            println!("NSE F&O decoder timeout");
            return None;
        }
    };

    if !output.status.success() {
        println!("NSE F&O decoder error:");
        println!("COMMAND    : {exe}");
        println!("RETURNCODE : {:?}", output.status.code());
        println!("STDOUT     : {:?}", String::from_utf8_lossy(&output.stdout));
        println!("STDERR     : {:?}", String::from_utf8_lossy(&output.stderr));
        return None;
    }

    if !output.stderr.is_empty() {
        // nse_fo_decoder.cpp writes non-fatal header-mismatch warnings here.
        println!(
            "NSE F&O decoder stderr: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = stdout.trim();
    if text.is_empty() {
        return None;
    }

    match serde_json::from_str(text) {
        Ok(v) => Some(v),
        Err(_) => {
            println!("Invalid JSON from NSE F&O decoder:");
            println!("{text}");
            None
        }
    }
}

struct RunOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

enum RunError {
    NotFound(std::io::Error),
    Timeout,
}

/// A hand-rolled equivalent of Python's `subprocess.run(..., timeout=2)`.
/// `std::process` has no built-in wait timeout, so this polls `try_wait`
/// and kills the child if it overruns. Reads stdout/stderr only after the
/// child exits (or is killed) -- fine here since the decoder's output is a
/// single short JSON line, not something large enough to risk filling the
/// pipe buffer before that point.
fn run_decoder(exe: &str, hex_line: &str, timeout: Duration) -> Result<RunOutput, RunError> {
    let mut child = Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::NotFound)?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(hex_line.as_bytes());
        let _ = stdin.write_all(b"\n");
        // stdin dropped here, closing the pipe.
    }

    let start = Instant::now();
    loop {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let output = child.wait_with_output().map_err(RunError::NotFound)?;
    Ok(RunOutput {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// ============================================================
// BOOK / FEED UPDATE LOGIC
// ============================================================

fn get_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn handle_mbp(decoded: &Value, state: &mut AppState) {
    let Some(token) = decoded.get("token").and_then(Value::as_i64) else {
        return;
    };
    if !state.allowed_tokens.contains(&token) {
        return;
    }

    let feed = get_feed(&mut state.feeds, token);
    feed.clear_book();

    if let Some(entries) = decoded.get("entries").and_then(Value::as_array) {
        for entry in entries {
            let md_type = get_i64(entry, "md_entry_type");
            let level = get_i64(entry, "level") - 1;
            let price = get_i64(entry, "price");
            let qty = get_i64(entry, "qty");
            let orders = get_i64(entry, "orders");

            if level < 0 || level as usize >= DEPTH {
                continue;
            }
            let level = level as usize;

            if md_type == 0 {
                feed.buy_price[level] = price;
                feed.buy_qty[level] = qty;
                feed.buy_orders[level] = orders;
            } else if md_type == 1 {
                feed.sell_price[level] = price;
                feed.sell_qty[level] = qty;
                feed.sell_orders[level] = orders;
            }
        }
    }

    let ltp = get_i64(decoded, "ltp");
    if ltp != 0 {
        feed.ltp = ltp;
        feed.ltq = get_i64(decoded, "ltq");
        feed.ltt = get_i64(decoded, "ltt");
    }

    feed.open_price = get_i64(decoded, "open");
    feed.high_price = get_i64(decoded, "high");
    feed.low_price = get_i64(decoded, "low");
    feed.close_price = get_i64(decoded, "close");
    feed.atp = get_i64(decoded, "atp");
    feed.total_traded_qty = get_i64(decoded, "total_traded_qty");
    feed.tbq = get_i64(decoded, "tbq");
    feed.tsq = get_i64(decoded, "tsq");

    feed.touch();
    feed.print_feed(&state.contracts);
    // (nse_fo.py also calls maybe_insert_ohlc()/maybe_insert_ohlc_1m() here
    // to persist 1s/1m candles to Postgres -- not ported, see module docs.)
}

fn handle_oi_ticker(decoded: &Value, state: &mut AppState) {
    let Some(token) = decoded.get("token").and_then(Value::as_i64) else {
        return;
    };
    if !state.allowed_tokens.contains(&token) {
        return;
    }

    let feed = get_feed(&mut state.feeds, token);
    feed.open_interest = get_i64(decoded, "open_interest");
    feed.day_high_oi = get_i64(decoded, "day_high_oi");
    feed.day_low_oi = get_i64(decoded, "day_low_oi");

    feed.touch();
    feed.print_feed(&state.contracts);
}

fn handle_security_range(decoded: &Value, state: &AppState) {
    let Some(token) = decoded.get("token").and_then(Value::as_i64) else {
        return;
    };
    if !state.allowed_tokens.contains(&token) {
        return;
    }

    println!(
        "[SECURITY_RANGE] token={token} low={} high={}",
        decoded.get("low_price_range").unwrap_or(&Value::Null),
        decoded.get("high_price_range").unwrap_or(&Value::Null),
    );
}

fn handle_exec_range(decoded: &Value, state: &AppState) {
    let Some(entries) = decoded.get("entries").and_then(Value::as_array) else {
        return;
    };

    for entry in entries {
        let Some(token) = entry.get("token").and_then(Value::as_i64) else {
            continue;
        };
        if !state.allowed_tokens.contains(&token) {
            continue;
        }

        println!(
            "[EXEC_RANGE] token={token} low={} high={}",
            entry.get("low_exec_band").unwrap_or(&Value::Null),
            entry.get("high_exec_band").unwrap_or(&Value::Null),
        );
    }
}

fn handle_decoded(decoded: &Value, state: &mut AppState) {
    match decoded.get("msg_type").and_then(Value::as_str) {
        Some("MBP") => handle_mbp(decoded, state),
        Some("OI_TICKER") => handle_oi_ticker(decoded, state),
        Some("SECURITY_RANGE") => handle_security_range(decoded, state),
        Some("EXEC_RANGE") => handle_exec_range(decoded, state),
        Some(other) => {
            if decoded.get("error").is_some() {
                println!(
                    "Decoder error: {} {}",
                    decoded.get("error").unwrap_or(&Value::Null),
                    decoded.get("reason").unwrap_or(&Value::Null),
                );
            } else {
                println!("Unknown msg_type: {other}");
            }
        }
        None => println!("Unknown msg_type: null"),
    }
}

// ============================================================
// RAW DEBUG
// ============================================================

fn print_raw_packet(data: &[u8], addr_display: &str) {
    println!("{}", "=".repeat(100));
    println!(
        "[{}] {} bytes from {addr_display}",
        chrono::Local::now().format("%H:%M:%S%.6f"),
        data.len()
    );
    println!("Binary packet received. Passing to C++ decoder...");

    println!("HEX FULL PACKET:");
    println!("{}", hex_encode(data));

    println!("HEX FIRST 64 BYTES:");
    println!("{}", hex_encode(&data[..data.len().min(64)]));
}

// ============================================================
// CONTRACT LOADER
// ============================================================

#[derive(Clone)]
struct ContractInfo {
    instrument_type: String,
    symbol: String,
    expiry_date: String,
    strike_display: String,
    option_type: String,
}

/// Parses NSE's raw (headerless) contract-stream CSV, one row per contract:
///   C,<series>,<token>,<instrument_type>,<symbol>,<expiry_epoch>,<strike*100>,<option_type>
/// The first line is a summary record and is skipped implicitly (it doesn't
/// start with "C" so the row filter below drops it).
fn load_contract_csv(path: &std::path::Path) -> (HashMap<i64, ContractInfo>, HashSet<i64>) {
    let mut contracts = HashMap::new();

    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => {
            println!("Contract CSV not found: {}", path.display());
            return (contracts, HashSet::new());
        }
    };

    for line in text.lines() {
        let row: Vec<&str> = line.split(',').collect();
        if row.len() < 8 || row[0] != "C" {
            continue;
        }

        let (Ok(token), Ok(expiry_epoch), Ok(strike_raw)) = (
            row[2].parse::<i64>(),
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

        let strike_display = if strike_raw != 0 {
            format!("{:.2}", strike_raw as f64 / 100.0)
        } else {
            String::new()
        };

        contracts.insert(
            token,
            ContractInfo {
                instrument_type: row[3].to_string(),
                symbol: row[4].to_string(),
                expiry_date,
                strike_display,
                option_type: row[7].to_string(),
            },
        );
    }

    println!("Loaded contracts: {}", contracts.len());

    let allowed_tokens: HashSet<i64> = contracts
        .iter()
        .filter(|(_, info)| {
            (ALLOWED_SYMBOLS.is_empty() || ALLOWED_SYMBOLS.contains(&info.symbol.as_str()))
                && ALLOWED_INSTRUMENTS.contains(&info.instrument_type.as_str())
        })
        .map(|(token, _)| *token)
        .collect();

    println!("Allowed tokens total : {}", allowed_tokens.len());

    (contracts, allowed_tokens)
}

fn contract_name(contracts: &HashMap<i64, ContractInfo>, token: i64) -> String {
    let Some(info) = contracts.get(&token) else {
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

// ============================================================
// PACKET SOURCE (addition beyond nse_fo.py -- see module docs)
// ============================================================

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err(format!("odd-length hex string ({} chars)", hex.len()));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

// ============================================================
// MAIN
// ============================================================

fn main() {
    let cfg = Config::from_env();
    let mode = env::args().nth(1).unwrap_or_else(|| "replay".to_string());

    let (contracts, allowed_tokens) = load_contract_csv(&cfg.contract_csv);
    let mut state = AppState {
        contracts,
        allowed_tokens,
        feeds: HashMap::new(),
    };

    println!("DB_CONFIG not set -- running without DB persistence.");
    println!("nse_fo_verify: mode={mode} decoder={}", cfg.decoder_exe);

    match mode.as_str() {
        "live" => {
            let socket = create_multicast_socket(&cfg)
                .unwrap_or_else(|e| panic!("failed to join NSE F&O multicast group: {e}"));
            println!("Listening on {}:{}", cfg.mcast_grp, cfg.mcast_port);
            println!("Local IF: {}", cfg.local_if);
            println!("Waiting for NSE F&O broadcast packets...\n");

            let mut buf = [0u8; 65535];
            loop {
                let (n, addr) = match socket.recv_from(&mut buf) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("Error: {e}");
                        continue;
                    }
                };
                let data = &buf[..n];

                match decode_using_cpp(&cfg.decoder_exe, data) {
                    None => print_raw_packet(data, &addr.to_string()),
                    Some(Value::Array(messages)) => {
                        for decoded in &messages {
                            handle_decoded(decoded, &mut state);
                        }
                    }
                    Some(_) => println!("Decoder returned non-array JSON, ignoring"),
                }
            }
        }
        "replay" => {
            let text = fs::read_to_string(&cfg.fixture).unwrap_or_else(|e| {
                panic!("failed to read fixture {}: {e}", cfg.fixture.display())
            });

            for (i, line) in text.lines().enumerate() {
                replay_one_line(&cfg, line, i, &mut state);
            }

            println!(
                "replay finished -- {} fixture line(s) processed",
                text.lines().count()
            );
        }
        // Addition beyond nse_fo.py: cycles the fixture file forever on a
        // real time interval instead of running once, so there's something
        // to watch print continuously without needing the live feed to
        // actually be flowing right now. Ctrl+C to stop.
        "replay-loop" => {
            let text = fs::read_to_string(&cfg.fixture).unwrap_or_else(|e| {
                panic!("failed to read fixture {}: {e}", cfg.fixture.display())
            });
            let delay = env::var("NSE_FO_REPLAY_DELAY_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(Duration::from_millis)
                .unwrap_or(Duration::from_millis(500));

            println!(
                "replay-loop: cycling fixtures every {delay:?} per packet -- Ctrl+C to stop\n"
            );

            let mut cycle = 1u64;
            loop {
                println!(
                    "-- cycle {cycle} start, {} --",
                    chrono::Local::now().format("%H:%M:%S%.3f")
                );
                for (i, line) in text.lines().enumerate() {
                    replay_one_line(&cfg, line, i, &mut state);
                    std::thread::sleep(delay);
                }
                cycle += 1;
            }
        }
        other => {
            panic!("unknown mode {other:?} -- expected \"replay\", \"replay-loop\", or \"live\"")
        }
    }
}

fn replay_one_line(cfg: &Config, line: &str, index: usize, state: &mut AppState) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let data = match hex_to_bytes(line) {
        Ok(d) => d,
        Err(e) => {
            println!("skipping malformed fixture line {}: {e}", index + 1);
            return;
        }
    };

    match decode_using_cpp(&cfg.decoder_exe, &data) {
        None => print_raw_packet(&data, &format!("fixture line {}", index + 1)),
        Some(Value::Array(messages)) => {
            if messages.is_empty() {
                // Visible tick even when there's nothing to decode -- without
                // this, lines that produce no messages are silent, and a run
                // of several in a row looks like a stall rather than several
                // fast, uneventful packets.
                println!(
                    "[{}] {} bytes -> no messages, {}",
                    index + 1,
                    data.len(),
                    chrono::Local::now().format("%H:%M:%S%.3f")
                );
            }
            for decoded in &messages {
                handle_decoded(decoded, state);
            }
        }
        Some(_) => println!("Decoder returned non-array JSON, ignoring"),
    }
}
