/// One decoded NSE F&O broadcast record. A single UDP datagram can bundle
/// several of these (`decode_packet` returns a `Vec<Message>`), mirroring
/// `nse_fo_decoder.cpp`'s JSON-array-per-line output.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Mbp(MbpMessage),
    EnhancedMbp(EnhancedMbpMessage),
    OiTicker(OiTickerMessage),
    SecurityRange(SecurityRangeMessage),
    ExecRange(ExecRangeMessage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Bid,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BookLevel {
    pub side: Side,
    /// 1-based depth level, matching the wire format and the old JSON output.
    pub level: u8,
    pub price: i32,
    pub qty: i32,
    pub orders: i16,
}

/// Transaction code 7208 -- best-5 market-by-price book plus LTP/OHLC/ATP/volume.
#[derive(Debug, Clone, PartialEq)]
pub struct MbpMessage {
    pub token: i32,
    pub sequence: i32,
    pub ltp: i32,
    pub atp: i32,
    pub close: i32,
    pub ltq: i32,
    pub ltt: i32,
    pub open: i32,
    pub high: i32,
    pub low: i32,
    pub tbq: i64,
    pub tsq: i64,
    pub total_traded_qty: i32,
    pub entries: Vec<BookLevel>,
}

/// Transaction code 17208 -- enhanced-lot variant of 7208. Field offsets here
/// are reverse-engineered (see `decode_enhanced_mbp` in the C++ source): only
/// the book levels are trustworthy, LTP/OHLC/ATP/TBQ/TSQ/volume are always
/// zero because no sample packet has carried non-zero values for them yet,
/// and only the first record is decoded even when NoOfRecords > 1.
#[derive(Debug, Clone, PartialEq)]
pub struct EnhancedMbpMessage {
    pub token: i32,
    pub sequence: i32,
    pub entries: Vec<BookLevel>,
}

/// Transaction code 7202 -- ticker / market index / open interest.
#[derive(Debug, Clone, PartialEq)]
pub struct OiTickerMessage {
    pub token: i32,
    pub market_type: i16,
    pub fill_price: i32,
    pub fill_volume: i32,
    pub open_interest: i32,
    pub day_high_oi: i32,
    pub day_low_oi: i32,
}

/// Transaction code 7305 -- security (price band) update.
#[derive(Debug, Clone, PartialEq)]
pub struct SecurityRangeMessage {
    pub token: i32,
    pub low_price_range: i32,
    pub high_price_range: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecRangeEntry {
    pub token: i32,
    pub high_exec_band: i32,
    pub low_exec_band: i32,
}

/// Transaction code 7220 -- trade execution (price) range, one entry per token.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecRangeMessage {
    pub entries: Vec<ExecRangeEntry>,
}
