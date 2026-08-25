# Box Spread — Third-Party Add Gateway

How an external application submits a Box Spread "Add Strategy" request
through the RMTrade desktop UI, without ever holding the real trading-server
login credentials.

Companion doc: [README.md](README.md) (the raw client↔server wire protocol
this gateway builds on top of).

## 1. Why this exists

The trading server authenticates a whole TCP session at once (`LOGIN_REQUEST`
with username/password); there is no separate, per-request or scoped
credential. So a third-party app cannot safely talk to the trading server
directly — the only way to do that is to hand it real login credentials.

Instead, the third party talks to a small gateway running **inside the
already-logged-in RMTrade UI process**, over a **loopback-only TCP socket**,
guarded by its own API key (unrelated to the trading login). The UI does the
actual write to the trading-server socket using the session it already has.

```
Third-party app ──(loopback TCP, API key)──▶ RMTrade UI (Box_Spread_ThirdPartyGateway)
                                                    │
                                                    ▼
                                         SERVER_CONNECTION->socket
                                        (already-authenticated session)
                                                    │
                                                    ▼
                                              Trading server
```

Implementation: [Box_Spread_ThirdPartyGateway.h](Box_Spread_ThirdPartyGateway.h) /
[Box_Spread_ThirdPartyGateway.cpp](Box_Spread_ThirdPartyGateway.cpp), wired
into [`mcx_Socket`](../../Socket/Socket.h) in
[Socket.cpp](../../Socket/Socket.cpp).

## 2. Enabling it (RMTrade side)

Off by default. To enable, add two keys to the app's ini settings file
(the same file referenced elsewhere as `m_sSettingsFile`):

```ini
ThirdPartyGatewayPort=48765
ThirdPartyGatewayApiKey=<a long random secret, not the trading password>
```

Both must be set (port > 0, non-empty key) for the gateway to start. It
binds `127.0.0.1` only — it is never reachable from another machine. On
startup it logs `Box_Spread_ThirdPartyGateway listening on 127.0.0.1:<port>`
(or a failure reason) through the usual `Logger`.

Rotate `ThirdPartyGatewayApiKey` independently of the trading login whenever
you need to cut off a third party.

## 3. Wire protocol (third-party side)

Plain TCP, **one request per connection**: open a socket to
`127.0.0.1:<ThirdPartyGatewayPort>`, write one line of JSON terminated by
`\n`, read one line of JSON response terminated by `\n`, then the gateway
closes the connection. Open a new connection for the next request.

### Request — `add_box_spread`

```json
{
  "api_key": "<ThirdPartyGatewayApiKey>",
  "action": "add_box_spread",
  "client_ref": "any string you choose, echoed back",
  "legs": [
    { "exchange": "EXCHG_NSE_FO", "token": 123456 },
    { "exchange": "EXCHG_NSE_FO", "token": 123457 },
    { "exchange": "EXCHG_NSE_FO", "token": 123458 },
    { "exchange": "EXCHG_NSE_FO", "token": 123459 }
  ],
  "qty": 1,
  "max_buy_lot": 5,
  "max_sell_lot": 5,
  "n_lot": 5,
  "pro": true,
  "client_code": "",
  "sell_spread": 1.0,
  "buy_spread": -1.0,
  "profit": 0,
  "jump": 0,
  "tick_depth": 0,
  "bid_time": 0,
  "delta": 0,
  "lot_threshold": 0,
  "buy_spread_enable": false,
  "sell_spread_enable": false
}
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `api_key` | string | yes | must equal `ThirdPartyGatewayApiKey` |
| `action` | string | yes | must be `"add_box_spread"` |
| `client_ref` | string | no | your own correlation id; echoed back verbatim, not interpreted |
| `legs` | array of 4 | yes | Box Spread is always 4-legged; each entry is `{exchange, token}` |
| `legs[].exchange` | string | yes | e.g. `"EXCHG_NSE_FO"`, `"EXCHG_MCX"` — must match the `EXCHG_*` names the desktop app itself uses |
| `legs[].token` | integer | yes | must be a token the running app already has in its contract master (i.e. a currently valid, downloaded instrument) |
| `qty` | number > 0 | yes | applied to **all 4 legs equally** — Box Spread trades the same quantity on every leg, same as the desktop Add dialog (its leg-2/3/4 quantity fields are locked to leg 1) |
| `max_buy_lot`, `max_sell_lot`, `n_lot` | number ≥ 0 | yes | strategy execution limits, same meaning as the corresponding fields in the desktop dialog |
| `pro` | bool | no (default `true`) | `true` = Pro account; `false` requires `client_code` |
| `client_code` | string | required if `pro:false` | max 10 characters |
| `sell_spread`, `buy_spread` | number | no (default `1.0` / `-1.0`) | same defaults as the desktop dialog |
| `profit`, `jump`, `delta` | number | no (default `0`) | rupee values, not paisa — the gateway does the ×100 conversion |
| `tick_depth`, `bid_time`, `lot_threshold` | integer | no (default `0`) | |
| `buy_spread_enable`, `sell_spread_enable` | bool | no (default `false`) | |

### Response

Success:
```json
{"ok": true, "strgy_id": 1861234567, "error_code": 0, "client_ref": "..."}
```

Failure:
```json
{"ok": false, "error_code": -4, "error": "leg 1: invalid exchange/token", "client_ref": "..."}
```

`strgy_id` is the id assigned to the new strategy — use it later for
anything that needs to reference this specific strategy instance (start,
stop, modify, delete — **not yet exposed through this gateway**, see
Limitations below).

### Error codes

| `error_code` | Meaning |
|---|---|
| `-1` | bad `api_key` |
| `-2` | malformed JSON, or unknown `action` |
| `-3` | RMTrade UI is not connected/logged in to the trading server right now |
| `-4` | invalid `legs` (wrong count, unknown exchange, or token not in the current contract master) |
| `-5` | missing/invalid required field (`qty`, lot limits, `client_code`, ...) |
| `-6` | timed out waiting for the trading server to respond (5s) — outcome unknown, check the strategy grid before retrying |
| `-7` | the write to the trading-server socket itself failed |
| *(other positive/negative values)* | echoed straight from the trading server's own `Error_Response_Type` for this request — `error` holds the server's message text |

## 4. Example (Python)

```python
import socket, json

req = {
    "api_key": "REPLACE_ME",
    "action": "add_box_spread",
    "client_ref": "order-42",
    "legs": [
        {"exchange": "EXCHG_NSE_FO", "token": 123456},
        {"exchange": "EXCHG_NSE_FO", "token": 123457},
        {"exchange": "EXCHG_NSE_FO", "token": 123458},
        {"exchange": "EXCHG_NSE_FO", "token": 123459},
    ],
    "qty": 1, "max_buy_lot": 5, "max_sell_lot": 5, "n_lot": 5,
    "pro": True,
}

with socket.create_connection(("127.0.0.1", 48765), timeout=10) as s:
    s.sendall((json.dumps(req) + "\n").encode())
    resp = s.recv(65536).decode()

print(json.loads(resp))
```

## 5. What this does under the hood (for context)

On a valid `add_box_spread` request, the gateway:

1. Resolves each leg's token against the live contract map (`TOKEN[]`) to
   read its price exponent/tick size — it does **not** trust the third
   party's numbers for anything beyond exchange/token.
2. Builds the identical `GUI_ADD_MESSAGE` + `BOX_SPREAD_STRUCT` byte buffer
   the Add/Modify dialog builds (see [README.md §4-5](README.md)) and writes
   it to `SERVER_CONNECTION->socket`, exactly as
   [Box_Spread_AddModifyWindow.cpp](Box_Spread_AddModifyWindow.cpp) does.
3. Feeds the same bookkeeping (`AddStrategyPendingQueue`) the dialog feeds,
   so the strategy is tracked and — once the server confirms — inserted into
   the live grid through the existing, unmodified
   `Box_Spread_Database::AddStrategyServerResponse` path. Nothing about how
   the server response is handled was changed; the gateway only adds a
   second listener on the existing `AddStrategyServer` signal to learn the
   outcome for its own reply.
4. Waits up to 5 seconds for that response, then replies to the third party.

## 6. Limitations / not yet built

Scoped deliberately narrow for a first version — flag any of these if you
need them:

- **Add only.** Modify / Start / Stop / Delete are not exposed through this
  gateway yet (the wire messages for them are simpler — see
  [README.md §4](README.md) — but they aren't wired up here).
- **Equal quantity across all 4 legs.** Matches the desktop dialog's own
  Box-mode behavior; per-leg quantities aren't accepted.
- **No third-party-specific authorization policy.** Any request with the
  right `api_key` can add a strategy under whatever `client_code` it
  supplies — there's no allow-list of instruments, client codes, or quantity
  caps beyond what you pass in `max_buy_lot`/`max_sell_lot`. If you need
  that, it belongs in this gateway (reject before building the wire
  message), not left to the trading server alone.
- **No rate limiting.** A burst of requests will each get their own
  synchronous wait; nothing currently throttles them.
- **No audit tag.** Strategies added this way look identical, in the grid
  and in logs, to a manually-added strategy — there's no "added via
  third-party gateway" marker.
- **Not yet compiled/run.** This was written and reasoned through against
  the existing code paths but hasn't been built or smoke-tested in a real
  Qt/qmake environment. Build it, run one add end-to-end against a test
  strategy, and confirm the row appears correctly in the grid before relying
  on it.

## 7. Files touched

- New: [Box_Spread_ThirdPartyGateway.h](Box_Spread_ThirdPartyGateway.h),
  [Box_Spread_ThirdPartyGateway.cpp](Box_Spread_ThirdPartyGateway.cpp)
- [RMTrade.pro](../../RMTrade.pro): registered the two new files under the
  existing `contains(STRATEGIES, Box_Spread)` block.
- [Socket/Socket.h](../../Socket/Socket.h): forward-declared the gateway
  class, added a `BoxSpreadThirdPartyGateway` member to `mcx_Socket`.
- [Socket/Socket.cpp](../../Socket/Socket.cpp): starts the gateway at the
  end of the `mcx_Socket` constructor, only if both ini keys are set.
