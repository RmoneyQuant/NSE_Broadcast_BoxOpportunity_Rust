//! Rust client for RMTrade's "Third-Party Add Gateway" (see
//! `Third_party_gateway.md`) -- lets the operator turn a priced opportunity
//! row straight into a live RMTrade Box Spread strategy without re-typing
//! the four leg tokens by hand. The gateway itself is part of the RMTrade
//! desktop app (C++/Qt, loopback TCP, one JSON line in/out per connection);
//! this module is only the caller.

use nse_refdata::InstrumentMaster;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

/// `(expiry_epoch, strike_raw [rupees x100], "CE"/"PE")` -> token. Built
/// once at startup from the full, unfiltered contract file -- the mapping
/// itself never changes at runtime, only which tokens are *allowed* does
/// (`InstrumentMaster::restrict_tokens`), so this index stays valid across
/// every "Apply Filter" the operator makes afterwards.
pub type TokenIndex = HashMap<(i64, i64, String), i32>;

/// Filtered to `symbol == "NIFTY"` -- **required**, not a nicety. Different
/// underlyings routinely share the same (expiry_epoch, strike_raw,
/// option_type) key (e.g. NIFTY and FINNIFTY both trading a CE at the same
/// expiry timestamp and the same rupee strike), so an unfiltered `.collect()`
/// into this map would silently let whichever symbol's row happened to be
/// inserted last win the slot -- concretely observed: token 50280
/// (FINNIFTY CE) overwrote 51392 (NIFTY CE) at the same key, which is
/// exactly the leg RMTrade then rejected with `-4: token ... not found`
/// (right symbol format, wrong instrument entirely). This whole pipeline is
/// NIFTY-only already (see `main.rs`'s `ALLOWED_SYMBOLS`), so filtering here
/// too keeps this index consistent with what the rest of the app prices.
pub fn build_token_index(instruments: &InstrumentMaster) -> TokenIndex {
    instruments
        .all_contracts()
        .filter(|c| c.symbol == "NIFTY" && c.instrument_type == "OPTIDX" && (c.option_type == "CE" || c.option_type == "PE"))
        .map(|c| ((c.expiry_epoch, c.strike_raw, c.option_type.clone()), c.token))
        .collect()
}

// `pub(crate)`, not private: `order_log.rs` reuses these exact values so the
// order log's record of "what we sent" can never drift from what this
// module actually put on the wire -- one source of truth, not two
// hand-copied literals.
pub(crate) const EXCHANGE: &str = "EXCHG_NSE_FO";
const DEFAULT_GATEWAY_HOST: &str = "127.0.0.1";
pub(crate) const PRO_DEFAULT: bool = true;
pub(crate) const CLIENT_CODE_DEFAULT: &str = "";
// Doc defaults (`Third_party_gateway.md` §3); not exposed in the GUI yet --
// flag if per-request control over these is ever needed.
pub(crate) const SELL_SPREAD_DEFAULT: f64 = 1.0;
pub(crate) const BUY_SPREAD_DEFAULT: f64 = -1.0;

/// The four leg tokens for one box (K1 Call, K2 Call, K1 Put, K2 Put) --
/// verified against the desktop Add dialog's own leg wiring
/// (`Box_Spread_AddModifyWindow.cpp`: leg 3's strike is forced equal to leg
/// 1's, leg 4's to leg 2's -- i.e. legs 1&3 share K1, legs 2&4 share K2).
/// The gateway's wire protocol carries no buy/sell flag per leg (see
/// `Third_party_gateway.md` §3) -- direction is implied by this fixed leg
/// order on the RMTrade side, not something this client decides.
pub struct BoxLegs {
    pub k1_ce: i32,
    pub k1_pe: i32,
    pub k2_ce: i32,
    pub k2_pe: i32,
}

/// `None` if any of the four (expiry, strike, type) combinations isn't in
/// the loaded contract file -- e.g. an expiry/strike whose CE or PE token
/// was never in `NSE_FO_CONTRACT_CSV` to begin with.
pub fn resolve_legs(index: &TokenIndex, expiry_epoch: i64, k1: i64, k2: i64) -> Option<BoxLegs> {
    let k1_raw = k1 * 100;
    let k2_raw = k2 * 100;
    Some(BoxLegs {
        k1_ce: *index.get(&(expiry_epoch, k1_raw, "CE".to_string()))?,
        k1_pe: *index.get(&(expiry_epoch, k1_raw, "PE".to_string()))?,
        k2_ce: *index.get(&(expiry_epoch, k2_raw, "CE".to_string()))?,
        k2_pe: *index.get(&(expiry_epoch, k2_raw, "PE".to_string()))?,
    })
}

#[derive(Serialize)]
struct Leg {
    exchange: &'static str,
    token: i32,
}

/// Every operator-editable numeric field for one send -- bundled into a
/// struct rather than nine positional `f64`/`i64` arguments, which would be
/// an easy way to silently swap two same-typed values at the call site.
#[derive(Clone)]
pub struct BoxSpreadParams {
    pub qty: f64,
    pub max_buy_lot: f64,
    pub max_sell_lot: f64,
    pub n_lot: f64,
    pub profit: f64,
    pub jump: f64,
    pub bid_time: i64,
    pub delta: f64,
    pub lot_threshold: i64,
}

#[derive(Serialize)]
struct AddBoxSpreadRequest {
    api_key: String,
    action: &'static str,
    client_ref: String,
    legs: [Leg; 4],
    qty: f64,
    max_buy_lot: f64,
    max_sell_lot: f64,
    n_lot: f64,
    pro: bool,
    client_code: String,
    // Doc defaults (`Third_party_gateway.md` §3); not exposed in the GUI
    // yet -- flag if per-request control over these is ever needed.
    sell_spread: f64,
    buy_spread: f64,
    profit: f64,
    jump: f64,
    bid_time: i64,
    delta: f64,
    lot_threshold: i64,
}

#[derive(Deserialize, Debug)]
pub struct AddBoxSpreadResponse {
    pub ok: bool,
    pub strgy_id: Option<i64>,
    pub error_code: Option<i32>,
    pub error: Option<String>,
    #[allow(dead_code)]
    pub client_ref: Option<String>,
}

#[derive(Debug)]
pub enum SendError {
    MissingApiKey,
    MissingPort(String),
    Connect(std::io::Error),
    Io(std::io::Error),
    BadResponse(String),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::MissingApiKey => write!(f, "RMTRADE_GATEWAY_API_KEY is not set"),
            SendError::MissingPort(reason) => write!(f, "RMTRADE_GATEWAY_PORT is not set or invalid: {reason}"),
            SendError::Connect(e) => write!(f, "couldn't connect to the RMTrade gateway: {e}"),
            SendError::Io(e) => write!(f, "communication with the RMTrade gateway failed: {e}"),
            SendError::BadResponse(e) => write!(f, "unexpected response from the RMTrade gateway: {e}"),
        }
    }
}

/// One request, one connection, matching `Third_party_gateway.md` §3: open,
/// write one JSON line, read one JSON line back, done. Reads
/// `RMTRADE_GATEWAY_API_KEY`/`RMTRADE_GATEWAY_PORT` (and optionally
/// `RMTRADE_GATEWAY_HOST`, default `127.0.0.1`) fresh on every call -- this
/// runs once per deliberate operator click, not on any hot path, so there's
/// no reason to cache them the way the pricing pipeline's own startup
/// config is read.
pub fn send_add_box_spread(legs: &BoxLegs, client_ref: String, params: BoxSpreadParams) -> Result<AddBoxSpreadResponse, SendError> {
    let api_key = env::var("RMTRADE_GATEWAY_API_KEY").map_err(|_| SendError::MissingApiKey)?;
    let port: u16 = env::var("RMTRADE_GATEWAY_PORT")
        .map_err(|e| SendError::MissingPort(e.to_string()))?
        .trim()
        .parse()
        .map_err(|e: std::num::ParseIntError| SendError::MissingPort(e.to_string()))?;
    let host = env::var("RMTRADE_GATEWAY_HOST").unwrap_or_else(|_| DEFAULT_GATEWAY_HOST.to_string());

    let request = AddBoxSpreadRequest {
        api_key,
        action: "add_box_spread",
        client_ref,
        legs: [
            Leg { exchange: EXCHANGE, token: legs.k1_ce },
            Leg { exchange: EXCHANGE, token: legs.k2_ce },
            Leg { exchange: EXCHANGE, token: legs.k1_pe },
            Leg { exchange: EXCHANGE, token: legs.k2_pe },
        ],
        qty: params.qty,
        max_buy_lot: params.max_buy_lot,
        max_sell_lot: params.max_sell_lot,
        n_lot: params.n_lot,
        pro: PRO_DEFAULT,
        client_code: CLIENT_CODE_DEFAULT.to_string(),
        sell_spread: SELL_SPREAD_DEFAULT,
        buy_spread: BUY_SPREAD_DEFAULT,
        profit: params.profit,
        jump: params.jump,
        bid_time: params.bid_time,
        delta: params.delta,
        lot_threshold: params.lot_threshold,
    };

    let body = serde_json::to_string(&request).map_err(|e| SendError::BadResponse(e.to_string()))?;

    let mut stream = TcpStream::connect((host.as_str(), port)).map_err(SendError::Connect)?;
    // The gateway itself waits up to 5s on the trading server before
    // replying (`Third_party_gateway.md` §5.4) -- give it headroom past that.
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    stream.write_all(body.as_bytes()).map_err(SendError::Io)?;
    stream.write_all(b"\n").map_err(SendError::Io)?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(SendError::Io)?;

    serde_json::from_str(&line).map_err(|e| SendError::BadResponse(format!("{e} (raw: {})", line.trim())))
}
