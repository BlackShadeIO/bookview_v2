# 02 - Polymarket CLOB Collector

Source: `collector/polymarket.py`
Config: `collector/config.py`
Models: `collector/models.py`

This document specifies the complete behavior of the Polymarket collector for reimplementation in Rust. The collector has three phases: Gamma API lookup, WebSocket streaming via 10 redundant connections, and local order book maintenance with deduplication.

---

## 1. Gamma API Lookup

### Purpose

Before opening WebSocket connections, the collector must resolve the market's CLOB token IDs. These are two opaque strings (one per outcome: Up/Down) required to subscribe to the WebSocket feed. The Gamma API is Polymarket's REST API for market metadata.

### Slug Construction

The market slug is deterministic:

```
btc-updown-5m-{market_start}
```

Where `market_start` is the Unix epoch timestamp of the 5-minute market boundary (always divisible by 300).

### URL

```
https://gamma-api.polymarket.com/markets/slug/{slug}
```

Constructed from `GAMMA_API_BASE + "/markets/slug/{slug}"`. Single GET request, no authentication, no query parameters.

### Retry Logic and Backoff

The retry loop runs until a hard deadline:

```
deadline = market_start + GAMMA_LOOKUP_DEADLINE
```

Constants:
- `GAMMA_LOOKUP_DEADLINE = 10` seconds after market start
- `GAMMA_RETRY_BASE = 1.0` second (initial backoff)
- `GAMMA_RETRY_MAX = 8.0` seconds (backoff cap)

The backoff sequence is: 1.0, 2.0, 4.0, 8.0, 8.0, 8.0, ...

Since collection starts `PRE_START_SECONDS = 70` seconds before market start, the total available window for Gamma lookup is up to 80 seconds (70 pre-start + 10 post-start). In practice, the first attempt usually succeeds.

**Retry behavior:**
- On HTTP non-200: log the status code and response body, sleep, retry.
- On any exception (network error, DNS failure, timeout): log the error string, sleep, retry.
- On HTTP 200: return the parsed JSON immediately (no retry).
- On deadline exceeded: return `None`, which aborts the entire Polymarket collector for this market window.

Every retry writes a `market_lookup_retry` event to the JSONL output with both raw (status/error + attempt number) and normalized (slug + attempt + status/error) fields. This is important for post-hoc analysis of API reliability.

### Response Validation (`validate_market()`)

After a successful 200 response, the raw JSON dict is validated. The function returns an error string on failure, `None` on success. Checks are performed in this exact order:

1. **Non-empty response** -- `if not data` catches `None`, empty dict, empty list.

2. **Slug match** -- `data["slug"]` must exactly equal the constructed slug. Guards against Gamma API returning a different market (e.g., stale cache, redirect).

3. **Not closed** -- `data["closed"]` must not be `True`. A closed market cannot accept orders; no point streaming its book.

4. **Order book enabled** -- `data["enableOrderBook"]` must be truthy. Some markets exist but have their CLOB disabled.

5. **Outcomes count** -- `data["outcomes"]` must parse to a list of exactly 2 elements. The field can arrive as either a native JSON array or a JSON-encoded string (the code handles both). For BTC Up/Down markets, the two outcomes are always `["Up", "Down"]` or similar. Any other count is an error.

6. **CLOB token IDs count** -- `data["clobTokenIds"]` must parse to a list of exactly 2 elements. Same string-or-array handling as outcomes. These are the asset IDs needed for WebSocket subscription.

**String-or-array parsing pattern** (used for both `outcomes` and `clobTokenIds`):
```python
field = data.get("fieldName")
if not field:
    field_str = data.get("fieldName", "")
    if isinstance(field_str, str):
        field = json.loads(field_str)  # may raise
```

This exists because Gamma API inconsistently returns these fields as either native JSON arrays or JSON-encoded strings. The Rust implementation must handle both formats.

### Metadata Extraction (`parse_market_meta()`)

On successful validation, the following fields are extracted:

| Extracted Key       | Source Field         | Description                                    |
|---------------------|----------------------|------------------------------------------------|
| `market_id`         | `id`                 | Gamma's internal market identifier             |
| `condition_id`      | `conditionId`        | On-chain condition ID for the CTF contract     |
| `slug`              | `slug`               | URL-friendly market identifier                 |
| `question`          | `question`           | Human-readable market question text            |
| `outcomes`          | `outcomes`           | `["Up", "Down"]` (parsed from string if needed)|
| `clob_token_ids`    | `clobTokenIds`       | Two asset IDs for WS subscription (parsed)     |
| `end_date`          | `endDate`            | Market expiry timestamp                        |
| `game_start_time`   | `gameStartTime`      | When the underlying event starts               |

The `clob_token_ids` list is the critical output. Index 0 and index 1 correspond to the two outcomes. Both are used for WebSocket subscription and as keys in the `books` and `last_trade` dicts.

### JSONL Output Events from Gamma Phase

| Event Type              | When                        | `raw` field      | `normalized` field                                    |
|-------------------------|-----------------------------|------------------|-------------------------------------------------------|
| `market_lookup_retry`   | Each failed attempt         | status/error     | slug, attempt, status/error                           |
| `market_lookup_failed`  | Deadline exceeded or invalid| slug or full resp| slug + reason string                                  |
| `market_lookup`         | Successful lookup           | Full Gamma resp  | Extracted metadata dict                               |

---

## 2. WebSocket Architecture

### Connection Redundancy

The collector opens `POLY_WS_CONNECTIONS = 10` simultaneous WebSocket connections to the same endpoint. All 10 connect to:

```
wss://ws-subscriptions-clob.polymarket.com/ws/market
```

Each connection is an independent asyncio task spawned via `asyncio.gather(..., return_exceptions=True)`. The `return_exceptions=True` is critical -- if one connection crashes, the other 9 continue. Each task has a unique `conn_id` (0-9) for log correlation.

**Rationale**: Polymarket's WebSocket feed is unreliable. Messages can be delayed or dropped on any single connection. Running 10 connections with content-based deduplication ensures that if any single connection receives a message, it gets recorded. The dedup mechanism (Section 4) prevents duplicate writes.

### Subscription Payload

After connecting, each connection sends one JSON message:

```json
{
    "assets_ids": ["<token_id_0>", "<token_id_1>"],
    "type": "market",
    "custom_feature_enabled": true
}
```

- `assets_ids`: The two CLOB token IDs from the Gamma lookup. Note the field name uses `assets_ids` (plural "assets", plural "ids").
- `type`: Always `"market"`.
- `custom_feature_enabled`: Always `true`. This likely enables additional event types (tick_size_change, best_bid_ask, etc.).

### Message Reception

Each connection loops with a 5-second `recv()` timeout:

```python
raw_msg = await asyncio.wait_for(ws.recv(), timeout=5.0)
```

- **Timeout**: Silently continues the loop (heartbeat-like). No ping/pong is sent -- the 5s timeout just prevents the task from blocking forever if the server goes silent.
- **ConnectionClosed**: Breaks the inner loop, triggers reconnection.
- **Successful recv**: Parse with `orjson.loads()`. If parsing fails, write an `error` event and continue.

Messages from the server can be either a single JSON object or a JSON array of objects. The code normalizes both cases:

```python
if not isinstance(msgs, list):
    msgs = [msgs]
```

Each individual message object is then dedup-checked and processed.

### Reconnection with Exponential Backoff

The connection loop has two layers:

**Inner layer** (`websockets.connect` iterator pattern): The `async for ws in connect(...)` pattern from the `websockets` library automatically handles reconnection. When the inner loop breaks due to `ConnectionClosed`, the iterator yields a new connection. On success, `reconnect_backoff` resets to `WS_RECONNECT_BASE = 1.0`.

**Outer layer** (exception handler): If `connect()` itself fails (DNS error, TCP refused, TLS failure), the outer `except Exception` handler catches it and applies manual backoff:

```
Backoff sequence: 1.0, 2.0, 4.0, 8.0, 16.0, 16.0, ...
```

Constants:
- `WS_RECONNECT_BASE = 1.0` second
- `WS_RECONNECT_MAX = 16.0` seconds

The loop exits only when `stop_event.is_set()` -- checked at every break point and after every backoff sleep.

### JSONL Output Events from WebSocket Lifecycle

| Event Type (system) | `system_type`      | When                                         | `normalized` fields           |
|----------------------|--------------------|----------------------------------------------|-------------------------------|
| `system`             | `ws_connecting`    | Before each connection attempt                | url, conn_id                  |
| `system`             | `ws_connected`     | After successful WebSocket handshake          | conn_id                       |
| `system`             | `ws_subscribed`    | After sending subscription payload            | token_ids, conn_id            |
| `system`             | `ws_disconnected`  | After connection drops (clean or not)         | conn_id, msgs_received, msgs_deduped |
| `system`             | `ws_reconnecting`  | Before sleeping for backoff                   | backoff, conn_id              |
| `error`              | --                 | JSON decode failure or connection-level error | reason, conn_id               |

The `msgs_received` and `msgs_deduped` counters in `ws_disconnected` are cumulative per connection (not reset on reconnect within the same task).

---

## 3. LocalBook Order Book

### Data Structure

```python
class LocalBook:
    bids: dict[str, str]  # price_string -> size_string
    asks: dict[str, str]  # price_string -> size_string
```

One `LocalBook` instance per CLOB token ID (so two total: one for "Up" token, one for "Down" token). Stored in a shared `books: dict[str, LocalBook]` keyed by token ID.

### Why Strings, Not Floats

Polymarket prices are decimal values between 0 and 1, typically with 2-4 decimal places (e.g., `"0.55"`, `"0.001"`). Sizes are also decimal strings. Using strings preserves:

1. **Exact representation** -- No IEEE 754 floating-point artifacts. `0.1 + 0.2 != 0.3` in floats.
2. **Round-trip fidelity** -- The stored value matches exactly what the exchange sent. No loss of trailing zeros or precision changes.
3. **No computation needed** -- The collector only stores and snapshots the book. It never does arithmetic on prices or sizes (except `float()` conversion for sorting, which is ephemeral).

For the Rust rewrite, consider `rust_decimal::Decimal` or a fixed-point type for any computation, but keep the string representation in serialized output to match the Python version's format exactly.

### `replace(bids, asks)` -- Full Snapshot

Called on `book` events. Completely replaces both sides of the book.

Input format is polymorphic. Each entry can be either:
- A dict: `{"price": "0.55", "size": "1200"}` -- access via `entry["price"]`, `entry["size"]`
- A list/tuple: `["0.55", "1200"]` -- access via `entry[0]`, `entry[1]`

The method:
1. Clears `self.bids` entirely (creates new empty dict).
2. Iterates all bid entries, calling `str()` on both price and size, inserts into `self.bids`.
3. Clears `self.asks` entirely.
4. Iterates all ask entries, same str() + insert.

This is a destructive replacement, not a merge. Any price levels from the previous state that are not in the new snapshot are gone.

### `apply_delta(side, price, size)` -- Incremental Update

Called per-change from `price_change` events.

- `side`: `"BUY"` for bids, anything else (typically `"SELL"`) for asks.
- `price`: String price level.
- `size`: String size. If `"0"`, the price level is removed (using `dict.pop(price, None)` to avoid KeyError). Otherwise, the price level is inserted or updated.

This is the standard price-level book delta protocol: size=0 means delete, any other size means set.

### `snapshot()` -- Current State Export

Produces a dict representing the full book state at the moment of the call:

```python
{
    "bid_count": 42,
    "ask_count": 38,
    "bids": [["0.55", "1200"], ["0.54", "800"], ...],  # sorted highest-first
    "asks": [["0.56", "500"], ["0.57", "300"], ...],    # sorted lowest-first
    "best_bid": "0.55",   # or None if empty
    "best_ask": "0.56",   # or None if empty
}
```

Sorting:
- Bids: sorted by `float(price)` descending (highest bid first).
- Asks: sorted by `float(price)` ascending (lowest ask first).

The sort uses `float()` conversion as the sort key -- this is an ephemeral computation, the stored values remain strings. In Rust, this is where `Decimal` or `Ord`-implementing fixed-point would be useful for the sort comparator.

Before being written, the caller adds three additional fields to the snapshot dict:
- `asset_id`: which token this book belongs to
- `last_trade_price`: most recent trade price for this asset (may be `None`)
- `trigger_event_type`: `"book"` or `"price_change"` depending on what caused this snapshot

---

## 4. Dedup Mechanism

### The Problem

With 10 redundant WebSocket connections receiving the same feed, every message arrives up to 10 times. Without dedup, the output file would contain ~10x the actual market events.

### Content Hashing Approach

```python
def _dedup_key(msg: dict) -> bytes:
    return orjson.dumps(msg, option=orjson.OPT_SORT_KEYS)
```

The dedup key is the full message content, serialized to bytes with sorted keys. This is content-addressable deduplication.

**Why content hashing, not sequence numbers:**
- Polymarket WebSocket messages do not have reliable, monotonically increasing sequence numbers.
- Content hashing is exchange-agnostic -- it works regardless of what fields the server includes.
- Two messages with identical content are semantically identical regardless of which connection delivered them.

**Why `OPT_SORT_KEYS`:** JSON object key order is not guaranteed. Without sorting, `{"a":1,"b":2}` and `{"b":2,"a":1}` would produce different byte strings despite being semantically identical. `orjson.OPT_SORT_KEYS` normalizes key order before serialization, making the byte representation canonical.

### Shared `seen` Set

```python
seen: set[str] = set()  # Note: actually set of bytes, since orjson.dumps returns bytes
```

The `seen` set is shared across all 10 connection tasks. Since all tasks run on the same asyncio event loop (single-threaded), there are no race conditions -- the check-then-add pattern is atomic within a single `await` boundary:

```python
key = _dedup_key(msg)
if key in seen:
    msgs_deduped += 1
    continue
seen.add(key)
```

First connection to deliver a message wins. The other 9 connections' copies are silently dropped (only the per-connection `msgs_deduped` counter increments).

### Memory Implications

**The `seen` set is unbounded.** It grows for the entire lifetime of the worker process (one 5-minute market window plus pre/post padding = ~390 seconds). Every unique message adds one entry. The entry is the full serialized message as bytes.

Estimated memory for a typical market window:
- ~1,000-10,000 unique messages per market
- Average serialized message: ~200-500 bytes
- Total: ~2-5 MB in the set, plus Python object overhead (~50 bytes per set entry)
- Worst case (very active market): ~50,000 messages * 500 bytes = ~25 MB + overhead

This is acceptable because each worker process is short-lived (~6.5 minutes). The set is garbage-collected when the process exits.

**For the Rust rewrite**, consider:
- Using a `HashSet<Vec<u8>>` or `HashSet<u64>` (if hashing the bytes to a 64-bit hash is acceptable -- collision probability is negligible for <100K messages).
- A bounded set with LRU eviction is unnecessary given the short process lifetime.
- If moving to a long-lived process, implement periodic clearing or use a time-bucketed approach.

---

## 5. Message Processing (`_process_poly_message`)

This function is the core dispatcher. It receives one deduplicated message dict and writes one or more JSONL lines depending on the event type.

### Event Type: `book`

**Trigger**: Full order book snapshot from the server.

**Message fields**:
- `event_type`: `"book"`
- `asset_id`: CLOB token ID this book belongs to
- `bids`: Array of bid entries (dicts or arrays)
- `asks`: Array of ask entries (dicts or arrays)
- `last_trade_price`: (optional) Latest trade price for this asset

**Processing**:
1. Write `book_raw` event -- raw=full message, normalized={asset_id, event_type}.
2. If `last_trade_price` is present and `asset_id` is non-empty, update `last_trade[asset_id]`.
3. If `asset_id` is in `books` dict, call `books[asset_id].replace(bids, asks)`.
4. Take a snapshot, annotate with `asset_id`, `last_trade_price`, `trigger_event_type="book"`.
5. Write `book_state` event -- raw=None, normalized=annotated snapshot.

**JSONL writes: 2** (book_raw + book_state). The book_state is the full sorted book after replacement.

### Event Type: `price_change`

**Trigger**: Incremental order book update (one or more price level changes).

**Message fields**:
- `event_type`: `"price_change"`
- `asset_id`: Top-level asset ID (may be overridden per-change)
- `price_changes` OR `changes`: Array of change objects. The code checks `price_changes` first, falls back to `changes`, defaults to empty list.

**Each change object fields**:
- `asset_id`: (optional) Per-change asset ID, falls back to top-level `asset_id`
- `side`: `"BUY"` or `"SELL"`
- `price`: Price level string
- `size`: New size string (`"0"` = delete level)

**Processing**:
1. Write `price_change_raw` event -- raw=full message, normalized={asset_id, event_type}.
2. Iterate all changes. For each, resolve the asset_id (per-change overrides top-level), call `apply_delta(side, price, size)`, track which assets were affected.
3. For each affected asset:
   a. Write `price_change_applied` event -- raw=None, normalized={asset_id, changes_count}.
   b. Take a snapshot, annotate with `asset_id`, `last_trade_price`, `trigger_event_type="price_change"`.
   c. Write `book_state` event -- raw=None, normalized=annotated snapshot.

**JSONL writes: 1 + 2*N** where N = number of distinct affected assets. Typically N=1, so 3 writes. If a single price_change message contains deltas for both token IDs, N=2 and it writes 5 lines.

### Event Type: `last_trade_price`

**Trigger**: The last trade price for an asset changed.

**Message fields**:
- `event_type`: `"last_trade_price"`
- `asset_id`: CLOB token ID
- `price`: New last trade price

**Processing**:
1. Update `last_trade[asset_id]` with `str(price)`.
2. Write `last_trade_price_raw` event -- raw=full message, normalized={asset_id, price}.

**JSONL writes: 1**. Does NOT trigger a book_state snapshot (the book itself hasn't changed).

### Event Type: `best_bid_ask`

**Trigger**: Server-pushed best bid/ask update.

**Message fields**:
- `event_type`: `"best_bid_ask"`
- `asset_id`: CLOB token ID
- (other fields vary -- the code does not parse specific sub-fields)

**Processing**:
1. Write `best_bid_ask_raw` event -- raw=full message, normalized={asset_id}.

**JSONL writes: 1**. Passthrough only. No book state mutation.

### Event Type: `tick_size_change`

**Trigger**: The market's tick size (minimum price increment) changed.

**Message fields**:
- `event_type`: `"tick_size_change"`
- `asset_id`: CLOB token ID
- (other fields vary)

**Processing**:
1. Write `tick_size_change_raw` event -- raw=full message, normalized={asset_id}.

**JSONL writes: 1**. Passthrough only.

### Event Type: `market_resolved`

**Trigger**: The market outcome has been determined.

**Message fields**:
- `event_type`: `"market_resolved"`
- `asset_id`: CLOB token ID
- (other fields vary -- likely includes resolution outcome)

**Processing**:
1. Write `market_resolved_raw` event -- raw=full message, normalized={asset_id}.

**JSONL writes: 1**. Passthrough only. This event typically arrives near the end of the collection window.

### Unknown Event Types

Any `event_type` not matching the above cases:

1. Write `{event_type}_raw` event -- raw=full message, normalized={asset_id, event_type}.

**JSONL writes: 1**. This catch-all ensures no data is silently dropped if Polymarket adds new event types.

### Summary: Events That Trigger Book State Snapshots

Only two event types mutate the book and produce `book_state` lines:

| Event Type      | Book Mutation    | Snapshot Written |
|-----------------|------------------|------------------|
| `book`          | `replace()`      | Yes              |
| `price_change`  | `apply_delta()`  | Yes              |
| `last_trade_price` | None (only updates last_trade dict) | No |
| `best_bid_ask`  | None             | No               |
| `tick_size_change` | None          | No               |
| `market_resolved` | None           | No               |

### Write Serialization and Flushing

All `write_line()` calls go through the `LineWriter` background thread. The writer:
- Accepts dicts via a `queue.Queue`.
- Serializes with `orjson.dumps()` and appends `\n`.
- Batch-drains the queue on each iteration (writes all available lines before checking again).
- Flushes to disk every 100ms or on shutdown.

This means writes from all 10 WebSocket tasks are serialized through a single queue. Order in the output file reflects the order they were enqueued, which is the order of dedup-winning arrival.

---

## 6. Rust Implementation Approach

### HTTP Client for Gamma API

**Crate**: `reqwest` (with `tokio` runtime)

```toml
reqwest = { version = "0.12", features = ["json", "gzip"] }
```

- Use `reqwest::Client` with a shared instance (connection pooling).
- The retry loop is straightforward: `loop` with `tokio::time::sleep` for backoff.
- Deadline check: `Instant::now() < deadline` or `SystemTime` comparison.
- Parse response with `.json::<serde_json::Value>()` or a typed struct.
- For the string-or-array polymorphism in `outcomes`/`clobTokenIds`, use a custom serde deserializer:

```rust
#[derive(Deserialize)]
#[serde(untagged)]
enum StringOrVec {
    String(String),    // JSON-encoded array as string
    Vec(Vec<String>),  // native JSON array
}
```

### WebSocket Client

**Crate**: `tokio-tungstenite`

```toml
tokio-tungstenite = { version = "0.24", features = ["native-tls"] }
```

- Spawn 10 `tokio::task::spawn` tasks, one per connection.
- Each task runs its own reconnection loop.
- Use `tokio::select!` for the recv-with-timeout pattern:

```rust
tokio::select! {
    msg = ws_stream.next() => { /* process */ }
    _ = tokio::time::sleep(Duration::from_secs(5)) => { continue; }
    _ = stop_token.cancelled() => { break; }
}
```

- Use `tokio_util::sync::CancellationToken` instead of `asyncio.Event` for stop signaling. It is clone-safe, await-able, and designed for this exact pattern.
- Subscription payload: serialize with `serde_json::to_string()`, send as text frame.

### Order Book Data Structure

**Two viable approaches:**

**Option A: `BTreeMap<Decimal, Decimal>`**
```toml
rust_decimal = "1.36"
```
- `BTreeMap` gives O(log n) insert/delete and sorted iteration for free.
- `snapshot()` becomes trivial: iterate bids in reverse, asks forward.
- `Decimal` handles exact decimal arithmetic.
- This is the recommended approach for correctness.

**Option B: `HashMap<String, String>` (match Python exactly)**
- Fastest for insert/delete (O(1) amortized).
- Requires sorting on snapshot (allocates a Vec, sorts by parsed price).
- String keys/values match the wire format, zero parsing on insert.
- Best if you want to minimize per-delta overhead and only pay sorting cost on snapshot.

**Recommendation**: Use `BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>` for the internal book. Parse price/size strings to Decimal on insert. On snapshot, iterate in order (no sort needed). Serialize to string format in the output for compatibility with the existing data pipeline.

For the `replace()` operation, clear and rebuild:
```rust
fn replace(&mut self, bids: &[PriceLevel], asks: &[PriceLevel]) {
    self.bids.clear();
    for level in bids {
        self.bids.insert(level.price, level.size);
    }
    // same for asks
}
```

For `apply_delta()`:
```rust
fn apply_delta(&mut self, side: Side, price: Decimal, size: Decimal) {
    let book = match side { Side::Buy => &mut self.bids, Side::Sell => &mut self.asks };
    if size.is_zero() {
        book.remove(&price);
    } else {
        book.insert(price, size);
    }
}
```

### Dedup Strategy

**Approach**: `HashSet<u64>` using a fast non-cryptographic hash.

```toml
ahash = "0.8"
```

Rather than storing the full serialized bytes (as Python does), hash the canonical bytes to a 64-bit integer:

```rust
use ahash::AHasher;
use std::hash::Hasher;

fn dedup_key(msg: &serde_json::Value) -> u64 {
    // Serialize with sorted keys for canonical representation
    let bytes = serde_json::to_vec(msg).unwrap(); // serde_json sorts by default? No.
    let mut hasher = AHasher::default();
    hasher.write(&bytes);
    hasher.finish()
}
```

**Critical**: `serde_json` does NOT sort keys by default when serializing `serde_json::Value` (it preserves insertion order). You need either:
1. Use `serde_json::Value` with a `BTreeMap` backend: parse with `serde_json::from_str::<serde_json::Value>()` which uses `Map<String, Value>` -- this preserves insertion order by default. To get sorted keys, enable the `preserve_order` feature to be OFF (it uses BTreeMap by default when `preserve_order` is disabled).
2. Or use a custom canonical serializer.

**Without `preserve_order` feature** (default): `serde_json::Map` is backed by `BTreeMap`, keys are sorted. This gives you canonical key order for free.

**With `preserve_order` feature**: Uses `IndexMap`, insertion-order. Do NOT enable this feature if you need canonical dedup keys.

Collision probability with 64-bit hash over 100K messages: ~2.7 * 10^-10. Acceptable for this use case.

Memory: 100K messages * 8 bytes = 800KB vs Python's ~25MB. Significant improvement.

**Alternative**: `HashSet<Vec<u8>>` storing the full canonical bytes. Higher memory but zero collision risk. Only worthwhile if you need to recover the original message from the dedup set (you don't).

### JSON Parsing

**Two options:**

**Option A: `serde_json` (recommended for correctness)**
```toml
serde_json = "1.0"
```
- Battle-tested, correct, good performance.
- Use `serde_json::from_slice::<Value>()` to avoid UTF-8 validation overhead (WebSocket text frames are already valid UTF-8, but tungstenite gives you bytes).
- For typed deserialization of known event types, define structs with `#[derive(Deserialize)]`.

**Option B: `simd-json` (for maximum throughput)**
```toml
simd-json = "0.14"
```
- 2-4x faster parsing on x86-64 with AVX2/SSE4.2.
- Requires mutable input buffer (it modifies the input in place).
- API is compatible with serde, but the `Value` type is different.
- Worth benchmarking, but adds complexity. Only adopt if JSON parsing is a measured bottleneck.

**Recommendation**: Start with `serde_json`. Profile. Switch to `simd-json` only if parsing is >10% of CPU time.

### Concurrency Model

```
                                  +--> ws_task_0 --+
                                  |                |
run_polymarket (tokio task)       +--> ws_task_1 --+
  |                               |                |
  +-- gamma_lookup (reqwest)      +--> ws_task_2 --+--> dedup_tx --> writer_task
  |                               |    ...         |
  +-- spawn 10 ws tasks ----------+--> ws_task_9 --+
```

**Channel-based architecture:**

1. **Main task**: Performs Gamma lookup, validates, extracts metadata, spawns 10 WS tasks.

2. **WS tasks** (10x `tokio::spawn`): Each manages its own connection, reconnection, and message parsing. After dedup, sends processed messages through an `mpsc` channel.

3. **Writer task** (1x `tokio::spawn`): Receives from the channel, serializes with `serde_json` or `simd-json`, writes to file with buffered I/O.

**Channel types:**
```rust
let (tx, rx) = tokio::sync::mpsc::channel::<OutputLine>(1024);
```

Each WS task gets a `tx.clone()`. The writer task owns `rx`.

**Dedup synchronization**: The `seen` set must be shared across all 10 WS tasks. Options:

- **`DashMap` / `DashSet`** (concurrent hash set, lock-free reads):
  ```toml
  dashmap = "6.1"
  ```
  Each task does `if !seen.insert(key) { continue; }` -- atomic check-and-insert.

- **`std::sync::Mutex<HashSet<u64>>`**: Simpler, fine for this workload. Lock contention is minimal because each task holds the lock only for the duration of a hash lookup + insert (~50ns).

- **Single-threaded approach**: If all 10 tasks run on the same tokio runtime thread (using `LocalSet`), you can use `Rc<RefCell<HashSet>>` with no synchronization. But this is fragile and prevents work-stealing.

**Recommendation**: `DashSet<u64>` from `dashmap`. Zero contention, zero unsafe, trivial API.

### Zero-Copy Message Handling

Opportunities for zero-copy or reduced-copy:

1. **WebSocket frame to parse**: `tungstenite` gives you the raw frame bytes. Pass directly to `serde_json::from_slice()` without converting to `String` first.

2. **Dedup key computation**: Hash the raw bytes directly. Don't serialize to a new buffer -- hash the incoming bytes. However, this only works if the server always sends keys in the same order (which is NOT guaranteed). If you need canonical key order, you must parse then re-serialize, which eliminates zero-copy here.

3. **Raw message passthrough**: For `_raw` event types, the `raw` field is the entire incoming message. Instead of parsing to `Value` and re-serializing, you could store the original bytes as a `RawValue`:
   ```rust
   use serde_json::value::RawValue;
   ```
   This avoids the parse-then-reserialize round trip for the `raw` field.

4. **Book snapshot**: The snapshot is constructed fresh each time. No zero-copy opportunity here -- it's inherently a new allocation.

5. **`bytes::Bytes`**: Use the `bytes` crate for shared ownership of incoming message buffers if passing them across task boundaries without cloning.

### Recommended Crate Summary

| Purpose                   | Crate                         | Version | Notes                                    |
|---------------------------|-------------------------------|---------|------------------------------------------|
| Async runtime             | `tokio`                       | 1.x     | Features: full                           |
| HTTP client               | `reqwest`                     | 0.12    | Features: json, gzip                     |
| WebSocket client          | `tokio-tungstenite`           | 0.24    | Features: native-tls                     |
| JSON parsing/serialization| `serde_json`                  | 1.x     | Do NOT enable `preserve_order`           |
| Serde framework           | `serde`                       | 1.x     | Features: derive                         |
| Decimal arithmetic        | `rust_decimal`                | 1.36    | Features: serde                          |
| Concurrent hash set       | `dashmap`                     | 6.x     | For shared `seen` set                    |
| Fast hashing              | `ahash`                       | 0.8     | Non-crypto hash for dedup keys           |
| Cancellation              | `tokio-util`                  | 0.7     | `CancellationToken` for stop signaling   |
| Logging                   | `tracing`                     | 0.1     | Structured logging with span context     |
| Time                      | `chrono`                      | 0.4     | Timestamps in output (or `std::time`)    |
| Buffered I/O              | `std::io::BufWriter`          | std     | Wrap the output file                     |
| Byte buffers              | `bytes`                       | 1.x     | Shared ownership of message buffers      |

### File Output Strategy

Replace the Python `LineWriter` background thread with a tokio task:

```rust
use tokio::io::AsyncWriteExt;
use tokio::fs::File;
use tokio::io::BufWriter;

async fn writer_task(mut rx: mpsc::Receiver<OutputLine>, path: PathBuf) {
    let file = File::create(&path).await.unwrap();
    let mut writer = BufWriter::with_capacity(64 * 1024, file);

    while let Some(line) = rx.recv().await {
        let bytes = serde_json::to_vec(&line).unwrap();
        writer.write_all(&bytes).await.unwrap();
        writer.write_all(b"\n").await.unwrap();

        // Drain all available messages before flushing
        while let Ok(line) = rx.try_recv() {
            let bytes = serde_json::to_vec(&line).unwrap();
            writer.write_all(&bytes).await.unwrap();
            writer.write_all(b"\n").await.unwrap();
        }

        writer.flush().await.unwrap();
    }
}
```

The Python version flushes every 100ms. The Rust version can flush after draining all pending messages, which achieves the same batching effect with lower latency.

---

## Appendix: Complete JSONL Event Type Reference

All event types written by the Polymarket collector, in order of frequency:

| `event_type`              | `source`      | `raw`                | `normalized`                                            | Trigger               |
|---------------------------|---------------|----------------------|---------------------------------------------------------|-----------------------|
| `book_state`              | `polymarket`  | `null`               | Full sorted book snapshot + asset_id + last_trade_price + trigger_event_type | After book/price_change |
| `price_change_raw`        | `polymarket`  | Full WS message      | {asset_id, event_type}                                  | WS price_change       |
| `price_change_applied`    | `polymarket`  | `null`               | {asset_id, changes_count}                               | After applying deltas |
| `book_raw`                | `polymarket`  | Full WS message      | {asset_id, event_type}                                  | WS book snapshot      |
| `last_trade_price_raw`    | `polymarket`  | Full WS message      | {asset_id, price}                                       | WS last_trade_price   |
| `best_bid_ask_raw`        | `polymarket`  | Full WS message      | {asset_id}                                              | WS best_bid_ask       |
| `tick_size_change_raw`    | `polymarket`  | Full WS message      | {asset_id}                                              | WS tick_size_change   |
| `market_resolved_raw`     | `polymarket`  | Full WS message      | {asset_id}                                              | WS market_resolved    |
| `{unknown}_raw`           | `polymarket`  | Full WS message      | {asset_id, event_type}                                  | Unknown event type    |
| `system`                  | `polymarket`  | Varies (null or payload) | Varies (see WS lifecycle table)                     | Connection lifecycle  |
| `error`                   | `polymarket`  | {raw/error, conn_id} | {reason, conn_id}                                       | Parse/connection error|
| `market_lookup`           | `polymarket`  | Full Gamma response  | Extracted metadata                                      | Successful Gamma call |
| `market_lookup_retry`     | `polymarket`  | {status/error, attempt} | {slug, attempt, status/error}                        | Failed Gamma attempt  |
| `market_lookup_failed`    | `polymarket`  | {slug} or full resp  | {slug, reason}                                          | Gamma deadline/invalid|
