use std::path::PathBuf;
use std::sync::LazyLock;

// Timing
pub const PRE_START_SECONDS: u64 = 70;
pub const MARKET_DURATION: u64 = 300;
pub const TAIL_SECONDS: u64 = 20;
pub const MARKET_INTERVAL: u64 = 300;
pub const GAMMA_LOOKUP_DEADLINE: u64 = 10;
pub const GAMMA_RETRY_BASE_SECS: f64 = 1.0;
pub const GAMMA_RETRY_MAX_SECS: f64 = 8.0;
pub const WS_RECONNECT_BASE_SECS: f64 = 1.0;
pub const WS_RECONNECT_MAX_SECS: f64 = 16.0;
pub const POLY_WS_CONNECTIONS: usize = 10;
pub const MAX_RESTARTS: u32 = 1;

// URLs
pub const GAMMA_API_BASE: &str = "https://gamma-api.polymarket.com";
pub const POLYMARKET_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
pub const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/stream?streams=btcusdt@bookTicker/btcusdt@depth@100ms/btcusdt@trade";
pub const BINANCE_DEPTH_SNAPSHOT_URL: &str =
    "https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=5000";

// File names
pub const CLOB_FILENAME: &str = "clob.jsonl";
pub const DEPTH_FILENAME: &str = "depth-trade.jsonl";

pub static DATA_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    if let Ok(dir) = std::env::var("BOOKVIEW_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .unwrap_or(std::path::Path::new("."))
        .join("data")
});

// Fair value engine
pub const FV_FILENAME: &str = "fair-value.jsonl";
pub const FV_TICK_MS: u64 = 100;
pub const FV_DEPTH_LEVELS: usize = 10;
pub const FV_IMBALANCE_LEVELS: usize = 10;
pub const FV_MICROPRICE_WEIGHT: f64 = 0.5;
pub const FV_DEPTH_WEIGHT: f64 = 0.5;
pub const FV_TRADE_HALFLIFE_MS: f64 = 4000.0;
pub const FV_DRIFT_COEFF: f64 = 0.3;
pub const FV_VOL_FLOOR: f64 = 1e-8;

// LSTM model directory
pub static MODEL_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    if let Ok(dir) = std::env::var("BOOKVIEW_MODEL_DIR") {
        return PathBuf::from(dir);
    }
    DATA_DIR
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("models")
});

pub fn gamma_market_url(slug: &str) -> String {
    format!("{GAMMA_API_BASE}/markets/slug/{slug}")
}
