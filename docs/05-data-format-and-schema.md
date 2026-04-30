# 05 - Data Format and Schema

Comprehensive reference for the JSONL data contract between the Python collector and all
consumers (viewer, future Rust rewrite, backtesting pipelines). This document is the
authoritative source for every field, every event type, and every semantic invariant.

---

## 1. File Structure

### Directory layout

```
data/
  {market_start_epoch}/          # e.g. data/1746057600/
    clob.jsonl                   # Polymarket CLOB events
    depth-trade.jsonl            # Binance BTCUSDT events
    .cache/                      # viewer-generated, not collector output
      meta.json
      frames.json
```

`market_start_epoch` is the Unix timestamp (seconds) of the 5-minute market boundary.
Markets fall on exact multiples of 300 seconds. Example: `1746057600` = 2025-05-01 00:00:00 UTC.

### Why two files

Source separation provides three benefits:

1. **Independent I/O paths.** Each file has its own `LineWriter` thread with its own
   file descriptor and flush cadence. This eliminates contention between the Binance
   stream (~2000 events/sec during volatile periods) and the Polymarket stream.

2. **Independent sequence counters.** Each file has a monotonically increasing `seq`
   starting from 1. A gap in `seq` within a single file means events were lost or
   errors occurred. Cross-file seq comparison is meaningless.

3. **Independent lifecycle.** Polymarket lookup can fail (slug not found, market not
   yet created) while Binance collection proceeds normally. The viewer handles
   missing `clob.jsonl` gracefully.

### File naming

Defined in `config.py`:

| Constant        | Value              | Contents                              |
|-----------------|--------------------|---------------------------------------|
| `CLOB_FILENAME` | `clob.jsonl`       | All Polymarket events                 |
| `DEPTH_FILENAME`| `depth-trade.jsonl`| All Binance depth, bookTicker, trades |

Files are opened in append-binary mode (`"ab"`). If a worker crashes and is restarted
(supervisor allows max 1 restart), the same files are appended to. The `worker_started`
system event at the top of the restarted section marks the boundary.

### File sizes

Typical per-market (390 seconds of collection = 70s pre + 300s market + 20s tail):

| File             | Typical size | Range          |
|------------------|-------------|----------------|
| `clob.jsonl`     | 200-800 MB  | depends on Polymarket activity |
| `depth-trade.jsonl` | 800 MB - 2 GB | Binance depth@100ms drives most volume |

The `book_state` events (full book snapshots after every diff) are the dominant
contributor to file size in both files.

---

## 2. Base Line Schema

Every line in both files is a JSON object serialized with `orjson` followed by `\n`.
The base schema is constructed by `make_line()` in `models.py`.

### Fields

| Field            | Type              | Description |
|------------------|-------------------|-------------|
| `ts_recv_ms`     | `int`             | Millisecond Unix timestamp when the collector received/generated this event. `int(time.time() * 1000)`. This is the **collector's clock**, not the exchange timestamp. |
| `ts_recv_unix`   | `float`           | Same instant as `ts_recv_ms` but as seconds with fractional milliseconds. `ts_recv_ms / 1000.0`. Provided for convenience; always derivable from `ts_recv_ms`. |
| `market_start`   | `int`             | Unix timestamp (seconds) of the market this worker is collecting. Matches the directory name. Constant within a file. |
| `collector_pid`  | `int`             | OS process ID of the worker subprocess. Useful for correlating logs. Changes on restart. |
| `source`         | `string`          | `"polymarket"` in `clob.jsonl`, `"binance"` in `depth-trade.jsonl`. Constant within a file. |
| `event_type`     | `string`          | Discriminator for the event. See sections 3 and 4 for complete enumeration. |
| `seq`            | `int`             | Monotonically increasing per-file sequence number starting from 1. Gaps indicate errors or lost events. Resets to 1 on worker restart (new `SeqCounter` instance). |
| `raw`            | `any \| null`     | The original data from the upstream source (WS message, REST response, etc.). `null` for derived/synthetic events. |
| `normalized`     | `any \| null`     | Collector-computed fields: parsed, validated, enriched. The viewer primarily consumes this field. `null` when normalization is not applicable (some raw-only events). |
| `system_type`    | `string \| absent`| Present only when `event_type == "system"`. Sub-discriminator for system lifecycle events. The field is **omitted entirely** (not `null`) when not applicable. |

### Serialization details

- Serializer: `orjson.dumps()` (no pretty-printing, no sorting).
- Numeric precision: Python `float` for timestamps. `str` for all prices and quantities
  (both Polymarket and Binance preserve string representation to avoid float rounding).
  `Decimal` is used internally in `BinanceBook` but serialized as `str`.
- Line terminator: `\n` (single byte, no `\r`).
- Encoding: UTF-8 (orjson default).
- Flush cadence: `LineWriter` flushes every 100ms or on queue drain, whichever comes first.
  The `stop()` method does a final flush before closing.

### Ordering guarantees

Within a single file:
- `seq` is strictly monotonically increasing (within one worker lifetime).
- `ts_recv_ms` is **mostly** monotonically increasing but **not guaranteed** due to
  `time.time()` resolution and the fact that multiple coroutines feed the same writer
  concurrently. Two events written in the same millisecond may have the same `ts_recv_ms`.
- Causal ordering is maintained by `seq`: if event A caused event B (e.g., `depth_raw`
  then `depth_applied` then `book_state`), A.seq < B.seq.

---

## 3. Polymarket Event Types (clob.jsonl)

### 3.1 System events (`event_type: "system"`)

System events use the `system_type` field as a sub-discriminator. `raw` is typically
`null`; meaningful data goes in `normalized`.

#### `system_type: "worker_started"`

**When:** First event in the file. Written by `worker.py` at startup.

```
raw: null
normalized: {
  "market_start": int,       // epoch seconds
  "pid": int,                // os.getpid()
  "stop_at": float           // market_start + MARKET_DURATION + TAIL_SECONDS
}
```

#### `system_type: "ws_connecting"`

**When:** Before each WebSocket connection attempt (including reconnects). Written per
connection; with `POLY_WS_CONNECTIONS=10`, you will see 10 of these at startup.

```
raw: null
normalized: {
  "url": string,             // POLYMARKET_WS_URL
  "conn_id": int             // 0-indexed connection identifier (0..POLY_WS_CONNECTIONS-1)
}
```

#### `system_type: "ws_connected"`

**When:** WebSocket handshake completed successfully.

```
raw: null
normalized: {
  "conn_id": int
}
```

#### `system_type: "ws_subscribed"`

**When:** After sending the subscription payload on the WebSocket.

```
raw: {                       // the subscription payload sent to the server
  "assets_ids": [string, string],   // the two CLOB token IDs
  "type": "market",
  "custom_feature_enabled": true
}
normalized: {
  "token_ids": [string, string],
  "conn_id": int
}
```

#### `system_type: "ws_disconnected"`

**When:** WebSocket connection closed (clean or unclean).

```
raw: null
normalized: {
  "conn_id": int,
  "msgs_received": int,      // total messages received on this connection
  "msgs_deduped": int        // messages dropped as duplicates
}
```

#### `system_type: "ws_reconnecting"`

**When:** About to sleep before reconnecting. Written after `ws_disconnected` or after
a connection error.

```
raw: null
normalized: {
  "backoff": float,           // seconds until next attempt (doubles each time, capped at WS_RECONNECT_MAX=16)
  "conn_id": int
}
```

### 3.2 Market lookup events

#### `event_type: "market_lookup"`

**When:** Gamma API returned a valid market for the slug. Exactly one per successful
collection session. This is the **critical metadata event** -- the viewer requires it.

```
raw: { ... }                 // full Gamma API response (large JSON with all market fields)
normalized: {
  "market_id": string,       // Gamma market UUID
  "condition_id": string,    // on-chain condition ID
  "slug": string,            // e.g. "btc-updown-5m-1746057600"
  "question": string,        // e.g. "Will BTC go up in the next 5 minutes?"
  "outcomes": [string, string],     // e.g. ["Up", "Down"]
  "clob_token_ids": [string, string], // two CLOB token IDs: [up_token, down_token]
  "end_date": string,        // ISO date
  "game_start_time": string | null
}
```

**Viewer dependency:** The viewer **aborts processing** if this event is missing.
`clobTokenIds[0]` is treated as the Up token, `[1]` as the Down token.

#### `event_type: "market_lookup_failed"`

**When:** Gamma lookup failed (deadline exceeded, slug mismatch, market closed,
order book not enabled, outcome count wrong, etc.).

```
raw: { ... } | {"slug": string}   // Gamma response or just the slug if deadline exceeded
normalized: {
  "slug": string,
  "reason": string           // "deadline exceeded", "slug mismatch: ...", "market is closed", etc.
}
```

#### `event_type: "market_lookup_retry"`

**When:** A Gamma API request failed (non-200 status or exception) but the deadline
has not been reached yet. Written before each retry sleep.

```
raw: {
  "status": int, "body": string, "attempt": int    // on HTTP error
  // OR
  "error": string, "attempt": int                  // on exception
}
normalized: {
  "slug": string,
  "attempt": int,
  "status": int | absent,     // present on HTTP error
  "error": string | absent    // present on exception
}
```

### 3.3 Book events

#### `event_type: "book_raw"`

**When:** Received a `"book"` event from the Polymarket WebSocket. This is a full
order book snapshot (not a delta). Polymarket sends these periodically and on subscription.

```
raw: { ... }                 // full WS message including "event_type": "book", "asset_id", "bids", "asks"
                             // bids/asks are either [{price, size}, ...] or [[price, size], ...]
normalized: {
  "asset_id": string,
  "event_type": "book"
}
```

#### `event_type: "book_state"`

**When:** Immediately after every `book_raw` or `price_change_applied` event. This is a
**full snapshot of the local order book** as maintained by the collector's `LocalBook`.
This is the primary event the viewer consumes from `clob.jsonl`.

```
raw: null
normalized: {
  "bid_count": int,
  "ask_count": int,
  "bids": [[price_str, size_str], ...],   // sorted descending by price (best bid first)
  "asks": [[price_str, size_str], ...],   // sorted ascending by price (best ask first)
  "best_bid": string | null,
  "best_ask": string | null,
  "asset_id": string,
  "last_trade_price": string | null,      // last known trade price for this asset
  "trigger_event_type": "book" | "price_change"  // what caused this snapshot
}
```

Prices and sizes are **strings**, not floats. The viewer `parseFloat()`s them.

### 3.4 Price change events

#### `event_type: "price_change_raw"`

**When:** Received a `"price_change"` event from the Polymarket WebSocket. These are
order book deltas (level updates).

```
raw: { ... }                 // full WS message, contains "price_changes" or "changes" array
normalized: {
  "asset_id": string,
  "event_type": "price_change"
}
```

The changes array contains objects with `{asset_id, side, price, size}`. `side` is
`"BUY"` or `"SELL"`. `size == "0"` means remove the level.

#### `event_type: "price_change_applied"`

**When:** After applying the deltas from a `price_change_raw` to the local book.
One per affected asset (a single price_change message can affect multiple assets).

```
raw: null
normalized: {
  "asset_id": string,
  "changes_count": int        // total number of changes in the original message
}
```

Always followed by a `book_state` event for the affected asset.

### 3.5 Trade and ticker events

#### `event_type: "last_trade_price_raw"`

**When:** Received a `"last_trade_price"` event from the Polymarket WebSocket.

```
raw: { ... }                 // full WS message
normalized: {
  "asset_id": string,
  "price": string            // last trade price as string
}
```

#### `event_type: "best_bid_ask_raw"`

**When:** Received a `"best_bid_ask"` event from the Polymarket WebSocket.

```
raw: { ... }                 // full WS message
normalized: {
  "asset_id": string
}
```

Not currently consumed by the viewer (the viewer derives best bid/ask from `book_state`).

#### `event_type: "tick_size_change_raw"`

**When:** Received a `"tick_size_change"` event from the Polymarket WebSocket.

```
raw: { ... }                 // full WS message
normalized: {
  "asset_id": string
}
```

#### `event_type: "market_resolved_raw"`

**When:** Received a `"market_resolved"` event from the Polymarket WebSocket.

```
raw: { ... }                 // full WS message
normalized: {
  "asset_id": string
}
```

### 3.6 Catch-all for unknown event types

Any Polymarket WS event with an `event_type` not matching the known set above is logged
as `"{event_type}_raw"` (the original event_type with `_raw` appended).

```
raw: { ... }                 // full WS message
normalized: {
  "asset_id": string,
  "event_type": string       // the original unrecognized event_type
}
```

### 3.7 Error events

#### `event_type: "error"`

**When:** Multiple situations:
- JSON decode error on a WS message
- WebSocket connection error (exception during connect)
- Worker-level exception caught by `worker.py`

```
// JSON decode error:
raw: {"raw": string, "conn_id": int}
normalized: {"reason": "json_decode_error", "conn_id": int}

// WS connection error:
raw: {"error": string, "type": string, "conn_id": int}   // type = exception class name
normalized: {"reason": "ws_connection_error", "conn_id": int}

// Worker exception:
raw: {"error": string, "type": string}
normalized: {"reason": "worker_exception"}
```

### 3.8 Collector stopped

#### `event_type: "collector_stopped"`

**When:** Last event in the file (written in the `finally` block of `worker.py`).

```
raw: null
normalized: {
  "market_start": int,
  "pid": int
}
```

### 3.9 Polymarket deduplication

The collector runs `POLY_WS_CONNECTIONS` (default: 10) redundant WebSocket connections
to the same endpoint, subscribing to the same assets. Messages are deduplicated via
`orjson.dumps(msg, option=OPT_SORT_KEYS)` keyed into a `set`. Only the first occurrence
of each unique message (across all connections) is processed and written. The
`ws_disconnected` event reports `msgs_deduped` per connection.

---

## 4. Binance Event Types (depth-trade.jsonl)

### 4.1 System events (`event_type: "system"`)

#### `system_type: "worker_started"`

**When:** First event in the file.

```
raw: null
normalized: {
  "market_start": int,
  "pid": int,
  "stop_at": float
}
```

#### `system_type: "ws_connecting"`

**When:** Before each WebSocket connection attempt.

```
raw: null
normalized: {
  "url": string              // BINANCE_WS_URL (combined stream URL)
}
```

#### `system_type: "ws_connected"`

**When:** WebSocket handshake completed.

```
raw: null
normalized: null
```

#### `system_type: "ws_disconnected"`

**When:** WebSocket connection closed.

```
raw: null
normalized: null
```

#### `system_type: "ws_reconnecting"`

**When:** About to sleep before reconnecting.

```
raw: null
normalized: {
  "backoff": float
}
```

#### `system_type: "sync_ready"`

**When:** The local order book is synchronized -- either after initial snapshot+buffer
replay or after a resync. This is the signal that subsequent `depth_applied` events
are producing correct `book_state` snapshots.

```
raw: null
normalized: {
  "lastUpdateId": int         // the Binance lastUpdateId at sync point
}
```

#### `system_type: "gap_detected"`

**When:** A depth update event has `U` (first update ID) greater than
`local_update_id + 1`, meaning one or more depth events were lost.

```
raw: null
normalized: {
  "last_good_update_id": int,
  "incoming_U": int,
  "incoming_u": int
}
```

Always followed immediately by `resync_started`.

#### `system_type: "resync_started"`

**When:** Beginning a resync cycle after a gap detection. The collector re-enters
Phase 1 (buffer + snapshot) without closing the WebSocket.

```
raw: null
normalized: {
  "reason": "gap_in_update_ids"
}
```

#### `system_type: "resync_completed"`

**When:** Resync finished successfully (only on sync_cycle > 1; the initial sync
does not emit this).

```
raw: null
normalized: {
  "sync_cycle": int,          // 2, 3, etc.
  "lastUpdateId": int
}
```

#### `system_type: "strike_price"`

**When:** First `bookTicker` event received at or after `market_start` time. Emitted
exactly once per worker. Records the BTC price at the moment the prediction market opens.

```
raw: null
normalized: {
  "strike_price": string,     // midpoint as Decimal string, e.g. "96543.215"
  "strike_bid": string,       // best bid at market open
  "strike_ask": string,       // best ask at market open
  "market_start": int         // epoch seconds
}
```

**Viewer dependency:** The viewer looks for this event to determine the strike price
for fair value calculations. Falls back to finding the first bookTicker at/after
market_start if this event is absent.

### 4.2 Trade events

#### `event_type: "trade_raw"`

**When:** Every trade event from the `btcusdt@trade` stream.

```
raw: {                       // full Binance combined stream wrapper
  "stream": "btcusdt@trade",
  "data": {
    "e": "trade",            // event type
    "E": int,                // event time (ms)
    "s": "BTCUSDT",          // symbol
    "t": int,                // trade ID
    "p": string,             // price
    "q": string,             // quantity
    "b": int,                // buyer order ID
    "a": int,                // seller order ID
    "T": int,                // trade time (ms)
    "m": bool,               // is buyer market maker? (true = SELL aggressor)
    ...
  }
}
normalized: null
```

#### `event_type: "trade_normalized"`

**When:** Immediately after every `trade_raw`. Same trade, parsed into a clean format.

```
raw: null
normalized: {
  "price": string,           // trade price
  "qty": string,             // trade quantity
  "side": "BUY" | "SELL",   // aggressor side (SELL if m=true, BUY if m=false)
  "trade_id": int,
  "trade_time": int,         // exchange trade timestamp (ms)
  "event_time": int          // exchange event timestamp (ms)
}
```

**Viewer dependency:** The viewer consumes `trade_normalized` events. It uses `qty`
(parsed as float) and `side` for trade imbalance EWMA calculation.

### 4.3 Book ticker events

#### `event_type: "book_ticker_raw"`

**When:** Every best bid/ask update from the `btcusdt@bookTicker` stream.

```
raw: {                       // full combined stream wrapper
  "stream": "btcusdt@bookTicker",
  "data": {
    "u": int,                // update ID
    "s": "BTCUSDT",
    "b": string,             // best bid price
    "B": string,             // best bid qty
    "a": string,             // best ask price
    "A": string              // best ask qty
  }
}
normalized: null
```

#### `event_type: "book_ticker_normalized"`

**When:** Immediately after every `book_ticker_raw`.

During Phase 1 (pre-sync):
```
raw: null
normalized: {
  "bid_price": string,
  "bid_qty": string,
  "ask_price": string,
  "ask_qty": string,
  "update_id": int
}
```

During Phase 2 (post-sync, steady state):
```
raw: null
normalized: {
  "bid_price": string,
  "bid_qty": string,
  "ask_price": string,
  "ask_qty": string,
  "update_id": int,
  "local_best_bid": string | null,   // from local depth book
  "local_best_ask": string | null,   // from local depth book
  "consistent": bool                 // true if bookTicker matches local book BBO
}
```

The `consistent` field and `local_best_*` fields are only present after the local
book has been synced (`book.update_id > 0`). They serve as a sanity check.

**Viewer dependency:** The viewer consumes `book_ticker_normalized` for BTC price
tracking. Fields used: `bid_price`, `ask_price`, `bid_qty`, `ask_qty`, all parsed
as `float`.

### 4.4 Depth events

These implement the standard Binance depth management protocol:
1. Open WS, begin receiving depth diffs
2. Fetch REST snapshot
3. Discard diffs where `u <= lastUpdateId`
4. Verify first remaining diff spans `lastUpdateId + 1`
5. Apply diffs in sequence, verify `U <= prev_u + 1 <= u` for continuity

#### `event_type: "depth_raw_buffered"`

**When:** During Phase 1 only. Depth diff events received while waiting for the REST
snapshot to establish sync.

```
raw: { ... }                 // full combined stream wrapper with stream="btcusdt@depth@100ms"
normalized: {
  "U": int,                  // first update ID in this event
  "u": int,                  // last update ID in this event
  "buffer_size": int         // current buffer length after adding this event
}
```

#### `event_type: "depth_raw"`

**When:** During Phase 2 (steady state). Every depth diff event after sync is
established.

```
raw: { ... }                 // full combined stream wrapper
normalized: {
  "U": int,
  "u": int,
  "local_update_id": int     // book.update_id before applying this diff
}
```

Events where `u <= book.update_id` are silently dropped (stale). Events where
`U > book.update_id + 1` trigger a gap detection and resync.

#### `event_type: "depth_applied"`

**When:** After a depth diff is successfully applied to the local book. Can occur
during both Phase 1 (replaying buffered diffs after snapshot) and Phase 2
(steady-state diffs).

During Phase 1 (buffer replay):
```
raw: { ... }                 // the depth diff event data
normalized: {
  "U": int,
  "u": int,
  "local_update_id": int     // book.update_id after applying
}
```

During Phase 2 (steady state):
```
raw: null                    // raw already logged in depth_raw
normalized: {
  "U": int,
  "u": int,
  "local_update_id": int
}
```

Always followed by a `book_state` event.

### 4.5 Snapshot events

#### `event_type: "snapshot_raw"`

**When:** After successfully fetching a REST depth snapshot from
`/api/v3/depth?symbol=BTCUSDT&limit=5000`.

```
raw: {                       // full Binance REST response
  "lastUpdateId": int,
  "bids": [[price_str, qty_str], ...],   // up to 5000 levels
  "asks": [[price_str, qty_str], ...]
}
normalized: {
  "lastUpdateId": int,
  "bid_levels": int,         // len(bids)
  "ask_levels": int,         // len(asks)
  "reason": "startup" | "resync_after_gap",
  "attempt": int
}
```

#### `event_type: "snapshot_loaded"`

**When:** After loading a snapshot into the local `BinanceBook`.

```
raw: null
normalized: {
  "lastUpdateId": int,
  "buffered_remaining": int  // number of buffered diffs to replay (0 if snapshot was fresh enough)
}
```

### 4.6 Book state

#### `event_type: "book_state"`

**When:** After `snapshot_loaded` and after every `depth_applied`. This is a **full
snapshot of the local order book**. This is the primary event the viewer consumes
from `depth-trade.jsonl` for depth data.

```
raw: null
normalized: {
  "update_id": int,
  "best_bid_price": string | null,
  "best_bid_qty": string | null,
  "best_ask_price": string | null,
  "best_ask_qty": string | null,
  "spread": string | null,    // best_ask - best_bid as Decimal string
  "bid_count": int,
  "ask_count": int,
  "bids": [[price_str, qty_str], ...],   // sorted descending by price
  "asks": [[price_str, qty_str], ...],   // sorted ascending by price
  "trigger_event": {
    "U": int,                 // 0 for snapshot-only loads
    "u": int                  // lastUpdateId for snapshot-only loads
  }
}
```

**Viewer dependency:** The viewer takes `bids` (up to 2000 levels) and `asks` (up to
2000 levels), parsing prices and quantities as floats. Used for depth chart
visualization and fair value depth microprice calculation.

### 4.7 Error events

#### `event_type: "error"`

**When:** Multiple situations:

```
// Snapshot fetch HTTP error:
raw: {"status": int, "body": string}
normalized: {"reason": "snapshot_fetch_failed", "attempt": int}

// Snapshot fetch exception:
raw: {"error": string, "type": string}
normalized: {"reason": "snapshot_fetch_error", "attempt": int}

// First buffered event doesn't span snapshot:
raw: {"first_U": int, "first_u": int, "lastUpdateId": int}
normalized: {"reason": "first_event_does_not_span_snapshot"}

// WS connection error:
raw: {"error": string, "type": string}
normalized: {"reason": "ws_connection_error"}

// Worker exception:
raw: {"error": string, "type": string}
normalized: {"reason": "worker_exception"}
```

### 4.8 Collector stopped

#### `event_type: "collector_stopped"`

**When:** Last event in the file.

```
raw: null
normalized: {
  "market_start": int,
  "pid": int
}
```

---

## 5. Viewer Compatibility

The viewer (`viewer/`) is a Next.js 15 app that processes raw JSONL into pre-computed
frames at 100ms intervals, cached to `data/{epoch}/.cache/`.

### Processing pipeline

1. **Trigger:** User selects a market in the UI. If `.cache/meta.json` doesn't exist
   or `cacheVersion != 7`, processing starts.

2. **Worker:** Processing runs in a child process (`scripts/process-worker.mjs`) or
   in-process (`lib/process-market.ts`). Reads both JSONL files sequentially.

3. **Pass 1 (clob.jsonl):** Extracts:
   - `market_lookup` event -> market metadata (required; processing aborts without it)
   - `book_state` events -> Polymarket order book snapshots per asset

4. **Pass 2 (depth-trade.jsonl):** Extracts:
   - `system` with `system_type: "strike_price"` -> BTC strike price at market open
   - `book_ticker_normalized` events -> BTC best bid/ask/qty
   - `book_state` events -> full Binance depth (up to 2000 levels)
   - `trade_normalized` events -> BTC trades (qty, side)

5. **Frame generation:** Pointer-based merge of all sorted arrays into 100ms frames.
   Each frame contains the latest state of all data sources at that timestamp.

### Events consumed by the viewer

| File             | Event type              | Fields used from `normalized` |
|------------------|------------------------|-------------------------------|
| clob.jsonl       | `market_lookup`        | `market_id`, `slug`, `question`, `outcomes`, `clob_token_ids`, `end_date`, `game_start_time` |
| clob.jsonl       | `book_state`           | `asset_id`, `best_bid`, `best_ask`, `last_trade_price`, `bids`, `asks` |
| depth-trade.jsonl| `system` (strike_price)| `strike_price` |
| depth-trade.jsonl| `book_ticker_normalized`| `bid_price`, `ask_price`, `bid_qty`, `ask_qty` |
| depth-trade.jsonl| `book_state`           | `bids`, `asks` (up to 2000 levels, parsed as float) |
| depth-trade.jsonl| `trade_normalized`     | `qty`, `side` |

Also consumed from the base schema: `ts_recv_ms`, `market_start`, `event_type`,
`system_type`.

### What would break if format changes

**Hard breaks (viewer crashes or shows no data):**
- Removing or renaming `event_type` field
- Removing or renaming `market_lookup` event type
- Removing `normalized.clob_token_ids` from market_lookup (viewer cannot identify Up/Down tokens)
- Removing `normalized.bids`/`normalized.asks` from `book_state` events
- Changing `ts_recv_ms` from integer milliseconds to anything else
- Changing price/qty strings in a way that breaks `parseFloat()` (e.g., locale-specific formatting)

**Soft breaks (degraded functionality):**
- Removing `strike_price` system event: viewer falls back to computing strike from first bookTicker at market_start
- Removing `last_trade_price` from book_state: last trade lines disappear from chart
- Removing `trade_normalized`: trade imbalance EWMA goes to zero (fair value model degrades)
- Removing `spread` from Binance book_state: no impact (viewer doesn't use it)

**Safe to change (no viewer impact):**
- All `_raw` events (viewer ignores them entirely)
- `collector_pid`, `source`, `seq` fields
- System events other than `strike_price`
- `price_change_raw`, `price_change_applied` events
- `snapshot_raw`, `snapshot_loaded` events
- `depth_raw`, `depth_raw_buffered`, `depth_applied` events
- `book_ticker_raw`, `trade_raw` events
- Adding new fields to existing events (viewer uses named access, not positional)
- Adding new event types (viewer skips unknown events)

### Cache format

The `.cache/` directory contains two files:
- `meta.json`: `MarketMeta` object with epoch, slug, token IDs, timing, strike price, `cacheVersion`
- `frames.json`: Array of `Frame` objects at 100ms intervals

Cache is invalidated when `cacheVersion` (currently 7) does not match the expected
value. The Rust collector does not need to produce cache files; those are generated by
the viewer on first access.

---

## 6. Rust Rewrite Recommendations

### 6.1 Serialization format

**Recommendation: Keep JSON for viewer compatibility, add optional binary layer later.**

| Format      | Write speed | Size    | Viewer compat | Notes |
|-------------|-------------|---------|---------------|-------|
| JSON (serde_json) | ~300 MB/s | 1x (baseline) | Native | Current format. serde_json is fast enough for this throughput. |
| JSON (simd-json) | ~600 MB/s | 1x | Native | Faster parsing than serde_json, useful for read path. |
| MessagePack (rmp-serde) | ~500 MB/s | 0.6-0.7x | Needs converter | 30-40% smaller. Good for archival. |
| FlatBuffers | ~1 GB/s | 0.5x | Needs converter | Zero-copy reads, best for real-time consumption by trading engine. Schema evolution is awkward. |
| Cap'n Proto | ~1 GB/s | 0.5-0.6x | Needs converter | Zero-copy. Better schema evolution than FlatBuffers. |

The `book_state` events dominate file size (full book snapshot per depth update). For the
collector, JSON serialization is not the bottleneck -- the WS recv and book computation are.
`serde_json` at ~300 MB/s write throughput handles the ~5 MB/s peak write rate with 60x headroom.

**Recommended approach:**

Phase 1: Write JSON with `serde_json`, matching the current schema exactly. The viewer works
unchanged. Use `#[serde(skip_serializing_if = "Option::is_none")]` for optional fields like
`system_type`.

Phase 2 (when latency matters for the trading engine): Add a parallel binary output using
`rkyv` (zero-copy deserialization) or Cap'n Proto for the hot path (trading engine consumption).
Keep JSON for the viewer/archival path.

### 6.2 Schema definition with serde

```rust
use serde::Serialize;

/// Base line written to every JSONL file.
#[derive(Serialize)]
struct Line<R: Serialize, N: Serialize> {
    ts_recv_ms: i64,
    ts_recv_unix: f64,
    market_start: i64,
    collector_pid: u32,
    source: &'static str,        // "polymarket" or "binance"
    event_type: &'static str,
    seq: u64,
    raw: Option<R>,
    normalized: Option<N>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_type: Option<&'static str>,
}
```

Use an enum for event types with serde tag dispatch:

```rust
#[derive(Serialize)]
#[serde(tag = "event_type")]
enum PolymarketEvent {
    #[serde(rename = "system")]
    System { system_type: SystemType, ... },
    #[serde(rename = "book_state")]
    BookState { normalized: PolyBookState },
    #[serde(rename = "book_raw")]
    BookRaw { raw: serde_json::Value },
    // ...
}
```

However, the tagged enum approach changes the JSON structure. To maintain exact
compatibility with the current schema, use the flat `Line<R, N>` struct approach and
match on event_type in the read path rather than using serde's tag dispatch for the
write path.

**Important type mappings:**

| Python type | Rust type | Notes |
|-------------|-----------|-------|
| `int(time.time() * 1000)` | `i64` | Fits in i64 until year 292277026 |
| `time.time() * 1000 / 1000.0` | `f64` | |
| `os.getpid()` | `u32` | |
| `SeqCounter._v` | `u64` | |
| Price/qty strings (`"96543.21"`) | `String` or `&str` | Keep as strings; do NOT serialize as floats |
| `Decimal` (Binance internal) | `rust_decimal::Decimal` | Serialize with `.to_string()` into the string field |
| `dict` (raw WS messages) | `serde_json::Value` | Preserve upstream JSON exactly |
| `list[list[str, str]]` (bids/asks) | `Vec<[String; 2]>` or `Vec<(String, String)>` | |

### 6.3 Maintaining JSON compatibility

**Keep JSON as the primary format.** The reasons:

1. The viewer reads JSON directly. Changing the format requires rewriting the viewer
   processing pipeline.
2. Human debuggability. When a trade goes wrong at 3 AM, you want to `grep` and `jq`
   the JSONL files, not decode binary.
3. The collector is I/O-bound on WebSocket recv, not on serialization. JSON overhead
   is not a bottleneck.
4. orjson (Python) and serde_json (Rust) produce byte-identical output for the same
   input when configured the same way (no pretty-printing, no key sorting).

**Compatibility requirements for the Rust writer:**

- Use `serde_json::to_vec()` (not `to_string()`) for the same bytes-not-string
  approach as orjson.
- Append `b"\n"` after each line.
- Do NOT sort keys (orjson default is insertion order; serde_json preserves struct field
  order).
- `system_type` must be **omitted** (not `null`) when not applicable. Use
  `#[serde(skip_serializing_if = "Option::is_none")]`.
- Prices and quantities must remain as **strings**, not numbers. The viewer calls
  `parseFloat()` on them; if you write `96543.21` as a JSON number, it works, but
  `"0"` as a number becomes `0` and the book level removal logic (`size == "0"`)
  would need to change.

### 6.4 Compression for ~1.3 TB/day

At 288 markets/day (24h * 12 per hour) with ~1-3 GB per market, daily volume is
roughly 0.3-0.9 TB uncompressed. With `book_state` on every depth tick, the upper
bound is around 1.3 TB/day.

**Recommended compression strategy:**

| Layer | Approach | Ratio | CPU cost | Notes |
|-------|----------|-------|----------|-------|
| **Hot (live)** | No compression | 1x | 0 | Write raw JSONL during collection. Compression adds latency and complexity to the write path. |
| **Warm (recent)** | zstd --level 3 | 8-12x | Low | Compress completed market folders after collection ends. zstd level 3 is fast enough to run inline. |
| **Cold (archive)** | zstd --level 19 | 15-25x | High | Batch compress older data. JSONL compresses extremely well due to repetitive structure. |
| **Pruning** | Drop `raw` fields | 0.3-0.5x pre-compression | 0 | The `raw` fields duplicate information already in `normalized`. A post-processing pass that strips `raw` from `book_state`, `depth_applied`, `book_ticker_normalized`, and `trade_normalized` events would cut file sizes roughly in half. |

**Implementation notes:**

- Use `zstd` crate with streaming compression. A background thread can compress
  completed JSONL files while the next market is being collected.
- Consider writing directly to zstd-compressed JSONL for the cold path:
  `zstd::stream::write::Encoder` wrapping a `BufWriter<File>`. This requires the reader
  to decompress, but the viewer already does a full sequential read.
- For the trading engine (future), the dominant data reduction is to **not write
  `book_state` on every depth tick**. Instead, maintain the book in memory and only
  write the diffs (`depth_applied`). The full book snapshot can be reconstructed from
  `snapshot_loaded` + all subsequent `depth_applied` events. This alone would reduce
  `depth-trade.jsonl` by 70-80%.

**Storage projections with compression:**

| Scenario | Daily volume | Monthly volume |
|----------|-------------|---------------|
| Uncompressed (current) | ~0.5-1.3 TB | 15-39 TB |
| zstd level 3 | ~50-130 GB | 1.5-3.9 TB |
| zstd level 3 + prune raw | ~15-50 GB | 0.5-1.5 TB |
| zstd level 3 + prune raw + drop book_state (keep diffs only) | ~5-15 GB | 0.15-0.45 TB |

---

## Appendix A: Complete Event Type Matrix

### clob.jsonl

| event_type | system_type | raw | normalized | Frequency | Viewer uses |
|------------|-------------|-----|------------|-----------|-------------|
| `system` | `worker_started` | null | market_start, pid, stop_at | 1/market | No |
| `system` | `ws_connecting` | null | url, conn_id | N/market (per conn, per reconnect) | No |
| `system` | `ws_connected` | null | conn_id | N/market | No |
| `system` | `ws_subscribed` | subscription payload | token_ids, conn_id | N/market | No |
| `system` | `ws_disconnected` | null | conn_id, msgs_received, msgs_deduped | N/market | No |
| `system` | `ws_reconnecting` | null | backoff, conn_id | N/market | No |
| `market_lookup` | - | full Gamma response | market_id, slug, question, outcomes, clob_token_ids, end_date, game_start_time | 1/market | **Yes** (critical) |
| `market_lookup_failed` | - | Gamma response or slug | slug, reason | 0-1/market | No |
| `market_lookup_retry` | - | status+body or error | slug, attempt, status/error | 0-N/market | No |
| `book_raw` | - | full WS book message | asset_id, event_type | ~10-50/market | No |
| `book_state` | - | null | bid_count, ask_count, bids, asks, best_bid, best_ask, asset_id, last_trade_price, trigger_event_type | ~1K-50K/market | **Yes** (primary) |
| `price_change_raw` | - | full WS price_change message | asset_id, event_type | ~500-25K/market | No |
| `price_change_applied` | - | null | asset_id, changes_count | ~500-25K/market | No |
| `last_trade_price_raw` | - | full WS message | asset_id, price | ~100-5K/market | No |
| `best_bid_ask_raw` | - | full WS message | asset_id | ~100-5K/market | No |
| `tick_size_change_raw` | - | full WS message | asset_id | 0-1/market | No |
| `market_resolved_raw` | - | full WS message | asset_id | 0-1/market | No |
| `error` | - | varies | reason (+ conn_id) | 0-N/market | No |
| `collector_stopped` | - | null | market_start, pid | 1/market | No |

### depth-trade.jsonl

| event_type | system_type | raw | normalized | Frequency | Viewer uses |
|------------|-------------|-----|------------|-----------|-------------|
| `system` | `worker_started` | null | market_start, pid, stop_at | 1/market | No |
| `system` | `ws_connecting` | null | url | 1+/market | No |
| `system` | `ws_connected` | null | null | 1+/market | No |
| `system` | `ws_disconnected` | null | null | 0+/market | No |
| `system` | `ws_reconnecting` | null | backoff | 0+/market | No |
| `system` | `sync_ready` | null | lastUpdateId | 1+/market | No |
| `system` | `gap_detected` | null | last_good_update_id, incoming_U, incoming_u | 0+/market | No |
| `system` | `resync_started` | null | reason | 0+/market | No |
| `system` | `resync_completed` | null | sync_cycle, lastUpdateId | 0+/market | No |
| `system` | `strike_price` | null | strike_price, strike_bid, strike_ask, market_start | 0-1/market | **Yes** |
| `trade_raw` | - | full combined stream wrapper | null | ~5K-50K/market | No |
| `trade_normalized` | - | null | price, qty, side, trade_id, trade_time, event_time | ~5K-50K/market | **Yes** |
| `book_ticker_raw` | - | full combined stream wrapper | null | ~2K-10K/market | No |
| `book_ticker_normalized` | - | null | bid_price, bid_qty, ask_price, ask_qty, update_id (+optional local_best_bid, local_best_ask, consistent) | ~2K-10K/market | **Yes** (primary) |
| `depth_raw_buffered` | - | full combined stream wrapper | U, u, buffer_size | ~1-50/market | No |
| `depth_raw` | - | full combined stream wrapper | U, u, local_update_id | ~2K-4K/market | No |
| `depth_applied` | - | diff data or null | U, u, local_update_id | ~2K-4K/market | No |
| `book_state` | - | null | update_id, best_bid_price, best_bid_qty, best_ask_price, best_ask_qty, spread, bid_count, ask_count, bids, asks, trigger_event | ~2K-4K/market | **Yes** (depth) |
| `snapshot_raw` | - | full REST response (large) | lastUpdateId, bid_levels, ask_levels, reason, attempt | 1+/market | No |
| `snapshot_loaded` | - | null | lastUpdateId, buffered_remaining | 1+/market | No |
| `error` | - | varies | reason (+attempt) | 0-N/market | No |
| `collector_stopped` | - | null | market_start, pid | 1/market | No |
