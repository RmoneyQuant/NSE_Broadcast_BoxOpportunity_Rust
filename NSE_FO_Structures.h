#ifndef NSE_FO_STRUCTURES_H
#define NSE_FO_STRUCTURES_H

#include <cstdint>

// ============================================================================
// NSE F&O broadcast (NNF) wire structures.
//
// Confidence per section is marked explicitly. The 7208/MS_BCAST_ONLY_MBP
// shape is cross-checked against TWO independent call sites already running
// in production (D:\Socket\Socket.cpp: BroadCastRead_NSE_FO() and the older
// file-replay test harness ~line 3789) -- field names, types and access
// order agree between them. Everything else is inferred from a single call
// site's field usage plus the well-known public NSE structure layout, and
// has NOT been independently verified. Before trusting this in production,
// validate against a real captured packet (the decoder emits a stderr
// warning if the header doesn't parse as expected).
// ============================================================================

namespace BROADCAST {

#pragma pack(push, 1)

// ---------------------------------------------------------------------------
// Outer packet framing. CONFIDENCE: HIGH (matches Socket.cpp's loop/advance
// arithmetic exactly: loc += MessageLength + 2, i.e. iCompLen's own 2 bytes
// plus the message bytes it prefixes).
// ---------------------------------------------------------------------------

// One UDP datagram. iNoPackets back-to-back BcastCmpPacket entries follow in
// cPackData. cPackData is an oversized scratch view, not a real allocation --
// always bound reads by the actual datagram size, never by this array size.
struct BcastPackData
{
    int16_t iNoPackets;
    char    cPackData[65000];
};

// One bundled message. iCompLen == 0 means cCompData is raw (uncompressed);
// iCompLen > 0 means cCompData holds that many LZO1Z-compressed bytes which
// decompress to the same layout: [8 framing bytes][BCAST_HEADER or
// MESSAGE_HEADER][payload]. Byte 0 of the (decompressed or raw) body is a
// marker that must equal 2 for the message to be treated as valid.
struct BcastCmpPacket
{
    int16_t iCompLen;
    char    cCompData[65000];
};

// ---------------------------------------------------------------------------
// Message header, found at offset +8 inside the decompressed/raw body.
// CONFIDENCE: MEDIUM. TransCode, MessageLength, LogTime and BCSeqNo are
// confirmed by exact field usage in Socket.cpp. The AlphaChar[2] filler
// between LogTime and BCSeqNo is the standard published NSE layout but is
// NOT independently confirmed against your feed -- if decoded values look
// wrong, this is the first thing to re-check against a real packet.
// ---------------------------------------------------------------------------

struct BCAST_HEADER
{
    int16_t TransCode;
    int32_t LogTime;
    char    AlphaChar[2];
    int32_t BCSeqNo;
    int16_t MessageLength;
};

// Fallback layout tried when BCAST_HEADER reads TransCode == 0.
// CONFIDENCE: MEDIUM, same caveat as BCAST_HEADER.
struct MESSAGE_HEADER
{
    int16_t TransactionCode;
    int32_t LogTime;
    char    AlphaChar[2];
    int32_t BCSeqNo;
    int16_t MessageLength;
};

// ---------------------------------------------------------------------------
// 7208 -- BCAST_ONLY_MBP. CONFIDENCE: HIGH (cross-checked against two
// independent call sites in Socket.cpp).
// ---------------------------------------------------------------------------

struct MBP_INFO
{
    int16_t BbBuySellFlag;
    int16_t NumberOfOrders;
    int32_t Price;
    int32_t Quantity;
};

struct MBP_RECORD
{
    int16_t  BookType;              // == 1 for the regular/normal-lot book
    int32_t  Token;
    MBP_INFO Levels[10];            // [0..4] = buy best->worst, [5..9] = sell best->worst
    int32_t  LastTradedPrice;
    int32_t  AverageTradePrice;
    int32_t  ClosingPrice;
    int32_t  LastTradeQuantity;
    int32_t  LastTradeTime;
    int32_t  OpenPrice;
    int32_t  HighPrice;
    int32_t  LowPrice;
    double   TotalBuyQuantity;
    double   TotalSellQuantity;
    int32_t  VolumeTradedToday;
};

struct MS_BCAST_ONLY_MBP
{
    BCAST_HEADER Header;
    int16_t      NoOfRecords;
    MBP_RECORD   MBP_DATA[40];      // oversized scratch view; bound loops by NoOfRecords
};

// ---------------------------------------------------------------------------
// 17208 -- Enhanced BCAST_ONLY_MBP. CONFIDENCE: MEDIUM. Same shape as 7208
// except Quantity and VolumeTradedToday are wider (Socket.cpp byte-swaps
// them with the generic swap_endian<T>() template rather than the 4-byte
// _byteswap_NSE_LONG helper, which only makes sense if they're 8-byte
// fields). The exact width (int64 vs double) is inferred, not confirmed --
// validate against a real 17208 packet before trusting this path.
// ---------------------------------------------------------------------------

struct ENHNCD_MBP_INFO
{
    int16_t BbBuySellFlag;
    int16_t NumberOfOrders;
    int32_t Price;
    int64_t Quantity;
};

struct ENHNCD_MBP_RECORD
{
    int16_t         BookType;
    int32_t         Token;
    ENHNCD_MBP_INFO Levels[10];
    int32_t         LastTradedPrice;
    int32_t         AverageTradePrice;
    int32_t         ClosingPrice;
    int32_t         LastTradeQuantity;
    int32_t         LastTradeTime;
    int32_t         OpenPrice;
    int32_t         HighPrice;
    int32_t         LowPrice;
    double          TotalBuyQuantity;
    double          TotalSellQuantity;
    int64_t         VolumeTradedToday;
};

struct ENHNCD_MS_BCAST_ONLY_MBP
{
    BCAST_HEADER      Header;
    int16_t           NoOfRecords;
    ENHNCD_MBP_RECORD ENHNCD_MBP_DATA[40];
};

// ---------------------------------------------------------------------------
// 7305 -- Security (price band) update. CONFIDENCE: MEDIUM. Only the three
// fields Socket.cpp actually reads are represented; the real NSE message
// likely carries more trailing fields (tick size, freeze qty, board lot...)
// not reproduced here. Do not rely on sizeof(MS_SECURITY_UPDATE_INFO) to
// mean anything -- only the three named fields are trustworthy.
// ---------------------------------------------------------------------------

struct MS_SECURITY_UPDATE_INFO
{
    BCAST_HEADER Header;
    int32_t      Token;
    int32_t      LowPriceRange;
    int32_t      HighPriceRange;
};

// ---------------------------------------------------------------------------
// 7202 -- Ticker & market index / open interest. CONFIDENCE: MEDIUM.
// ---------------------------------------------------------------------------

struct TICKER_DATA
{
    int32_t Token;
    int16_t MktType;
    int32_t FillPrice;
    int32_t FillVolume;
    int32_t OI;
    int32_t DayHiOI;
    int32_t DayLoOI;
};

struct BCAST_TICKER_AND_MKT_INDEX
{
    BCAST_HEADER Header;
    int16_t      NoOfRecords;
    TICKER_DATA  Data[40];
};

// ---------------------------------------------------------------------------
// 7220 -- Trade execution range (price band per token). CONFIDENCE: MEDIUM.
// ---------------------------------------------------------------------------

struct TRADE_EXEC_RANGE_ENTRY
{
    int32_t TokenNumber;
    int32_t HighExecBand;
    int32_t LowExecBand;
};

struct TRADE_EXECUTION_RANGE_DATA
{
    int32_t                MsgCount;
    TRADE_EXEC_RANGE_ENTRY Detail[80];
};

struct MS_BCAST_TRADE_EXECUTION_RANGE
{
    BCAST_HEADER                Header;
    TRADE_EXECUTION_RANGE_DATA  TER;
};

// ---------------------------------------------------------------------------
// 7200 -- legacy combined MBO+MBP. CONFIDENCE: LOW. Socket.cpp only reads
// MBO.Token before jumping straight to a flat MBP[10] array, but the real
// MBO record almost certainly carries more fields between Token and the
// MBP array (book counts, total qtys) that this reference code just
// happens not to read -- meaning the true offset of MBP[10] is NOT
// determinable from usage alone. Deliberately NOT decoded (see decoder's
// case 7200: it is recognized but skipped rather than guessed).
// ---------------------------------------------------------------------------

#pragma pack(pop)

} // namespace BROADCAST

#endif // NSE_FO_STRUCTURES_H
