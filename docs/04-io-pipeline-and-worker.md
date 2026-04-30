# 04 - I/O Pipeline and Worker Lifecycle

Reference for the Rust rewrite. Covers the threaded write path, line construction,
sequence numbering, and the per-market worker process from spawn to shutdown.

Source files: `collector/models.py`, `collector/worker.py`, `collector/supervisor.py`, `collector/config.py`.

---

## 1. LineWriter -- Threaded Write Decoupling

### Why it exists

The original implementation used `json.dumps()` + `file.write()` + `file.flush()` directly
inside the asyncio event loop. During market-close bursts (both Polymarket and Binance send
hundreds of events within milliseconds), the synchronous serialization and kernel I/O calls
blocked the event loop. Measured latency spikes reached **23 seconds** -- messages piled up
in WebSocket buffers, causing sequence gaps, missed events, and Binance depth desyncs that
required full snapshot re-fetches.

`LineWriter` moves all serialization and file I/O to a dedicated OS thread so the asyncio
event loop never blocks on disk.

### Architecture

```
asyncio event loop                 writer thread
     |                                  |
  put(dict) -----> queue.Queue -----> _run() loop
     |          (thread-safe,          |
     |           unbounded)            |-- orjson.dumps(dict) -> bytes
     |                                 |-- file.write(bytes + b"\n")
     |                                 |-- periodic flush every 100ms
     |                                 |
  put(dict) ---->  ...                 |
     |                                 |
  stop() ------> put(None) sentinel    |
     |           thread.join(10s)      |-- final flush, return
     |           file.close()
```

### Queue mechanics

- **Type**: `queue.Queue[dict | None]`. Unbounded (no maxsize). This is intentional: backpressure
  from a bounded queue would block the event loop, which is exactly what we are trying to avoid.
  Memory pressure from an unbounded queue is acceptable because events are small dicts and the
  writer thread drains faster than the event loop produces.

- **Blocking get with timeout**: The writer thread calls `q.get(timeout=0.05)` (50ms). This
  means the thread wakes at worst every 50ms even when idle, which keeps flush latency bounded
  without busy-spinning.

- **Batch draining**: After processing one item from `get()`, the writer immediately enters a
  `get_nowait()` loop to drain all queued items without blocking. This is the critical
  optimization for bursts: if 200 events arrived while the thread was writing, they all get
  serialized and written in one tight loop with no queue synchronization overhead between items.

```python
# After processing the first item:
while True:
    try:
        line = q.get_nowait()
    except queue.Empty:
        break
    if line is None:
        f.flush()
        return
    f.write(orjson.dumps(line) + newline)
```

### Serialization: orjson

The codebase uses `orjson` instead of stdlib `json` for serialization. Key differences:

- **3-10x faster** than `json.dumps()`. orjson is implemented in Rust (using serde internally),
  which is why the performance gap is so large.
- **Returns `bytes`**, not `str`. This is why the file is opened in binary mode (`"ab"`) -- no
  need for a `.encode()` call, saving one allocation and copy per line.
- **Handles Python types natively**: `Decimal`, `datetime`, `UUID` without custom encoders.

The serialization call is `orjson.dumps(line) + newline` where `newline = b"\n"`. This creates
one bytes concatenation per line. In Rust, this concatenation would be a `Vec<u8>` push.

### File mode: binary append ("ab")

- **Append mode** (`"a"`): safe for concurrent processes writing to different files (though in
  practice each file has exactly one writer). More importantly, append mode means a crashed
  writer does not truncate existing data on restart.
- **Binary mode** (`"b"`): required because orjson returns `bytes`. Avoids the overhead of
  Python's text mode (no encoding layer, no universal newline translation).

### Periodic flush (100ms)

The writer tracks `last_flush` using `time.monotonic()` (not `time.time()` -- monotonic is
immune to NTP adjustments and wall clock jumps). Flush happens in two places:

1. **After the batch drain loop**: if `now - last_flush >= 0.1` (100ms elapsed), call `f.flush()`.
2. **On empty queue timeout**: same 100ms check after the `queue.Empty` exception.

This means data is guaranteed to hit the OS page cache within 100ms of being written, which
bounds the data loss window on a crash. The 100ms interval is a compromise: flushing every
write would be correct but slow (syscall per event); flushing every second would lose up to 1s
of data.

Note: `f.flush()` flushes Python's userspace buffer to the kernel. It does NOT call `fsync()`.
Data could still be lost on power failure (sitting in the kernel page cache), but this is
acceptable -- the risk is process crash, not power failure.

### Graceful shutdown

1. Caller calls `stop()`.
2. `stop()` pushes `None` sentinel onto the queue.
3. `stop()` calls `self._thread.join(timeout=10.0)` -- waits up to 10 seconds for the writer
   thread to drain remaining items and exit.
4. The writer thread, upon receiving `None` from either `get()` or `get_nowait()`, calls
   `f.flush()` and returns (ending the thread).
5. After `join()` returns (or times out), `stop()` calls `self._file.close()`.

The 10-second join timeout is a safety net. In practice the writer thread should drain and
exit within milliseconds. If it hangs (kernel I/O stall on a dying disk), the process still
shuts down.

**Edge case**: if `join()` times out, the file is closed from under the writer thread. This
is safe because the writer is a daemon thread (terminates when the process exits) and we are
in the shutdown path.

### Slot optimization

`LineWriter` uses `__slots__ = ("_queue", "_path", "_thread", "_file")` to avoid per-instance
`__dict__` allocation. Minor memory optimization, but there are only two instances per worker
so this is more about signaling "this class is a tight, fixed-shape object."

---

## 2. SeqCounter -- Per-File Monotonic Sequence

```python
class SeqCounter:
    __slots__ = ("_v",)

    def __init__(self) -> None:
        self._v = 0

    def next(self) -> int:
        self._v += 1
        return self._v
```

- **Starts at 0, first call returns 1**. Sequence numbers are 1-indexed in the output.
- **Per-file, not per-source**: each JSONL file (clob.jsonl, depth-trade.jsonl) has its own
  counter. This means sequence numbers within a file are strictly monotonic and gap-free under
  normal operation.
- **Not thread-safe**: called only from the asyncio event loop (single-threaded), so no locking
  needed. The writer thread never touches the counter.
- **Purpose**: total ordering within a file. When replaying events, `seq` resolves ties between
  events with identical `ts_recv_ms` timestamps (sub-millisecond bursts are common during
  Binance depth updates).

### Rust note

Replace with `AtomicU64` if multiple tasks share a counter, or a plain `u64` if the counter
stays within a single task (which is the current design). Using `AtomicU64` with
`Ordering::Relaxed` has negligible cost on x86 and gives future flexibility.

---

## 3. make_line() -- JSONL Line Builder

```python
def make_line(
    market_start: int,
    source: str,
    event_type: str,
    seq: int,
    raw: Any,
    normalized: Any,
    *,
    system_type: str | None = None,
) -> dict:
```

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `ts_recv_ms` | `int` | `int(time.time() * 1000)` -- wall clock milliseconds at the moment the collector received the event. Truncated (floor), not rounded. |
| `ts_recv_unix` | `float` | `ts_recv_ms / 1000.0` -- same instant as a Unix float with 1ms precision. Derived from `ts_recv_ms` to avoid a second `time.time()` call. |
| `market_start` | `int` | Unix epoch of the 5-minute market boundary. Identifies which market window this data belongs to. |
| `collector_pid` | `int` | `os.getpid()` -- which OS process wrote this line. Useful for debugging when a worker crashes and restarts (different PID, same market). |
| `source` | `str` | `"polymarket"` or `"binance"`. Which data feed produced this event. |
| `event_type` | `str` | Event classification. Examples: `"book"`, `"trade"`, `"depth_update"`, `"snapshot"`, `"system"`, `"error"`, `"collector_stopped"`. Source-specific. |
| `seq` | `int` | Monotonic sequence number from `SeqCounter`. 1-indexed, gap-free within a file. |
| `raw` | `Any` | The original event payload as received from the upstream API. Preserved verbatim for debugging and reprocessing. Can be `None` for system events. |
| `normalized` | `Any` | Processed/extracted fields from the raw event. Structure varies by `event_type`. Can be `None` for system events or when normalization is not applicable. |
| `system_type` | `str \| None` | Only present on system events (e.g. `"worker_started"`, `"worker_exception"`). Omitted from the dict entirely when `None` -- not set to null. |

### Timestamp design decisions

**Why `time.time()` and not event timestamps**: The `ts_recv_ms` field records when the
collector process received the event, not when the exchange says the event occurred. This is
deliberate:

1. Exchange timestamps may be missing, inconsistent, or in different formats across sources.
2. Receiver timestamps provide a ground truth for measuring collector latency.
3. For replay, what matters is the order events arrived at the collector, not when they
   originated.

**Why both `ts_recv_ms` (int) and `ts_recv_unix` (float)**: Historical accident that became
a feature. The integer millisecond form is compact and comparison-friendly (no float equality
issues). The float form is convenient for human-readable display and compatible with libraries
that expect Unix timestamps as floats. Both are derived from a single `time.time()` call so
they are always consistent.

**Why `int(time.time() * 1000)` not `time.time_ns() // 1_000_000`**: The `time.time()` path
is simpler and `time.time_ns()` was not in the original implementation. Both produce the same
result for the millisecond precision needed here. In Rust, use
`SystemTime::now().duration_since(UNIX_EPOCH).as_millis()`.

### Conditional field inclusion

`system_type` is only added to the dict when not `None`:

```python
if system_type is not None:
    line["system_type"] = system_type
```

This keeps the JSONL compact -- most lines (data events) do not have this field at all. The
downstream viewer/parser checks for the presence of the key, not for a null value.

---

## 4. write_line() -- Thin Wrapper

```python
def write_line(writer: LineWriter, line: dict) -> None:
    writer.put(line)
```

A one-liner that exists for call-site readability and to decouple callers from the `LineWriter`
API. Every event in both `polymarket.py` and `binance.py` flows through this function. In Rust,
this would likely be inlined or replaced by a direct channel send.

---

## 5. Worker Lifecycle

### Entry point: worker_main()

```python
def worker_main(market_start: int) -> None:
```

Called as `target` of `multiprocessing.Process(target=worker_main, args=(market_start,))` by
the supervisor. This function runs in a **separate OS process** -- it gets its own memory space,
its own Python interpreter, its own GIL.

### Startup sequence

1. **Configure logging**: per-worker format string includes `market_start` for log grep:
   `%(asctime)s [worker-{market_start}] %(name)s %(levelname)s: %(message)s`

2. **Create output directory**: `DATA_DIR / str(market_start)` with `mkdir(parents=True, exist_ok=True)`.
   `DATA_DIR` is `<repo>/data`. Example path: `data/1714521600/`.

3. **Resolve file paths**: `clob.jsonl` and `depth-trade.jsonl` within the market directory.

4. **Enter async runtime**: `asyncio.run(_async_main(...))`. Each worker creates its own event
   loop -- there is no shared event loop between workers.

### _async_main() flow

```
_async_main(market_start, clob_path, depth_path)
    |
    |-- Create stop_event (asyncio.Event)
    |-- Compute stop_at = market_start + 300 + 20 = market_start + 320
    |
    |-- Create and start two LineWriters (clob_writer, depth_writer)
    |-- Create two SeqCounters (clob_seq, depth_seq)
    |
    |-- Write "worker_started" system marker to BOTH files
    |
    |-- asyncio.gather(
    |       run_polymarket(market_start, clob_writer, stop_event),
    |       run_binance(market_start, depth_writer, stop_event),
    |       _timer_task(stop_at, stop_event),
    |   )
    |
    |-- [on exception] Write error markers to both files
    |
    |-- [finally, always] Write "collector_stopped" markers to both files
    |-- [finally, always] Stop both LineWriters (drain + close)
```

### The three concurrent tasks

1. **run_polymarket()**: Gamma API lookup -> WebSocket connection -> event stream. Writes to
   `clob_writer`. Checks `stop_event` between reconnect attempts and event processing.

2. **run_binance()**: WebSocket connection (combined stream) -> REST depth snapshot -> steady
   state. Writes to `depth_writer`. Checks `stop_event` similarly.

3. **_timer_task()**: Computes remaining time until `stop_at`, sleeps that duration, then sets
   `stop_event`. This is the only thing that triggers graceful shutdown under normal operation.

```python
async def _timer_task(stop_at: float, stop_event: asyncio.Event) -> None:
    now = time.time()
    remaining = stop_at - now
    if remaining > 0:
        await asyncio.sleep(remaining)
    stop_event.set()
```

The timer uses `time.time()` (wall clock), not `asyncio.get_event_loop().time()` (monotonic),
because `stop_at` is computed from a Unix epoch (`market_start`). If the system clock is
adjusted by NTP during collection, the window could be slightly longer or shorter than 320
seconds. For an HFT bot, consider using monotonic time and computing the delta at worker start.

### stop_event coordination

`stop_event` is an `asyncio.Event` shared by all three tasks. When `_timer_task` sets it:

- `run_polymarket()` and `run_binance()` check `stop_event.is_set()` at various points in their
  event loops and reconnect logic. They break out of their loops and return.
- `asyncio.gather()` completes when all three tasks return.
- Execution falls through to the `finally` block.

There is no cancellation (`task.cancel()`) -- tasks cooperatively check the event. This avoids
`CancelledError` handling complexity and ensures each task can finish its current operation
cleanly (e.g., write a final event before exiting).

### Error handling

If any task raises an unhandled exception, `asyncio.gather()` propagates it (default behavior:
if one task raises, gather cancels the others and re-raises the first exception).

The `except Exception` block:
1. Logs the full traceback.
2. Writes an `"error"` event to **both** files with `error` (message) and `type` (exception
   class name) in `raw`, and `reason: "worker_exception"` in `normalized`.

This ensures that even if the collector crashes, the JSONL file contains a record of what
happened. The viewer can display these error markers.

### Shutdown sequence (finally block)

Always runs, whether the worker exited normally or via exception:

1. Write `"collector_stopped"` system event to both files with `market_start` and `pid`.
2. Call `clob_writer.stop()` and `depth_writer.stop()`:
   - Sends `None` sentinel to each writer's queue.
   - Joins writer thread with 10s timeout (drains remaining queued events).
   - Closes file handle.

The order matters: system markers are queued before `stop()` is called, so they are guaranteed
to be written to disk before the file is closed.

### Why separate processes, not threads

The supervisor spawns each worker as a `multiprocessing.Process`, not a `threading.Thread`.
Three reasons:

1. **GIL isolation**: Each worker runs CPU-bound work (orjson serialization of hundreds of
   events per second, Binance book maintenance with Decimal arithmetic). Under the GIL, these
   would contend with each other and with the supervisor. Separate processes eliminate GIL
   contention entirely.

2. **Crash isolation**: If a worker segfaults (e.g., a C extension bug in `websockets` or
   `orjson`), only that worker process dies. The supervisor detects it via `proc.is_alive()` /
   `proc.exitcode` and can restart it. With threads, a segfault kills the entire process.

3. **Clean restarts**: The supervisor can restart a crashed worker for the same market window
   (up to 1 restart). A restarted worker gets a fresh Python interpreter, fresh event loop,
   fresh file handles. No stale state to clean up.

The cost is higher memory usage (each process has its own Python interpreter, ~30-50MB baseline)
and no shared state. But workers are independent by design -- they share no state, only the
filesystem.

### Supervisor spawn and restart

From `supervisor.py`:

```python
p = Process(target=worker_main, args=(target,), daemon=True)
p.start()
```

- `daemon=True`: worker processes are killed if the supervisor exits (prevents orphaned workers).
- Restart policy: `_reap_workers()` checks if a worker died before its expected end time
  (`market_start + MARKET_DURATION + TAIL_SECONDS`) with a non-zero exit code. If so, it
  restarts once (configurable `max_restarts=1`).

---

## 6. Data Flow Summary

```
Exchange WebSocket
    |
    v
asyncio event loop (in worker process)
    |
    |-- time.time() * 1000 --> ts_recv_ms
    |-- SeqCounter.next() --> seq
    |-- make_line() --> dict with all fields
    |-- write_line() --> LineWriter.put(dict) --> queue.Queue
    |
    v
Writer thread (same process, different OS thread)
    |
    |-- queue.get(timeout=0.05)
    |-- queue.get_nowait() loop (batch drain)
    |-- orjson.dumps(dict) --> bytes
    |-- file.write(bytes + b"\n")
    |-- flush every 100ms
    |
    v
data/{market_start}/clob.jsonl
data/{market_start}/depth-trade.jsonl
```

Latency budget (typical): event loop put to disk write is <1ms under normal load, <10ms during
bursts. The old synchronous path could block for 23+ seconds during market close.

---

## 7. Rust Rewrite Recommendations

### Should the LineWriter thread pattern be preserved?

**Probably not.** The LineWriter exists because Python's `json.dumps()` is slow (pure Python
or C extension, either way 10-100us per call) and `file.write()` can block the asyncio event
loop. In Rust:

- `serde_json::to_vec()` serializes in ~1-5us (10-50x faster than Python json, 2-5x faster
  than orjson).
- `BufWriter::write_all()` copies into a userspace buffer and returns immediately (no syscall
  until the buffer is full or explicitly flushed).

So in Rust, serialization + buffered write on the event loop is likely fast enough. The total
time per event would be ~2-10us, well within budget for an async task.

**However**, if profiling shows that burst serialization (hundreds of events in <10ms) causes
unacceptable jitter on the event loop, the pattern can be replicated with a dedicated writer
task on a tokio blocking thread.

### I/O pipeline

| Python | Rust equivalent | Notes |
|--------|----------------|-------|
| `queue.Queue` | `tokio::sync::mpsc` | Bounded channel. Backpressure is acceptable in Rust because the sender is `async` and can `.await` without blocking the event loop. Size 4096-8192 is generous. |
| -- | `crossbeam::channel` | Only if the writer runs on a dedicated OS thread (not a tokio task). Crossbeam is lock-free and faster than std mpsc. |
| `orjson.dumps()` | `serde_json::to_vec()` or `serde_json::to_writer()` | `to_writer()` with a `BufWriter` avoids the intermediate `Vec<u8>` allocation. For maximum performance, use a pre-allocated `Vec<u8>` buffer and `serde_json::to_writer(&mut buf, &event)` then `buf.push(b'\n')` then write the buffer and clear it. |
| -- | `simd-json` | 2-3x faster than serde_json for serialization of simple structs. Worth benchmarking but serde_json is likely fast enough. |
| `open("ab")` | `OpenOptions::new().append(true).create(true).open()` | Binary append. Same semantics as Python. |
| `f.flush()` every 100ms | `BufWriter` + periodic `flush()` | Spawn a tokio interval task that calls `writer.flush()` every 100ms, or flush after each batch. `BufWriter` with 64KB buffer will batch most writes automatically. |
| -- | `mmap` | Memory-mapped I/O. Potentially faster for sequential writes, but complex (need to manage file growth, page faults). Only worth it if profiling shows file I/O as a bottleneck, which is unlikely. **Not recommended for initial implementation.** |

### Recommended Rust write path (simple)

```rust
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use tokio::sync::mpsc;
use serde::Serialize;

#[derive(Serialize)]
struct EventLine {
    ts_recv_ms: u64,
    ts_recv_unix: f64,
    market_start: u64,
    collector_pid: u32,
    source: &'static str,
    event_type: String,
    seq: u64,
    raw: serde_json::Value,
    normalized: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_type: Option<String>,
}

async fn writer_task(mut rx: mpsc::Receiver<EventLine>, path: &str) {
    let file = OpenOptions::new()
        .append(true).create(true).open(path).unwrap();
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    let mut buf = Vec::with_capacity(4096);

    while let Some(event) = rx.recv().await {
        buf.clear();
        serde_json::to_writer(&mut buf, &event).unwrap();
        buf.push(b'\n');
        writer.write_all(&buf).unwrap();

        // Batch drain: process all immediately available events
        while let Ok(event) = rx.try_recv() {
            buf.clear();
            serde_json::to_writer(&mut buf, &event).unwrap();
            buf.push(b'\n');
            writer.write_all(&buf).unwrap();
        }

        writer.flush().unwrap();
    }
}
```

Or even simpler -- inline the write on the event loop (no channel, no separate task):

```rust
// In the WebSocket event handler:
serde_json::to_writer(&mut writer, &event).unwrap();
writer.write_all(b"\n").unwrap();
// Flush periodically via a timer or after N writes.
```

### Worker isolation

| Python | Rust option | Recommendation |
|--------|------------|----------------|
| `multiprocessing.Process` | Separate OS processes | Maximum isolation, same as Python. Use `std::process::Command` or `fork`. Overkill for Rust -- no GIL to dodge. |
| -- | OS threads (`std::thread::spawn`) | Good isolation from async runtime. Each thread gets its own tokio runtime via `Runtime::new()`. Crash in one thread can be caught with `catch_unwind`. |
| -- | **Tokio tasks** (`tokio::spawn`) | **Recommended for initial implementation.** Each market gets a spawned task. Lightest weight, simplest code. A panic in one task does not crash others (with proper panic handling). No serialization overhead for cross-process data. |

**Recommendation**: Start with tokio tasks. Each market window is a spawned task that owns its
file handles and channels. If profiling shows that heavy serialization or book maintenance
contends with the event loop, move the compute-heavy work to `tokio::spawn_blocking` or a
dedicated thread.

For crash isolation in production, wrap each market task in `catch_unwind` or use
`tokio::task::JoinHandle` error handling. Unlike Python, Rust panics are recoverable per-thread.

### Timestamps

| Python | Rust | Notes |
|--------|------|-------|
| `time.time() * 1000` | `SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64` | Wall clock milliseconds. Direct equivalent. |
| `time.monotonic()` | `Instant::now()` | For internal timers (flush intervals, sleep durations). Cannot be converted to Unix time. |
| -- | `chrono::Utc::now().timestamp_millis()` | Alternative if using chrono. Slightly more ergonomic but adds a dependency. |

For the HFT bot, consider also recording `Instant::now()` at process start and using elapsed
durations for internal timing, while using `SystemTime` only for the output timestamps.

### Sequence counters

```rust
use std::sync::atomic::{AtomicU64, Ordering};

struct SeqCounter(AtomicU64);

impl SeqCounter {
    fn new() -> Self { Self(AtomicU64::new(0)) }
    fn next(&self) -> u64 { self.0.fetch_add(1, Ordering::Relaxed) + 1 }
}
```

`Ordering::Relaxed` is sufficient: the counter is only used within a single logical stream
(one file), and the ordering guarantee comes from the channel/write order, not from the atomic
itself. If the counter is truly single-threaded (owned by one tokio task), a plain `Cell<u64>`
or even `u64` with `&mut self` is fine and avoids the atomic overhead entirely.

### Key crates

| Purpose | Crate | Notes |
|---------|-------|-------|
| Async runtime | `tokio` | With `rt-multi-thread`, `macros`, `net`, `fs`, `time`, `sync` features. |
| WebSocket client | `tokio-tungstenite` | Or `fastwebsockets` for lower latency. |
| HTTP client | `reqwest` | For REST snapshots (Binance depth, Gamma API). |
| JSON serialization | `serde` + `serde_json` | Derive-based. Consider `simd-json` for deserialization of large payloads. |
| Decimal arithmetic | `rust_decimal` | For Binance price/quantity. Equivalent to Python's `decimal.Decimal`. |
| Logging | `tracing` + `tracing-subscriber` | Structured logging with per-market spans. |
| Signal handling | `tokio::signal` | For SIGINT/SIGTERM in the supervisor task. |
| CLI / config | `clap` | If the Rust binary needs command-line arguments. |
| Time | `std::time` | Prefer stdlib. `chrono` only if calendar math is needed. |

### What to keep, what to drop

- **Keep**: The queue-and-drain pattern (channel + `try_recv` loop). Even if writes are fast
  enough to inline, the pattern provides clean separation and makes it trivial to add
  compression, rotation, or network shipping later.
- **Keep**: Binary append mode, periodic flush, `None`-sentinel shutdown.
- **Keep**: Separate `raw` and `normalized` fields. Critical for debugging.
- **Keep**: Per-file sequence counters. Cheap and invaluable for replay.
- **Drop**: The dedicated writer thread. A tokio task with `mpsc` is lighter and integrates
  with the async runtime. Only add a blocking thread if BufWriter flush latency is measurable.
- **Drop**: `system_type` as a conditionally-present field. In Rust, use an enum:
  ```rust
  enum EventType {
      Book { ... },
      Trade { ... },
      System { system_type: SystemType },
      Error { error: String, error_type: String },
  }
  ```
  serde's `#[serde(tag = "event_type")]` or `#[serde(untagged)]` handles the JSON shape.
- **Consider**: Replacing JSONL with a binary format (FlatBuffers, Cap'n Proto, or custom) for
  the hot path, with JSONL export for the viewer. Only if file size or parse speed matters.
