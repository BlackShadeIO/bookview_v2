# 03 - Binance BTCUSDT Collector

Reference documentation for `collector/binance.py`. Written for an engineer reimplementing this in Rust.

Source file: `collector/binance.py` (~515 lines)
Config: `collector/config.py`
Output file: `data/{epoch}/depth-trade.jsonl`

---

## 1. Two-Phase Depth Sync Protocol

The collector implements Binance's official depth management protocol (documented at https://developers.binance.com/docs/binance-spot-api-docs/web-socket-streams#how-to-manage-a-local-order-book-correctly). The core problem: the REST snapshot and the WebSocket stream are independent. There is no atomic "subscribe and get the current state" operation. The protocol bridges this gap.

### 1.1 Phase 1: Buffer + Snapshot Sync

**Entry point:** `_run_sync_loop()`, lines 197-514.

On each sync cycle (initial connection or resync after gap):

1. A fresh `BinanceBook` is created and a `buffer: list[dict]` is initialized empty.
2. The WS is already connected and streaming. All incoming `depth` events are appended to `buffer` and written as `depth_raw_buffered` events. Trade and bookTicker events are processed normally (written to output, not buffered).
3. A REST snapshot fetch is triggered on the **first** depth event received (`len(buffer) == 1`), or on every subsequent depth event if a previous snapshot attempt failed (`snapshot_attempt > 0`). This means the collector does not pre-fetch the snapshot before receiving any WS data -- it waits for at least one depth update to arrive, ensuring the buffer contains events that can validate the snapshot.

**Snapshot validation** proceeds as follows after a successful REST fetch:

```
last_uid = snap["lastUpdateId"]
```

**Case A -- Snapshot too old:**
```python
if last_uid < buffer[0]["U"]:
    # Snapshot predates all buffered events.
    # Discard snapshot, retry on next depth event.
    continue
```
The snapshot's `lastUpdateId` is older than the first update ID (`U`) of the first buffered event. The snapshot cannot be used because applying the buffered events would skip updates. The collector discards this snapshot and fetches a new one on the next incoming depth event.

**Case B -- Snapshot covers all buffered events:**
```python
buffer = [e for e in buffer if e["u"] > last_uid]
```
After filtering, if `buffer` is empty, all buffered events are stale (the snapshot already includes them). The book is loaded from the snapshot directly, a `book_state` is emitted, and sync is complete.

**Case C -- Snapshot falls within buffered events (the normal case):**

After the stale-event filter, `buffer` still has entries. The first remaining event must satisfy:

```python
first["U"] <= last_uid + 1 <= first["u"]
```

This is the critical validation condition. Breaking it down:

- `U` = "First update ID in event" (the lowest update ID included in this diff)
- `u` = "Final update ID in event" (the highest update ID included in this diff)
- `lastUpdateId + 1` = the next update the book needs

The condition checks that the first buffered event's range **spans** the snapshot's boundary. If `lastUpdateId + 1` falls within `[U, u]` of the first event, then applying this event (and all subsequent ones) will produce a continuous sequence with no gaps from the snapshot.

**If the condition fails:** the snapshot and buffer are incompatible. The buffer is cleared and the cycle retries from scratch (new buffer, new snapshot).

**If the condition passes:** 
1. `book.load_snapshot(snap)` initializes the order book from the REST data.
2. A `snapshot_loaded` event is written with metadata.
3. A `book_state` event captures the post-snapshot state.
4. Each buffered event is applied via `book.apply_diff(evt)` and both `depth_applied` and `book_state` events are emitted.
5. The buffer is cleared, `sync_ready` system event is emitted, and the collector transitions to Phase 2.

### 1.2 Why This Protocol Exists

Binance depth WebSocket streams are **diff streams**, not full snapshots. Each message contains only the price levels that changed. Without a starting point (the REST snapshot), diffs are meaningless. But the REST API and the WS stream are not synchronized -- the snapshot could be from any point in time. The buffering protocol ensures no updates are lost between the snapshot fetch and the live stream, guaranteeing the local book is a faithful replica of the exchange's book.

### 1.3 Sync Cycle Counter

The variable `sync_cycle` (starts at 1) tracks how many times the sync protocol has run on a single WS connection. On the first sync (`sync_cycle == 1`), the `reason` for the snapshot fetch is `"startup"`. On subsequent syncs (after gap-triggered resyncs), it is `"resync_after_gap"`. When `sync_cycle > 1`, an additional `resync_completed` system event is emitted.

---

## 2. BinanceBook

Defined at lines 27-81. A minimal order book implementation.

### 2.1 Internal Representation

```python
self.bids: dict[str, str] = {}  # price_string -> qty_string
self.asks: dict[str, str] = {}  # price_string -> qty_string
self.update_id: int = 0
```

Both prices and quantities are stored as **strings**. Decimal conversion happens only at comparison/arithmetic boundaries. The `update_id` tracks the last applied event's `u` value (final update ID).

### 2.2 Why Decimal, Not Float

Binance prices like `"96543.21"` and quantities like `"0.00001"` require exact decimal representation. IEEE 754 floats introduce rounding errors:

```
>>> 0.1 + 0.2
0.30000000000000004
```

For an HFT system, these rounding errors compound in spread calculations and price comparisons. The `Decimal` type provides exact decimal arithmetic. The Python collector uses `Decimal` only for:
- Zero-quantity checks in `apply_diff()`: `Decimal(q) == 0`
- Sorting bids/asks by price in `snapshot()`: `key=lambda x: Decimal(x[0])`
- Spread calculation: `Decimal(ba_price) - Decimal(bb_price)`
- Strike price midpoint: `(Decimal(bid) + Decimal(ask)) / 2`

Storing as strings and converting to Decimal only when needed avoids the overhead of Decimal objects in the hot path (dict lookups are string-keyed).

### 2.3 `load_snapshot(snap: dict)`

```python
def load_snapshot(self, snap: dict) -> None:
    self.bids = {str(p): str(q) for p, q in snap["bids"]}
    self.asks = {str(p): str(q) for p, q in snap["asks"]}
    self.update_id = snap["lastUpdateId"]
```

Replaces the entire book. `snap["bids"]` and `snap["asks"]` are lists of `[price, qty]` pairs from the REST API. Both values are coerced to strings. Sets `update_id` to the snapshot's `lastUpdateId`.

### 2.4 `apply_diff(event: dict)`

```python
def apply_diff(self, event: dict) -> None:
    for p, q in event.get("b", []):
        p, q = str(p), str(q)
        if q == "0" or Decimal(q) == 0:
            self.bids.pop(p, None)
        else:
            self.bids[p] = q
    for p, q in event.get("a", []):
        p, q = str(p), str(q)
        if q == "0" or Decimal(q) == 0:
            self.asks.pop(p, None)
        else:
            self.asks[p] = q
    self.update_id = event["u"]
```

Applies a depth diff event. For each bid/ask update:
- If quantity is zero (checked both as string `"0"` and via Decimal for values like `"0.00000000"`), the price level is **removed** from the book.
- Otherwise, the price level is **inserted or updated** (upsert).
- `update_id` is set to the event's final update ID (`u`).

The dual zero check (`q == "0" or Decimal(q) == 0`) handles both the common case (`"0"`) and edge cases like `"0.00000000"` that Binance occasionally sends.

### 2.5 `snapshot(trigger_U: int, trigger_u: int) -> dict`

Produces a full book state dictionary. Used for `book_state` output events.

**Sorting:**
- Bids: sorted by price **descending** (highest bid first) via `Decimal` key
- Asks: sorted by price **ascending** (lowest ask first) via `Decimal` key

**Output fields:**

| Field | Type | Description |
|-------|------|-------------|
| `update_id` | int | Current book update ID |
| `best_bid_price` | str or None | Highest bid price |
| `best_bid_qty` | str or None | Quantity at best bid |
| `best_ask_price` | str or None | Lowest ask price |
| `best_ask_qty` | str or None | Quantity at best ask |
| `spread` | str or None | `best_ask - best_bid` as Decimal string |
| `bid_count` | int | Total number of bid price levels |
| `ask_count` | int | Total number of ask price levels |
| `bids` | list[list[str, str]] | All bid levels `[[price, qty], ...]`, descending |
| `asks` | list[list[str, str]] | All ask levels `[[price, qty], ...]`, ascending |
| `trigger_event` | dict | `{"U": trigger_U, "u": trigger_u}` -- the event that caused this snapshot |

The `trigger_U` and `trigger_u` parameters record which depth event triggered this snapshot. When emitted after `load_snapshot` (before applying buffered diffs), these are set to `(0, lastUpdateId)`.

---

## 3. Steady-State Streaming (Phase 2)

After sync completes, the collector enters the steady-state loop (lines 389-514). Same WS, same message parsing, but depth events are now applied directly to the book instead of buffered.

### 3.1 Depth Event Processing

For each depth event in steady state:

```python
evt_U = data.get("U", 0)  # first update ID in event
evt_u = data.get("u", 0)  # last update ID in event
```

Three cases:

**Normal:** `evt_U <= book.update_id + 1` and `evt_u > book.update_id`
- Apply diff, emit `depth_applied` + `book_state`.

**Stale event:** `evt_u <= book.update_id`
```python
if evt_u <= book.update_id:
    continue  # silently skip, already applied
```
This handles events that the book has already incorporated (e.g., events that overlapped with the snapshot). No output is written.

**Gap detected:** `evt_U > book.update_id + 1`
```python
if evt_U > book.update_id + 1:
    # emit gap_detected + resync_started system events
    gap_hit = True
    continue
```
The incoming event's starting update ID is **ahead** of where the book left off. One or more events were missed (network drop, WS buffer overflow, etc.). The local book is now unreliable.

### 3.2 Automatic Resync Without WS Reconnection

When a gap is detected:
1. `gap_detected` system event is emitted with `last_good_update_id`, `incoming_U`, `incoming_u`.
2. `resync_started` system event is emitted.
3. `gap_hit = True` breaks out of the steady-state loop.
4. The outer `while not stop_event.is_set()` loop in `_run_sync_loop` catches this and **re-enters Phase 1** on the **same WebSocket connection**. No reconnect needed.
5. A new `BinanceBook` and empty `buffer` are created. The sync cycle counter increments.
6. The full buffer-snapshot-validate cycle runs again.

This is a key design decision: the WS connection is fine (it did not close), only the book state is stale. Reconnecting would waste time and risk missing even more data. Instead, the collector just resyncs the book state on the existing stream.

### 3.3 Steady-State bookTicker Consistency Check

In steady state (but not during Phase 1), the `book_ticker_normalized` event includes extra fields when the book is synced (`book.update_id > 0`):

```python
normalized_ticker["local_best_bid"] = bsnap["best_bid_price"]
normalized_ticker["local_best_ask"] = bsnap["best_ask_price"]
normalized_ticker["consistent"] = (
    bsnap["best_bid_price"] == data.get("b")
    and bsnap["best_ask_price"] == data.get("a")
)
```

This cross-references the local book's best bid/ask against Binance's `bookTicker` best bid/ask, providing a real-time consistency check. A `consistent: false` value suggests the local book may be out of sync (stale diff, missed update, etc.).

---

## 4. Combined WebSocket Stream

### 4.1 URL and Streams

```python
BINANCE_WS_URL = (
    "wss://stream.binance.com:9443/stream"
    "?streams=btcusdt@bookTicker/btcusdt@depth@100ms/btcusdt@trade"
)
```

A single WebSocket connection carries three streams via Binance's combined stream endpoint:

| Stream | Update Frequency | Purpose |
|--------|-----------------|---------|
| `btcusdt@bookTicker` | Real-time (every BBO change) | Best bid/ask price and quantity |
| `btcusdt@depth@100ms` | Every 100ms | Order book depth diffs (all changed levels) |
| `btcusdt@trade` | Real-time (every trade) | Individual trade executions |

### 4.2 Message Format and Stream Routing

Combined stream messages are wrapped:

```json
{
  "stream": "btcusdt@depth@100ms",
  "data": { ... }
}
```

Routing logic extracts the `stream` field and dispatches:

```python
stream = wrapper.get("stream", "")
data = wrapper.get("data", {})

if stream.endswith("@trade"):
    # trade handling
elif "depth" in stream and "bookTicker" not in stream:
    # depth handling (Phase 1 only: "bookTicker" guard prevents misrouting)
elif "bookTicker" in stream:
    # bookTicker handling
```

Note the routing order and guards:
- Trade: matched by suffix `@trade`
- Depth: matched by substring `"depth"` with explicit exclusion of `"bookTicker"` (defensive, since `bookTicker` contains neither, but guards against future stream name collisions)
- BookTicker: matched by substring `"bookTicker"`

In Phase 1, the depth check uses `"depth" in stream and "bookTicker" not in stream`. In Phase 2, it simplifies to `"depth" in stream` since the bookTicker branch is checked first with `elif`.

### 4.3 Why a Single Connection

Binance rate-limits WebSocket connections. A combined stream uses one connection for all three data types, reducing connection overhead and avoiding rate limits. All messages arrive on the same socket and are demultiplexed by the `stream` field.

---

## 5. Strike Price Recording

### 5.1 Purpose

The prediction market is "BTC Up/Down 5m" -- will BTC be above or below its price at market open after 5 minutes? The "strike price" is the BTC price at the moment the market starts (the `market_start` epoch). This is captured from the Binance bookTicker feed.

### 5.2 The `strike_state` Flag Pattern

```python
strike_state = [False]  # mutable container, shared across reconnects
```

Defined in `run_binance()` (the outer function), `strike_state` is a one-element list used as a mutable boolean. It is passed into `_run_sync_loop()` by reference. This ensures:
- The strike is recorded **exactly once** per market, even across WS reconnections or resyncs.
- Using a list `[False]` instead of a bare `bool` allows mutation inside the inner function without `nonlocal`.

### 5.3 When the Strike is Recorded

The strike recording logic appears in **both** Phase 1 and Phase 2 bookTicker handlers (identical code in both locations):

```python
if not strike_state[0] and time.time() >= market_start:
    bid = data.get("b", "0")
    ask = data.get("a", "0")
    mid = (Decimal(bid) + Decimal(ask)) / 2
    write_line(f, make_line(
        market_start, "binance", "system", seq.next(),
        None, {
            "strike_price": str(mid),
            "strike_bid": bid,
            "strike_ask": ask,
            "market_start": market_start,
        },
        system_type="strike_price",
    ))
    strike_state[0] = True
```

Conditions:
1. `strike_state[0]` is `False` (not yet recorded)
2. `time.time() >= market_start` (wall clock has passed the market start epoch)

The strike price is the **midpoint** of the best bid and ask at that moment: `(bid + ask) / 2`, computed with `Decimal` for precision.

The strike is recorded from `bookTicker`, not from trades or depth, because `bookTicker` updates on every BBO change and provides the tightest spread information at the exact moment needed.

Since collection starts `PRE_START_SECONDS = 70` seconds before market start, the collector is already streaming when the market opens. The first bookTicker event received after `market_start` triggers the strike recording.

---

## 6. REST Snapshot Fetching

### 6.1 URL and Parameters

```python
BINANCE_DEPTH_SNAPSHOT_URL = (
    "https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=5000"
)
```

Fetches up to 5000 price levels on each side (bids and asks). This is the maximum allowed by Binance's API. The large limit ensures the local book has deep coverage.

### 6.2 Function Signature

```python
async def fetch_snapshot(
    session: aiohttp.ClientSession,
    market_start: int,
    f: LineWriter,
    seq: SeqCounter,
    reason: str,       # "startup" or "resync_after_gap"
    attempt: int,       # monotonic attempt counter per sync cycle
) -> dict | None:
```

Returns the parsed JSON dict on success, `None` on failure.

### 6.3 Response Fields

The REST response contains:

```json
{
  "lastUpdateId": 123456789,
  "bids": [["96500.00", "1.234"], ...],
  "asks": [["96500.01", "0.567"], ...]
}
```

### 6.4 Error Handling

**HTTP error (non-200 status):**
- Writes an `error` event with `status` and response `body` in `raw`, and `reason: "snapshot_fetch_failed"` with `attempt` number in `normalized`.
- Returns `None`.

**Exception (network error, timeout, JSON parse failure, etc.):**
- Writes an `error` event with `error` message and exception `type` in `raw`, and `reason: "snapshot_fetch_error"` with `attempt` in `normalized`.
- Returns `None`.

**Success:**
- Writes a `snapshot_raw` event containing the full snapshot in `raw` and metadata (`lastUpdateId`, `bid_levels`, `ask_levels`, `reason`, `attempt`) in `normalized`.
- Returns the parsed dict.

### 6.5 Retry Logic

There is no explicit retry loop in `fetch_snapshot()` itself. Retry behavior is implicit: when `fetch_snapshot` returns `None`, the caller in Phase 1 sets `snapshot_attempt > 0`, which causes the next incoming depth event to trigger another fetch attempt. This means retries are naturally paced by the depth stream (one retry per ~100ms depth event). The `snapshot_attempt` counter increments monotonically within each sync cycle and is logged with each attempt.

### 6.6 aiohttp Session Lifecycle

The `aiohttp.ClientSession` is created inside `_run_sync_loop()` with `async with`, so it is scoped to the lifetime of a single WS connection's sync loop. It is reused across multiple snapshot fetches (initial + retries + resyncs) within that scope.

---

## 7. WebSocket Connection Management

### 7.1 Connection Loop

The outer `run_binance()` function uses `websockets`' auto-reconnect pattern:

```python
async for ws in connect(BINANCE_WS_URL, open_timeout=10):
```

The `connect()` async iterator yields a new connection each time the previous one closes. On each connection:
1. `ws_connected` system event is emitted.
2. Reconnect backoff resets to `WS_RECONNECT_BASE = 1.0` seconds.
3. `_run_sync_loop()` runs until it returns (stop event, WS close, or unrecoverable error).
4. If not stopped, `ws_disconnected` and `ws_reconnecting` events are emitted.

### 7.2 Reconnect Backoff

On connection-level exceptions (not `ConnectionClosed`, which is handled by the `async for` iterator):

```python
reconnect_backoff = WS_RECONNECT_BASE  # 1.0 seconds
# ...
await asyncio.sleep(reconnect_backoff)
reconnect_backoff = min(reconnect_backoff * 2, WS_RECONNECT_MAX)  # caps at 16.0
```

Exponential backoff: 1s, 2s, 4s, 8s, 16s, 16s, 16s...
Reset to 1s on successful connection.

### 7.3 Graceful Shutdown

The `stop_event: asyncio.Event` is checked:
- Before each iteration of the outer reconnect loop
- Before each iteration of Phase 1 and Phase 2 inner loops
- After `ConnectionClosed` exceptions
- After connection errors, before sleeping for backoff

When `stop_event` is set (by the worker's timer task after `MARKET_DURATION + TAIL_SECONDS`), all loops exit cleanly.

---

## 8. Complete Event Type Reference

Every event written to `depth-trade.jsonl` by the Binance collector. All events share the base JSONL structure from `make_line()`:

```json
{
  "ts_recv_ms": 1714500000000,
  "ts_recv_unix": 1714500000.000,
  "market_start": 1714500000,
  "collector_pid": 12345,
  "source": "binance",
  "event_type": "...",
  "seq": 1,
  "raw": null or {...},
  "normalized": null or {...},
  "system_type": "..."
}
```

### 8.1 Data Events

#### `trade_raw`
- **When:** Every trade from the `btcusdt@trade` stream (both phases).
- **`raw`:** Full Binance combined stream wrapper `{"stream": "btcusdt@trade", "data": {...}}`.
- **`normalized`:** `null`

#### `trade_normalized`
- **When:** Immediately after each `trade_raw`.
- **`raw`:** `null`
- **`normalized`:**

| Field | Source | Description |
|-------|--------|-------------|
| `price` | `data.p` | Trade price (string) |
| `qty` | `data.q` | Trade quantity (string) |
| `side` | `data.m` | `"SELL"` if `m` is true (buyer is market maker = seller-initiated), `"BUY"` if false |
| `trade_id` | `data.t` | Binance trade ID |
| `trade_time` | `data.T` | Trade execution time (ms epoch) |
| `event_time` | `data.E` | Event time (ms epoch) |

Note on `side`: Binance's `m` field means "Is the buyer the market maker?" When true, the trade was initiated by a sell order hitting a resting buy order, so the aggressor side is SELL.

#### `book_ticker_raw`
- **When:** Every bookTicker update (both phases).
- **`raw`:** Full combined stream wrapper.
- **`normalized`:** `null`

#### `book_ticker_normalized`
- **When:** Immediately after each `book_ticker_raw`.
- **`raw`:** `null`
- **`normalized`:**

| Field | Source | Description |
|-------|--------|-------------|
| `bid_price` | `data.b` | Best bid price (string) |
| `bid_qty` | `data.B` | Best bid quantity (string) |
| `ask_price` | `data.a` | Best ask price (string) |
| `ask_qty` | `data.A` | Best ask quantity (string) |
| `update_id` | `data.u` | Binance update ID |
| `local_best_bid` | book | (Phase 2 only, if synced) Local book's best bid |
| `local_best_ask` | book | (Phase 2 only, if synced) Local book's best ask |
| `consistent` | computed | (Phase 2 only, if synced) `true` if local BBO matches Binance BBO |

#### `depth_raw_buffered`
- **When:** Phase 1 only. Each depth event received while buffering for sync.
- **`raw`:** Full combined stream wrapper.
- **`normalized`:**

| Field | Source | Description |
|-------|--------|-------------|
| `U` | `data.U` | First update ID in this event |
| `u` | `data.u` | Final update ID in this event |
| `buffer_size` | computed | Current buffer size after this addition |

#### `depth_raw`
- **When:** Phase 2 only. Each depth event in steady state.
- **`raw`:** Full combined stream wrapper.
- **`normalized`:**

| Field | Source | Description |
|-------|--------|-------------|
| `U` | `data.U` | First update ID in this event |
| `u` | `data.u` | Final update ID in this event |
| `local_update_id` | `book.update_id` | Book's current update ID before applying |

#### `depth_applied`
- **When:** After successfully applying a depth diff to the book (both phases -- after sync in Phase 1, and in Phase 2).
- **`raw`:** In Phase 1 (buffered apply): the full depth event dict. In Phase 2: `null`.
- **`normalized`:**

| Field | Source | Description |
|-------|--------|-------------|
| `U` | event | First update ID |
| `u` | event | Final update ID |
| `local_update_id` | `book.update_id` | Book's update ID after applying |

#### `book_state`
- **When:** After every `depth_applied`, and after `snapshot_loaded`.
- **`raw`:** `null`
- **`normalized`:** Output of `book.snapshot(trigger_U, trigger_u)` -- see Section 2.5 for full field listing.

#### `snapshot_raw`
- **When:** After successful REST snapshot fetch.
- **`raw`:** Full REST response (contains `lastUpdateId`, `bids`, `asks` with up to 5000 levels each).
- **`normalized`:**

| Field | Source | Description |
|-------|--------|-------------|
| `lastUpdateId` | response | Snapshot's update ID |
| `bid_levels` | computed | Number of bid levels |
| `ask_levels` | computed | Number of ask levels |
| `reason` | param | `"startup"` or `"resync_after_gap"` |
| `attempt` | param | Attempt number within this sync cycle |

#### `snapshot_loaded`
- **When:** After book is initialized from snapshot.
- **`raw`:** `null`
- **`normalized`:**

| Field | Source | Description |
|-------|--------|-------------|
| `lastUpdateId` | snapshot | The snapshot's update ID |
| `buffered_remaining` | computed | Number of buffered events to apply after snapshot |

### 8.2 System Events (event_type = "system")

All have `"event_type": "system"` and a `"system_type"` field.

| system_type | normalized fields | Description |
|-------------|------------------|-------------|
| `ws_connecting` | `url` | About to open WS connection |
| `ws_connected` | (none) | WS connection established |
| `ws_disconnected` | (none) | WS connection closed |
| `ws_reconnecting` | `backoff` | About to reconnect after delay |
| `sync_ready` | `lastUpdateId` | Book is synced and ready for steady-state |
| `resync_completed` | `sync_cycle`, `lastUpdateId` | Resync finished (only when `sync_cycle > 1`) |
| `gap_detected` | `last_good_update_id`, `incoming_U`, `incoming_u` | Sequence gap found in depth stream |
| `resync_started` | `reason` | Beginning resync (reason: `"gap_in_update_ids"`) |
| `strike_price` | `strike_price`, `strike_bid`, `strike_ask`, `market_start` | BTC strike price at market open |

### 8.3 Error Events (event_type = "error")

| normalized.reason | Description |
|-------------------|-------------|
| `snapshot_fetch_failed` | REST returned non-200. `raw` has `status`, `body`. |
| `snapshot_fetch_error` | Exception during fetch. `raw` has `error`, `type`. |
| `first_event_does_not_span_snapshot` | Validation `U <= lastUpdateId+1 <= u` failed. `raw` has `first_U`, `first_u`, `lastUpdateId`. |
| `ws_connection_error` | WS connection exception. `raw` has `error`, `type`. |

---

## 9. State Machine Summary

```
                    +-----------+
         +--------->|  CONNECT  |
         |          +-----+-----+
         |                |
   reconnect         ws connected
   (backoff)              |
         |          +-----v-----+
         |          |  PHASE 1  |<--------+
         |          |  BUFFER   |         |
         |          +-----+-----+    gap detected
         |                |         (resync on
         |           sync done      same WS)
         |                |              |
         |          +-----v-----+        |
         |          |  PHASE 2  |--------+
         |          |  STEADY   |
         |          +-----+-----+
         |                |
         |       ws closed / error
         |                |
         +----------------+

    stop_event -> exits from any state
```

---

## 10. Rust Implementation Guidance

### 10.1 Decimal Arithmetic

**Python:** `decimal.Decimal` -- arbitrary precision, slow.

**Rust options:**

1. **`rust_decimal` crate** (recommended): 128-bit decimal, 28-29 significant digits. More than enough for BTC prices (8 decimal places) and quantities. Implements `Ord` so it works directly as a `BTreeMap` key.

2. **Fixed-point integer arithmetic:** Store prices as `i64` in satoshi-like units (e.g., multiply by 10^8). Fastest option, but requires careful scaling at I/O boundaries. Good if you control serialization.

3. **`bigdecimal` crate:** Arbitrary precision like Python's Decimal. Overkill for this use case.

Recommendation: `rust_decimal` for the book implementation. It balances precision, performance, and ergonomics. For the hot path (applying diffs), string-keyed `HashMap` like the Python version is actually fine -- only convert to `Decimal` for sorting and spread calculation.

### 10.2 Order Book Data Structure

**Python:** `dict[str, str]` (HashMap) with on-demand sorting via `Decimal` key.

**Rust:** Two options depending on access patterns:

1. **`BTreeMap<Decimal, Decimal>`** -- sorted at all times. `O(log n)` insert/remove, `O(1)` best bid/ask via `.iter().next_back()` / `.iter().next()`. No need for separate sort step. Ideal if you query BBO frequently.

2. **`HashMap<String, String>`** + sort-on-demand -- mirrors the Python approach. `O(1)` insert/remove, `O(n log n)` snapshot. Better if snapshots are infrequent relative to updates.

Recommendation: `BTreeMap<Decimal, Decimal>` for the Rust version. The depth stream delivers ~10 events/second at 100ms intervals. At this rate, `BTreeMap` overhead is negligible, and you get O(1) BBO access which is more useful for an HFT bot than for this collector.

For the bid side, use `BTreeMap` with `Reverse<Decimal>` as the key (or use `.iter().next_back()` on a standard `BTreeMap`) to get highest-first ordering.

### 10.3 WebSocket Handling

**Crate:** `tokio-tungstenite` (async, production-proven) or `fastwebsockets` (lower overhead, less ergonomic).

The combined stream URL is a standard WS connection -- no subscription messages needed. Just connect and read frames.

```rust
// Pseudo-structure
let url = "wss://stream.binance.com:9443/stream?streams=btcusdt@bookTicker/btcusdt@depth@100ms/btcusdt@trade";
let (ws_stream, _) = connect_async(url).await?;
let (_, mut read) = ws_stream.split();

while let Some(msg) = read.next().await {
    let wrapper: CombinedStreamMessage = serde_json::from_slice(&msg?.into_data())?;
    match wrapper.stream.as_str() {
        s if s.ends_with("@trade") => handle_trade(wrapper.data),
        s if s.contains("bookTicker") => handle_book_ticker(wrapper.data),
        s if s.contains("depth") => handle_depth(wrapper.data),
        _ => {}
    }
}
```

**Message deserialization:** Use `serde_json` with a tagged enum or a generic wrapper struct. Since each stream has different `data` shapes, deserialize `data` as `serde_json::Value` first, then parse into the specific type based on `stream`.

### 10.4 REST Snapshot

**Crate:** `reqwest` with `tokio` runtime.

```rust
let resp = reqwest::get("https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=5000")
    .await?
    .json::<DepthSnapshot>()
    .await?;
```

Reuse a `reqwest::Client` instance across fetches (connection pooling).

### 10.5 Gap Detection and Resync State Machine

Model the state explicitly as an enum:

```rust
enum SyncState {
    Buffering {
        buffer: Vec<DepthEvent>,
        snapshot_attempt: u32,
    },
    Synced {
        book: BinanceBook,
    },
}
```

The main loop checks state and dispatches accordingly. On gap detection, transition back to `Buffering` with a fresh buffer. This is cleaner than the Python version's nested while loops with boolean flags.

### 10.6 Key Crates Summary

| Purpose | Crate | Notes |
|---------|-------|-------|
| Async runtime | `tokio` | Full features (`rt-multi-thread`, `macros`) |
| WebSocket | `tokio-tungstenite` | Async WS client |
| HTTP client | `reqwest` | REST snapshot fetching, reuse `Client` |
| JSON | `serde` + `serde_json` | Derive `Deserialize`/`Serialize` for all message types |
| Fast JSON | `simd-json` | Optional drop-in for hot path deserialization |
| Decimal | `rust_decimal` | With `serde` feature for JSON interop |
| Ordered map | `BTreeMap` (std) | No external crate needed |
| Logging | `tracing` | Structured logging, async-friendly |
| File I/O | `tokio::fs` or `std::fs` | JSONL output; consider `BufWriter` with periodic flush |
| Graceful shutdown | `tokio::signal` + `CancellationToken` | Replace Python's `asyncio.Event` |
| Time | `std::time::Instant` / `SystemTime` | For `ts_recv_ms` timestamps |

### 10.7 Performance Considerations for Rust

1. **Zero-copy deserialization:** For `depth_raw` and `book_state` events where you write the full raw message, consider writing the raw bytes directly rather than deserializing then re-serializing.

2. **String interning for price levels:** BTC has a finite set of tick prices. A string interner (or `Arc<str>`) can reduce allocations in the book's HashMap/BTreeMap.

3. **Buffered writer with timed flush:** The Python version flushes every 100ms via a background thread. In Rust, use `BufWriter` and flush on a `tokio::time::interval(Duration::from_millis(100))` or after each batch of events.

4. **Avoid `Decimal` in the hot path:** Like the Python version, store prices as strings in the book and only convert to `Decimal` for sorting/arithmetic. String comparison is cheaper than `Decimal` construction for the zero-check (`qty == "0"`).

5. **Batch writes:** The Python `LineWriter` drains its queue in a tight loop. In Rust, collect multiple lines into a single `write_all` call with a `Vec<u8>` buffer.
