# 01 -- Supervisor and Configuration

Reference documentation for porting the Python supervisor/config layer to Rust.
Covers every function, constant, timing decision, and data flow path.

Source files:
- `run.py` -- process entry point
- `collector/config.py` -- all constants
- `collector/supervisor.py` -- market scheduling, worker lifecycle, signal handling
- `collector/worker.py` -- referenced where relevant to explain supervisor's contract with workers

---

## 1. Entry Point: `run.py`

```python
from collector.supervisor import main

if __name__ == "__main__":
    main()
```

This is the only entry point. `python run.py` calls `supervisor.main()` directly
in the main process. There is no CLI argument parsing, no daemon mode, no config
file loading. Everything is hard-coded in `config.py`.

**Rust note:** This becomes `fn main()` in a binary crate. No argument parsing
needed initially, but consider `clap` if you later want to override timing
constants or data paths at runtime.

---

## 2. Configuration: `collector/config.py`

Every constant the system uses, grouped by purpose.

### 2.1 Timing Constants

| Constant | Value | Purpose |
|---|---|---|
| `PRE_START_SECONDS` | `70` | How many seconds before a market's start epoch to spawn its worker |
| `MARKET_DURATION` | `300` | Length of one market window (5 minutes) |
| `TAIL_SECONDS` | `20` | How long to keep collecting after the market ends |
| `MARKET_INTERVAL` | `300` | Markets are aligned to 5-minute UNIX epoch boundaries |
| `GAMMA_LOOKUP_DEADLINE` | `10` | Seconds after market start before giving up on Gamma API slug lookup |
| `GAMMA_RETRY_BASE` | `1.0` | Exponential backoff base for Gamma REST retries |
| `GAMMA_RETRY_MAX` | `8.0` | Exponential backoff ceiling for Gamma REST retries |

**Why PRE_START_SECONDS = 70:**
The worker needs time to: (1) fork the process and initialize, (2) look up the
Polymarket slug via Gamma REST API (which may require multiple retries with
backoff), (3) connect two WebSocket streams, (4) perform Binance depth snapshot
sync (REST fetch + buffered diff alignment). 70 seconds gives enough headroom
for all of this to complete before the market opens. The Gamma slug for a market
epoch may not even exist until shortly before the market starts, so starting
too early would just waste retries, but starting too late risks missing the open.

**Why MARKET_DURATION = 300 and MARKET_INTERVAL = 300:**
Polymarket BTC up/down markets are exactly 5 minutes long, aligned to 5-minute
UNIX epoch boundaries. The interval and duration are identical because markets
are contiguous -- one ends and the next begins immediately. These are separate
constants because conceptually they could differ (e.g., if markets had gaps).

**Why TAIL_SECONDS = 20:**
After a market closes, there is still valuable data: settlement events, final
trade prices, order book collapse. 20 seconds captures the post-market activity
without overlapping significantly with the next market's pre-start window.

**Why GAMMA_LOOKUP_DEADLINE = 10:**
The Gamma API sometimes publishes the market slug late. The system starts
looking 70 seconds before market start. If it still hasn't found the slug by
10 seconds after market start, it gives up -- the market is already live and
continuing to retry is pointless. Total lookup budget is therefore up to 80
seconds (70 pre + 10 post).

**Why GAMMA_RETRY_BASE = 1.0, GAMMA_RETRY_MAX = 8.0:**
Standard exponential backoff: 1s, 2s, 4s, 8s, 8s, 8s... This is aggressive
enough to find newly-published slugs quickly but won't hammer the API. With the
80-second budget, this allows roughly 13-14 attempts.

### 2.2 URLs

| Constant | Value |
|---|---|
| `GAMMA_API_BASE` | `https://gamma-api.polymarket.com` |
| `GAMMA_MARKET_BY_SLUG` | `{GAMMA_API_BASE}/markets/slug/{slug}` -- Python format string |
| `POLYMARKET_WS_URL` | `wss://ws-subscriptions-clob.polymarket.com/ws/market` |
| `BINANCE_WS_URL` | Combined stream: `btcusdt@bookTicker`, `btcusdt@depth@100ms`, `btcusdt@trade` |
| `BINANCE_DEPTH_SNAPSHOT_URL` | REST depth snapshot, limit=5000 |

The Binance WS URL uses the combined stream endpoint (`/stream?streams=...`)
rather than opening three separate WebSocket connections. This is the standard
Binance approach for subscribing to multiple streams on one socket.

### 2.3 Paths

| Constant | Value |
|---|---|
| `DATA_DIR` | `<repo>/data` (resolved relative to `config.py`'s parent's parent) |
| `CLOB_FILENAME` | `clob.jsonl` |
| `DEPTH_FILENAME` | `depth-trade.jsonl` |

Data layout on disk: `data/{market_start_epoch}/clob.jsonl` and
`data/{market_start_epoch}/depth-trade.jsonl`.

### 2.4 Reconnection / Redundancy

| Constant | Value | Purpose |
|---|---|---|
| `WS_RECONNECT_BASE` | `1.0` | WebSocket reconnect backoff base |
| `WS_RECONNECT_MAX` | `16.0` | WebSocket reconnect backoff ceiling |
| `POLY_WS_CONNECTIONS` | `10` | Number of redundant Polymarket WebSocket connections |

`POLY_WS_CONNECTIONS = 10` is notable -- the system opens 10 parallel WebSocket
connections to Polymarket's CLOB for the same market. This is a redundancy
strategy: Polymarket's WebSocket is unreliable and sometimes drops events.
By running 10 connections, the system can deduplicate and detect gaps.

### 2.5 Rust Configuration Approach

**Recommendation: compile-time constants with optional runtime overrides.**

```rust
// config.rs
pub const PRE_START_SECONDS: u64 = 70;
pub const MARKET_DURATION: u64 = 300;
pub const TAIL_SECONDS: u64 = 20;
pub const MARKET_INTERVAL: u64 = 300;
pub const GAMMA_LOOKUP_DEADLINE: u64 = 10;

// For paths and URLs that might need runtime override:
use std::path::PathBuf;
use once_cell::sync::Lazy;

pub static DATA_DIR: Lazy<PathBuf> = Lazy::new(|| {
    std::env::var("BOOKVIEW_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")
        })
});
```

Timing constants should be `const` (zero-cost, inlined). URLs can be `const &str`.
Paths should be `Lazy<PathBuf>` to allow env var override without adding a full
config framework. Avoid `config` / `figment` crates unless you actually need
TOML/YAML files -- this system is simple enough that env vars suffice.

---

## 3. Supervisor: `collector/supervisor.py`

### 3.1 State

The supervisor maintains three data structures:

```python
launched: set[int] = set()              # market_start epochs we've spawned (dedup guard)
active: dict[int, Process] = {}         # market_start -> live Process handle
restart_counts: dict[int, int] = {}     # market_start -> number of restarts performed
```

- `launched` prevents double-spawning the same market. Pruned every loop
  iteration to keep only the last hour of entries.
- `active` tracks processes that haven't been joined yet. Used for reaping
  and shutdown.
- `restart_counts` limits restarts to `max_restarts=1` per market.

### 3.2 Market Boundary Calculation

This is the most critical piece of logic to port exactly.

```python
def _next_market_start(now: float) -> int:
    """Compute next eligible market_start such that we still have 70s lead time."""
    return math.ceil((now + PRE_START_SECONDS) / MARKET_INTERVAL) * MARKET_INTERVAL
```

**Step-by-step breakdown:**

1. Take current UNIX timestamp as a float (e.g., `1714567890.123`).
2. Add `PRE_START_SECONDS` (70) to get the earliest market start we could
   fully prepare for: `1714567960.123`.
3. Divide by `MARKET_INTERVAL` (300) to get which 5-minute boundary that
   falls on: `5715226.534`.
4. `math.ceil()` rounds up to the next boundary: `5715227`.
5. Multiply back by 300 to get the epoch: `1714568100`.

**What this means:** Given the current time, find the next 5-minute-aligned
epoch that is at least 70 seconds in the future. This guarantees the worker
has the full PRE_START_SECONDS lead time.

**Edge case:** If `now` is exactly at a market boundary minus exactly 70 seconds,
`(now + 70) / 300` is an exact integer, and `math.ceil` of an integer returns
that integer. So the system targets that boundary. If `now` is even 0.001s
later, it jumps to the next boundary. This is correct -- we need *at least*
70 seconds.

**Rust equivalent:**

```rust
fn next_market_start(now_secs: f64) -> u64 {
    let adjusted = now_secs + PRE_START_SECONDS as f64;
    let boundary = (adjusted / MARKET_INTERVAL as f64).ceil() as u64;
    boundary * MARKET_INTERVAL
}
```

Use `f64` for the division to match Python's behavior exactly. The result is
always a `u64` (UNIX epoch, whole seconds).

### 3.3 Main Loop

```
supervisor.main()
  |
  |-- set up signal handlers (SIGINT, SIGTERM)
  |-- loop:
  |     |
  |     |-- now = time.time()
  |     |-- target = _next_market_start(now)      # next market epoch
  |     |-- launch_at = target - PRE_START_SECONDS # when to spawn worker
  |     |
  |     |-- if launch_at is in the future:
  |     |     sleep in 1-second increments until launch_at (or shutdown)
  |     |
  |     |-- if target not in launched:
  |     |     spawn Process(worker_main, args=(target,), daemon=True)
  |     |     add target to launched set
  |     |     add Process to active dict
  |     |
  |     |-- _reap_workers(active, restart_counts)
  |     |
  |     |-- prune launched set (remove entries older than 1 hour)
  |
  |-- shutdown: join all active workers (30s timeout), terminate stragglers
```

**Key observations:**

1. **Sleep granularity:** The supervisor sleeps in 1-second increments
   (`time.sleep(min(1.0, remaining))`), not one long sleep. This is solely
   for signal responsiveness -- SIGINT/SIGTERM set a `shutdown` flag that
   is checked each second.

2. **Launch timing:** The worker is spawned exactly `PRE_START_SECONDS`
   before the market starts. The `_next_market_start()` function guarantees
   this by computing the target relative to the current time plus the lead
   time.

3. **Dedup via `launched` set:** After sleeping and spawning, the loop
   immediately recalculates `_next_market_start()`. If the spawn was fast,
   the same target will come back, but it is already in `launched` so no
   double-spawn occurs. The loop then sleeps until the *next* market.

4. **Daemon processes:** Workers are spawned as `daemon=True`. This means
   if the supervisor dies unexpectedly (SIGKILL, OOM), all workers die too.
   This is a safety net -- orphaned workers collecting stale data are worse
   than no workers.

5. **No overlap management:** The supervisor does not prevent two workers
   from running simultaneously. In practice, a worker for market N is still
   in its tail phase (20s) when the worker for market N+1 is spawned (70s
   before N+1 starts, which is 70s after N starts, i.e., well within N's
   300s+20s lifetime). Multiple workers running concurrently is normal and
   expected.

### 3.4 Worker Reaping: `_reap_workers()`

```python
def _reap_workers(active, restart_counts, max_restarts=1):
```

Called every loop iteration. Scans all active processes:

1. **Find finished processes:** Check `proc.is_alive()` for each entry.
   Collect those that are not alive. Call `proc.join(timeout=1)` on each
   to clean up.

2. **Determine if death was premature:** Compute `expected_stop = ms +
   MARKET_DURATION + TAIL_SECONDS`. If `now < expected_stop` (the worker
   should still be running) AND `proc.exitcode != 0` (it crashed, not
   clean exit), this is a premature death.

3. **Restart logic:** If premature AND `restart_counts[ms] < max_restarts`
   (default 1), spawn a new `Process(worker_main, args=(ms,))` and
   increment the count. Otherwise, log an error and move on.

4. **Normal completion:** If `now >= expected_stop` OR `exitcode == 0`,
   log as normal finish.

**Why max_restarts = 1:**
A single restart handles transient failures (network blip, WS disconnect
during setup). If it crashes twice, there is likely a systematic issue
(API down, market doesn't exist) and retrying more would be pointless.
The market window is only 300s + 20s = 320s total; spending time on
repeated restart attempts wastes the collection window.

**Restart semantics:** The restarted worker gets the same `market_start`
epoch. It creates a new `data/{epoch}/` directory (or reuses the existing
one -- `mkdir(parents=True, exist_ok=True)`). It appends to the JSONL
files if they already exist (file opened in `"ab"` mode in `LineWriter`).
This means the restarted worker's data is appended after the crashed
worker's partial data. Downstream consumers must handle this (duplicate
`worker_started` system lines, possible gaps in sequence numbers).

### 3.5 Signal Handling and Graceful Shutdown

```python
shutdown = False

def _handle_signal(signum, frame):
    nonlocal shutdown
    shutdown = True

signal.signal(signal.SIGINT, _handle_signal)
signal.signal(signal.SIGTERM, _handle_signal)
```

Signals set a boolean flag. The main loop checks it:
- During sleep: exit the 1-second sleep loop
- Before spawn: skip the spawn
- At loop top: exit the loop

**Shutdown sequence:**

1. Log the number of active workers.
2. For each active worker: `proc.join(timeout=30)`. This gives workers up
   to 30 seconds to finish naturally (they have their own timer-based
   stop event).
3. If still alive after 30 seconds: `proc.terminate()` (sends SIGTERM to
   the child), then `proc.join(timeout=5)`.
4. If still alive after that: the process is abandoned (Python will clean
   it up on interpreter exit since they are daemon processes).

**Critical note:** The workers themselves have no signal handling. They stop
when their internal `_timer_task` fires (after `MARKET_DURATION + TAIL_SECONDS`)
or when the process is terminated. There is no mechanism for the supervisor
to tell a worker "stop early gracefully." The 30-second join timeout in
shutdown is just waiting for workers that might be near their natural end.

### 3.6 Launched Set Pruning

```python
cutoff = now - 3600
launched = {ms for ms in launched if ms > cutoff}
```

The `launched` set is pruned every loop iteration to remove entries older
than 1 hour. This is purely a memory optimization -- without it, the set
would grow indefinitely (one entry per 5-minute market = 288 per day).
One hour keeps ~12 entries, which is fine.

---

## 4. Data Flow: End to End

```
Time axis (not to scale):

  -70s            0s            +300s    +320s
   |               |              |        |
   v               v              v        v
  [SPAWN]     [MARKET START]  [MARKET END] [STOP]
   |               |              |        |
   | PRE_START     | MARKET_DUR   | TAIL   |
   | Worker init   | Active       | Drain  |
   | WS connect    | trading      | Settle |
   | Gamma lookup  |              |        |
   | Depth sync    |              |        |
```

1. **T-70s (spawn):** Supervisor calls `Process(worker_main, args=(market_start,))`.
   Worker forks, sets up logging, creates `data/{epoch}/` directory, opens JSONL
   files, writes `worker_started` system lines to both files.

2. **T-70s to T-0 (pre-start):** Worker runs `asyncio.gather()` with three tasks:
   - `run_polymarket()`: Gamma REST lookup (with retry/backoff) to find token IDs,
     then opens `POLY_WS_CONNECTIONS` WebSocket connections, begins streaming.
   - `run_binance()`: Opens combined WS stream, buffers depth events, fetches REST
     snapshot, aligns sequence IDs, begins steady-state streaming.
   - `_timer_task()`: Sleeps until `market_start + MARKET_DURATION + TAIL_SECONDS`.

3. **T-0 to T+300s (market active):** Both collectors are streaming. Polymarket
   receives order book snapshots, deltas, trades, price changes. Binance receives
   bookTicker (BBO), depth diffs (100ms), and trades. All events written as JSONL.

4. **T+300s to T+320s (tail):** Market has ended. Collectors continue capturing
   settlement events, final trades, book collapse.

5. **T+320s (stop):** `_timer_task` sets `stop_event`. Both collectors see the
   event, close WebSockets, flush remaining data. Worker writes
   `collector_stopped` system lines and exits.

6. **Supervisor reap:** Next loop iteration, `_reap_workers()` finds the process
   is no longer alive, joins it, logs normal completion.

**Concurrent workers:** At any given time during steady-state operation, there
are typically 1-2 workers running. Worker for market N is in its tail phase
(T+300s to T+320s) when worker for market N+1 is spawned (at T+230s relative
to N, i.e., T-70s relative to N+1). The overlap is about 90 seconds.

---

## 5. Rust Approach

### 5.1 Process Model: Tokio Tasks vs OS Processes

**Python's choice:** OS processes via `multiprocessing.Process`. This was
necessary because Python has the GIL -- CPU-bound JSON serialization in one
collector would block the other. Each process gets its own event loop.

**Rust recommendation: Tokio tasks (not OS processes).**

Rationale:
- Rust has no GIL. Async tasks on a multi-threaded Tokio runtime get true
  parallelism.
- Tokio tasks are far cheaper than OS processes (no fork, no IPC, shared memory
  space).
- Worker state (stop events, file handles) can use `Arc<AtomicBool>` and
  `Arc<Mutex<File>>` instead of multiprocessing primitives.
- If a collector panics, `tokio::spawn` returns a `JoinError` that the
  supervisor can catch. No need for exit code inspection.

**Structure:**

```rust
use tokio::task::JoinSet;

struct MarketWorker {
    market_start: u64,
    handle: tokio::task::JoinHandle<Result<(), WorkerError>>,
}

// In supervisor loop:
let mut workers: JoinSet<(u64, Result<(), WorkerError>)> = JoinSet::new();

// Spawn:
workers.spawn(async move {
    let result = run_worker(market_start).await;
    (market_start, result)
});

// Reap (non-blocking):
while let Some(Ok((market_start, result))) = workers.try_join_next() {
    match result {
        Ok(()) => info!("Worker {market_start} finished normally"),
        Err(e) if should_restart(market_start, &restart_counts) => {
            workers.spawn(async move { (market_start, run_worker(market_start).await) });
        }
        Err(e) => error!("Worker {market_start} failed: {e}"),
    }
}
```

`JoinSet` is the Rust equivalent of maintaining a dict of process handles --
it manages a set of spawned tasks and lets you poll for completions.

### 5.2 Signal Handling

**Python's choice:** `signal.signal()` with a closure that sets a boolean flag,
checked in a polling loop.

**Rust recommendation: `tokio::signal` with `select!`.**

```rust
use tokio::signal;
use tokio::sync::watch;

let (shutdown_tx, shutdown_rx) = watch::channel(false);

// In supervisor:
tokio::select! {
    _ = signal::ctrl_c() => {
        info!("Received SIGINT");
        shutdown_tx.send(true).ok();
    }
    _ = supervisor_loop(shutdown_rx.clone()) => {}
}

// For SIGTERM on Unix:
#[cfg(unix)]
{
    let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = sigterm.recv() => { shutdown_tx.send(true).ok(); }
        _ = signal::ctrl_c() => { shutdown_tx.send(true).ok(); }
        _ = supervisor_loop(shutdown_rx.clone()) => {}
    }
}
```

Use a `watch` channel to broadcast the shutdown signal to all tasks. Each worker
clones the receiver and checks it alongside its timer. This replaces the Python
pattern of a boolean flag + polling sleep.

Pass the `watch::Receiver<bool>` into each worker, which uses it in a
`tokio::select!` alongside its data streams. No polling needed -- the `select!`
wakes up immediately when the channel fires.

### 5.3 Timing and Scheduling

**Python's choice:** `time.sleep()` in a polling loop (1-second granularity).

**Rust recommendation: `tokio::time::sleep_until` with `Instant`.**

```rust
use tokio::time::{self, Instant, Duration};
use std::time::{SystemTime, UNIX_EPOCH};

fn next_market_start(now_secs: f64) -> u64 {
    let adjusted = now_secs + PRE_START_SECONDS as f64;
    let boundary = (adjusted / MARKET_INTERVAL as f64).ceil() as u64;
    boundary * MARKET_INTERVAL
}

// In supervisor loop:
let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
let target = next_market_start(now);
let launch_at = target - PRE_START_SECONDS;
let sleep_duration = Duration::from_secs_f64(launch_at as f64 - now);

tokio::select! {
    _ = time::sleep(sleep_duration) => {
        // Time to spawn
    }
    _ = shutdown_rx.changed() => {
        // Shutdown signal received during sleep
        break;
    }
}
```

This replaces the 1-second polling loop with a single async sleep that can
be cancelled by the shutdown signal. More precise, more efficient, and no
busy-waiting.

### 5.4 Worker Lifecycle Timer

**Python's choice:** `_timer_task` -- an async task that sleeps until the stop
time, then sets an `asyncio.Event`.

**Rust equivalent: `tokio::time::sleep` + `CancellationToken` or `watch` channel.**

```rust
use tokio_util::sync::CancellationToken;

let cancel = CancellationToken::new();
let stop_at = market_start + MARKET_DURATION + TAIL_SECONDS;
let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
let remaining = stop_at.saturating_sub(now);

// Timer task:
let cancel_clone = cancel.clone();
tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(remaining)).await;
    cancel_clone.cancel();
});

// In collector tasks, use select!:
tokio::select! {
    msg = ws_stream.next() => { /* process message */ }
    _ = cancel.cancelled() => { break; }
}
```

`CancellationToken` from `tokio-util` is the idiomatic Rust equivalent of
Python's `asyncio.Event` for "stop everything" signals. It supports cloning
and can be checked in `select!` branches.

### 5.5 Error Handling and Restart Strategy

**Python's choice:** Check `proc.exitcode != 0` and `now < expected_stop`.

**Rust recommendation: Typed errors + elapsed time check.**

```rust
#[derive(Debug, thiserror::Error)]
enum WorkerError {
    #[error("Polymarket collector failed: {0}")]
    Polymarket(#[source] anyhow::Error),
    #[error("Binance collector failed: {0}")]
    Binance(#[source] anyhow::Error),
    #[error("Both collectors failed")]
    Both {
        polymarket: anyhow::Error,
        binance: anyhow::Error,
    },
}

// Restart decision:
fn should_restart(market_start: u64, restart_count: u32) -> bool {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let expected_stop = market_start + MARKET_DURATION + TAIL_SECONDS;
    now < expected_stop && restart_count < MAX_RESTARTS
}
```

Use `thiserror` for error types, `anyhow` for error chaining. The restart
logic is the same as Python: only restart if the market window hasn't expired
and the restart budget hasn't been exhausted.

With Tokio tasks (vs OS processes), you also get `JoinError::is_panic()` to
distinguish panics from normal errors. A panic might warrant different handling
(e.g., log a backtrace, don't restart if it's a logic bug rather than a
network issue).

### 5.6 Key Crates

| Purpose | Crate | Why |
|---|---|---|
| Async runtime | `tokio` (full features) | Industry standard, required for everything else |
| WebSocket client | `tokio-tungstenite` | Native async WS, used with `tokio` |
| HTTP client | `reqwest` | For Gamma REST API and Binance depth snapshots |
| JSON serialization | `serde` + `serde_json` | For JSONL output; `simd-json` for parsing incoming WS messages if perf matters |
| Decimal arithmetic | `rust_decimal` | For Binance price levels (matches Python's `Decimal`) |
| Error handling | `thiserror` + `anyhow` | Typed errors for library code, `anyhow` for application-level chaining |
| Logging | `tracing` + `tracing-subscriber` | Structured logging, async-aware, replaces Python's `logging` |
| Signal handling | `tokio::signal` | Built into Tokio |
| Cancellation | `tokio-util` (`CancellationToken`) | For stop events and graceful shutdown |
| Time | `tokio::time` + `std::time` | Sleep, instant, system time |
| File I/O | `tokio::fs` or `std::fs` with `spawn_blocking` | For JSONL writes (see note below) |
| Config (optional) | `once_cell` or `std::sync::LazyLock` (1.80+) | For lazy static paths |

**Note on file I/O:** The Python system uses a dedicated writer thread per file
(`LineWriter` with a `queue.Queue`). In Rust, the equivalent is either:
- `tokio::sync::mpsc` channel -> `spawn_blocking` writer task (closest to Python's pattern)
- `tokio::fs::File` with `AsyncWriteExt` (simpler, but may have higher latency under load)
- `BufWriter<File>` in a `spawn_blocking` task with periodic flushes (best throughput)

The Python `LineWriter` flushes every 100ms or on queue drain. Replicate this
in Rust for comparable I/O behavior. Use `orjson`-equivalent speed by using
`serde_json::to_writer` writing directly to the buffer without intermediate
String allocation.

### 5.7 Suggested Module Structure

```
src/
  main.rs           -- entry point, signal setup, calls supervisor::run()
  config.rs         -- constants, path resolution
  supervisor.rs     -- scheduling loop, JoinSet management, restart logic
  worker.rs         -- per-market task: spawns collector tasks, manages timer
  polymarket.rs     -- Gamma lookup + WS streaming
  binance.rs        -- WS streaming + depth snapshot sync
  models.rs         -- SeqCounter, JSONL line builder, writer channel
  error.rs          -- WorkerError, CollectorError types
```

This mirrors the Python structure 1:1, which makes the port easier to verify.

---

## 6. Subtle Behaviors to Preserve

1. **File append on restart:** When a worker restarts, it appends to the same
   JSONL files (Python opens with `"ab"`). The Rust writer must use
   `OpenOptions::new().create(true).append(true)`. Downstream consumers must
   handle duplicate `worker_started` markers and sequence counter resets.

2. **Sequence counter reset on restart:** `SeqCounter` starts at 0 in each
   worker process. A restarted worker resets its counters. The JSONL data will
   have seq 1,2,3,...,N (crash), then 1,2,3,... again. This is by design --
   the `worker_started` system line marks the boundary.

3. **Market boundary at exact alignment:** If the supervisor starts at exactly
   `market_start - 70.000s`, `_next_market_start()` returns that market (not
   the one after). If it starts at `market_start - 69.999s`, it targets the
   *next* market. This is because `math.ceil` of an exact integer returns
   that integer. This behavior must be preserved.

4. **Launched set is checked after sleep:** The `if target not in launched`
   check happens after the sleep. This means if the supervisor is restarted,
   it will re-spawn a worker for a market that may have already been partially
   collected by a previous supervisor run. The `launched` set is in-memory
   only -- it does not persist across supervisor restarts.

5. **No inter-worker communication:** Workers are fully independent. They
   don't know about each other. The supervisor doesn't send them messages.
   The only coordination is the timer-based stop event within each worker.

6. **Worker exit code 0 is always "normal":** Even if a worker exits at
   `now < expected_stop`, if its exit code is 0, it is considered a normal
   finish and not restarted. This handles the case where a worker decides
   to stop early for a legitimate reason (e.g., market was resolved early).

7. **Graceful shutdown does not propagate to workers:** When the supervisor
   receives SIGINT/SIGTERM, it does NOT send any signal to workers. It just
   waits for them (30s join timeout). Workers will only stop when their
   internal timer fires, or when `proc.terminate()` is called after the
   timeout. In the Rust version with Tokio tasks, use the `watch` channel
   to propagate shutdown -- this is an improvement over the Python design.
