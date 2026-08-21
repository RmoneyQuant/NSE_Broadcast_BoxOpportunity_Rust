#include <iostream>
#include <sstream>
#include <string>
#include <vector>
#include <cstring>
#include <cstdint>
#include <algorithm>

#include <lzoconf.h>
#include <lzo1z.h>

#include "NSE_FO_Structures.h"

using namespace BROADCAST;


static std::vector<unsigned char> hex_to_bytes(const std::string& hex)
{
    std::vector<unsigned char> bytes;

    if (hex.size() % 2 != 0)
        return bytes;

    bytes.reserve(hex.size() / 2);

    for (size_t i = 0; i < hex.size(); i += 2)
    {
        std::string byte_string = hex.substr(i, 2);
        unsigned char byte = static_cast<unsigned char>(
            std::strtol(byte_string.c_str(), nullptr, 16)
        );
        bytes.push_back(byte);
    }

    return bytes;
}


// NSE broadcasts multi-byte fields in network (big-endian) byte order.
static inline int16_t bswap16(int16_t v)
{
    uint16_t u;
    std::memcpy(&u, &v, 2);
    u = static_cast<uint16_t>((u << 8) | (u >> 8));
    std::memcpy(&v, &u, 2);
    return v;
}

static inline int32_t bswap32(int32_t v)
{
    uint32_t u;
    std::memcpy(&u, &v, 4);
    u = ((u & 0x000000FFu) << 24) | ((u & 0x0000FF00u) << 8) |
        ((u & 0x00FF0000u) >> 8)  | ((u & 0xFF000000u) >> 24);
    std::memcpy(&v, &u, 4);
    return v;
}

template <typename T>
static inline T bswap_wide(T v)
{
    char* p = reinterpret_cast<char*>(&v);
    std::reverse(p, p + sizeof(T));
    return v;
}


static void decode_mbp(const BCAST_HEADER* hdr, int32_t seq, std::vector<std::string>& out)
{
    const auto* data = reinterpret_cast<const MS_BCAST_ONLY_MBP*>(hdr);
    int16_t n = bswap16(data->NoOfRecords);

    std::cerr << "[DEBUG] decode_mbp: raw NoOfRecords(host order)=" << data->NoOfRecords
              << " swapped=" << n << std::endl;

    if (n > 40) n = 40;   // clamp to the declared scratch view; a wire value
                          // this large indicates a corrupt/misaligned header

    for (int16_t i = 0; i < n; ++i)
    {
        std::cerr << "[DEBUG] decode_mbp: reading record i=" << i << " of n=" << n << std::endl;
        const MBP_RECORD& rec = data->MBP_DATA[i];

        std::cerr << "[DEBUG] decode_mbp: record read ok, BookType(raw)=" << rec.BookType << std::endl;

        if (bswap16(rec.BookType) != 1)
            continue;

        int32_t token = bswap32(rec.Token);

        double tbq = bswap_wide(rec.TotalBuyQuantity);
        double tsq = bswap_wide(rec.TotalSellQuantity);

        std::ostringstream js;
        js << "{"
           << "\"transaction_code\":7208,"
           << "\"msg_type\":\"MBP\","
           << "\"token\":" << token << ","
           << "\"sequence\":" << seq << ","
           << "\"ltp\":" << bswap32(rec.LastTradedPrice) << ","
           << "\"atp\":" << bswap32(rec.AverageTradePrice) << ","
           << "\"close\":" << bswap32(rec.ClosingPrice) << ","
           << "\"ltq\":" << bswap32(rec.LastTradeQuantity) << ","
           << "\"ltt\":" << bswap32(rec.LastTradeTime) << ","
           << "\"open\":" << bswap32(rec.OpenPrice) << ","
           << "\"high\":" << bswap32(rec.HighPrice) << ","
           << "\"low\":" << bswap32(rec.LowPrice) << ","
           << "\"tbq\":" << static_cast<int64_t>(tbq) << ","
           << "\"tsq\":" << static_cast<int64_t>(tsq) << ","
           << "\"total_traded_qty\":" << bswap32(rec.VolumeTradedToday) << ","
           << "\"entries\":[";

        for (int j = 0; j < 5; ++j)
        {
            const MBP_INFO& lvl = rec.Levels[j];
            if (j) js << ",";
            js << "{\"md_entry_type\":0,\"level\":" << (j + 1)
               << ",\"price\":" << bswap32(lvl.Price)
               << ",\"qty\":" << bswap32(lvl.Quantity)
               << ",\"orders\":" << bswap16(lvl.NumberOfOrders) << "}";
        }

        for (int j = 0; j < 5; ++j)
        {
            const MBP_INFO& lvl = rec.Levels[5 + j];
            js << ",{\"md_entry_type\":1,\"level\":" << (j + 1)
               << ",\"price\":" << bswap32(lvl.Price)
               << ",\"qty\":" << bswap32(lvl.Quantity)
               << ",\"orders\":" << bswap16(lvl.NumberOfOrders) << "}";
        }

        js << "]}";
        out.push_back(js.str());
    }
}


static void decode_enhanced_mbp(const BCAST_HEADER* hdr, int32_t seq, std::vector<std::string>& out)
{
    // REAL WIRE OFFSETS -- reverse-engineered 2026-08-18 from real captured
    // 17208 packets, cross-validated on 2 independent samples (distinct
    // BCSeqNo/Token/price/qty values in each, all landing on plausible,
    // consistently-positioned fields). ENHNCD_MS_BCAST_ONLY_MBP in
    // NSE_FO_Structures.h does NOT match the real layout -- offsets below
    // are read directly from raw bytes relative to `hdr` (which points at
    // TransCode) instead of trusting that struct.
    //
    // CONFIDENCE: HIGH  -- NoOfRecords, Token, BookType-flag, and each
    //   level's Quantity/Price/NumberOfOrders (16 bytes/level, 10 levels).
    // CONFIDENCE: UNKNOWN -- LTP/ATP/OHLC/TBQ/TSQ/total_traded_qty. Every
    //   sample seen so far had these as all-zero bytes, so their real
    //   offsets could not be inferred from the data. Emitted as 0 rather
    //   than guessed; needs a packet with an actual last-traded price to
    //   pin these down.
    // CONFIDENCE: UNVALIDATED -- the record-to-record stride for
    //   NoOfRecords > 1. Only NoOfRecords==1 has been observed so far, so
    //   only the first record is decoded; additional records are skipped
    //   with a warning rather than guessed at.
    const auto* base = reinterpret_cast<const unsigned char*>(hdr);

    int16_t rawNoOfRecords;
    std::memcpy(&rawNoOfRecords, base + 30, 2);
    int16_t n = bswap16(rawNoOfRecords);

    if (n > 40) n = 40;
    if (n < 0) n = 0;

    if (n > 1)
    {
        std::cerr << "[nse_fo_decoder] warning: 17208 message has NoOfRecords=" << n
                     << " but the multi-record stride is unvalidated -- decoding only "
                     "the first record." << std::endl;
        n = 1;
    }

    const int TOKEN_OFF = 32;
    const int BOOKTYPE_OFF = 36;
    const int LEVELS_OFF = 96;
    const int LEVEL_STRIDE = 16;

    for (int16_t i = 0; i < n; ++i)
    {
        int16_t rawBookType;
        std::memcpy(&rawBookType, base + BOOKTYPE_OFF, 2);
        if (bswap16(rawBookType) != 1)
            continue;

        int32_t rawToken;
        std::memcpy(&rawToken, base + TOKEN_OFF, 4);
        int32_t token = bswap32(rawToken);

        std::ostringstream js;
        js << "{"
           << "\"transaction_code\":17208,"
           << "\"msg_type\":\"MBP\","
           << "\"token\":" << token << ","
           << "\"sequence\":" << seq << ","
           << "\"ltp\":0,\"atp\":0,\"close\":0,\"ltq\":0,\"ltt\":0,"
           << "\"open\":0,\"high\":0,\"low\":0,\"tbq\":0,\"tsq\":0,\"total_traded_qty\":0,"
           << "\"entries\":[";

        for (int j = 0; j < 10; ++j)
        {
            const unsigned char* lvl = base + LEVELS_OFF + j * LEVEL_STRIDE;

            int32_t rawQty, rawPrice;
            int16_t rawOrders;
            std::memcpy(&rawQty, lvl + 0, 4);
            std::memcpy(&rawPrice, lvl + 4, 4);
            std::memcpy(&rawOrders, lvl + 8, 2);

            int md_entry_type = (j < 5) ? 0 : 1;
            int level = (j < 5) ? (j + 1) : (j - 5 + 1);

            if (j) js << ",";
            js << "{\"md_entry_type\":" << md_entry_type << ",\"level\":" << level
               << ",\"price\":" << bswap32(rawPrice)
               << ",\"qty\":" << bswap32(rawQty)
               << ",\"orders\":" << bswap16(rawOrders) << "}";
        }

        js << "]}";
        out.push_back(js.str());
    }
}


static void decode_ticker(const BCAST_HEADER* hdr, std::vector<std::string>& out)
{
    const auto* data = reinterpret_cast<const BCAST_TICKER_AND_MKT_INDEX*>(hdr);
    int16_t n = bswap16(data->NoOfRecords);

    if (n > 40) n = 40;

    for (int16_t i = 0; i < n; ++i)
    {
        const TICKER_DATA& rec = data->Data[i];

        std::ostringstream js;
        js << "{"
           << "\"transaction_code\":7202,"
           << "\"msg_type\":\"OI_TICKER\","
           << "\"token\":" << bswap32(rec.Token) << ","
           << "\"market_type\":" << bswap16(rec.MktType) << ","
           << "\"fill_price\":" << bswap32(rec.FillPrice) << ","
           << "\"fill_volume\":" << bswap32(rec.FillVolume) << ","
           << "\"open_interest\":" << bswap32(rec.OI) << ","
           << "\"day_high_oi\":" << bswap32(rec.DayHiOI) << ","
           << "\"day_low_oi\":" << bswap32(rec.DayLoOI)
           << "}";

        out.push_back(js.str());
    }
}


static void decode_security_range(const BCAST_HEADER* hdr, std::vector<std::string>& out)
{
    const auto* data = reinterpret_cast<const MS_SECURITY_UPDATE_INFO*>(hdr);

    std::ostringstream js;
    js << "{"
       << "\"transaction_code\":7305,"
       << "\"msg_type\":\"SECURITY_RANGE\","
       << "\"token\":" << bswap32(data->Token) << ","
       << "\"low_price_range\":" << bswap32(data->LowPriceRange) << ","
       << "\"high_price_range\":" << bswap32(data->HighPriceRange)
       << "}";

    out.push_back(js.str());
}


static void decode_exec_range(const BCAST_HEADER* hdr, std::vector<std::string>& out)
{
    const auto* data = reinterpret_cast<const MS_BCAST_TRADE_EXECUTION_RANGE*>(hdr);
    int32_t n = bswap32(data->TER.MsgCount);

    if (n > 80) n = 80;

    std::ostringstream js;
    js << "{\"transaction_code\":7220,\"msg_type\":\"EXEC_RANGE\",\"entries\":[";

    for (int32_t i = 0; i < n; ++i)
    {
        const TRADE_EXEC_RANGE_ENTRY& d = data->TER.Detail[i];
        if (i) js << ",";
        js << "{\"token\":" << bswap32(d.TokenNumber)
           << ",\"high_exec_band\":" << bswap32(d.HighExecBand)
           << ",\"low_exec_band\":" << bswap32(d.LowExecBand) << "}";
    }

    js << "]}";
    out.push_back(js.str());
}


// Walks one UDP datagram's worth of bundled/compressed NSE messages and
// appends one JSON object per decoded record to `out`. Mirrors the framing
// and dispatch logic in D:\Socket\Socket.cpp (mcx_Socket::BroadCastRead_NSE_FO).
static void decode_packet(const std::vector<unsigned char>& buf, std::vector<std::string>& out)
{
    // OUTER_PREFIX: reverse-engineered 2026-08-18 from real captured
    // packets. Every real datagram observed so far carries 2 extra bytes
    // (value differs per packet -- not part of NSE's own NNF structure,
    // likely something a local feed-handler/gateway prepends) before
    // BcastPackData actually begins. Neither Socket.cpp nor the original
    // port accounted for this. See NSE_FO_Structures.h.
    const size_t OUTER_PREFIX = 2;

    if (buf.size() < OUTER_PREFIX + 2)
        return;

    const auto* broadcastData = reinterpret_cast<const BcastPackData*>(buf.data() + OUTER_PREFIX);
    int16_t noPackets = bswap16(broadcastData->iNoPackets);

    if (noPackets < 0)
        return;

    int32_t loc = 0;
    const int32_t bufLen = static_cast<int32_t>(buf.size() - OUTER_PREFIX);

    for (int32_t numpack = 0; numpack < noPackets; ++numpack)
    {
        if (loc + 2 > bufLen - 2)
            break;

        const auto* compressed = reinterpret_cast<const BcastCmpPacket*>(broadcastData->cPackData + loc);
        int16_t compLen = bswap16(compressed->iCompLen);

        static unsigned char scratch[2048];
        const unsigned char* body = nullptr;
        int32_t messageLength = 0;

        if (compLen > 0)
        {
            if (loc + 2 + compLen > bufLen)
                break;

            lzo_uint newLen = 0;
            int ret = lzo1z_decompress(
                reinterpret_cast<const unsigned char*>(compressed->cCompData),
                compLen, scratch, &newLen, nullptr);

            if (ret != LZO_E_OK || scratch[0] != 2)
                break;

            messageLength = compLen;
            body = scratch;
        }
        else
        {
            if (static_cast<unsigned char>(compressed->cCompData[0]) != 2)
                break;

            // HEADER_GAP: reverse-engineered 2026-08-18 from real captured
            // packets (cross-validated on 2 independent samples). The real
            // gap from the marker byte to BCAST_HEADER/TransCode is 18
            // bytes, not the 8 the original reference code (Socket.cpp)
            // assumes -- there's an extra ~10-byte field (looks like a
            // local timestamp) in between that neither Socket.cpp nor this
            // port originally accounted for. See NSE_FO_Structures.h.
            const int HEADER_GAP = 18;

            const auto* h = reinterpret_cast<const BCAST_HEADER*>(compressed->cCompData + HEADER_GAP);
            messageLength = bswap16(h->MessageLength) + HEADER_GAP;
            body = reinterpret_cast<const unsigned char*>(compressed->cCompData);
        }

        const int HEADER_GAP = 18;
        const auto* bHeader = reinterpret_cast<const BCAST_HEADER*>(body + HEADER_GAP);
        int16_t transactionCode = bswap16(bHeader->TransCode);

        if (transactionCode == 0)
        {
            const auto* mHeader = reinterpret_cast<const MESSAGE_HEADER*>(body + HEADER_GAP);
            transactionCode = bswap16(mHeader->TransactionCode);

            if (transactionCode == 0)
            {
                std::cerr << "[nse_fo_decoder] warning: header did not parse under either "
                             "BCAST_HEADER or MESSAGE_HEADER layout -- struct offsets likely "
                             "need adjustment against a real captured packet." << std::endl;
            }
        }

        // BCSeqNo: validated at header+4 in real packets, NOT header+8 as
        // BCAST_HEADER's field order implies (TransCode+LogTime+AlphaChar
        // precede it in the struct, but that layout doesn't match the real
        // wire data -- see NSE_FO_Structures.h). Read the raw bytes
        // directly rather than trusting BCAST_HEADER::BCSeqNo's position.
        const auto* bHeaderBytes = reinterpret_cast<const unsigned char*>(bHeader);
        int32_t bcSeqNoRaw;
        std::memcpy(&bcSeqNoRaw, bHeaderBytes + 4, 4);
        int32_t bcSeqNo = bswap32(bcSeqNoRaw);

        switch (transactionCode)
        {
            case 7208:  decode_mbp(bHeader, bcSeqNo, out);            break;
            case 17208: decode_enhanced_mbp(bHeader, bcSeqNo, out);   break;
            case 7202:  decode_ticker(bHeader, out);                 break;
            case 7305:  decode_security_range(bHeader, out);         break;
            case 7220:  decode_exec_range(bHeader, out);              break;
            case 7200:
                // Legacy combined MBO+MBP. Deliberately not decoded --
                // see NSE_FO_Structures.h for why the offsets past
                // MBO.Token can't be trusted from usage alone.
                break;
            default:
                break;
        }

        loc += messageLength + 2;
    }
}


int main()
{
    std::string hex_line;

    while (std::getline(std::cin, hex_line))
    {
        if (hex_line.empty())
            continue;

        std::vector<unsigned char> packet;
        try
        {
            packet = hex_to_bytes(hex_line);
        }
        catch (const std::exception& e)
        {
            std::cerr << "[nse_fo_decoder] hex_to_bytes threw: " << e.what() << std::endl;
            continue;
        }

        if (packet.empty())
        {
            std::cout << "[]" << std::endl;
            continue;
        }

        std::vector<std::string> messages;
        try
        {
            decode_packet(packet, messages);
        }
        catch (const std::exception& e)
        {
            std::cerr << "[nse_fo_decoder] decode_packet threw: " << e.what() << std::endl;
            continue;
        }

        std::cout << "[";
        for (size_t i = 0; i < messages.size(); ++i)
        {
            if (i) std::cout << ",";
            std::cout << messages[i];
        }
        std::cout << "]" << std::endl;
    }

    return 0;
}
