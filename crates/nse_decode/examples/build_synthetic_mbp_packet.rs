//! Builds hand-crafted, realistic 7208 MBP packets and prints their hex
//! encodings, one per line -- the same byte-layout construction used by
//! `decodes_a_synthetic_mbp_packet` in `src/lib.rs`, factored out here so
//! they can be appended to `fixtures/sample_packets.hex` as real non-empty
//! fixture lines. The shipped fixtures all have `iNoPackets == 0`, so
//! nothing decodes to anything; this exists purely to give `nse_decode`,
//! `nse_fo_verify`, and `box_scanner_live`'s replay mode real, *varied*
//! data -- enough strikes across enough expiries to exercise the ATM-window
//! and strike-gap-band filter (`box_scanner_live/src/main.rs`) with the
//! market closed, not just one repeating strike pair.
//!
//! 7 real NIFTY OPTIDX strikes (23000..26000, matching
//! `fixtures/fo_contract_stream_info.csv`'s real tokens) x CE/PE x 3 real
//! expiries = 42 contracts. Prices are a simple linear synthetic model, not
//! a real options pricer -- just enough that call/put mids move sensibly
//! with strike distance from the 24500 ATM used elsewhere in this session,
//! so every leg has a plausible non-zero quote.
//!
//! Run with: cargo run -p nse_decode --example build_synthetic_mbp_packet

fn be16(v: i16) -> [u8; 2] {
    v.to_be_bytes()
}
fn be32(v: i32) -> [u8; 4] {
    v.to_be_bytes()
}
fn be64f(v: f64) -> [u8; 8] {
    v.to_be_bytes()
}

struct Level {
    price: i32,
    qty: i32,
    orders: i16,
}

struct PacketSpec {
    token: i32,
    seq: i32,
    ltp: i32,
    atp: i32,
    close: i32,
    ltq: i32,
    open: i32,
    high: i32,
    low: i32,
    tbq: f64,
    tsq: f64,
    total_traded_qty: i32,
    bids: [Level; 5],
    asks: [Level; 5],
}

fn build_packet(spec: &PacketSpec) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();

    // 2-byte local prefix ahead of BcastPackData (content is unread by the
    // decoder, matches OUTER_PREFIX).
    buf.extend_from_slice(&[0xAA, 0xBB]);

    // BcastPackData: iNoPackets = 1.
    buf.extend_from_slice(&be16(1));

    // BcastCmpPacket for that one bundled message: iCompLen = 0 (raw/uncompressed).
    buf.extend_from_slice(&be16(0));

    let body_start = buf.len();
    buf.push(2); // marker byte, must equal 2
    while buf.len() - body_start < 18 {
        buf.push(0); // pad out to HEADER_GAP
    }

    // BCAST_HEADER (14 bytes).
    let header_start = buf.len();
    buf.extend_from_slice(&be16(7208)); // [0..2) TransCode
    buf.extend_from_slice(&[0, 0]); // [2..4) filler
    buf.extend_from_slice(&be32(spec.seq)); // [4..8) BCSeqNo, read raw at +4
    buf.extend_from_slice(&[0, 0, 0, 0]); // [8..12) filler
    let message_length_pos = buf.len();
    buf.extend_from_slice(&be16(0)); // [12..14) MessageLength, patched below

    // NoOfRecords = 1.
    buf.extend_from_slice(&be16(1));

    buf.extend_from_slice(&be16(1)); // BookType = 1 (regular/normal-lot book)
    buf.extend_from_slice(&be32(spec.token));
    for level in spec.bids.iter().chain(spec.asks.iter()) {
        buf.extend_from_slice(&be16(0)); // BbBuySellFlag (unused by the decoder)
        buf.extend_from_slice(&be16(level.orders));
        buf.extend_from_slice(&be32(level.price));
        buf.extend_from_slice(&be32(level.qty));
    }

    buf.extend_from_slice(&be32(spec.ltp));
    buf.extend_from_slice(&be32(spec.atp));
    buf.extend_from_slice(&be32(spec.close));
    buf.extend_from_slice(&be32(spec.ltq));
    buf.extend_from_slice(&be32(0)); // LastTradeTime
    buf.extend_from_slice(&be32(spec.open));
    buf.extend_from_slice(&be32(spec.high));
    buf.extend_from_slice(&be32(spec.low));
    buf.extend_from_slice(&be64f(spec.tbq));
    buf.extend_from_slice(&be64f(spec.tsq));
    buf.extend_from_slice(&be32(spec.total_traded_qty));

    let message_length = (buf.len() - header_start) as i16;
    buf[message_length_pos..message_length_pos + 2].copy_from_slice(&be16(message_length));

    buf
}

/// One strike's real CE/PE token pair at one expiry, looked up from
/// `fixtures/fo_contract_stream_info.csv` directly -- `strike` is rupees
/// (not the raw ×100 wire scale) since it's only used here to compute a
/// synthetic mid price and isn't sent on the wire itself.
struct StrikeTokens {
    strike: i32,
    ce_token: i32,
    pe_token: i32,
}

const EXPIRY_25_AUG_2026: [StrikeTokens; 7] = [
    StrikeTokens { strike: 23000, ce_token: 61428, pe_token: 61429 },
    StrikeTokens { strike: 23500, ce_token: 61499, pe_token: 61500 },
    StrikeTokens { strike: 24000, ce_token: 61593, pe_token: 61604 },
    StrikeTokens { strike: 24500, ce_token: 61734, pe_token: 61771 },
    StrikeTokens { strike: 25000, ce_token: 61897, pe_token: 61898 },
    StrikeTokens { strike: 25500, ce_token: 61975, pe_token: 61976 },
    StrikeTokens { strike: 26000, ce_token: 62038, pe_token: 62039 },
];

const EXPIRY_29_SEP_2026: [StrikeTokens; 7] = [
    StrikeTokens { strike: 23000, ce_token: 73903, pe_token: 65899 },
    StrikeTokens { strike: 23500, ce_token: 73985, pe_token: 73994 },
    StrikeTokens { strike: 24000, ce_token: 65900, pe_token: 65901 },
    StrikeTokens { strike: 24500, ce_token: 74241, pe_token: 74242 },
    StrikeTokens { strike: 25000, ce_token: 74365, pe_token: 65903 },
    StrikeTokens { strike: 25500, ce_token: 55286, pe_token: 55287 },
    StrikeTokens { strike: 26000, ce_token: 65904, pe_token: 65905 },
];

const EXPIRY_27_OCT_2026: [StrikeTokens; 7] = [
    StrikeTokens { strike: 23000, ce_token: 51352, pe_token: 51353 },
    StrikeTokens { strike: 23500, ce_token: 51372, pe_token: 51373 },
    StrikeTokens { strike: 24000, ce_token: 51392, pe_token: 51393 },
    StrikeTokens { strike: 24500, ce_token: 51418, pe_token: 51419 },
    StrikeTokens { strike: 25000, ce_token: 51440, pe_token: 51441 },
    StrikeTokens { strike: 25500, ce_token: 51468, pe_token: 51469 },
    StrikeTokens { strike: 26000, ce_token: 51490, pe_token: 51491 },
];

/// Simple linear synthetic pricing (not a real options model): call mid
/// falls and put mid rises as strike moves away from ATM (24500), so every
/// leg the ATM-window/gap-band filter selects has a plausible non-zero
/// quote. Returns paise (wire scale, matching the rest of `PacketSpec`).
fn synthetic_mid_paise(strike: i32, is_call: bool) -> i32 {
    if is_call {
        (26_500 - strike).max(50) * 10
    } else {
        (strike - 21_500).max(50) * 10
    }
}

fn build_spec(token: i32, seq: i32, strike: i32, is_call: bool) -> PacketSpec {
    let mid = synthetic_mid_paise(strike, is_call);
    let bid_mid = mid - 25;
    let ask_mid = mid + 25;

    let bids: [Level; 5] = std::array::from_fn(|i| Level {
        price: bid_mid - (i as i32) * 25,
        qty: (600 - (i as i32) * 100).max(20),
        orders: (12 - (i as i32) * 2).max(1) as i16,
    });
    let asks: [Level; 5] = std::array::from_fn(|i| Level {
        price: ask_mid + (i as i32) * 25,
        qty: (550 - (i as i32) * 90).max(20),
        orders: (11 - (i as i32) * 2).max(1) as i16,
    });

    PacketSpec {
        token,
        seq,
        ltp: mid,
        atp: mid,
        close: mid - 500,
        ltq: 50,
        open: mid - 1000,
        high: mid + 1000,
        low: mid - 1500,
        tbq: 10000.0,
        tsq: 9000.0,
        total_traded_qty: 500_000,
        bids,
        asks,
    }
}

/// Real NIFTY FUTIDX token, nearest expiry (25-Aug-2026) in the contract
/// file -- the one real, decodable "current NIFTY level" source this feed
/// has (no index/spot broadcast exists; see `main.rs`'s
/// `nearest_future_token`). LTP deliberately isn't a round number
/// (24487.35, not 24500.00) so the ATM-rounds-to-nearest-50 logic actually
/// exercises rounding instead of a no-op identity case -- it still lands on
/// 24500 after rounding, matching the 7-strike synthetic option set above.
const NIFTY_FUTURE_TOKEN: i32 = 58072;

fn build_future_spec(seq: i32) -> PacketSpec {
    let mid = 2_448_735; // paise -- 24487.35
    let bid_mid = mid - 25;
    let ask_mid = mid + 25;

    let bids: [Level; 5] = std::array::from_fn(|i| Level {
        price: bid_mid - (i as i32) * 25,
        qty: (900 - (i as i32) * 120).max(30),
        orders: (18 - (i as i32) * 3).max(1) as i16,
    });
    let asks: [Level; 5] = std::array::from_fn(|i| Level {
        price: ask_mid + (i as i32) * 25,
        qty: (850 - (i as i32) * 110).max(30),
        orders: (17 - (i as i32) * 3).max(1) as i16,
    });

    PacketSpec {
        token: NIFTY_FUTURE_TOKEN,
        seq,
        ltp: mid,
        atp: mid,
        close: mid - 5000,
        ltq: 75,
        open: mid - 10000,
        high: mid + 10000,
        low: mid - 15000,
        tbq: 50000.0,
        tsq: 48000.0,
        total_traded_qty: 5_000_000,
        bids,
        asks,
    }
}

fn main() {
    // 3 real expiries x 7 real strikes x CE/PE = 42 option contracts, plus
    // 1 NIFTY future tick (the ATM-derivation reference) -- see the module
    // doc for why (exercises the ATM-window + strike-gap-band filter with
    // the market closed).
    let expiries: [&[StrikeTokens]; 3] = [&EXPIRY_25_AUG_2026, &EXPIRY_29_SEP_2026, &EXPIRY_27_OCT_2026];

    let mut seq = 9001;
    for strikes in expiries {
        for st in strikes {
            for (token, is_call) in [(st.ce_token, true), (st.pe_token, false)] {
                let spec = build_spec(token, seq, st.strike, is_call);
                let buf = build_packet(&spec);
                let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
                println!("{hex}");
                seq += 1;
            }
        }
    }

    let future_spec = build_future_spec(seq);
    let buf = build_packet(&future_spec);
    let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    println!("{hex}");
}
