# /// script
# requires-python = ">=3.9"
# dependencies = [
#     "psycopg2-binary>=2.9.12",
# ]
# ///
import errno
import socket
import struct
import subprocess
import json
import os
from dataclasses import dataclass, field
from datetime import datetime
import csv

try:
    import psycopg2
except ImportError:
    psycopg2 = None   # fine as long as DB_CONFIG stays None (DB persistence disabled)

MSYS_DLL_PATH = r"C:\msys64\ucrt64\bin"
os.environ["PATH"] = MSYS_DLL_PATH + os.pathsep + os.environ.get("PATH", "")

CONTRACT_CSV = os.environ.get("NSE_FO_CONTRACT_CSV", r"fo_contract_stream_info.csv")   # TODO: point at your NSE F&O contract master CSV

CONTRACTS = {}


# ============================================================
# CONFIG
# ============================================================

# Env-var overrides (NSE_FO_MCAST_GRP / _PORT / _LOCAL_IF / _DECODER_EXE) let
# this run against a local test setup without editing the real values below.
MCAST_GRP = os.environ.get("NSE_FO_MCAST_GRP", "239.60.60.8")     # TODO: set NSE F&O multicast group IP
MCAST_PORT = int(os.environ.get("NSE_FO_MCAST_PORT", "10008"))    # TODO: set NSE F&O multicast port
LOCAL_IF = os.environ.get("NSE_FO_LOCAL_IF", "192.168.50.210")     # TODO: set the local NIC IP bound to your NSE leased line/VPN

# Set to a file path to append the hex of every raw packet received, one per
# line -- feed that file straight into `decode_hex.py -f <path>` to replay
# real wire data through the decoder. Off by default (no path set).
DUMP_HEX_FILE = os.environ.get("NSE_FO_DUMP_HEX_FILE")

PRICE_DIVISOR = 100
DEPTH = 5

# NSE epoch timestamps (contract expiry, LastTradeTime, ...) are seconds
# since 1980-01-01, not the standard Unix epoch (1970-01-01) -- add this
# offset before treating them as a normal Unix timestamp. Matches the same
# +315532800 correction Socket.cpp applies to LastTradedTime.
NSE_EPOCH_OFFSET = 315532800

# Your C++ decoder executable (see nse_fo_decoder.cpp).
# It accepts one HEX packet per stdin line and returns one JSON ARRAY per
# line on stdout (a single NSE UDP datagram can bundle several messages,
# and an MBP message can carry several token records, unlike the MCX
# decoder which emits exactly one JSON object per packet).
_DEFAULT_DECODER_EXE = (
    r"C:\Users\rishav.raj\Desktop\decoder\nse_fo_decoder_dyn.exe"
    if os.name == "nt" else "nse_fo_decoder"
)
CPP_DECODER_EXE = os.environ.get("NSE_FO_DECODER_EXE", _DEFAULT_DECODER_EXE)

# NSE F&O instrument types: index futures/options, stock futures/options.
ALLOWED_SYMBOLS: set = {"NIFTY"}
ALLOWED_INSTRUMENTS = {"OPTIDX"}
# populated from CSV at startup — only these tokens are processed
ALLOWED_TOKENS: set = set()


# ──────────────────────────────────────────────
# DATABASE
# ──────────────────────────────────────────────

# DB_CONFIG = {
#     "host":     "192.168.50.110",   # TODO: confirm this is the right DB host for NSE F&O
#     "dbname":   "NSE_FO",           # TODO: create/confirm this database
#     "user":     "postgres",
#     "password": "Quant@123",
#     "port":     5432,
# }
DB_CONFIG = None   # DB persistence disabled until the block above is filled in and uncommented

CREATE_OHLC_TABLE_SQL = """
CREATE TABLE IF NOT EXISTS nse_fo_ohlc (
    id            SERIAL PRIMARY KEY,
    ts            TIMESTAMP NOT NULL,
    token         INTEGER NOT NULL,
    symbol        TEXT NOT NULL,
    instrument_type TEXT,
    strike_price  NUMERIC(12, 2),
    option_type   VARCHAR(4),
    open_price    NUMERIC(12, 2),
    high_price    NUMERIC(12, 2),
    low_price     NUMERIC(12, 2),
    close_price   NUMERIC(12, 2),
    ltp           NUMERIC(12, 2),
    open_interest BIGINT,
    UNIQUE (ts, token)
);
CREATE UNIQUE INDEX IF NOT EXISTS uix_nsefo_ts_token ON nse_fo_ohlc (ts, token);
"""

CREATE_OHLC_1M_TABLE_SQL = """
CREATE TABLE IF NOT EXISTS nse_fo_ohlc_1m (
    id            SERIAL PRIMARY KEY,
    ts            TIMESTAMP NOT NULL,
    token         INTEGER NOT NULL,
    symbol        TEXT NOT NULL,
    instrument_type TEXT,
    strike_price  NUMERIC(12, 2),
    option_type   VARCHAR(4),
    open_price    NUMERIC(12, 2),
    high_price    NUMERIC(12, 2),
    low_price     NUMERIC(12, 2),
    close_price   NUMERIC(12, 2),
    ltp           NUMERIC(12, 2),
    open_interest BIGINT,
    UNIQUE (ts, token)
);
CREATE UNIQUE INDEX IF NOT EXISTS uix_nsefo1m_ts_token ON nse_fo_ohlc_1m (ts, token);
"""

_db_conn = None
_candle:    dict = {}   # token -> {ts, open, high, low, close}  — running 1-sec candle per token
_candle_1m: dict = {}   # token -> {ts, open, high, low, close}  — running 1-min candle per token


def get_db_conn():
    conn = psycopg2.connect(**DB_CONFIG)
    conn.autocommit = True
    return conn


def ensure_table(conn):
    with conn.cursor() as cur:
        cur.execute(CREATE_OHLC_TABLE_SQL)
        cur.execute(CREATE_OHLC_1M_TABLE_SQL)


def insert_ohlc(conn, ts, token, candle):
    """Insert a completed 1-second candle. candle = {open, high, low, close}."""
    info   = CONTRACTS.get(token, {})
    o, h, l, c = candle["open"], candle["high"], candle["low"], candle["close"]
    strike = float(info.get("strike_display") or 0) or None
    opt    = info.get("option_type") or None

    with conn.cursor() as cur:
        cur.execute(
            """
            INSERT INTO nse_fo_ohlc
                (ts, token, symbol, instrument_type, strike_price, option_type,
                 open_price, high_price, low_price, close_price, ltp, open_interest)
            VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s)
            ON CONFLICT (ts, token) DO UPDATE SET
                open_price    = EXCLUDED.open_price,
                high_price    = EXCLUDED.high_price,
                low_price     = EXCLUDED.low_price,
                close_price   = EXCLUDED.close_price,
                ltp           = EXCLUDED.ltp,
                open_interest = EXCLUDED.open_interest
            """,
            (ts, token, contract_name(token), info.get("instrument_type"),
             strike, opt, o, h, l, c, c, get_feed(token).open_interest),
        )


def _flush_candle(token, state):
    global _db_conn
    if _db_conn is None:
        return
    try:
        insert_ohlc(_db_conn, state["ts"], token, state)
    except psycopg2.Error as e:
        print(f"DB error: {e}")
        try:
            _db_conn = get_db_conn()
        except Exception:
            pass


def maybe_insert_ohlc(feed):
    if not feed.ltp:
        return

    ltp = feed.ltp / PRICE_DIVISOR
    ts  = datetime.now().replace(microsecond=0)

    state = _candle.get(feed.token)

    if state is None or state["ts"] != ts:
        # second rolled over — flush the completed candle, open a new one
        if state is not None:
            _flush_candle(feed.token, state)
        _candle[feed.token] = {"ts": ts, "open": ltp, "high": ltp, "low": ltp, "close": ltp}
    else:
        # same second — update running candle
        if ltp > state["high"]:
            state["high"] = ltp
        if ltp < state["low"]:
            state["low"] = ltp
        state["close"] = ltp


def insert_ohlc_1m(conn, ts, token, candle):
    """Insert a completed 1-minute candle."""
    info   = CONTRACTS.get(token, {})
    o, h, l, c = candle["open"], candle["high"], candle["low"], candle["close"]
    strike = float(info.get("strike_display") or 0) or None
    opt    = info.get("option_type") or None

    with conn.cursor() as cur:
        cur.execute(
            """
            INSERT INTO nse_fo_ohlc_1m
                (ts, token, symbol, instrument_type, strike_price, option_type,
                 open_price, high_price, low_price, close_price, ltp, open_interest)
            VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s)
            ON CONFLICT (ts, token) DO UPDATE SET
                open_price    = EXCLUDED.open_price,
                high_price    = EXCLUDED.high_price,
                low_price     = EXCLUDED.low_price,
                close_price   = EXCLUDED.close_price,
                ltp           = EXCLUDED.ltp,
                open_interest = EXCLUDED.open_interest
            """,
            (ts, token, contract_name(token), info.get("instrument_type"),
             strike, opt, o, h, l, c, c, get_feed(token).open_interest),
        )


def _flush_candle_1m(token, state):
    global _db_conn
    if _db_conn is None:
        return
    try:
        insert_ohlc_1m(_db_conn, state["ts"], token, state)
    except psycopg2.Error as e:
        print(f"DB error (1m): {e}")
        try:
            _db_conn = get_db_conn()
        except Exception:
            pass


def maybe_insert_ohlc_1m(feed):
    if not feed.ltp:
        return

    ltp = feed.ltp / PRICE_DIVISOR
    ts  = datetime.now().replace(second=0, microsecond=0)   # truncate to minute

    state = _candle_1m.get(feed.token)

    if state is None or state["ts"] != ts:
        # minute rolled over — flush the completed candle, open a new one
        if state is not None:
            _flush_candle_1m(feed.token, state)
        _candle_1m[feed.token] = {"ts": ts, "open": ltp, "high": ltp, "low": ltp, "close": ltp}
    else:
        # same minute — update running candle
        if ltp > state["high"]:
            state["high"] = ltp
        if ltp < state["low"]:
            state["low"] = ltp
        state["close"] = ltp


# ============================================================
# FEED STATE
# ============================================================

@dataclass
class FeedState:
    token: int

    buy_price: list = field(default_factory=lambda: [0] * DEPTH)
    buy_qty: list = field(default_factory=lambda: [0] * DEPTH)
    buy_orders: list = field(default_factory=lambda: [0] * DEPTH)

    sell_price: list = field(default_factory=lambda: [0] * DEPTH)
    sell_qty: list = field(default_factory=lambda: [0] * DEPTH)
    sell_orders: list = field(default_factory=lambda: [0] * DEPTH)

    ltp: int = 0
    ltq: int = 0
    ltt: int = 0

    open_price: int = 0
    high_price: int = 0
    low_price: int = 0
    close_price: int = 0

    atp: int = 0
    total_traded_qty: int = 0

    open_interest: int = 0
    day_high_oi: int = 0
    day_low_oi: int = 0

    tbq: int = 0
    tsq: int = 0

    last_update_time: str = ""

    def price(self, value):
        return value / PRICE_DIVISOR if value else 0.0

    def clear_book(self):
        self.buy_price = [0] * DEPTH
        self.buy_qty = [0] * DEPTH
        self.buy_orders = [0] * DEPTH
        self.sell_price = [0] * DEPTH
        self.sell_qty = [0] * DEPTH
        self.sell_orders = [0] * DEPTH

    def touch(self):
        self.last_update_time = datetime.now().strftime("%H:%M:%S.%f")

    def print_feed(self):
        print("=" * 120)
        print(f"TOKEN: {self.token} | {contract_name(self.token)} | UPDATE: {self.last_update_time}")

        print(
            f"LTP={self.price(self.ltp):.2f} "
            f"LTQ={self.ltq} "
            f"O={self.price(self.open_price):.2f} "
            f"H={self.price(self.high_price):.2f} "
            f"L={self.price(self.low_price):.2f} "
            f"C={self.price(self.close_price):.2f} "
            f"ATP={self.price(self.atp):.2f}"
        )

        print(
            f"TTQ={self.total_traded_qty} "
            f"OI={self.open_interest} "
            f"DAY_HI_OI={self.day_high_oi} "
            f"DAY_LO_OI={self.day_low_oi} "
            f"TBQ={self.tbq} "
            f"TSQ={self.tsq}"
        )

        print("-" * 120)
        print(
            f"{'LEVEL':<8} "
            f"{'BID_QTY':>12} "
            f"{'BID_PRICE':>15} "
            f"{'BID_ORD':>10} || "
            f"{'ASK_PRICE':>15} "
            f"{'ASK_QTY':>12} "
            f"{'ASK_ORD':>10}"
        )
        print("-" * 120)

        for i in range(DEPTH):
            print(
                f"{i + 1:<8} "
                f"{self.buy_qty[i]:>12} "
                f"{self.price(self.buy_price[i]):>15.2f} "
                f"{self.buy_orders[i]:>10} || "
                f"{self.price(self.sell_price[i]):>15.2f} "
                f"{self.sell_qty[i]:>12} "
                f"{self.sell_orders[i]:>10}"
            )

        print()


FEEDS = {}


def get_feed(token):
    token = int(token)

    if token not in FEEDS:
        FEEDS[token] = FeedState(token=token)

    return FEEDS[token]


# ============================================================
# SOCKET
# ============================================================

def create_multicast_socket():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)

    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)

    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    except Exception:
        pass

    sock.bind(("", MCAST_PORT))

    mreq = struct.pack(
        "4s4s",
        socket.inet_aton(MCAST_GRP),
        socket.inet_aton(LOCAL_IF),
    )

    try:
        sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, mreq)
    except OSError as e:
        # WSAEADDRNOTAVAIL on Windows, ENODEV ("No such device") on Linux —
        # both mean LOCAL_IF isn't bound to any NIC on this machine.
        if getattr(e, "winerror", None) == 10049 or e.errno == errno.ENODEV:
            local_ips = sorted({
                info[4][0]
                for info in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET)
            })
            raise OSError(
                f"LOCAL_IF={LOCAL_IF} is not a real interface on this machine. "
                f"IPv4 addresses actually available here: {', '.join(local_ips) or '(none found)'}"
            ) from e
        raise

    return sock


# ============================================================
# C++ DECODER BRIDGE
# ============================================================

def decode_using_cpp(data):
    """
    Sends a raw NSE F&O broadcast UDP packet to nse_fo_decoder.exe.

    Input  : raw UDP packet bytes
    Output : decoded JSON ARRAY (list of dicts) — a single datagram can carry
             several bundled/compressed messages, and an MBP message can
             carry several token records, so this is a list, not one dict.

    Each element looks like one of:

    {
      "transaction_code": 7208, "msg_type": "MBP", "token": 12345,
      "ltp": ..., "open": ..., "high": ..., "low": ..., "close": ...,
      "atp": ..., "total_traded_qty": ..., "tbq": ..., "tsq": ...,
      "entries": [
        {"md_entry_type": 0, "level": 1, "price": ..., "qty": ..., "orders": ...},
        ...
      ]
    }

    {
      "transaction_code": 7202, "msg_type": "OI_TICKER", "token": 12345,
      "open_interest": ..., "day_high_oi": ..., "day_low_oi": ...
    }
    """

    hex_packet = data.hex()

    try:
        result = subprocess.run(
            [CPP_DECODER_EXE],
            input=hex_packet,
            text=True,
            capture_output=True,
            timeout=2,
        )
    except FileNotFoundError:
        print(f"NSE F&O decoder not found: {CPP_DECODER_EXE}")
        return None
    except subprocess.TimeoutExpired:
        print("NSE F&O decoder timeout")
        return None

    if result.returncode != 0:
        print("NSE F&O decoder error:")
        print("COMMAND    :", CPP_DECODER_EXE)
        print("RETURNCODE :", result.returncode)
        print("STDOUT     :", repr(result.stdout))
        print("STDERR     :", repr(result.stderr))
        return None

    if result.stderr:
        # nse_fo_decoder.cpp writes non-fatal header-mismatch warnings here.
        print("NSE F&O decoder stderr:", result.stderr.strip())

    output = result.stdout.strip()

    if not output:
        return None

    try:
        return json.loads(output)
    except json.JSONDecodeError:
        print("Invalid JSON from NSE F&O decoder:")
        print(output)
        return None


# ============================================================
# BOOK / FEED UPDATE LOGIC
#
# Unlike MCX's incremental ('X') vs snapshot ('W') FAST messages, NSE's
# 7208/17208 MBP broadcast always carries the full current best-5 book plus
# LTP/OHLC/ATP/volume in one shot — so there's only one update path here,
# shaped like MCX's handle_snapshot.
# ============================================================

def handle_mbp(decoded):
    token = decoded.get("token")

    if token is None:
        return

    token = int(token)

    if token not in ALLOWED_TOKENS:
        return

    feed = get_feed(token)
    feed.clear_book()

    for entry in decoded.get("entries", []):
        md_type = int(entry.get("md_entry_type", -1))
        level = int(entry.get("level", 1)) - 1

        price = int(entry.get("price", 0))
        qty = int(entry.get("qty", 0))
        orders = int(entry.get("orders", 0))

        if md_type == 0 and 0 <= level < DEPTH:      # BID
            feed.buy_price[level] = price
            feed.buy_qty[level] = qty
            feed.buy_orders[level] = orders

        elif md_type == 1 and 0 <= level < DEPTH:    # ASK
            feed.sell_price[level] = price
            feed.sell_qty[level] = qty
            feed.sell_orders[level] = orders

    ltp = int(decoded.get("ltp", 0))
    if ltp:
        feed.ltp = ltp
        feed.ltq = int(decoded.get("ltq", 0))
        feed.ltt = int(decoded.get("ltt", 0))

    feed.open_price = int(decoded.get("open", 0))
    feed.high_price = int(decoded.get("high", 0))
    feed.low_price = int(decoded.get("low", 0))
    feed.close_price = int(decoded.get("close", 0))
    feed.atp = int(decoded.get("atp", 0))
    feed.total_traded_qty = int(decoded.get("total_traded_qty", 0))
    feed.tbq = int(decoded.get("tbq", 0))
    feed.tsq = int(decoded.get("tsq", 0))

    feed.touch()
    feed.print_feed()
    maybe_insert_ohlc(feed)
    maybe_insert_ohlc_1m(feed)


def handle_oi_ticker(decoded):
    token = decoded.get("token")

    if token is None:
        return

    token = int(token)

    if token not in ALLOWED_TOKENS:
        return

    feed = get_feed(token)
    feed.open_interest = int(decoded.get("open_interest", 0))
    feed.day_high_oi = int(decoded.get("day_high_oi", 0))
    feed.day_low_oi = int(decoded.get("day_low_oi", 0))

    feed.touch()
    feed.print_feed()


def handle_security_range(decoded):
    token = decoded.get("token")

    if token is None or int(token) not in ALLOWED_TOKENS:
        return

    print(
        f"[SECURITY_RANGE] token={token} "
        f"low={decoded.get('low_price_range')} "
        f"high={decoded.get('high_price_range')}"
    )


def handle_exec_range(decoded):
    for entry in decoded.get("entries", []):
        token = entry.get("token")

        if token is None or int(token) not in ALLOWED_TOKENS:
            continue

        print(
            f"[EXEC_RANGE] token={token} "
            f"low={entry.get('low_exec_band')} "
            f"high={entry.get('high_exec_band')}"
        )


def handle_decoded(decoded):
    msg_type = decoded.get("msg_type")

    if msg_type == "MBP":
        handle_mbp(decoded)

    elif msg_type == "OI_TICKER":
        handle_oi_ticker(decoded)

    elif msg_type == "SECURITY_RANGE":
        handle_security_range(decoded)

    elif msg_type == "EXEC_RANGE":
        handle_exec_range(decoded)

    elif "error" in decoded:
        print("Decoder error:", decoded["error"], decoded.get("reason", ""))

    else:
        print("Unknown msg_type:", msg_type)


# ============================================================
# RAW DEBUG
# ============================================================

def print_raw_packet(data, addr):
    print("=" * 100)
    print(f"[{datetime.now().strftime('%H:%M:%S.%f')}] {len(data)} bytes from {addr}")
    print("Binary packet received. Passing to C++ decoder...")

    print("HEX FULL PACKET:")
    print(data.hex())

    print("HEX FIRST 64 BYTES:")
    print(data[:64].hex())


# ============================================================
# CONTRACT LOADER
# ============================================================

def load_contract_csv(path=CONTRACT_CSV):
    """
    Parses NSE's raw (headerless) contract-stream CSV, one row per contract:
        C,<series>,<token>,<instrument_type>,<symbol>,<expiry_epoch>,<strike*100>,<option_type>
    The first line is a summary record ("<timestamp>,<row_count>,") and is skipped.
    """
    global CONTRACTS, ALLOWED_TOKENS

    CONTRACTS = {}

    try:
        with open(path, "r", newline="", encoding="utf-8") as file:
            reader = csv.reader(file)

            for row in reader:
                if len(row) < 8 or row[0] != "C":
                    continue

                try:
                    token = int(row[2])
                    expiry_epoch = int(row[5])
                    strike_raw = int(row[6])
                except ValueError:
                    continue

                CONTRACTS[token] = {
                    "token": row[2],
                    "instrument_type": row[3],
                    "symbol": row[4],
                    "expiry_date": (
                        datetime.fromtimestamp(expiry_epoch + NSE_EPOCH_OFFSET).strftime("%d-%b-%Y")
                        if expiry_epoch else ""
                    ),
                    "strike_display": f"{strike_raw / 100:.2f}" if strike_raw else "",
                    "option_type": row[7],
                }

        print(f"Loaded contracts: {len(CONTRACTS)}")

    except FileNotFoundError:
        print(f"Contract CSV not found: {path}")

    except Exception as e:
        print(f"Error loading contract CSV: {e}")

    ALLOWED_TOKENS = {
        token
        for token, row in CONTRACTS.items()
        if (not ALLOWED_SYMBOLS or row.get("symbol") in ALLOWED_SYMBOLS)
        and row.get("instrument_type") in ALLOWED_INSTRUMENTS
    }

    print(f"Allowed tokens total : {len(ALLOWED_TOKENS)}")


def contract_name(token):
    info = CONTRACTS.get(int(token))

    if not info:
        return "UNKNOWN CONTRACT"

    symbol = info.get("symbol", "")
    inst = info.get("instrument_type", "")
    expiry = info.get("expiry_date", "")
    strike = info.get("strike_display", "")
    opt = info.get("option_type", "")
    lot = info.get("lot_size", "")
    tick = info.get("tick_size", "")

    parts = [symbol, inst, expiry]

    if strike:
        parts.append(strike)

    if opt and opt != "XX":
        parts.append(opt)

    if lot:
        parts.append(f"LOT={lot}")

    if tick:
        parts.append(f"TICK={tick}")

    return " ".join([p for p in parts if p])

# ============================================================
# MAIN
# ============================================================

def main():
    global _db_conn
    load_contract_csv()

    if DB_CONFIG:
        _db_conn = get_db_conn()
        ensure_table(_db_conn)
    else:
        print("DB_CONFIG not set — running without DB persistence.")

    sock = create_multicast_socket()

    print(f"Listening on {MCAST_GRP}:{MCAST_PORT}")
    print(f"Local IF: {LOCAL_IF}")
    print("Waiting for NSE F&O broadcast packets...\n")

    while True:
        try:
            data, addr = sock.recvfrom(65535)

            messages = decode_using_cpp(data)

            if messages is None:
                print_raw_packet(data, addr)
                continue

            for decoded in messages:
                handle_decoded(decoded)

        except KeyboardInterrupt:
            print("Stopped")
            break

        except Exception as e:
            print("Error:", e)


if __name__ == "__main__":
    main()
