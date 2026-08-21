//! Native Rust port of `nse_fo_decoder.cpp` -- decodes raw NSE F&O broadcast
//! (NNF) UDP datagrams without shelling out to the old `.exe`. See
//! `PacketDecoder` for the entry point.

mod cursor;
mod error;
mod lzo;
mod message;
mod packet;

pub use error::DecodeError;
pub use message::{
    BookLevel, EnhancedMbpMessage, ExecRangeEntry, ExecRangeMessage, MbpMessage, Message,
    OiTickerMessage, SecurityRangeMessage, Side,
};
pub use packet::PacketDecoder;

#[cfg(test)]
mod tests {
    use super::*;

    // The first five lines are the packets nse_fo.py/nse_fo_decoder.cpp
    // actually ship with. Every one of them decodes to zero messages against
    // the real exe (verified by piping each line into nse_fo_decoder_dyn.exe
    // directly) -- their outer BcastPackData.iNoPackets field is 0, so the
    // bundled-message loop never runs. These are regression tests against
    // that observed ground truth, not synthetic data, so they can't exercise
    // the per-message field parsing below.
    //
    // Lines 6-46 are different: 42 hand-built 7208 MBP packets (see
    // examples/build_synthetic_mbp_packet.rs) covering 7 real NIFTY strikes'
    // CE/PE pairs across 3 real expiries, added so nse_decode/nse_fo_verify
    // have real, *varied* non-empty messages to print, and so
    // box_scanner_live's replay mode has enough strikes/expiries to
    // exercise the ATM-window + strike-gap-band filter with the market
    // closed. Prices are the generator's own linear synthetic model
    // (re-derived below, not independently cross-checked against the real
    // exe the way the original 4-line version was) -- the byte-layout
    // construction itself is unchanged and still exercised by
    // `decodes_a_synthetic_mbp_packet` below.
    const FIXTURES: &str = include_str!("../../../fixtures/sample_packets.hex");
    const EMPTY_FIXTURE_LINE_COUNT: usize = 5;

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn real_world_fixture_packets_decode_to_no_messages() {
        let decoder = PacketDecoder::new();
        let lines: Vec<&str> = FIXTURES.lines().filter(|l| !l.trim().is_empty()).collect();
        for line in &lines[..EMPTY_FIXTURE_LINE_COUNT] {
            let packet = hex_to_bytes(line.trim());
            assert_eq!(
                decoder.decode_packet(&packet),
                Vec::new(),
                "expected no messages for fixture line (matches the reference exe's output)"
            );
        }
    }

    /// Same linear synthetic pricing formula as
    /// `examples/build_synthetic_mbp_packet.rs`'s `synthetic_mid_paise` --
    /// duplicated here (rather than shared, since examples aren't a library
    /// dependency) so every one of the 42 option lines can be checked
    /// against its expected price, not just a hand-picked few.
    fn synthetic_mid_paise(strike: i32, is_call: bool) -> i32 {
        if is_call {
            (26_500 - strike).max(50) * 10
        } else {
            (strike - 21_500).max(50) * 10
        }
    }

    #[test]
    fn synthetic_fixture_lines_decode_to_the_expected_mbp_messages() {
        let lines: Vec<&str> = FIXTURES.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            5 + 42 + 1,
            "expected 5 real-world + 42 synthetic option + 1 synthetic future fixture lines"
        );

        // (token, strike, is_call) for every one of the 42 option lines, in
        // the exact order examples/build_synthetic_mbp_packet.rs emits
        // them: 3 expiries x 7 strikes x [CE, PE].
        const STRIKES: [i32; 7] = [23000, 23500, 24000, 24500, 25000, 25500, 26000];
        const EXPIRY_TOKENS: [[(i32, i32); 7]; 3] = [
            [(61428, 61429), (61499, 61500), (61593, 61604), (61734, 61771), (61897, 61898), (61975, 61976), (62038, 62039)],
            [(73903, 65899), (73985, 73994), (65900, 65901), (74241, 74242), (74365, 65903), (55286, 55287), (65904, 65905)],
            [(51352, 51353), (51372, 51373), (51392, 51393), (51418, 51419), (51440, 51441), (51468, 51469), (51490, 51491)],
        ];

        let decoder = PacketDecoder::new();
        let mut seq = 9001;
        let mut line = EMPTY_FIXTURE_LINE_COUNT;
        for expiry_tokens in EXPIRY_TOKENS {
            for (strike, (ce_token, pe_token)) in STRIKES.into_iter().zip(expiry_tokens) {
                for (token, is_call) in [(ce_token, true), (pe_token, false)] {
                    let packet = hex_to_bytes(lines[line].trim());
                    let messages = decoder.decode_packet(&packet);
                    assert_eq!(messages.len(), 1, "fixture line {}", line + 1);
                    let Message::Mbp(mbp) = &messages[0] else {
                        panic!("fixture line {}: expected an Mbp message, got {:?}", line + 1, messages[0]);
                    };

                    assert_eq!(mbp.token, token, "fixture line {}", line + 1);
                    assert_eq!(mbp.sequence, seq, "fixture line {}", line + 1);
                    assert_eq!(mbp.ltp, synthetic_mid_paise(strike, is_call), "fixture line {}", line + 1);
                    assert_eq!(mbp.entries.len(), 10, "fixture line {}", line + 1);

                    seq += 1;
                    line += 1;
                }
            }
        }

        // Line 48: the NIFTY FUTIDX tick (token 58072, nearest 25-Aug-2026
        // expiry) used as the ATM-derivation reference -- see
        // examples/build_synthetic_mbp_packet.rs's NIFTY_FUTURE_TOKEN.
        let packet = hex_to_bytes(lines[line].trim());
        let messages = decoder.decode_packet(&packet);
        assert_eq!(messages.len(), 1, "fixture line {}", line + 1);
        let Message::Mbp(mbp) = &messages[0] else {
            panic!("fixture line {}: expected an Mbp message, got {:?}", line + 1, messages[0]);
        };
        assert_eq!(mbp.token, 58072);
        assert_eq!(mbp.sequence, seq);
        assert_eq!(mbp.ltp, 2_448_735);
    }

    /// Builds a minimal, hand-crafted datagram carrying exactly one 7208 MBP
    /// record, uncompressed, and checks every field round-trips. This is the
    /// P2 curriculum's suggested exercise ("parse a fixed layout out of a
    /// &[u8] with from_be_bytes, return a Result instead of panicking")
    /// applied directly to the real wire format, since the shipped fixtures
    /// don't exercise this path at all.
    #[test]
    fn decodes_a_synthetic_mbp_packet() {
        let mut buf = Vec::new();

        // 2-byte local prefix ahead of BcastPackData (unrelated content).
        buf.extend_from_slice(&[0xAA, 0xBB]);

        // BcastPackData: iNoPackets = 1.
        buf.extend_from_slice(&1i16.to_be_bytes());

        // BcastCmpPacket for that one bundled message: iCompLen = 0 (raw).
        let comp_len_pos = buf.len();
        buf.extend_from_slice(&0i16.to_be_bytes());

        let body_start = buf.len();
        buf.push(2); // marker byte, must equal 2

        // Pad out to HEADER_GAP (18) with filler bytes not read by this port.
        while buf.len() - body_start < 18 {
            buf.push(0);
        }

        // BCAST_HEADER (14 bytes): TransCode=7208, LogTime=0, AlphaChar=[0,0],
        // BCSeqNo (read from +4, i.e. overlapping LogTime's bytes) = 42,
        // MessageLength = filled in once known.
        let header_start = buf.len();
        buf.extend_from_slice(&7208i16.to_be_bytes()); // [0..2) TransCode
        buf.extend_from_slice(&[0, 0]); // [2..4) filler (nominal LogTime, unread)
        buf.extend_from_slice(&42i32.to_be_bytes()); // [4..8) BCSeqNo, read raw at +4
        buf.extend_from_slice(&[0, 0, 0, 0]); // [8..12) filler, unread
        let message_length_pos = buf.len();
        buf.extend_from_slice(&0i16.to_be_bytes()); // [12..14) MessageLength, patched below

        // NoOfRecords, right after the 14-byte header.
        buf.extend_from_slice(&1i16.to_be_bytes());

        // One MBP_RECORD: BookType=1, Token=12345, 10 empty levels, then
        // LTP/ATP/close/ltq/ltt/open/high/low/tbq/tsq/volume.
        buf.extend_from_slice(&1i16.to_be_bytes()); // BookType
        buf.extend_from_slice(&12345i32.to_be_bytes()); // Token
        for level in 0..10i32 {
            buf.extend_from_slice(&0i16.to_be_bytes()); // BbBuySellFlag (unused)
            buf.extend_from_slice(&(level as i16 + 1).to_be_bytes()); // NumberOfOrders
            buf.extend_from_slice(&((level + 1) * 100).to_be_bytes()); // Price
            buf.extend_from_slice(&((level + 1) * 10).to_be_bytes()); // Quantity
        }
        buf.extend_from_slice(&25000i32.to_be_bytes()); // LastTradedPrice
        buf.extend_from_slice(&24950i32.to_be_bytes()); // AverageTradePrice
        buf.extend_from_slice(&24900i32.to_be_bytes()); // ClosingPrice
        buf.extend_from_slice(&50i32.to_be_bytes()); // LastTradeQuantity
        buf.extend_from_slice(&0i32.to_be_bytes()); // LastTradeTime
        buf.extend_from_slice(&24800i32.to_be_bytes()); // OpenPrice
        buf.extend_from_slice(&25100i32.to_be_bytes()); // HighPrice
        buf.extend_from_slice(&24700i32.to_be_bytes()); // LowPrice
        buf.extend_from_slice(&1500.0f64.to_be_bytes()); // TotalBuyQuantity
        buf.extend_from_slice(&1200.0f64.to_be_bytes()); // TotalSellQuantity
        buf.extend_from_slice(&987654i32.to_be_bytes()); // VolumeTradedToday

        // MessageLength = bytes from the marker byte through the record we
        // just wrote, minus HEADER_GAP (matches how the decoder derives it).
        let message_length = (buf.len() - header_start) as i16;
        buf[message_length_pos..message_length_pos + 2]
            .copy_from_slice(&message_length.to_be_bytes());

        // iCompLen stays 0 (raw path); nothing to patch there.
        let _ = comp_len_pos;

        let messages = PacketDecoder::new().decode_packet(&buf);

        assert_eq!(messages.len(), 1);
        let Message::Mbp(mbp) = &messages[0] else {
            panic!("expected an Mbp message, got {:?}", messages[0]);
        };
        assert_eq!(mbp.token, 12345);
        assert_eq!(mbp.sequence, 42);
        assert_eq!(mbp.ltp, 25000);
        assert_eq!(mbp.total_traded_qty, 987654);
        assert_eq!(mbp.tbq, 1500);
        assert_eq!(mbp.tsq, 1200);
        assert_eq!(mbp.entries.len(), 10);
        assert_eq!(mbp.entries[0].side, Side::Bid);
        assert_eq!(mbp.entries[0].level, 1);
        assert_eq!(mbp.entries[0].price, 100);
        assert_eq!(mbp.entries[5].side, Side::Ask);
        assert_eq!(mbp.entries[5].level, 1);
    }

    #[test]
    fn book_type_other_than_one_is_skipped() {
        // Same shape as the MBP test above, but BookType = 2 (not the
        // regular/normal-lot book) -- should decode zero messages.
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0xAA, 0xBB]);
        buf.extend_from_slice(&1i16.to_be_bytes());
        buf.extend_from_slice(&0i16.to_be_bytes());

        let body_start = buf.len();
        buf.push(2);
        while buf.len() - body_start < 18 {
            buf.push(0);
        }

        let header_start = buf.len();
        buf.extend_from_slice(&7208i16.to_be_bytes()); // [0..2) TransCode
        buf.extend_from_slice(&[0, 0]); // [2..4) filler
        buf.extend_from_slice(&1i32.to_be_bytes()); // [4..8) BCSeqNo
        buf.extend_from_slice(&[0, 0, 0, 0]); // [8..12) filler
        let message_length_pos = buf.len();
        buf.extend_from_slice(&0i16.to_be_bytes()); // [12..14) MessageLength

        buf.extend_from_slice(&1i16.to_be_bytes()); // NoOfRecords
        buf.extend_from_slice(&2i16.to_be_bytes()); // BookType = 2
        buf.extend_from_slice(&1i32.to_be_bytes()); // Token
        for _ in 0..10 {
            buf.extend_from_slice(&[0u8; 12]); // 10 zeroed MBP_INFO levels
        }
        buf.extend_from_slice(&[0u8; 8 * 4]); // ltp..lowPrice
        buf.extend_from_slice(&0.0f64.to_be_bytes()); // tbq
        buf.extend_from_slice(&0.0f64.to_be_bytes()); // tsq
        buf.extend_from_slice(&0i32.to_be_bytes()); // volume

        let message_length = (buf.len() - header_start) as i16;
        buf[message_length_pos..message_length_pos + 2]
            .copy_from_slice(&message_length.to_be_bytes());

        assert_eq!(PacketDecoder::new().decode_packet(&buf), Vec::new());
    }

    /// Smoke-tests the liblzo2 FFI binding itself: loads the real DLL and
    /// confirms the `lzo1z_decompress` symbol resolves. Ignored by default
    /// since it needs a path that only exists on this machine -- none of the
    /// fixtures or synthetic packets above exercise the compressed branch,
    /// so `cargo test` stays green without it. Run explicitly with
    /// `cargo test -p nse_decode -- --ignored` on a machine that has the DLL.
    #[test]
    #[ignore = "requires liblzo2-2.dll from the machine's decoder folder"]
    fn loads_real_liblzo2() {
        PacketDecoder::with_lzo(r"C:\Users\rishav.raj\Desktop\decoder\liblzo2-2.dll")
            .expect("liblzo2-2.dll should load and export lzo1z_decompress");
    }
}
