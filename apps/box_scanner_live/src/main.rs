mod feed;
mod gui;
mod order_log;
mod rmtrade_gateway;
mod source;

use gui::{FilterSpec, GuiState, SharedFilterRequest, SharedGuiState};
use nse_decode::PacketDecoder;
use nse_refdata::InstrumentMaster;
use nse_sink::{AlertSink, AlertThresholds, StructuredLogSink};
use nse_state::{PricingSession, SchedulerEvent, Session, StrikePairDependencyIndex};
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

const ALLOWED_SYMBOLS: &[&str] = &["NIFTY"];
// FUTIDX alongside OPTIDX -- restrict_tokens() only ever *narrows*
// allowed_tokens() (it's retain-based, an intersection), so the NIFTY
// future used as the ATM-derivation reference has to already be a member
// of the initial allowed set here, or no later restriction could ever add
// it back. Futures never leak into box pairing regardless (strike_raw == 0
// skips them in both expiry_and_strike_catalog and
// StrikePairDependencyIndex::build).
const ALLOWED_INSTRUMENTS: &[&str] = &["OPTIDX", "FUTIDX"];

const DEFAULT_LZO_DLL: &str = r"C:\Users\rishav.raj\Desktop\decoder\liblzo2-2.dll";
const DEFAULT_FIXTURE: &str = "fixtures/sample_packets.hex";
/// `replay` mode defaults to the bundled fixture snapshot -- fine for
/// offline testing, but a fixed-in-time file, not the real current
/// contract universe. `live` mode instead defaults to the same real
/// contract file `nse_fo.py`/the decoder folder already uses, so a live
/// run reflects today's actual NIFTY strikes/expiries/tokens, not
/// whatever was true when the fixture snapshot was captured. Either can
/// still be overridden with `NSE_FO_CONTRACT_CSV`.
const DEFAULT_CONTRACT_CSV_REPLAY: &str = "fixtures/fo_contract_stream_info.csv";
const DEFAULT_CONTRACT_CSV_LIVE: &str = r"C:\Users\rishav.raj\Desktop\workspace\box_scanner_rust\fixtures\fo_contract_stream_info.csv";
const DEFAULT_OPPORTUNITY_LOG: &str = "opportunities.jsonl";
const DEFAULT_ALERT_LOG: &str = "alerts.jsonl";
const DEFAULT_RMTRADE_ORDER_LOG: &str = "rmtrade_orders.csv";
/// No real risk-free/MIBOR feed exists in this pipeline -- 0.0 until the
/// operator sets a real reference rate. See `NSE_FO_BENCHMARK_RATE` below.
const DEFAULT_BENCHMARK_RATE: f64 = 0.0;

/// (expiry_epoch, display date) for every NIFTY OPTIDX expiry in the
/// contract file, sorted -- feeds the GUI's Expiry dropdown.
type ExpiryList = Vec<(i64, String)>;
/// expiry_epoch -> sorted raw ×100 strikes that have both a CE and a PE
/// token at that expiry (the only kind that can be a box leg) -- feeds the
/// GUI's strike list box once an expiry is picked.
type StrikesByExpiry = HashMap<i64, Vec<i64>>;

/// Computed once from the unrestricted instrument set, before any filter
/// narrows it, so the GUI's expiry/strike pickers have the full real
/// universe to choose from regardless of what's currently applied.
fn expiry_and_strike_catalog(instruments: &InstrumentMaster) -> (ExpiryList, StrikesByExpiry) {
    let mut expiry_labels: HashMap<i64, String> = HashMap::new();
    let mut strike_sides: HashMap<i64, HashMap<i64, (bool, bool)>> = HashMap::new();

    for &token in instruments.allowed_tokens() {
        let Some(info) = instruments.get(token) else {
            continue;
        };
        if info.strike_raw == 0 {
            continue; // no strike -- not an option leg (e.g. a future)
        }
        expiry_labels.entry(info.expiry_epoch).or_insert_with(|| info.expiry_date.clone());
        let sides = strike_sides.entry(info.expiry_epoch).or_default().entry(info.strike_raw).or_insert((false, false));
        match info.option_type.as_str() {
            "CE" => sides.0 = true,
            "PE" => sides.1 = true,
            _ => {}
        }
    }

    let mut expiries: Vec<(i64, String)> = expiry_labels.into_iter().collect();
    expiries.sort_by_key(|(epoch, _)| *epoch);

    let strikes_by_expiry: HashMap<i64, Vec<i64>> = strike_sides
        .into_iter()
        .map(|(epoch, sides)| {
            let mut strikes: Vec<i64> = sides.into_iter().filter(|(_, (ce, pe))| *ce && *pe).map(|(strike, _)| strike).collect();
            strikes.sort_unstable();
            (epoch, strikes)
        })
        .collect();

    (expiries, strikes_by_expiry)
}

/// The real NIFTY future with the soonest expiry -- the one real,
/// already-decodable "current NIFTY level" source this feed has (confirmed
/// earlier: no index/spot broadcast exists -- NSE tx code 7207 "Market
/// Index" isn't implemented anywhere in this decoder or in
/// `NSE_FO_Structures.h`). `all_contracts()` reaches it even though
/// `ALLOWED_INSTRUMENTS` is `["OPTIDX"]`, since that only narrows
/// `allowed_tokens()`, not the underlying contract set.
fn nearest_future_token(instruments: &InstrumentMaster, symbol: &str) -> Option<i32> {
    instruments.all_contracts().filter(|c| c.symbol == symbol && c.instrument_type == "FUTIDX").min_by_key(|c| c.expiry_epoch).map(|c| c.token)
}

/// Loads contracts fresh from `contract_csv` and narrows the universe down
/// to `filter`'s one chosen expiry plus either the manually-selected
/// strikes (if any) or the K1 Min/K2 Max bounds -- reusable so the GUI's
/// "Apply Filter" can trigger it again at runtime without restarting the
/// process. Reloading from disk each time (rather than mutating a shared
/// `InstrumentMaster`) keeps this a plain, easily-testable function with no
/// interior mutability to reason about -- contract reloads are already
/// near-instant (see startup timing), so the cost is a non-issue.
/// `future_token`, if any, is always kept allowed regardless of the
/// filter -- it's the ATM-derivation reference, not a box leg, so it must
/// keep flowing into `Session`'s quote store no matter what's selected.
fn build_pipeline(contract_csv: &Path, filter: &FilterSpec, future_token: Option<i32>) -> (InstrumentMaster, Arc<StrikePairDependencyIndex>) {
    let mut instruments = InstrumentMaster::load(contract_csv, ALLOWED_SYMBOLS, ALLOWED_INSTRUMENTS);

    let strike_ok: Box<dyn Fn(i64) -> bool> = match &filter.selected_strikes {
        Some(strikes) => {
            let set: HashSet<i64> = strikes.iter().copied().collect();
            Box::new(move |s: i64| set.contains(&s))
        }
        None => {
            let k1_min_raw = (filter.k1_min * 100.0).round() as i64;
            let k2_max_raw = (filter.k2_max * 100.0).round() as i64;
            Box::new(move |s: i64| s >= k1_min_raw && s <= k2_max_raw)
        }
    };

    // None means "every expiry" -- the GUI's "All Expiries" selection.
    let expiry_epoch = filter.expiry_epoch;
    let mut kept_tokens: HashSet<i32> = instruments
        .allowed_tokens()
        .iter()
        .copied()
        .filter(|&token| instruments.get(token).is_some_and(|info| expiry_epoch.is_none_or(|e| info.expiry_epoch == e) && strike_ok(info.strike_raw)))
        .collect();
    if let Some(token) = future_token {
        kept_tokens.insert(token);
    }
    instruments.restrict_tokens(&kept_tokens);

    // Plain combinatorial pairing within the (already expiry+strike
    // narrowed) instruments -- no separate gap-band predicate anymore.
    // The future token has strike_raw == 0, so build() already skips it
    // when forming pairs (see StrikePairDependencyIndex::build_with).
    let index = Arc::new(StrikePairDependencyIndex::build(&instruments));

    (instruments, index)
}

fn main() {
    // Source thread hands raw packets across an mpsc channel to this thread,
    // which now decodes each one natively via nse_decode -- no subprocess.
    // Two source modes share the same channel + decoder:
    //   replay (default) -- read fixtures/sample_packets.hex, no network needed
    //   live             -- join the real NSE F&O multicast group
    let mode = env::args().nth(1).unwrap_or_else(|| "replay".to_string());

    let (tx, rx) = mpsc::channel::<Vec<u8>>();

    let source_handle = match mode.as_str() {
        "live" => {
            let mcast_grp = env::var("NSE_FO_MCAST_GRP").unwrap_or_else(|_| "239.60.60.8".to_string());
            let mcast_port: u16 = env::var("NSE_FO_MCAST_PORT").ok().and_then(|v| v.parse().ok()).unwrap_or(10008);
            let local_if = env::var("NSE_FO_LOCAL_IF").expect("NSE_FO_LOCAL_IF must be set to this machine's NIC IP for live mode");

            source::spawn_live(tx, mcast_grp, mcast_port, local_if).unwrap_or_else(|e| panic!("failed to join NSE F&O multicast group: {e}"))
        }
        "replay" => {
            let fixture = env::var("NSE_FO_FIXTURE").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(DEFAULT_FIXTURE));
            source::spawn_replay(tx, fixture)
        }
        other => panic!("unknown mode {other:?} -- expected \"replay\" or \"live\""),
    };

    let lzo_dll = env::var("NSE_FO_LZO_DLL").unwrap_or_else(|_| DEFAULT_LZO_DLL.to_string());
    let decoder = match PacketDecoder::with_lzo(&lzo_dll) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("warning: couldn't load {lzo_dll} ({e}) -- compressed messages will be skipped");
            PacketDecoder::new()
        }
    };

    let default_contract_csv = if mode == "live" { DEFAULT_CONTRACT_CSV_LIVE } else { DEFAULT_CONTRACT_CSV_REPLAY };
    let contract_csv = env::var("NSE_FO_CONTRACT_CSV").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(default_contract_csv));
    let mut instruments = InstrumentMaster::load(&contract_csv, ALLOWED_SYMBOLS, ALLOWED_INSTRUMENTS);
    println!("Loaded contracts: {} (from {})", instruments.len(), contract_csv.display());

    let (expiries, strikes_by_expiry) = expiry_and_strike_catalog(&instruments);
    println!("{} expiry/expiries available", expiries.len());

    // Built once from the full contract file, before any expiry/strike
    // filter narrows `instruments` -- feeds the GUI's "Send to RMTrade"
    // button, which needs to resolve any priced pair's four leg tokens
    // regardless of which filter happens to be active when it's clicked.
    let token_index = rmtrade_gateway::build_token_index(&instruments);

    // One CSV row per RMTrade send attempt, success or failure -- opened
    // once here (append mode, like the opportunity/alert logs below) and
    // handed to the GUI, which is the only thing that ever writes to it.
    let rmtrade_order_log = env::var("RMTRADE_ORDER_LOG").unwrap_or_else(|_| DEFAULT_RMTRADE_ORDER_LOG.to_string());
    let order_log = order_log::OrderLog::open(&rmtrade_order_log).unwrap_or_else(|e| panic!("failed to open RMTrade order log {rmtrade_order_log}: {e}"));
    println!("Logging RMTrade send attempts to {rmtrade_order_log}");

    let future_token = nearest_future_token(&instruments, "NIFTY");
    match future_token {
        Some(token) => println!("ATM reference: NIFTY future token {token} (nearest expiry)"),
        None => println!("warning: no NIFTY future found in the contract file -- ATM will stay unavailable, K1 Min/K2 Max won't auto-fill"),
    }

    // Nothing selected yet except the ATM-reference future (if found) --
    // the GUI starts with an empty pairing universe (0 pairs) until the
    // operator picks an expiry, chooses strikes or bounds, and clicks Apply
    // Filter. Matches the reference tool's flow: nothing is calculated
    // until explicitly configured, but the spot reference starts tracking
    // immediately so K1 Min/K2 Max have something to default from right away.
    instruments.restrict_tokens(&future_token.into_iter().collect());
    let index = Arc::new(StrikePairDependencyIndex::build(&instruments));

    let mut pricing_session = PricingSession::with_real_pricing(index);

    let opportunity_log = env::var("NSE_FO_OPPORTUNITY_LOG").unwrap_or_else(|_| DEFAULT_OPPORTUNITY_LOG.to_string());
    let mut sink = StructuredLogSink::open(&opportunity_log).unwrap_or_else(|e| panic!("failed to open opportunity log {opportunity_log}: {e}"));
    println!("Logging box-spread opportunities to {opportunity_log}");

    let benchmark_rate: f64 = env::var("NSE_FO_BENCHMARK_RATE").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_BENCHMARK_RATE);

    // Disabled (fires nothing) unless the operator sets real thresholds --
    // see AlertThresholds::disabled(). No sensible default rate exists to
    // invent here any more than a default benchmark rate did.
    let alert_thresholds =
        AlertThresholds { min_long_rate: env::var("NSE_FO_ALERT_MIN_LONG_RATE").ok().and_then(|v| v.parse().ok()), min_short_rate: env::var("NSE_FO_ALERT_MIN_SHORT_RATE").ok().and_then(|v| v.parse().ok()) };
    let alert_log = env::var("NSE_FO_ALERT_LOG").unwrap_or_else(|_| DEFAULT_ALERT_LOG.to_string());
    let mut alerts = AlertSink::with_log(alert_thresholds, &alert_log).unwrap_or_else(|e| panic!("failed to open alert log {alert_log}: {e}"));
    if alert_thresholds.min_long_rate.is_some() || alert_thresholds.min_short_rate.is_some() {
        println!("Alerting to {alert_log} (min_long_rate={:?}, min_short_rate={:?})", alert_thresholds.min_long_rate, alert_thresholds.min_short_rate);
    }

    let mut session = Session::new(instruments);

    let gui_state: SharedGuiState = Arc::new(Mutex::new(GuiState { benchmark_rate, ..Default::default() }));
    let gui_state_bg = Arc::clone(&gui_state);

    // Set by the GUI thread's "Apply Filter" button, read (and cleared) by
    // the background thread below at most once per incoming packet.
    let filter_request: SharedFilterRequest = Arc::new(Mutex::new(None));
    let filter_request_bg = Arc::clone(&filter_request);

    println!("box_scanner_live: mode={mode} (native decoder, no subprocess)");
    println!("Opening desktop window...");

    // Needed inside the background thread below (to label a newly-applied
    // filter's expiry) *and* by the GUI (to populate the Expiry dropdown) --
    // small (one row per expiry), so cloning once here is cheap.
    let expiries_bg = expiries.clone();

    // The whole decode/price/log pipeline moves to a background thread --
    // eframe's event loop (gui::run, below) needs the main thread on some
    // platforms, so it can no longer be the one blocking on `for packet in rx`.
    std::thread::spawn(move || {
        let mut packet_count = 0usize;
        let mut message_count = 0usize;
        let mut opportunity_count = 0usize;
        let mut drop_count = 0usize;
        let mut events = Vec::new();

        for packet in rx {
            // A new filter request replaces `instruments`/`session`/
            // `pricing_session` wholesale -- previously-accumulated quote
            // state doesn't carry over (the old pairs may not even be in
            // the new selection), so GUI opportunities are cleared too;
            // fresh ticks repopulate everything within a few seconds either way.
            if let Some(new_filter) = filter_request_bg.lock().unwrap().take() {
                let (new_instruments, new_index) = build_pipeline(&contract_csv, &new_filter, future_token);
                let pair_count = new_index.pair_count();
                let expiry_label = match new_filter.expiry_epoch {
                    Some(epoch) => expiries_bg.iter().find(|(e, _)| *e == epoch).map(|(_, label)| label.clone()).unwrap_or_default(),
                    None => "All Expiries".to_string(),
                };
                println!(
                    "Filter applied: expiry {expiry_label}, {} -> {} tokens, {pair_count} pair(s)",
                    match &new_filter.selected_strikes {
                        Some(strikes) => format!("{} manually selected strike(s)", strikes.len()),
                        None => format!("bounds {:.2}-{:.2}", new_filter.k1_min, new_filter.k2_max),
                    },
                    new_instruments.allowed_tokens().len()
                );
                pricing_session = PricingSession::with_real_pricing(new_index);
                session = Session::new(new_instruments);

                let mut gui = gui_state_bg.lock().unwrap();
                gui.long.clear();
                gui.short.clear();
                gui.active_expiry = expiry_label;
                gui.pair_count = pair_count;
            }

            packet_count += 1;
            let messages = decoder.decode_packet(&packet);
            message_count += messages.len();
            for message in &messages {
                feed::handle_message(&mut session, message);
                feed::feed_pricing_session(&mut pricing_session, message);

                // ATM, live: nearest strike (50-rupee steps) to the NIFTY
                // future's LTP -- the only real "current level" this feed
                // can produce (see nearest_future_token). Read after
                // handle_message so the store already has this tick applied.
                if let Some(token) = future_token
                    && let Some(quote) = session.state.find(token)
                    && quote.ltp != 0
                {
                    let spot = quote.ltp as f64 / 100.0;
                    let atm = (spot / 50.0).round() * 50.0;
                    gui_state_bg.lock().unwrap().current_atm = Some(atm);
                }

                events.clear();
                pricing_session.drain_dirty(&mut events);
                for event in &events {
                    let result = match event {
                        SchedulerEvent::Opportunity { record, long_qualifies, short_qualifies } => {
                            opportunity_count += 1;
                            gui_state_bg.lock().unwrap().emit_opportunity(record, *long_qualifies, *short_qualifies);
                            for alert in alerts.check(record, *long_qualifies, *short_qualifies) {
                                println!("[ALERT] {alert:?}");
                            }
                            sink.write_opportunity(record)
                        }
                        SchedulerEvent::Dropped(drop) => {
                            drop_count += 1;
                            gui_state_bg.lock().unwrap().emit_drop(*drop);
                            if let Some(alert) = alerts.on_drop(*drop) {
                                println!("[ALERT] {alert:?}");
                            }
                            sink.write_drop(drop)
                        }
                    };
                    if let Err(e) = result {
                        eprintln!("warning: failed to write opportunity log line: {e}");
                    }
                }
            }

            let mut gui = gui_state_bg.lock().unwrap();
            gui.packet_count = packet_count;
            gui.message_count = message_count;
        }

        sink.flush().ok();
        alerts.flush().ok();

        source_handle.join().expect("source thread panicked");
        println!(
            "source exhausted -- {packet_count} packet(s) processed, {message_count} message(s) decoded, \
             {opportunity_count} opportunity event(s), {drop_count} drop event(s) logged to {opportunity_log}"
        );
    });

    if let Err(e) = gui::run(gui_state, filter_request, expiries, strikes_by_expiry, token_index, order_log) {
        eprintln!(
            "desktop window failed to open: {e}\n\
             This usually means no interactive Windows desktop session is available to this \
             process (e.g. running as a headless service or scheduled task) -- run this from a \
             normal interactive desktop session instead. The pricing pipeline above still ran \
             to completion; only the window itself failed to open."
        );
        std::process::exit(1);
    }
}
