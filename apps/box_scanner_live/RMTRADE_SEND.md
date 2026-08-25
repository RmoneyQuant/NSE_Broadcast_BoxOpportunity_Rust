# Sending a Box to RMTrade — how box_scanner_live does it

How the desktop scanner turns one priced row into a live RMTrade Box Spread
strategy. This is the *client* side; the wire protocol and the gateway
itself (inside RMTrade) are specified in
[`../../Third_party_gateway.md`](../../Third_party_gateway.md) — this doc
only covers what lives in this repo.

## Tech used

| Concern | What |
|---|---|
| UI | [`native-windows-gui`](https://crates.io/crates/native-windows-gui) (`nwg`) — Win32 common controls over GDI, no GPU. Same reason as the rest of this app's window (see `gui.rs`'s module doc): no GPU adapter reachable in this RDP/VDI deployment. |
| Request/response encoding | `serde` + `serde_json` — one Rust struct per JSON shape, `to_string`/`from_str` |
| Transport | `std::net::TcpStream` — plain loopback TCP, no HTTP client, matching the gateway's "one JSON line in, one JSON line out per connection" protocol |
| Config | `std::env` — three env vars, no config file |

No new dependency does anything exotic; `serde`/`serde_json` were already
workspace dependencies (`nse_pricing`, `nse_sink`) before this feature.

## Where the code lives

- [`src/rmtrade_gateway.rs`](src/rmtrade_gateway.rs) — everything RMTrade-specific:
  building the token lookup, resolving a pair's 4 leg tokens, building the
  request JSON, and the TCP round-trip. No `nwg` in this file — it doesn't
  know it's being called from a GUI.
- [`src/gui.rs`](src/gui.rs) — the "RMTrade" column, the click handler that
  triggers a send, and the modal dialogs that report the outcome.
- [`src/order_log.rs`](src/order_log.rs) — the CSV audit trail, one row per
  send attempt. No `nwg` here either.
- [`src/main.rs`](src/main.rs) — builds the token index and opens the order
  log once at startup, hands both to `gui::run`.

## Data flow

```
startup:
  NSE_FO_CONTRACT_CSV
        │
        ▼
  InstrumentMaster::load()          (nse_refdata, already existed)
        │
        ▼
  rmtrade_gateway::build_token_index()
        │  (expiry_epoch, strike_raw, "CE"/"PE") -> token
        ▼
  gui::run(..., token_index)        -- moved in once, read-only after that

per click, on the GUI thread:
  operator clicks a row's "RMTrade" cell
        │
        ▼
  OnListViewClick  →  hit-tested (row_index, column_index)
        │  column_index == 0 ("RMTrade")?  else: ignored
        ▼
  record_at_row()                   -- row_index -> WirePairId -> WireOpportunityRecord
        │  (via a RefCell<Vec<WirePairId>> refreshed alongside every 300ms table redraw)
        ▼
  submit_box_to_rmtrade()
        │
        ├─ rmtrade_gateway::resolve_legs()      -- pair -> 4 tokens (K1 CE/PE, K2 CE/PE)
        ├─ read Qty / Max Buy Lot / Max Sell Lot / N Lot from their TextInputs
        ├─ rmtrade_gateway::send_add_box_spread()
        │        │
        │        ▼
        │   TcpStream::connect(127.0.0.1:<port>)
        │   write one JSON line + "\n"
        │   read one JSON line back
        │        │
        │        ▼
        │   RMTrade's Box_Spread_ThirdPartyGateway (separate C++/Qt process)
        │
        └─ nwg::modal_info_message / modal_error_message  -- report strgy_id or the error
```

## Exact request payload

One JSON line, matching `AddBoxSpreadRequest` in `rmtrade_gateway.rs`:

```json
{
  "api_key": "<RMTRADE_GATEWAY_API_KEY>",
  "action": "add_box_spread",
  "client_ref": "box_scanner-<expiry_epoch>-<k1>-<k2>",
  "legs": [
    { "exchange": "EXCHG_NSE_FO", "token": <K1 Call token> },
    { "exchange": "EXCHG_NSE_FO", "token": <K2 Call token> },
    { "exchange": "EXCHG_NSE_FO", "token": <K1 Put token> },
    { "exchange": "EXCHG_NSE_FO", "token": <K2 Put token> }
  ],
  "qty": <Quantity input>,
  "max_buy_lot": <Max Buy Lot input>,
  "max_sell_lot": <Max Sell Lot input>,
  "n_lot": <N Lot input>,
  "pro": true,
  "client_code": "",
  "sell_spread": 1.0,
  "buy_spread": -1.0,
  "profit": <Profit input>,
  "jump": <Jump input>,
  "bid_time": <eBidTime input>,
  "delta": <Delta input>,
  "lot_threshold": <Lot Threshold input>
}
```

| Field | Where it comes from |
|---|---|
| `api_key` | `RMTRADE_GATEWAY_API_KEY` env var, read fresh at click time |
| `action` | always `"add_box_spread"` -- the only action this client ever sends |
| `client_ref` | built as `box_scanner-{expiry_epoch}-{k1}-{k2}` -- a correlation id RMTrade echoes back unread; doesn't affect anything server-side |
| `legs` | the clicked row's 4 tokens, resolved from its expiry+K1+K2 via `resolve_legs` -- see **Leg order** above for the exact slot order |
| `qty` | the **Quantity** textbox above the tables, read live at click time |
| `max_buy_lot` / `max_sell_lot` / `n_lot` | the **Max Buy Lot** / **Max Sell Lot** / **N Lot** textboxes, read live at click time |
| `profit` / `jump` / `delta` | the **Profit** / **Jump** / **Delta** textboxes -- any number, 0 and negative both allowed (matches RMTrade's own Add dialog fields of the same name) |
| `bid_time` / `lot_threshold` | the **eBidTime** / **Lot Threshold** textboxes -- non-negative whole numbers only |
| `pro` | hardcoded `true` -- there's no **client_code** input in the GUI yet, so a non-Pro account isn't supported from here |
| `client_code` | hardcoded `""` (unused while `pro` is always `true`) |
| `sell_spread` / `buy_spread` | hardcoded `1.0` / `-1.0` -- the gateway's own documented defaults; not exposed as GUI inputs |

`tick_depth` and `buy_spread_enable`/`sell_spread_enable` are still **not
sent at all** -- no GUI input exists for them yet, so the gateway falls
back to its own defaults (`Third_party_gateway.md` §3's table) rather than
this client guessing values.

## Order log

Every send attempt -- success or failure -- appends one row to a local CSV
file (`order_log.rs`), independent of whatever RMTrade's own grid shows.
Opened once at startup in [`main.rs`](src/main.rs) and handed into
`gui::run`; written from `submit_box_to_rmtrade` right after the response
comes back (or the send fails outright), flushed immediately so a crash
right after can't lose the row.

Path: `RMTRADE_ORDER_LOG` env var, default `rmtrade_orders.csv` (same
directory the app was launched from). The header is written once, the
first time the file is created -- re-running the app keeps appending to
the same file rather than duplicating headers.

Each row has 37 columns: a timestamp, `side` (`LONG`/`SHORT`), the same 17
values the operator saw in the table (`row_strings`, lot-multiplier
included, so `net_lot`/`profit_lot` match what was on screen), the 4
resolved leg tokens, `client_ref`, all 9 request parameters (prefixed
`req_`), and the outcome (`ok`, `strgy_id`, `error_code`, `error`). A
failed send (couldn't connect, bad response, ...) still gets a row --
`ok` is `false` and `error` holds the client-side failure text (e.g.
*"couldn't connect to the RMTrade gateway: ..."*), not just RMTrade's own
rejections.

## Trigger

Clicking the **"RMTrade"** cell (leftmost column, always visible — an
earlier version put it last and it scrolled out of view) on a row sends
that row **immediately** — no confirmation dialog, no separate button, by
explicit request. Clicking anywhere else in the row just selects it, same
as before this feature existed. Two earlier variants were tried and
rejected: a single global button + manual selection (selection state
doesn't map cleanly to "which table"), and send-on-selection (fired on
ordinary "just looking" clicks) — see the row-click design in `gui.rs` for
the reasoning trail.

## Leg order (verified against the desktop dialog, not assumed)

The gateway's wire protocol carries no buy/sell flag per leg — just 4
`{exchange, token}` pairs. This client sends them in **K1 Call, K2 Call, K1
Put, K2 Put** order. This was originally guessed as K1 Call/K1 Put/K2
Call/K2 Put and shipped that way — wrong, and it silently produced
strategies whose "STK2" column echoed STK1 instead of the real second
strike (RMTrade reads the strike out of leg-array slots 0 and 1, and slot 1
held the K1 Put, not the K2 Call). Confirmed correct against
`Box_Spread_AddModifyWindow.cpp`, which forces leg 3's strike equal to leg
1's and leg 4's equal to leg 2's — i.e. legs 1&3 share K1, legs 2&4 share
K2. Direction is implied by this fixed order on the RMTrade side, not
decided here.

## Config (env vars, read fresh on every send — not cached at startup)

| Var | Required | Default |
|---|---|---|
| `RMTRADE_GATEWAY_API_KEY` | yes | none |
| `RMTRADE_GATEWAY_PORT` | yes | none |
| `RMTRADE_GATEWAY_HOST` | no | `127.0.0.1` |

Must match the `ThirdPartyGatewayApiKey`/`ThirdPartyGatewayPort` set in
RMTrade's own ini (see `Third_party_gateway.md` §2) — RMTrade only starts
listening on process startup after those are set, so editing the ini
requires restarting RMTrade, not just this scanner.

## What happens on failure

`rmtrade_gateway::SendError` covers: missing api key/port config, TCP
connect failure (gateway not listening — `os error 10061` is this),
read/write failure mid-request, and an unparseable response. A resolved-OK
response that RMTrade itself rejects (bad token, bad exchange, not
logged in, ...) surfaces its `error_code`/`error` text instead — see
`Third_party_gateway.md` §3's error code table for what each code means.
