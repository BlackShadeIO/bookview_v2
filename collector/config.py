"""Constants and configuration for the BTC collector."""

from pathlib import Path

# ── Timing ──────────────────────────────────────────────────────────────────
PRE_START_SECONDS = 70          # start collecting this many seconds before market
MARKET_DURATION = 300           # 5-minute markets
TAIL_SECONDS = 20               # keep collecting after market end
MARKET_INTERVAL = 300           # markets on 5-minute boundaries
GAMMA_LOOKUP_DEADLINE = 10      # give up Gamma lookup this many seconds after market start
GAMMA_RETRY_BASE = 1.0          # initial retry backoff seconds
GAMMA_RETRY_MAX = 8.0           # max retry backoff seconds

# ── URLs ────────────────────────────────────────────────────────────────────
GAMMA_API_BASE = "https://gamma-api.polymarket.com"
GAMMA_MARKET_BY_SLUG = GAMMA_API_BASE + "/markets/slug/{slug}"

POLYMARKET_WS_URL = "wss://ws-subscriptions-clob.polymarket.com/ws/market"

BINANCE_WS_URL = (
    "wss://stream.binance.com:9443/stream"
    "?streams=btcusdt@bookTicker/btcusdt@depth@100ms/btcusdt@trade"
)
BINANCE_DEPTH_SNAPSHOT_URL = (
    "https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=5000"
)

# ── Paths ───────────────────────────────────────────────────────────────────
DATA_DIR = Path(__file__).resolve().parent.parent / "data"
CLOB_FILENAME = "clob.jsonl"
DEPTH_FILENAME = "depth-trade.jsonl"

# ── Reconnect ───────────────────────────────────────────────────────────────
WS_RECONNECT_BASE = 1.0
WS_RECONNECT_MAX = 16.0

# ── Polymarket redundancy ──────────────────────────────────────────────────
POLY_WS_CONNECTIONS = 10
