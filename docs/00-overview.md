# BookView v2 — Rust Rewrite Reference

This directory contains the complete documentation of the Python data collector, written as a reference for reimplementing it in Rust. Every design decision, protocol detail, data format, and edge case is documented.

## Why Rust

The Python collector works — 23ms median latency, ~0% data loss with 10 redundant WebSocket connections. But it's the backbone of an HFT bot. Python's GIL, garbage collector pauses, and asyncio overhead create a P95 ceiling of ~5 seconds during activity bursts. Rust eliminates all three: no GIL, no GC, and async with zero-cost abstractions. Target: P95 within 1-2ms of physical network latency.

## System Overview

```
run.rs → supervisor task
              │
              ├─ spawns tokio task per 5-min market window
              │   each market task runs:
              │     ├─ polymarket_collector()  → writes clob.jsonl
              │     ├─ binance_collector()     → writes depth-trade.jsonl
              │     └─ timer (CancellationToken after MARKET_DURATION + TAIL_SECONDS)
              │
              └─ reaps finished tasks, handles shutdown signals
```

Two data sources collected simultaneously per 5-minute BTC prediction market:
- **Polymarket CLOB**: REST lookup → 10 redundant WebSocket connections → order book reconstruction → JSONL
- **Binance BTCUSDT**: Combined WS stream (bookTicker + depth@100ms + trades) → two-phase depth sync → order book → JSONL

## Document Index

| Doc | Covers | Key Rust Decisions |
|---|---|---|
| [01-supervisor-and-config](01-supervisor-and-config.md) | Process supervision, market scheduling, timing constants, signal handling | `tokio::JoinSet` over OS processes, `CancellationToken` for shutdown, compile-time constants |
| [02-polymarket-collector](02-polymarket-collector.md) | Gamma API, 10 WS connections, LocalBook, dedup, all event types | `BTreeMap<Decimal,Decimal>` book, `DashSet<u64>` dedup with ahash, `tokio-tungstenite` |
| [03-binance-collector](03-binance-collector.md) | Two-phase depth sync, BinanceBook, gap detection, strike price, combined stream | `rust_decimal`, explicit `SyncState` enum state machine, `BTreeMap` price levels |
| [04-io-pipeline-and-worker](04-io-pipeline-and-worker.md) | LineWriter thread, SeqCounter, make_line(), worker lifecycle | Likely no writer thread needed — serde_json fast enough inline. `BufWriter` + periodic flush |
| [05-data-format-and-schema](05-data-format-and-schema.md) | JSONL schema, every event type with fields, viewer compatibility | `serde_json` for viewer compat, typed structs with `#[serde(tag)]` enums |

## Critical Protocols to Port Exactly

1. **Market boundary calculation** — `ceil(now / 300) * 300` with pre-start offset of 70s. Must match Python exactly or markets will be missed. (Doc 01, §3.2)

2. **Binance depth sync** — The two-phase buffer→snapshot→validate→apply protocol. The `U <= lastUpdateId + 1 <= u` condition. Gap detection and resync without WS reconnect. (Doc 03, §1)

3. **Polymarket dedup** — Content-hash dedup across 10 connections. Must use canonical serialization (sorted keys). First-write-wins. (Doc 02, §4)

4. **JSONL format** — The viewer depends on specific event types and field names. Breaking changes = viewer can't replay data. (Doc 05, §5)

## Recommended Crate Stack

| Purpose | Crate | Notes |
|---|---|---|
| Async runtime | `tokio` | Full features: rt-multi-thread, macros, time, signal, sync |
| HTTP client | `reqwest` | Gamma API + Binance REST snapshots |
| WebSocket | `tokio-tungstenite` | With `native-tls` or `rustls` |
| JSON | `serde_json` | Primary serializer. Consider `simd-json` for parsing hot path |
| Decimal | `rust_decimal` | Binance price arithmetic. Polymarket can use string-only |
| Hashing | `ahash` | Fast non-crypto hash for dedup set |
| Concurrent set | `dashmap` | `DashSet` for cross-connection dedup |
| Shutdown | `tokio-util` | `CancellationToken` for cooperative cancellation |
| Logging | `tracing` + `tracing-subscriber` | Structured, async-aware |
| Time | `chrono` or `std::time` | Wall clock timestamps |
| CLI/config | `clap` | If runtime config needed |
| Error | `anyhow` + `thiserror` | Application vs library errors |

## Architecture Decisions for Rust

### No writer thread needed
Python needed a dedicated writer thread because `json.dumps()` blocked the asyncio event loop. Rust's `serde_json::to_writer()` serializes fast enough (~1-5μs per line) to stay inline. Use `BufWriter<File>` with periodic `flush()` via a tokio interval.

### Tokio tasks, not OS processes
Python uses `multiprocessing.Process` per market to escape the GIL. Rust has no GIL — use `tokio::spawn` for each market. Lighter weight, shared memory, easier communication.

### Order book as BTreeMap
Python uses `dict[str, str]` with ephemeral `sorted()` calls in `snapshot()`. Rust should use `BTreeMap<Decimal, Decimal>` — always sorted, O(log n) insert/remove, O(1) best bid/ask via `iter().next_back()` / `iter().next()`.

### Dedup with hash, not full serialization
Python serializes the full message to JSON for dedup keys. Rust can hash the raw message bytes with `ahash` — no serialization needed, just hash the incoming `&[u8]` from the WebSocket frame.

### Typed events with serde enums
Python uses stringly-typed `event_type` fields and `dict` for everything. Rust should define `enum EventType` with `#[serde(tag = "event_type")]` for compile-time exhaustiveness checks while maintaining JSON compatibility.

## Build Order

Recommended implementation sequence:

1. **Config + types** — Constants, JSONL line struct, event enums with serde
2. **IO layer** — BufWriter wrapper with periodic flush, line builder
3. **Binance collector** — Simpler protocol (single WS), well-documented sync algorithm
4. **Polymarket collector** — More complex (Gamma API + 10 WS + book + dedup)
5. **Worker orchestration** — Market task lifecycle, timer, error handling
6. **Supervisor** — Market scheduling, signal handling, task management
7. **Integration** — End-to-end test with real market data, verify viewer compatibility

Start with Binance because it has a single WebSocket connection and a well-defined state machine. Polymarket is harder due to the redundant connections and dedup logic.

## Verification Checklist

- [ ] Market boundary calculation matches Python output for edge cases
- [ ] Binance depth sync produces identical book state as Python for same input
- [ ] Polymarket dedup correctly filters duplicates across 10 connections
- [ ] JSONL output parseable by existing Next.js viewer
- [ ] Latency: median within 2x of physical network latency
- [ ] Latency: P95 within 10x of physical network latency
- [ ] Data completeness: no dropped messages vs Python running side-by-side
- [ ] Graceful shutdown: collector_stopped marker present in every JSONL
- [ ] Crash recovery: supervisor restarts failed market tasks
- [ ] File sizes comparable to Python output (same data, similar JSON)
