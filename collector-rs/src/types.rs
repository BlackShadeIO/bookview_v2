use serde::ser::Serializer;
use serde::Serialize;
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

static PID: LazyLock<u32> = LazyLock::new(std::process::id);

pub enum RawJson {
    Null,
    Raw(Box<RawValue>),
    Value(serde_json::Value),
}

impl RawJson {
    pub fn from_string(s: String) -> Self {
        match RawValue::from_string(s) {
            Ok(rv) => Self::Raw(rv),
            Err(_) => Self::Null,
        }
    }

    pub fn from_serialize(v: &impl Serialize) -> Self {
        match serde_json::to_string(v) {
            Ok(s) => Self::from_string(s),
            Err(_) => Self::Null,
        }
    }
}

impl std::fmt::Debug for RawJson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => write!(f, "Null"),
            Self::Raw(rv) => write!(f, "Raw({})", rv.get()),
            Self::Value(v) => write!(f, "Value({v})"),
        }
    }
}

impl Serialize for RawJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_none(),
            Self::Raw(rv) => rv.serialize(serializer),
            Self::Value(v) => v.serialize(serializer),
        }
    }
}

impl From<serde_json::Value> for RawJson {
    fn from(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Self::Null,
            other => Self::Value(other),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Line {
    pub ts_recv_ms: i64,
    pub ts_recv_unix: f64,
    pub market_start: i64,
    pub collector_pid: u32,
    pub source: &'static str,
    pub event_type: Cow<'static, str>,
    pub seq: u64,
    pub raw: RawJson,
    pub normalized: RawJson,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_type: Option<String>,
}

pub fn make_line(
    market_start: i64,
    source: &'static str,
    event_type: impl Into<Cow<'static, str>>,
    seq: u64,
    raw: impl Into<RawJson>,
    normalized: impl Into<RawJson>,
) -> Line {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let ts_recv_ms = now.as_millis() as i64;
    let ts_recv_unix = ts_recv_ms as f64 / 1000.0;

    Line {
        ts_recv_ms,
        ts_recv_unix,
        market_start,
        collector_pid: *PID,
        source,
        event_type: event_type.into(),
        seq,
        raw: raw.into(),
        normalized: normalized.into(),
        system_type: None,
    }
}

pub fn make_system_line(
    market_start: i64,
    source: &'static str,
    seq: u64,
    normalized: impl Into<RawJson>,
    system_type: impl Into<String>,
) -> Line {
    let mut line = make_line(
        market_start,
        source,
        "system",
        seq,
        serde_json::Value::Null,
        normalized,
    );
    line.system_type = Some(system_type.into());
    line
}

// -- Shared utilities --

pub fn parse_decimal_key(value: &str, scale: i64) -> i64 {
    let mut whole = 0_i64;
    let mut frac = 0_i64;
    let mut frac_scale = scale;
    let mut seen_dot = false;

    for b in value.bytes() {
        match b {
            b'0'..=b'9' if seen_dot && frac_scale > 1 => {
                frac_scale /= 10;
                frac += ((b - b'0') as i64) * frac_scale;
            }
            b'0'..=b'9' if !seen_dot => {
                whole = whole.saturating_mul(10).saturating_add((b - b'0') as i64);
            }
            b'.' => seen_dot = true,
            _ => {}
        }
    }

    whole.saturating_mul(scale).saturating_add(frac)
}

pub fn is_zero_decimal(value: &str) -> bool {
    value.bytes().all(|b| b == b'0' || b == b'.')
}

// -- Polymarket normalized structs --

#[derive(Debug, Serialize)]
pub struct PolyBookState<'a> {
    pub bid_count: usize,
    pub ask_count: usize,
    pub bids: Vec<[&'a str; 2]>,
    pub asks: Vec<[&'a str; 2]>,
    pub best_bid: Option<&'a str>,
    pub best_ask: Option<&'a str>,
    pub asset_id: &'a str,
    pub last_trade_price: Option<&'a str>,
    pub trigger_event_type: &'a str,
}

// -- Binance normalized structs --

#[derive(Debug, Serialize)]
pub struct BinanceBookState<'a> {
    pub update_id: i64,
    pub best_bid_price: Option<&'a str>,
    pub best_bid_qty: Option<&'a str>,
    pub best_ask_price: Option<&'a str>,
    pub best_ask_qty: Option<&'a str>,
    pub spread: Option<String>,
    pub bid_count: usize,
    pub ask_count: usize,
    pub bids: Vec<[&'a str; 2]>,
    pub asks: Vec<[&'a str; 2]>,
    pub trigger_event: TriggerEvent,
}

#[derive(Debug, Serialize)]
pub struct TriggerEvent {
    #[serde(rename = "U")]
    pub first_update_id: i64,
    #[serde(rename = "u")]
    pub last_update_id: i64,
}

#[derive(Debug, Serialize)]
pub struct TradeNormalized {
    pub price: String,
    pub qty: String,
    pub side: &'static str,
    pub trade_id: i64,
    pub trade_time: i64,
    pub event_time: i64,
}

#[derive(Debug, Serialize)]
pub struct BookTickerNormalized {
    pub bid_price: String,
    pub bid_qty: String,
    pub ask_price: String,
    pub ask_qty: String,
    pub update_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_best_bid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_best_ask: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consistent: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct StrikePrice {
    pub strike_price: String,
    pub strike_bid: String,
    pub strike_ask: String,
    pub market_start: i64,
}

// ── Executor channel payloads ───────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct PolyBbaSnapshot {
    pub yes_best_bid: Option<f64>,
    pub yes_best_ask: Option<f64>,
    pub no_best_bid: Option<f64>,
    pub no_best_ask: Option<f64>,
    pub ts_recv_ms: i64,
}

#[derive(Clone, Debug)]
pub struct MarketInfo {
    pub condition_id: String,
    pub clob_token_ids: Vec<String>,
    pub slug: String,
    pub market_start: i64,
}

// ── Fair value channel payloads ──────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct BbaSnapshot {
    pub bid_price: f64,
    pub bid_qty: f64,
    pub ask_price: f64,
    pub ask_qty: f64,
    pub ts_recv_ms: i64,
}

#[derive(Clone, Debug)]
pub struct FvDepthSnapshot {
    pub bids: Vec<(f64, f64)>,
    pub asks: Vec<(f64, f64)>,
    pub ts_recv_ms: i64,
}

#[derive(Clone, Debug)]
pub struct FvTradeSnapshot {
    pub qty: f64,
    pub is_buy: bool,
    pub ts_recv_ms: i64,
}

#[derive(Clone, Debug)]
pub struct FvStrikeInfo {
    pub strike_price: f64,
    pub market_start_ms: i64,
    pub market_end_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct FairValueSnapshot {
    pub tau: f64,
    pub microprice: f64,
    pub depth_microprice: f64,
    pub blended_spot: f64,
    pub strike_price: f64,
    pub rv_1s: f64,
    pub rv_5s: f64,
    pub rv_15s: f64,
    pub sigma_returns: f64,
    pub sigma_dollars: f64,
    pub trade_imbalance: f64,
    pub book_imbalance: f64,
    pub mu: f64,
    pub z_score: f64,
    pub fair_yes: f64,
    pub fair_no: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lstm_fair_yes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lstm_fair_no: Option<f64>,
}
