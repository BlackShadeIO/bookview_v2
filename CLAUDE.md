# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Real-time order book collector for BTC prediction markets. Runs a supervisor process that spawns worker subprocesses on 5-minute market boundaries, each collecting data from two sources simultaneously:

1. **Polymarket CLOB** — Gamma API lookup for market metadata (slug `btc-updown-5m-{epoch}`), then WebSocket streaming of order book events
2. **Binance BTCUSDT** — Combined `bookTicker` + `depth@100ms` WebSocket stream with REST snapshot sync (standard Binance depth management protocol)

## Running

```bash
pip install -r requirements.txt   # aiohttp, websockets
python run.py                     # starts supervisor (runs indefinitely)
```

No tests, no linter, no build step configured.

## Architecture

```
run.py → supervisor.main()
             │
             ├─ spawns multiprocessing.Process(worker_main) per 5-min market
             │   each worker runs asyncio.gather():
             │     ├─ run_polymarket()  → writes to data/{epoch}/clob.jsonl
             │     ├─ run_binance()     → writes to data/{epoch}/depth-trade.jsonl
             │     └─ _timer_task()     → sets stop_event after MARKET_DURATION + TAIL_SECONDS
             │
             └─ reaps finished workers, restarts crashed ones (max 1 restart)
```

- **supervisor.py** — Main loop. Calculates next market start, sleeps until 70s before it, spawns worker. Handles SIGINT/SIGTERM gracefully.
- **worker.py** — One process per market window. Opens two JSONL files, runs both collectors concurrently via asyncio, writes system markers at start/stop.
- **polymarket.py** — Gamma REST lookup with retry+backoff → WS connection with `LocalBook` (price-level book from full snapshots + deltas). Handles book, price_change, last_trade_price, best_bid_ask, tick_size_change, market_resolved events.
- **binance.py** — Two-phase depth sync: buffer WS depth events → fetch REST snapshot → validate `lastUpdateId` alignment → apply buffered diffs → steady-state streaming. `BinanceBook` uses `Decimal` for price arithmetic. Auto-resyncs on sequence gaps without reconnecting WS.
- **config.py** — All timing, URLs, paths as constants. `DATA_DIR = <repo>/data`.
- **models.py** — `SeqCounter` (per-file monotonic seq), `make_line()` (JSONL line builder with `ts_recv_ms`, `source`, `event_type`, `raw`, `normalized`), `write_line()` (JSON serialize + flush).

## Data Format

Output lives in `data/{market_start_epoch}/`. Each line is JSON with fields: `ts_recv_ms`, `ts_recv_unix`, `market_start`, `collector_pid`, `source`, `event_type`, `seq`, `raw`, `normalized`, and optionally `system_type`.

## Key Timing Constants (config.py)

- `PRE_START_SECONDS = 70` — start collecting before market
- `MARKET_DURATION = 300` — 5-minute windows
- `TAIL_SECONDS = 20` — keep collecting after market end
- `GAMMA_LOOKUP_DEADLINE = 10` — give up Gamma lookup this many seconds after market start

## Viewer (viewer/)

Next.js 15 app for replaying and studying collected market data. Bloomberg terminal aesthetic.

### Running the Viewer

```bash
cd viewer
npm install
npm run build && npm start    # production mode (recommended — Turbopack dev has streaming issues)
# or: npm run serve           # build + start in one command
```

Open http://localhost:3000. Select a market → processes JSONL on first access → interactive chart playback.

### Viewer Architecture

- **Pre-processing**: Raw JSONL (~2GB/market) → frames at 100ms intervals (~4K frames, ~2MB). Runs in child process (`scripts/process-worker.mjs`), cached to `data/{epoch}/.cache/`
- **State**: Zustand store (`stores/playback-store.ts`) — single store for frames, playback state, UI
- **Playback**: `requestAnimationFrame` engine (`hooks/usePlaybackEngine.ts`) with accumulator pattern. Speeds: 1x/2x/5x/10x
- **Charts**: lightweight-charts for price lines (BTC, Up/Down tokens), custom canvas for 10-level depth bars. Updated imperatively via Zustand subscriptions, not React re-renders
- **API**: `/api/markets` (list), `/api/markets/[epoch]` (data), `/api/markets/[epoch]/process` (POST=start, GET=poll progress)

### Known Issue

Turbopack dev mode (`next dev`) hangs during JSONL processing. Use production mode (`npm run build && npm start`).
