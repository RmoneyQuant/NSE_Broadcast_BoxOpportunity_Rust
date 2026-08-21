//! One-off capture tool, not part of the production pipeline: listens to
//! the real NSE F&O feed for a short window, accumulates every quote into a
//! plain `LiveMarketStateStore`, then -- once the window closes -- prices
//! every *adjacent*-strike pair (the strikes an actual box spread would
//! use, unlike `StrikePairDependencyIndex`'s full combinatorial pairing)
//! that ended up with all four legs quoted. Prints the tightest ones as
//! ready-to-paste `fixtures/pricing_test_vectors.json` entries -- a real
//! captured market snapshot instead of hand-picked synthetic numbers.
//!
//! Pricing after the window closes, rather than dirtying pairs live via
//! `PricingSession`, is deliberate: with ~4000 allowed tokens and NSE's
//! combinatorial strike-pair count in the hundreds of thousands, waiting
//! for one *specific* pair's dirty event to land inside the capture window
//! is far less likely to succeed than just checking every adjacent pair
//! once against whatever quotes arrived by the end.
//!
//! Needs the real multicast feed reachable, same env vars as
//! `box_scanner_live`'s live mode:
//!
//!     NSE_FO_LOCAL_IF=192.168.50.210 cargo run -p nse_state --release \
//!         --example capture_real_pricing_vector

use nse_adapt::QuoteUpdate;
use nse_decode::{Message, PacketDecoder};
use nse_pricing::{box_pricer, days_to_expiry, interest_calc};
use nse_refdata::InstrumentMaster;
use nse_state::LiveMarketStateStore;
use std::collections::HashMap;
use std::env;
use std::net::{Ipv4Addr, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_LZO_DLL: &str = r"C:\Users\rishav.raj\Desktop\decoder\liblzo2-2.dll";
const CAPTURE_SECONDS: u64 = 40;
const TOP_N: usize = 5;

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// One expiry's two neighboring valid strikes (both CE and PE present) --
/// the leg tokens an actual box spread would trade.
struct AdjacentPair {
    expiry_id: i64,
    k1_raw: i64,
    k2_raw: i64,
    call_k1: i32,
    put_k1: i32,
    call_k2: i32,
    put_k2: i32,
}

/// Same expiry/strike grouping `StrikePairDependencyIndex::build` uses, but
/// pairs only immediate strike neighbors instead of every combination --
/// this tool wants realistic box-spread candidates, not the full dependency
/// fan-out the live scheduler needs.
fn adjacent_pairs(instruments: &InstrumentMaster) -> Vec<AdjacentPair> {
    type StrikeSides = HashMap<i64, (Option<i32>, Option<i32>)>;
    let mut by_expiry: HashMap<i64, StrikeSides> = HashMap::new();

    for &token in instruments.allowed_tokens() {
        let Some(info) = instruments.get(token) else {
            continue;
        };
        if info.strike_raw == 0 {
            continue;
        }
        let entry = by_expiry
            .entry(info.expiry_epoch)
            .or_default()
            .entry(info.strike_raw)
            .or_insert((None, None));
        match info.option_type.as_str() {
            "CE" => entry.0 = Some(token),
            "PE" => entry.1 = Some(token),
            _ => {}
        }
    }

    let mut pairs = Vec::new();
    for (&expiry_id, strikes) in &by_expiry {
        let mut valid: Vec<(i64, i32, i32)> = strikes
            .iter()
            .filter_map(|(&strike, &(ce, pe))| match (ce, pe) {
                (Some(c), Some(p)) => Some((strike, c, p)),
                _ => None,
            })
            .collect();
        valid.sort_by_key(|&(strike, _, _)| strike);

        for w in valid.windows(2) {
            let (k1, call_k1, put_k1) = w[0];
            let (k2, call_k2, put_k2) = w[1];
            pairs.push(AdjacentPair {
                expiry_id,
                k1_raw: k1,
                k2_raw: k2,
                call_k1,
                put_k1,
                call_k2,
                put_k2,
            });
        }
    }
    pairs
}

fn main() {
    let mcast_grp = env::var("NSE_FO_MCAST_GRP").unwrap_or_else(|_| "239.60.60.8".to_string());
    let mcast_port: u16 = env::var("NSE_FO_MCAST_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10008);
    let local_if = env::var("NSE_FO_LOCAL_IF")
        .expect("NSE_FO_LOCAL_IF must be set to this machine's NIC IP for live mode");
    let contract_csv = env::var("NSE_FO_CONTRACT_CSV")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("fixtures/fo_contract_stream_info.csv"));
    let lzo_dll = env::var("NSE_FO_LZO_DLL").unwrap_or_else(|_| DEFAULT_LZO_DLL.to_string());

    let instruments = InstrumentMaster::load(&contract_csv, &["NIFTY"], &["OPTIDX"]);
    println!(
        "Loaded {} contracts, {} allowed tokens",
        instruments.len(),
        instruments.allowed_tokens().len()
    );

    let pairs = adjacent_pairs(&instruments);
    println!(
        "{} adjacent strike pair(s) across all expiries",
        pairs.len()
    );

    let decoder = match PacketDecoder::with_lzo(&lzo_dll) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "warning: couldn't load {lzo_dll} ({e}) -- compressed messages will be skipped"
            );
            PacketDecoder::new()
        }
    };

    let raw = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)
        .expect("create socket");
    raw.set_reuse_address(true).expect("set SO_REUSEADDR");
    let addr: std::net::SocketAddr = ([0, 0, 0, 0], mcast_port).into();
    raw.bind(&addr.into()).expect("bind");
    let socket: UdpSocket = raw.into();
    let grp: Ipv4Addr = mcast_grp.parse().expect("valid NSE_FO_MCAST_GRP");
    let iface: Ipv4Addr = local_if.parse().expect("valid NSE_FO_LOCAL_IF");
    socket
        .join_multicast_v4(&grp, &iface)
        .expect("join multicast group");
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .expect("set read timeout");

    println!("Listening on {mcast_grp}:{mcast_port} for {CAPTURE_SECONDS}s ...\n");

    let mut store = LiveMarketStateStore::new();
    let start = Instant::now();
    let mut buf = [0u8; 65535];

    while start.elapsed() < Duration::from_secs(CAPTURE_SECONDS) {
        let n = match socket.recv_from(&mut buf) {
            Ok((n, _)) => n,
            Err(_) => continue, // read timeout -- keep polling until the window closes
        };

        for message in decoder.decode_packet(&buf[..n]) {
            let now = now_ns();
            match &message {
                Message::Mbp(m) if instruments.is_allowed(m.token) => {
                    store.apply(QuoteUpdate::from_mbp(m, now))
                }
                Message::EnhancedMbp(m) if instruments.is_allowed(m.token) => {
                    store.apply(QuoteUpdate::from_enhanced_mbp(m.token, &m.entries, now))
                }
                _ => {}
            }
        }
    }

    println!(
        "Window closed -- {} distinct token(s) quoted at least once.\n",
        store.len()
    );

    let today = chrono::Local::now().date_naive();
    let bid_ask = |q: &QuoteUpdate| (q.bid_price[0] as f64 / 100.0, q.ask_price[0] as f64 / 100.0);

    struct Priced {
        pair: AdjacentPair,
        short_spread: f64,
        long_spread: f64,
        short_rate: Option<f64>,
        long_rate: Option<f64>,
        days: i64,
        legs: [(String, f64, f64); 4],
    }

    let mut results = Vec::new();
    for pair in pairs {
        let (Some(c1), Some(p1), Some(c2), Some(p2)) = (
            store.find(pair.call_k1),
            store.find(pair.put_k1),
            store.find(pair.call_k2),
            store.find(pair.put_k2),
        ) else {
            continue;
        };

        let (c1_bid, c1_ask) = bid_ask(c1);
        let (p1_bid, p1_ask) = bid_ask(p1);
        let (c2_bid, c2_ask) = bid_ask(c2);
        let (p2_bid, p2_ask) = bid_ask(p2);

        let Some(short_spread) = box_pricer::box_sell_price(c1_bid, p1_ask, c2_ask, p2_bid) else {
            continue;
        };
        let Some(long_spread) = box_pricer::box_long_price(c1_ask, p1_bid, c2_bid, p2_ask) else {
            continue;
        };

        let days = days_to_expiry(pair.expiry_id, today);
        let strike_difference = (pair.k2_raw - pair.k1_raw) as f64 / 100.0;
        let short_rate =
            interest_calc::annualized_interest_rate(short_spread, strike_difference, days);
        let long_rate =
            interest_calc::annualized_interest_rate(long_spread, strike_difference, days);

        let legs = [
            (instruments.contract_name(pair.call_k1), c1_bid, c1_ask),
            (instruments.contract_name(pair.put_k1), p1_bid, p1_ask),
            (instruments.contract_name(pair.call_k2), c2_bid, c2_ask),
            (instruments.contract_name(pair.put_k2), p2_bid, p2_ask),
        ];

        results.push(Priced {
            pair,
            short_spread,
            long_spread,
            short_rate,
            long_rate,
            days,
            legs,
        });
    }

    println!("{} fully-quoted adjacent pair(s) found.\n", results.len());

    // Tightest long-minus-short spread as a liquidity proxy -- a wide gap
    // between what you'd pay to buy the box and receive to sell it usually
    // means at least one leg's book is thin.
    results.sort_by(|a, b| {
        (a.long_spread - a.short_spread)
            .abs()
            .partial_cmp(&(b.long_spread - b.short_spread).abs())
            .unwrap()
    });

    for r in results.into_iter().take(TOP_N) {
        println!(
            "-- {} / {} | {} / {}  (days_to_expiry={})",
            r.legs[0].0, r.legs[1].0, r.legs[2].0, r.legs[3].0, r.days
        );
        println!(
            "   short_spread={:.2} long_spread={:.2} short_rate={:?} long_rate={:?}",
            r.short_spread, r.long_spread, r.short_rate, r.long_rate
        );
        println!(
            "{{\n  \"name\": \"real_capture_{}_{}\",\n  \"k1\": {:.1},\n  \"k2\": {:.1},\n  \"days_to_expiry\": {},\n  \"call_k1\": {{ \"bid\": {:.2}, \"ask\": {:.2} }},\n  \"put_k1\": {{ \"bid\": {:.2}, \"ask\": {:.2} }},\n  \"call_k2\": {{ \"bid\": {:.2}, \"ask\": {:.2} }},\n  \"put_k2\": {{ \"bid\": {:.2}, \"ask\": {:.2} }}\n}}\n",
            r.pair.k1_raw / 100,
            r.pair.k2_raw / 100,
            r.pair.k1_raw as f64 / 100.0,
            r.pair.k2_raw as f64 / 100.0,
            r.days,
            r.legs[0].1,
            r.legs[0].2,
            r.legs[1].1,
            r.legs[1].2,
            r.legs[2].1,
            r.legs[2].2,
            r.legs[3].1,
            r.legs[3].2,
        );
    }
}
