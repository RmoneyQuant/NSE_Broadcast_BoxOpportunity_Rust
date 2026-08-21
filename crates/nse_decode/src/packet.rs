use crate::cursor::{Cursor, peek_i16_at};
use crate::error::DecodeError;
use crate::lzo::LzoDecompressor;
use crate::message::{
    BookLevel, EnhancedMbpMessage, ExecRangeEntry, ExecRangeMessage, MbpMessage, Message,
    OiTickerMessage, SecurityRangeMessage, Side,
};
use std::path::Path;

// Every datagram observed so far carries 2 bytes ahead of the real NSE
// BcastPackData structure (value differs per packet -- not part of NSE's own
// NNF framing, most likely something a local feed-handler/gateway prepends).
const OUTER_PREFIX: usize = 2;

// Gap from the raw message body's marker byte (must equal 2) to the start of
// BCAST_HEADER/TransCode. The published reference (Socket.cpp) assumes 8;
// real captured packets need 18.
const HEADER_GAP: usize = 18;

// TransCode(2) + LogTime(4) + AlphaChar(2) + BCSeqNo(4) + MessageLength(2).
const BCAST_HEADER_LEN: usize = 14;

// BCAST_HEADER::MessageLength's nominal byte offset within the header (after
// TransCode + LogTime + AlphaChar + BCSeqNo).
const MESSAGE_LENGTH_OFF: usize = 12;

const LZO_SCRATCH_LEN: usize = 2048;

/// Decodes raw NSE F&O broadcast UDP datagrams. LZO is only needed for
/// compressed bundled messages (`iCompLen > 0`) -- see `lzo.rs` -- so it's
/// optional: `new()` works anywhere, `with_lzo()` additionally handles
/// compressed messages when a real `liblzo2` is available.
pub struct PacketDecoder {
    lzo: Option<LzoDecompressor>,
}

impl PacketDecoder {
    pub fn new() -> Self {
        Self { lzo: None }
    }

    pub fn with_lzo(lzo_dll_path: impl AsRef<Path>) -> Result<Self, DecodeError> {
        Ok(Self {
            lzo: Some(LzoDecompressor::load(lzo_dll_path)?),
        })
    }
}

impl Default for PacketDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl PacketDecoder {
    /// Walks one datagram's worth of bundled/compressed messages and returns
    /// every record this port recognizes. Malformed framing (a corrupt
    /// length, a bad marker byte, a failed decompression) stops the walk
    /// early and returns whatever was already decoded, rather than treating
    /// it as a hard error for the whole datagram -- the same behavior
    /// `decode_packet`'s `break` statements produce in the C++ original.
    pub fn decode_packet(&self, buf: &[u8]) -> Vec<Message> {
        let mut out = Vec::new();

        if buf.len() < OUTER_PREFIX + 2 {
            return out;
        }

        let Ok(no_packets) = peek_i16_at(buf, OUTER_PREFIX) else {
            return out;
        };
        if no_packets < 0 {
            return out;
        }

        // Measured from the start of BcastPackData (i.e. still includes its
        // own 2-byte iNoPackets field) -- matches the C++ source's `bufLen`
        // exactly, off-by-2-looking bound checks below and all.
        let buf_len = (buf.len() - OUTER_PREFIX) as i64;
        let mut loc: i64 = 0;

        for _ in 0..no_packets {
            if loc + 2 > buf_len - 2 {
                break;
            }

            let comp_len_offset = OUTER_PREFIX + 2 + loc as usize;
            let Ok(comp_len) = peek_i16_at(buf, comp_len_offset) else {
                break;
            };
            let comp_data_offset = comp_len_offset + 2;

            let (body, message_length): (Vec<u8>, i64) = if comp_len > 0 {
                let comp_len = comp_len as i64;
                if loc + 2 + comp_len > buf_len {
                    break;
                }
                let Some(compressed) =
                    buf.get(comp_data_offset..comp_data_offset + comp_len as usize)
                else {
                    break;
                };
                let Some(lzo) = &self.lzo else {
                    break; // compressed message, but no LZO loaded to decode it
                };
                let Ok(decompressed) = lzo.decompress(compressed, LZO_SCRATCH_LEN) else {
                    break;
                };
                if decompressed.first() != Some(&2) {
                    break;
                }
                (decompressed, comp_len)
            } else {
                if buf.get(comp_data_offset) != Some(&2) {
                    break;
                }
                let Ok(header_message_length) =
                    peek_i16_at(buf, comp_data_offset + HEADER_GAP + MESSAGE_LENGTH_OFF)
                else {
                    break;
                };
                let body = buf[comp_data_offset..].to_vec();
                (body, header_message_length as i64 + HEADER_GAP as i64)
            };

            decode_message(&body, &mut out);

            loc += message_length + 2;
        }

        out
    }
}

/// Dispatches one message body (BCAST_HEADER expected at `HEADER_GAP`) to
/// its transaction-code-specific decoder.
fn decode_message(body: &[u8], out: &mut Vec<Message>) {
    if body.len() < HEADER_GAP + BCAST_HEADER_LEN {
        return;
    }

    let header_start = HEADER_GAP;
    let trans_code = peek_i16_at(body, header_start).unwrap_or(0);
    let trans_code = if trans_code != 0 {
        trans_code
    } else {
        // MESSAGE_HEADER is byte-identical to BCAST_HEADER in this port (see
        // NSE_FO_Structures.h) -- re-reading the same offset can't actually
        // change the value. Kept anyway to mirror the original's fallback
        // and its warning, both verbatim.
        let fallback = peek_i16_at(body, header_start).unwrap_or(0);
        if fallback == 0 {
            eprintln!(
                "[nse_decode] warning: header did not parse under either BCAST_HEADER or \
                 MESSAGE_HEADER layout -- struct offsets likely need adjustment against a real \
                 captured packet."
            );
        }
        fallback
    };

    // BCSeqNo: validated at header+4 in real packets, not header+8 as
    // BCAST_HEADER's nominal field order implies -- read directly rather
    // than trusting the struct's own layout for this one field.
    let bc_seq_no = Cursor::at(body, header_start + 4).read_i32().unwrap_or(0);

    let payload_start = header_start + BCAST_HEADER_LEN;

    match trans_code {
        7208 => decode_mbp(body, payload_start, bc_seq_no, out),
        17208 => decode_enhanced_mbp(body, header_start, bc_seq_no, out),
        7202 => decode_ticker(body, payload_start, out),
        7305 => decode_security_range(body, payload_start, out),
        7220 => decode_exec_range(body, payload_start, out),
        // 7200 is the legacy combined MBO+MBP message. Recognized but not
        // decoded -- see NSE_FO_Structures.h for why the offsets past
        // MBO.Token can't be trusted from usage alone.
        _ => {}
    }
}

struct MbpInfoRaw {
    orders: i16,
    price: i32,
    qty: i32,
}

fn read_mbp_info(cursor: &mut Cursor) -> Result<MbpInfoRaw, DecodeError> {
    let _bb_buy_sell_flag = cursor.read_i16()?;
    let orders = cursor.read_i16()?;
    let price = cursor.read_i32()?;
    let qty = cursor.read_i32()?;
    Ok(MbpInfoRaw { orders, price, qty })
}

fn side_and_level(index: usize) -> (Side, u8) {
    if index < 5 {
        (Side::Bid, index as u8 + 1)
    } else {
        (Side::Ask, (index - 5) as u8 + 1)
    }
}

/// 7208 -- BCAST_ONLY_MBP: best-5 book plus LTP/OHLC/ATP/volume, one record
/// per token. A single datagram can carry several distinct-token records.
fn decode_mbp(body: &[u8], payload_start: usize, seq: i32, out: &mut Vec<Message>) {
    let mut cursor = Cursor::at(body, payload_start);
    let Ok(n) = cursor.read_i16() else { return };

    for _ in 0..n.clamp(0, 40) {
        if decode_one_mbp_record(&mut cursor, seq, out).is_err() {
            return; // truncated mid-record -- nothing further is trustworthy
        }
    }
}

fn decode_one_mbp_record(
    cursor: &mut Cursor,
    seq: i32,
    out: &mut Vec<Message>,
) -> Result<(), DecodeError> {
    let book_type = cursor.read_i16()?;
    let token = cursor.read_i32()?;

    let mut entries = Vec::with_capacity(10);
    for i in 0..10 {
        let info = read_mbp_info(cursor)?;
        let (side, level) = side_and_level(i);
        entries.push(BookLevel {
            side,
            level,
            price: info.price,
            qty: info.qty,
            orders: info.orders,
        });
    }

    let ltp = cursor.read_i32()?;
    let atp = cursor.read_i32()?;
    let close = cursor.read_i32()?;
    let ltq = cursor.read_i32()?;
    let ltt = cursor.read_i32()?;
    let open = cursor.read_i32()?;
    let high = cursor.read_i32()?;
    let low = cursor.read_i32()?;
    let tbq = cursor.read_f64()?;
    let tsq = cursor.read_f64()?;
    let total_traded_qty = cursor.read_i32()?;

    if book_type == 1 {
        out.push(Message::Mbp(MbpMessage {
            token,
            sequence: seq,
            ltp,
            atp,
            close,
            ltq,
            ltt,
            open,
            high,
            low,
            tbq: tbq as i64,
            tsq: tsq as i64,
            total_traded_qty,
            entries,
        }));
    }

    Ok(())
}

/// 17208 -- enhanced-lot MBP. Field offsets are reverse-engineered directly
/// from captured packets rather than trusted from the nominal
/// ENHNCD_MS_BCAST_ONLY_MBP struct, which doesn't match the real layout (see
/// NSE_FO_Structures.h). Only the book levels are high-confidence; only the
/// first record is decoded even when NoOfRecords > 1, matching the original.
fn decode_enhanced_mbp(body: &[u8], header_start: usize, seq: i32, out: &mut Vec<Message>) {
    const NO_OF_RECORDS_OFF: usize = 30;
    const TOKEN_OFF: usize = 32;
    const BOOKTYPE_OFF: usize = 36;
    const LEVELS_OFF: usize = 96;
    const LEVEL_STRIDE: usize = 16;

    let Ok(raw_n) = peek_i16_at(body, header_start + NO_OF_RECORDS_OFF) else {
        return;
    };
    let n = raw_n.clamp(0, 40);
    let n = if n > 1 {
        eprintln!(
            "[nse_decode] warning: 17208 message has NoOfRecords={n} but the multi-record \
             stride is unvalidated -- decoding only the first record."
        );
        1
    } else {
        n
    };

    for _ in 0..n {
        let Ok(book_type) = peek_i16_at(body, header_start + BOOKTYPE_OFF) else {
            return;
        };
        if book_type != 1 {
            continue;
        }

        let Ok(token) = Cursor::at(body, header_start + TOKEN_OFF).read_i32() else {
            return;
        };

        let mut entries = Vec::with_capacity(10);
        for j in 0..10 {
            let level_offset = header_start + LEVELS_OFF + j * LEVEL_STRIDE;
            let mut level_cursor = Cursor::at(body, level_offset);
            let (Ok(qty), Ok(price), Ok(orders)) = (
                level_cursor.read_i32(),
                level_cursor.read_i32(),
                level_cursor.read_i16(),
            ) else {
                return;
            };
            let (side, level) = side_and_level(j);
            entries.push(BookLevel {
                side,
                level,
                price,
                qty,
                orders,
            });
        }

        out.push(Message::EnhancedMbp(EnhancedMbpMessage {
            token,
            sequence: seq,
            entries,
        }));
    }
}

/// 7202 -- ticker / market index / open interest, one record per token.
fn decode_ticker(body: &[u8], payload_start: usize, out: &mut Vec<Message>) {
    let mut cursor = Cursor::at(body, payload_start);
    let Ok(n) = cursor.read_i16() else { return };

    for _ in 0..n.clamp(0, 40) {
        if decode_one_ticker(&mut cursor, out).is_err() {
            return;
        }
    }
}

fn decode_one_ticker(cursor: &mut Cursor, out: &mut Vec<Message>) -> Result<(), DecodeError> {
    let token = cursor.read_i32()?;
    let market_type = cursor.read_i16()?;
    let fill_price = cursor.read_i32()?;
    let fill_volume = cursor.read_i32()?;
    let open_interest = cursor.read_i32()?;
    let day_high_oi = cursor.read_i32()?;
    let day_low_oi = cursor.read_i32()?;

    out.push(Message::OiTicker(OiTickerMessage {
        token,
        market_type,
        fill_price,
        fill_volume,
        open_interest,
        day_high_oi,
        day_low_oi,
    }));
    Ok(())
}

/// 7305 -- security (price band) update. A single record, not a
/// NoOfRecords-prefixed array.
fn decode_security_range(body: &[u8], payload_start: usize, out: &mut Vec<Message>) {
    let mut cursor = Cursor::at(body, payload_start);
    let (Ok(token), Ok(low_price_range), Ok(high_price_range)) =
        (cursor.read_i32(), cursor.read_i32(), cursor.read_i32())
    else {
        return;
    };

    out.push(Message::SecurityRange(SecurityRangeMessage {
        token,
        low_price_range,
        high_price_range,
    }));
}

/// 7220 -- trade execution (price) range, one entry per affected token.
fn decode_exec_range(body: &[u8], payload_start: usize, out: &mut Vec<Message>) {
    let mut cursor = Cursor::at(body, payload_start);
    let Ok(n) = cursor.read_i32() else { return };

    let mut entries = Vec::new();
    for _ in 0..n.clamp(0, 80) {
        let (Ok(token), Ok(high_exec_band), Ok(low_exec_band)) =
            (cursor.read_i32(), cursor.read_i32(), cursor.read_i32())
        else {
            break;
        };
        entries.push(ExecRangeEntry {
            token,
            high_exec_band,
            low_exec_band,
        });
    }

    out.push(Message::ExecRange(ExecRangeMessage { entries }));
}
