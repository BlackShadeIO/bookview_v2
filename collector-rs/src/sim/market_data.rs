use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::config::{MARKET_DURATION, TAIL_SECONDS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenSide {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TradeSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct PriceLevel {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone)]
pub enum SimEvent {
    PolyBookState {
        ts_ms: i64,
        token: TokenSide,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        best_bid: f64,
        best_ask: f64,
    },
    PolyTrade {
        ts_ms: i64,
        token: TokenSide,
        price: f64,
        side: TradeSide,
        size: f64,
    },
    BtcTicker {
        ts_ms: i64,
        bid: f64,
        ask: f64,
        mid: f64,
    },
    BtcTrade {
        ts_ms: i64,
        price: f64,
        qty: f64,
        is_buy: bool,
    },
    BtcDepth {
        ts_ms: i64,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
    },
    FairValue {
        ts_ms: i64,
        fair_yes: f64,
        fair_no: f64,
        lstm_fair_yes: Option<f64>,
        lstm_fair_no: Option<f64>,
        tau: f64,
        z_score: f64,
        trade_imbalance: f64,
        book_imbalance: f64,
        sigma_returns: f64,
    },
}

impl SimEvent {
    pub fn ts_ms(&self) -> i64 {
        match self {
            Self::PolyBookState { ts_ms, .. } => *ts_ms,
            Self::PolyTrade { ts_ms, .. } => *ts_ms,
            Self::BtcTicker { ts_ms, .. } => *ts_ms,
            Self::BtcTrade { ts_ms, .. } => *ts_ms,
            Self::BtcDepth { ts_ms, .. } => *ts_ms,
            Self::FairValue { ts_ms, .. } => *ts_ms,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BookSnapshot {
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub best_bid: f64,
    pub best_ask: f64,
}

#[derive(Debug, Clone)]
pub struct MarketState {
    pub ts_ms: i64,
    pub tau: f64,
    pub up_book: Option<BookSnapshot>,
    pub down_book: Option<BookSnapshot>,
    pub btc_mid: f64,
    pub btc_bid: f64,
    pub btc_ask: f64,
    pub btc_depth_bids: Vec<PriceLevel>,
    pub btc_depth_asks: Vec<PriceLevel>,
    pub fair_yes: Option<f64>,
    pub fair_no: Option<f64>,
    pub lstm_fair_yes: Option<f64>,
    pub lstm_fair_no: Option<f64>,
    pub z_score: Option<f64>,
    pub trade_imbalance: Option<f64>,
    pub book_imbalance: Option<f64>,
    pub sigma_returns: Option<f64>,
    pub last_trade_up: Option<(f64, TradeSide, i64)>,
    pub last_trade_down: Option<(f64, TradeSide, i64)>,
}

impl MarketState {
    pub fn new() -> Self {
        Self {
            ts_ms: 0,
            tau: 300.0,
            up_book: None,
            down_book: None,
            btc_mid: 0.0,
            btc_bid: 0.0,
            btc_ask: 0.0,
            btc_depth_bids: Vec::new(),
            btc_depth_asks: Vec::new(),
            fair_yes: None,
            fair_no: None,
            lstm_fair_yes: None,
            lstm_fair_no: None,
            z_score: None,
            trade_imbalance: None,
            book_imbalance: None,
            sigma_returns: None,
            last_trade_up: None,
            last_trade_down: None,
        }
    }

    pub fn update(&mut self, event: &SimEvent, market_end_ms: i64) {
        self.ts_ms = event.ts_ms();
        self.tau = ((market_end_ms - self.ts_ms) as f64 / 1000.0).max(0.0);

        match event {
            SimEvent::PolyBookState { token, bids, asks, best_bid, best_ask, .. } => {
                let snap = BookSnapshot {
                    bids: bids.clone(),
                    asks: asks.clone(),
                    best_bid: *best_bid,
                    best_ask: *best_ask,
                };
                match token {
                    TokenSide::Up => self.up_book = Some(snap),
                    TokenSide::Down => self.down_book = Some(snap),
                }
            }
            SimEvent::PolyTrade { token, price, side, ts_ms, .. } => {
                match token {
                    TokenSide::Up => self.last_trade_up = Some((*price, *side, *ts_ms)),
                    TokenSide::Down => self.last_trade_down = Some((*price, *side, *ts_ms)),
                }
            }
            SimEvent::BtcTicker { bid, ask, mid, .. } => {
                self.btc_mid = *mid;
                self.btc_bid = *bid;
                self.btc_ask = *ask;
            }
            SimEvent::BtcTrade { .. } => {}
            SimEvent::BtcDepth { bids, asks, .. } => {
                self.btc_depth_bids = bids.clone();
                self.btc_depth_asks = asks.clone();
            }
            SimEvent::FairValue { fair_yes, fair_no, lstm_fair_yes, lstm_fair_no, tau, z_score, trade_imbalance, book_imbalance, sigma_returns, .. } => {
                self.fair_yes = Some(*fair_yes);
                self.fair_no = Some(*fair_no);
                self.lstm_fair_yes = *lstm_fair_yes;
                self.lstm_fair_no = *lstm_fair_no;
                self.tau = *tau;
                self.z_score = Some(*z_score);
                self.trade_imbalance = Some(*trade_imbalance);
                self.book_imbalance = Some(*book_imbalance);
                self.sigma_returns = Some(*sigma_returns);
            }
        }
    }

    pub fn up_best_bid(&self) -> Option<f64> {
        self.up_book.as_ref().map(|b| b.best_bid).filter(|p| *p > 0.0)
    }
    pub fn up_best_ask(&self) -> Option<f64> {
        self.up_book.as_ref().map(|b| b.best_ask).filter(|p| *p > 0.0)
    }
    pub fn down_best_bid(&self) -> Option<f64> {
        self.down_book.as_ref().map(|b| b.best_bid).filter(|p| *p > 0.0)
    }
    pub fn down_best_ask(&self) -> Option<f64> {
        self.down_book.as_ref().map(|b| b.best_ask).filter(|p| *p > 0.0)
    }
}

pub struct MarketReplay {
    pub epoch: i64,
    pub strike: f64,
    pub outcome: bool,
    pub up_token_id: String,
    pub down_token_id: String,
    pub events: Vec<SimEvent>,
    pub market_start_ms: i64,
    pub market_end_ms: i64,
}

pub fn load_market(data_dir: &Path, epoch: i64) -> Result<MarketReplay, String> {
    let folder = data_dir.join(epoch.to_string());
    let clob_path = folder.join("clob.jsonl");
    let depth_path = folder.join("depth-trade.jsonl");
    let fv_path = folder.join("fair-value.jsonl");

    let market_start_ms = epoch * 1000;
    let market_end_ms = market_start_ms + (MARKET_DURATION as i64) * 1000;
    let collection_end_ms = market_end_ms + (TAIL_SECONDS as i64) * 1000;

    let mut events: Vec<SimEvent> = Vec::new();
    let mut strike = 0.0f64;
    let mut final_btc_mid = 0.0f64;
    let mut up_token_id = String::new();
    let mut down_token_id = String::new();

    // Parse clob.jsonl
    if clob_path.exists() {
        parse_clob(&clob_path, &mut events, &mut up_token_id, &mut down_token_id)?;
    }

    // Parse depth-trade.jsonl
    if depth_path.exists() {
        parse_depth_trade(&depth_path, &mut events, &mut strike, &mut final_btc_mid, collection_end_ms)?;
    } else {
        return Err("No depth-trade.jsonl".into());
    }

    // Parse fair-value.jsonl
    if fv_path.exists() {
        parse_fair_value(&fv_path, &mut events)?;
    }

    if strike == 0.0 {
        return Err("No strike price found".into());
    }

    events.sort_by_key(|e| e.ts_ms());

    Ok(MarketReplay {
        epoch,
        strike,
        outcome: final_btc_mid > strike,
        up_token_id,
        down_token_id,
        events,
        market_start_ms,
        market_end_ms,
    })
}

fn parse_clob(path: &Path, events: &mut Vec<SimEvent>, up_id: &mut String, down_id: &mut String) -> Result<(), String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = BufReader::with_capacity(512 * 1024, file);

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Read error: {e}"))?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let ts_ms = v["ts_recv_ms"].as_i64().unwrap_or(0);
        let event_type = v["event_type"].as_str().unwrap_or("");

        match event_type {
            "market_lookup" => {
                let raw = v.get("raw").or_else(|| v.get("normalized"));
                if let Some(obj) = raw.and_then(|r| r.as_object()) {
                    if let Some(ids_val) = obj.get("clobTokenIds") {
                        let ids: Vec<String> = if let Some(s) = ids_val.as_str() {
                            serde_json::from_str(s).unwrap_or_default()
                        } else if let Some(arr) = ids_val.as_array() {
                            arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                        } else {
                            Vec::new()
                        };
                        if ids.len() >= 2 {
                            *up_id = ids[0].clone();
                            *down_id = ids[1].clone();
                        }
                    }
                }
            }
            "book_state" => {
                let n = &v["normalized"];
                let asset_id = n["asset_id"].as_str().unwrap_or("");
                let token = if asset_id == up_id.as_str() {
                    TokenSide::Up
                } else if asset_id == down_id.as_str() {
                    TokenSide::Down
                } else {
                    continue;
                };
                let best_bid = parse_str_f64_val(n.get("best_bid"));
                let best_ask = parse_str_f64_val(n.get("best_ask"));
                if best_bid <= 0.0 || best_ask <= 0.0 { continue; }

                let bids = parse_poly_levels(n.get("bids"), 50);
                let asks = parse_poly_levels(n.get("asks"), 50);

                events.push(SimEvent::PolyBookState { ts_ms, token, bids, asks, best_bid, best_ask });
            }
            "last_trade_price_raw" => {
                let raw = &v["raw"];
                let asset_id = raw["asset_id"].as_str().unwrap_or("");
                let token = if asset_id == up_id.as_str() {
                    TokenSide::Up
                } else if asset_id == down_id.as_str() {
                    TokenSide::Down
                } else {
                    continue;
                };
                let price = parse_str_f64_val(raw.get("price"));
                let size = parse_str_f64_val(raw.get("size"));
                let side_str = raw["side"].as_str().unwrap_or("");
                let side = if side_str == "BUY" { TradeSide::Buy } else { TradeSide::Sell };
                if price > 0.0 && size > 0.0 {
                    events.push(SimEvent::PolyTrade { ts_ms, token, price, side, size });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_depth_trade(path: &Path, events: &mut Vec<SimEvent>, strike: &mut f64, final_btc_mid: &mut f64, collection_end_ms: i64) -> Result<(), String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = BufReader::with_capacity(1024 * 1024, file);

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Read error: {e}"))?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let ts_ms = v["ts_recv_ms"].as_i64().unwrap_or(0);
        let event_type = v["event_type"].as_str().unwrap_or("");

        match event_type {
            "book_ticker_normalized" => {
                let n = &v["normalized"];
                let bid = parse_str_f64_val(n.get("bid_price"));
                let ask = parse_str_f64_val(n.get("ask_price"));
                if bid > 0.0 && ask > 0.0 {
                    let mid = (bid + ask) / 2.0;
                    events.push(SimEvent::BtcTicker { ts_ms, bid, ask, mid });
                    if ts_ms <= collection_end_ms {
                        *final_btc_mid = mid;
                    }
                }
            }
            "trade_normalized" => {
                let n = &v["normalized"];
                let price = parse_str_f64_val(n.get("price"));
                let qty = parse_str_f64_val(n.get("qty"));
                let is_buy = n["side"].as_str() == Some("BUY");
                if price > 0.0 && qty > 0.0 {
                    events.push(SimEvent::BtcTrade { ts_ms, price, qty, is_buy });
                }
            }
            "book_state" => {
                let n = &v["normalized"];
                let bids = parse_depth_levels(n.get("bids"), 20);
                let asks = parse_depth_levels(n.get("asks"), 20);
                if !bids.is_empty() && !asks.is_empty() {
                    events.push(SimEvent::BtcDepth { ts_ms, bids, asks });
                }
            }
            "system" => {
                if v["system_type"].as_str() == Some("strike_price") {
                    let sp = parse_str_f64_val(v["normalized"].get("strike_price"));
                    if sp > 0.0 { *strike = sp; }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_fair_value(path: &Path, events: &mut Vec<SimEvent>) -> Result<(), String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let reader = BufReader::with_capacity(256 * 1024, file);

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Read error: {e}"))?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if v["event_type"].as_str() != Some("fv_sample") { continue; }
        let ts_ms = v["ts_recv_ms"].as_i64().unwrap_or(0);
        let n = &v["normalized"];

        let fair_yes = n["fair_yes"].as_f64().unwrap_or(0.0);
        let fair_no = n["fair_no"].as_f64().unwrap_or(0.0);
        if fair_yes == 0.0 { continue; }

        events.push(SimEvent::FairValue {
            ts_ms,
            fair_yes,
            fair_no,
            lstm_fair_yes: n["lstm_fair_yes"].as_f64(),
            lstm_fair_no: n["lstm_fair_no"].as_f64(),
            tau: n["tau"].as_f64().unwrap_or(0.0),
            z_score: n["z_score"].as_f64().unwrap_or(0.0),
            trade_imbalance: n["trade_imbalance"].as_f64().unwrap_or(0.0),
            book_imbalance: n["book_imbalance"].as_f64().unwrap_or(0.5),
            sigma_returns: n["sigma_returns"].as_f64().unwrap_or(0.0),
        });
    }
    Ok(())
}

fn parse_str_f64_val(v: Option<&serde_json::Value>) -> f64 {
    match v {
        Some(val) => val.as_str().and_then(|s| s.parse().ok())
            .or_else(|| val.as_f64())
            .unwrap_or(0.0),
        None => 0.0,
    }
}

fn parse_poly_levels(v: Option<&serde_json::Value>, max: usize) -> Vec<PriceLevel> {
    let Some(arr) = v.and_then(|v| v.as_array()) else { return Vec::new() };
    arr.iter().take(max).filter_map(|entry| {
        let a = entry.as_array()?;
        let price: f64 = a.first()?.as_str()?.parse().ok()?;
        let size: f64 = a.get(1)?.as_str()?.parse().ok()?;
        Some(PriceLevel { price, size })
    }).collect()
}

fn parse_depth_levels(v: Option<&serde_json::Value>, max: usize) -> Vec<PriceLevel> {
    let Some(arr) = v.and_then(|v| v.as_array()) else { return Vec::new() };
    arr.iter().take(max).filter_map(|entry| {
        let a = entry.as_array()?;
        let price: f64 = a.first()?.as_str()?.parse().ok()?;
        let size: f64 = a.get(1)?.as_str()?.parse().ok()?;
        Some(PriceLevel { price, size })
    }).collect()
}

pub fn discover_markets(data_dir: &Path) -> Vec<i64> {
    let mut markets = Vec::new();
    let Ok(entries) = std::fs::read_dir(data_dir) else { return markets };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if let Ok(epoch) = name_str.parse::<i64>() {
            let path = entry.path();
            if path.join("depth-trade.jsonl").exists() && path.join("clob.jsonl").exists() {
                markets.push(epoch);
            }
        }
    }
    markets.sort();
    markets
}
