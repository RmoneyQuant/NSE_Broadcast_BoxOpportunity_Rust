//! Presentation layer: prints decoded `nse_decode::Message`s in the
//! `nse_fo.py` `print_feed()` block shape. Backed by `nse_state::Session`
//! (Phase 3's refdata + state store) instead of the ad hoc local structs
//! this file held before -- `ContractInfo`/`FeedState` are gone from here,
//! replaced by `nse_refdata::InstrumentMaster` and
//! `nse_state::LiveMarketStateStore` respectively.

use nse_adapt::QuoteUpdate;
use nse_decode::Message;
use nse_state::{PricingSession, Session};
use std::time::{SystemTime, UNIX_EPOCH};

const DEPTH: usize = 5;
const PRICE_DIVISOR: f64 = 100.0;

pub fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn format_local(ns: u64) -> String {
    let secs = (ns / 1_000_000_000) as i64;
    let nanos = (ns % 1_000_000_000) as u32;
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%H:%M:%S%.6f")
                .to_string()
        })
        .unwrap_or_default()
}

fn price(value: i64) -> f64 {
    if value != 0 {
        value as f64 / PRICE_DIVISOR
    } else {
        0.0
    }
}

/// Routes one decoded message into `session`'s state store and prints the
/// resulting feed block, filtered to `session.instruments`' allowed tokens.
pub fn handle_message(session: &mut Session, message: &Message) {
    let now = now_ns();

    match message {
        Message::Mbp(m) => {
            if !session.instruments.is_allowed(m.token) {
                return;
            }
            session.state.apply(QuoteUpdate::from_mbp(m, now));
            print_quote(session, m.token, now);
        }
        Message::EnhancedMbp(m) => {
            if !session.instruments.is_allowed(m.token) {
                return;
            }
            session
                .state
                .apply(QuoteUpdate::from_enhanced_mbp(m.token, &m.entries, now));
            print_quote(session, m.token, now);
        }
        Message::OiTicker(m) => {
            if !session.instruments.is_allowed(m.token) {
                return;
            }
            let merged = QuoteUpdate::merge_oi(session.state.find(m.token), m, now);
            session.state.apply(merged);
            print_quote(session, m.token, now);
        }
        Message::SecurityRange(m) => {
            if !session.instruments.is_allowed(m.token) {
                return;
            }
            println!(
                "[SECURITY_RANGE] token={} low={} high={}",
                m.token, m.low_price_range, m.high_price_range
            );
        }
        Message::ExecRange(m) => {
            for entry in &m.entries {
                if !session.instruments.is_allowed(entry.token) {
                    continue;
                }
                println!(
                    "[EXEC_RANGE] token={} low={} high={}",
                    entry.token, entry.low_exec_band, entry.high_exec_band
                );
            }
        }
    }
}

/// Feeds one decoded message into the Phase 4/6 pricing pipeline
/// (`PricingSession`), separately from `handle_message`'s console display.
/// No token-allowlist check here, unlike `handle_message` -- unnecessary,
/// since `pricing_session`'s dependency index was built only from allowed
/// tokens (see `main.rs`), so a token outside it simply dirties nothing.
pub fn feed_pricing_session(pricing_session: &mut PricingSession, message: &Message) {
    let now = now_ns();

    match message {
        Message::Mbp(m) => pricing_session.on_quote(QuoteUpdate::from_mbp(m, now)),
        Message::EnhancedMbp(m) => {
            pricing_session.on_quote(QuoteUpdate::from_enhanced_mbp(m.token, &m.entries, now))
        }
        Message::OiTicker(m) => {
            let merged = QuoteUpdate::merge_oi(pricing_session.state().find(m.token), m, now);
            pricing_session.on_quote(merged);
        }
        Message::SecurityRange(_) | Message::ExecRange(_) => {}
    }
}

fn print_quote(session: &Session, token: i32, update_ns: u64) {
    let Some(q) = session.state.find(token) else {
        return;
    };

    println!("{}", "=".repeat(120));
    println!(
        "TOKEN: {} | {} | UPDATE: {}",
        token,
        session.instruments.contract_name(token),
        format_local(update_ns)
    );

    println!(
        "LTP={:.2} LTQ={} O={:.2} H={:.2} L={:.2} C={:.2} ATP={:.2}",
        price(q.ltp),
        q.ltq,
        price(q.open),
        price(q.high),
        price(q.low),
        price(q.close),
        price(q.atp),
    );

    println!(
        "TTQ={} OI={} DAY_HI_OI={} DAY_LO_OI={} TBQ={} TSQ={}",
        q.total_traded_qty, q.open_interest, q.day_high_oi, q.day_low_oi, q.tbq, q.tsq,
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
            q.bid_qty[i],
            price(q.bid_price[i]),
            q.bid_orders[i],
            price(q.ask_price[i]),
            q.ask_qty[i],
            q.ask_orders[i],
        );
    }

    println!();
}
