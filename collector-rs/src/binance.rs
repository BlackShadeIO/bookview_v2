use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use bookview_collector::config::*;
use bookview_collector::error::CollectorError;
use bookview_collector::fair_value::FairValueFeeds;
use bookview_collector::types::*;
use bookview_collector::writer::{SeqCounter, WriterHandle};

// ── Wire format structs ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct CombinedStreamMsg<'a> {
    stream: &'a str,
    #[serde(borrow)]
    data: &'a RawValue,
}

#[derive(Deserialize, serde::Serialize)]
struct DepthUpdateMsg {
    #[serde(rename = "U")]
    first_update_id: i64,
    #[serde(rename = "u")]
    last_update_id: i64,
    b: Vec<[String; 2]>,
    a: Vec<[String; 2]>,
}

#[derive(Deserialize)]
struct BookTickerMsg {
    b: String,
    #[serde(rename = "B")]
    bid_qty: String,
    a: String,
    #[serde(rename = "A")]
    ask_qty: String,
    u: i64,
}

#[derive(Deserialize)]
struct TradeMsg {
    p: String,
    q: String,
    t: i64,
    #[serde(rename = "T")]
    trade_time: i64,
    #[serde(rename = "E")]
    event_time: i64,
    m: bool,
}

enum Msg {
    Trade(TradeMsg),
    Depth(DepthUpdateMsg),
    BookTicker(BookTickerMsg),
}

fn parse_msg(raw_msg: &str) -> Option<Msg> {
    let w: CombinedStreamMsg = serde_json::from_str(raw_msg).ok()?;
    let data = w.data.get();
    if w.stream.ends_with("@trade") {
        Some(Msg::Trade(serde_json::from_str(data).ok()?))
    } else if w.stream.contains("depth") && !w.stream.contains("bookTicker") {
        Some(Msg::Depth(serde_json::from_str(data).ok()?))
    } else if w.stream.contains("bookTicker") {
        Some(Msg::BookTicker(serde_json::from_str(data).ok()?))
    } else {
        None
    }
}

// ── Local order book ──────────────────────────────────────────────────────

pub struct BinanceBook {
    pub bids: BTreeMap<i64, BookLevel>,
    pub asks: BTreeMap<i64, BookLevel>,
    pub update_id: i64,
}

pub struct BookLevel {
    pub price: String,
    pub qty: String,
}

impl BinanceBook {
    pub fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            update_id: 0,
        }
    }

    pub fn load_snapshot(&mut self, snap: &Value) {
        self.bids.clear();
        self.asks.clear();

        if let Some(bids) = snap["bids"].as_array() {
            for entry in bids {
                if let (Some(p), Some(q)) = (entry[0].as_str(), entry[1].as_str()) {
                    self.bids.insert(
                        parse_decimal_key(p, 100_000_000),
                        BookLevel {
                            price: p.to_string(),
                            qty: q.to_string(),
                        },
                    );
                }
            }
        }

        if let Some(asks) = snap["asks"].as_array() {
            for entry in asks {
                if let (Some(p), Some(q)) = (entry[0].as_str(), entry[1].as_str()) {
                    self.asks.insert(
                        parse_decimal_key(p, 100_000_000),
                        BookLevel {
                            price: p.to_string(),
                            qty: q.to_string(),
                        },
                    );
                }
            }
        }

        if let Some(uid) = snap["lastUpdateId"].as_i64() {
            self.update_id = uid;
        }
    }

    fn apply_diff(&mut self, update: &DepthUpdateMsg) {
        for [p, q] in &update.b {
            let price_key = parse_decimal_key(p, 100_000_000);
            if is_zero_decimal(q) {
                self.bids.remove(&price_key);
            } else {
                self.bids.insert(
                    price_key,
                    BookLevel {
                        price: p.clone(),
                        qty: q.clone(),
                    },
                );
            }
        }

        for [p, q] in &update.a {
            let price_key = parse_decimal_key(p, 100_000_000);
            if is_zero_decimal(q) {
                self.asks.remove(&price_key);
            } else {
                self.asks.insert(
                    price_key,
                    BookLevel {
                        price: p.clone(),
                        qty: q.clone(),
                    },
                );
            }
        }

        self.update_id = update.last_update_id;
    }

    pub fn best_bid_price(&self) -> Option<&str> {
        self.bids
            .iter()
            .next_back()
            .map(|(_, level)| level.price.as_str())
    }

    pub fn best_ask_price(&self) -> Option<&str> {
        self.asks
            .iter()
            .next()
            .map(|(_, level)| level.price.as_str())
    }

    pub fn top_levels(&self, n: usize) -> (Vec<(f64, f64)>, Vec<(f64, f64)>) {
        let bids: Vec<(f64, f64)> = self
            .bids
            .iter()
            .rev()
            .take(n)
            .map(|(_, l)| {
                (
                    l.price.parse().unwrap_or(0.0),
                    l.qty.parse().unwrap_or(0.0),
                )
            })
            .collect();
        let asks: Vec<(f64, f64)> = self
            .asks
            .iter()
            .take(n)
            .map(|(_, l)| {
                (
                    l.price.parse().unwrap_or(0.0),
                    l.qty.parse().unwrap_or(0.0),
                )
            })
            .collect();
        (bids, asks)
    }

    pub fn snapshot(&self, trigger_u_upper: i64, trigger_u_lower: i64) -> RawJson {
        let sorted_bids: Vec<[&str; 2]> = self
            .bids
            .iter()
            .rev()
            .map(|(_, level)| [level.price.as_str(), level.qty.as_str()])
            .collect();

        let sorted_asks: Vec<[&str; 2]> = self
            .asks
            .iter()
            .map(|(_, level)| [level.price.as_str(), level.qty.as_str()])
            .collect();

        let (best_bid_price, best_bid_qty) = self
            .bids
            .iter()
            .next_back()
            .map(|(_, level)| (Some(level.price.as_str()), Some(level.qty.as_str())))
            .unwrap_or((None, None));

        let (best_ask_price, best_ask_qty) = self
            .asks
            .iter()
            .next()
            .map(|(_, level)| (Some(level.price.as_str()), Some(level.qty.as_str())))
            .unwrap_or((None, None));

        let spread = match (best_ask_price, best_bid_price) {
            (Some(ask), Some(bid)) => {
                if let (Ok(a), Ok(b)) = (ask.parse::<Decimal>(), bid.parse::<Decimal>()) {
                    Some((a - b).to_string())
                } else {
                    None
                }
            }
            _ => None,
        };

        RawJson::from_serialize(&BinanceBookState {
            update_id: self.update_id,
            best_bid_price,
            best_bid_qty,
            best_ask_price,
            best_ask_qty,
            spread,
            bid_count: sorted_bids.len(),
            ask_count: sorted_asks.len(),
            bids: sorted_bids,
            asks: sorted_asks,
            trigger_event: TriggerEvent {
                first_update_id: trigger_u_upper,
                last_update_id: trigger_u_lower,
            },
        })
    }
}

// ── REST snapshot ──────────────────────────────────────────────────────────

async fn fetch_snapshot(
    client: &reqwest::Client,
    market_start: i64,
    writer: &WriterHandle,
    seq: &SeqCounter,
    reason: &str,
    attempt: u32,
) -> Option<Value> {
    match client.get(BINANCE_DEPTH_SNAPSHOT_URL).send().await {
        Ok(resp) => {
            if resp.status() != 200 {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                let _ = writer.send(make_line(
                    market_start,
                    "binance",
                    "error",
                    seq.next(),
                    json!({"status": status, "body": body}),
                    json!({"reason": "snapshot_fetch_failed", "attempt": attempt}),
                ));
                return None;
            }
            match resp.json::<Value>().await {
                Ok(data) => {
                    let bid_levels = data["bids"].as_array().map_or(0, |a| a.len());
                    let ask_levels = data["asks"].as_array().map_or(0, |a| a.len());
                    let _ = writer.send(make_line(
                        market_start,
                        "binance",
                        "snapshot_raw",
                        seq.next(),
                        RawJson::from_serialize(&data),
                        json!({
                            "lastUpdateId": data["lastUpdateId"],
                            "bid_levels": bid_levels,
                            "ask_levels": ask_levels,
                            "reason": reason,
                            "attempt": attempt,
                        }),
                    ));
                    Some(data)
                }
                Err(e) => {
                    let _ = writer.send(make_line(
                        market_start,
                        "binance",
                        "error",
                        seq.next(),
                        json!({"error": e.to_string(), "type": "JsonParseError"}),
                        json!({"reason": "snapshot_fetch_error", "attempt": attempt}),
                    ));
                    None
                }
            }
        }
        Err(e) => {
            let _ = writer.send(make_line(
                market_start,
                "binance",
                "error",
                seq.next(),
                json!({"error": e.to_string(), "type": "RequestError"}),
                json!({"reason": "snapshot_fetch_error", "attempt": attempt}),
            ));
            None
        }
    }
}

// ── Main collector ────────────────────────────────────────────────────────

pub async fn run_binance(
    market_start: i64,
    writer: WriterHandle,
    cancel: CancellationToken,
    client: reqwest::Client,
    fv_feeds: Option<FairValueFeeds>,
) -> Result<(), CollectorError> {
    let seq = Arc::new(SeqCounter::new());
    let mut reconnect_backoff = WS_RECONNECT_BASE_SECS;
    let mut strike_recorded = false;

    while !cancel.is_cancelled() {
        let _ = writer.send(make_system_line(
            market_start,
            "binance",
            seq.next(),
            json!({"url": BINANCE_WS_URL}),
            "ws_connecting",
        ));

        match connect_ws(
            &client,
            market_start,
            &writer,
            &seq,
            &cancel,
            &mut strike_recorded,
            &fv_feeds,
        )
        .await
        {
            Ok(()) => break,
            Err(e) => {
                let _ = writer.send(make_line(
                    market_start,
                    "binance",
                    "error",
                    seq.next(),
                    json!({"error": e.to_string(), "type": "WsConnectionError"}),
                    json!({"reason": "ws_connection_error"}),
                ));

                if cancel.is_cancelled() {
                    break;
                }

                let _ = writer.send(make_system_line(
                    market_start,
                    "binance",
                    seq.next(),
                    json!({"backoff": reconnect_backoff}),
                    "ws_reconnecting",
                ));

                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs_f64(reconnect_backoff)) => {}
                    _ = cancel.cancelled() => break,
                }
                reconnect_backoff = (reconnect_backoff * 2.0).min(WS_RECONNECT_MAX_SECS);
            }
        }
    }

    Ok(())
}

async fn connect_ws(
    client: &reqwest::Client,
    market_start: i64,
    writer: &WriterHandle,
    seq: &SeqCounter,
    cancel: &CancellationToken,
    strike_recorded: &mut bool,
    fv_feeds: &Option<FairValueFeeds>,
) -> Result<(), CollectorError> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(BINANCE_WS_URL).await?;
    if let tokio_tungstenite::MaybeTlsStream::Rustls(tls) = ws_stream.get_ref() {
        let _ = tls.get_ref().0.set_nodelay(true);
    }

    let _ = writer.send(make_system_line(
        market_start,
        "binance",
        seq.next(),
        Value::Null,
        "ws_connected",
    ));

    let (_, mut read) = ws_stream.split();

    run_sync_loop(
        client,
        &mut read,
        market_start,
        writer,
        seq,
        cancel,
        strike_recorded,
        fv_feeds,
    )
    .await?;

    if !cancel.is_cancelled() {
        let _ = writer.send(make_system_line(
            market_start,
            "binance",
            seq.next(),
            Value::Null,
            "ws_disconnected",
        ));
    }

    Ok(())
}

type WsRead = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

async fn run_sync_loop(
    client: &reqwest::Client,
    read: &mut WsRead,
    market_start: i64,
    writer: &WriterHandle,
    seq: &SeqCounter,
    cancel: &CancellationToken,
    strike_recorded: &mut bool,
    fv_feeds: &Option<FairValueFeeds>,
) -> Result<(), CollectorError> {
    let mut sync_cycle: u32 = 0;

    while !cancel.is_cancelled() {
        sync_cycle += 1;
        let mut book = BinanceBook::new();
        let mut buffer: Vec<DepthUpdateMsg> = Vec::new();
        let mut synced = false;
        let mut snapshot_attempt: u32 = 0;

        // ── Phase 1: Buffer + snapshot sync ───────────────────────────
        while !cancel.is_cancelled() && !synced {
            let raw_msg = match recv_ws_msg(read, cancel).await {
                Some(msg) => msg,
                None => return Ok(()),
            };

            let msg = match parse_msg(&raw_msg) {
                Some(m) => m,
                None => continue,
            };

            match msg {
                Msg::Trade(trade) => {
                    emit_trade(market_start, writer, seq, RawJson::from_string(raw_msg), &trade, fv_feeds);
                }
                Msg::Depth(update) => {
                    let _ = writer.send(make_line(
                        market_start,
                        "binance",
                        "depth_raw_buffered",
                        seq.next(),
                        RawJson::from_string(raw_msg),
                        json!({"U": update.first_update_id, "u": update.last_update_id, "buffer_size": buffer.len() + 1}),
                    ));
                    buffer.push(update);

                    if buffer.len() == 1 || snapshot_attempt > 0 {
                        snapshot_attempt += 1;
                        let reason = if sync_cycle == 1 {
                            "startup"
                        } else {
                            "resync_after_gap"
                        };

                        let snap = match fetch_snapshot(
                            client,
                            market_start,
                            writer,
                            seq,
                            reason,
                            snapshot_attempt,
                        )
                        .await
                        {
                            Some(s) => s,
                            None => continue,
                        };

                        let last_uid = snap["lastUpdateId"].as_i64().unwrap_or(0);
                        let first_buf_u = buffer[0].first_update_id;

                        if last_uid < first_buf_u {
                            tracing::warn!(last_uid, first_buf_u, "Snapshot too old, refetching");
                            continue;
                        }

                        buffer.retain(|e| e.last_update_id > last_uid);

                        if buffer.is_empty() {
                            book.load_snapshot(&snap);
                            let _ = writer.send(make_line(
                                market_start,
                                "binance",
                                "snapshot_loaded",
                                seq.next(),
                                Value::Null,
                                json!({"lastUpdateId": last_uid, "buffered_remaining": 0}),
                            ));
                            let _ = writer.send(make_line(
                                market_start,
                                "binance",
                                "book_state",
                                seq.next(),
                                Value::Null,
                                book.snapshot(0, last_uid),
                            ));
                            let _ = writer.send(make_system_line(
                                market_start,
                                "binance",
                                seq.next(),
                                json!({"lastUpdateId": last_uid}),
                                "sync_ready",
                            ));
                            if sync_cycle > 1 {
                                let _ = writer.send(make_system_line(
                                    market_start,
                                    "binance",
                                    seq.next(),
                                    json!({"sync_cycle": sync_cycle, "lastUpdateId": last_uid}),
                                    "resync_completed",
                                ));
                            }
                            synced = true;
                            continue;
                        }

                        let first = &buffer[0];
                        let first_u = first.first_update_id;
                        let first_uu = first.last_update_id;

                        if !(first_u <= last_uid + 1 && last_uid + 1 <= first_uu) {
                            let _ = writer.send(make_line(
                                market_start,
                                "binance",
                                "error",
                                seq.next(),
                                json!({"first_U": first_u, "first_u": first_uu, "lastUpdateId": last_uid}),
                                json!({"reason": "first_event_does_not_span_snapshot"}),
                            ));
                            buffer.clear();
                            continue;
                        }

                        book.load_snapshot(&snap);
                        let _ = writer.send(make_line(
                            market_start,
                            "binance",
                            "snapshot_loaded",
                            seq.next(),
                            Value::Null,
                            json!({"lastUpdateId": last_uid, "buffered_remaining": buffer.len()}),
                        ));
                        let _ = writer.send(make_line(
                            market_start,
                            "binance",
                            "book_state",
                            seq.next(),
                            Value::Null,
                            book.snapshot(0, last_uid),
                        ));

                        for evt in &buffer {
                            book.apply_diff(evt);
                            let _ = writer.send(make_line(
                                market_start,
                                "binance",
                                "depth_applied",
                                seq.next(),
                                RawJson::from_serialize(evt),
                                json!({"U": evt.first_update_id, "u": evt.last_update_id, "local_update_id": book.update_id}),
                            ));
                            let _ = writer.send(make_line(
                                market_start,
                                "binance",
                                "book_state",
                                seq.next(),
                                Value::Null,
                                book.snapshot(evt.first_update_id, evt.last_update_id),
                            ));
                        }
                        send_fv_depth(fv_feeds, &book);

                        buffer.clear();
                        let _ = writer.send(make_system_line(
                            market_start,
                            "binance",
                            seq.next(),
                            json!({"lastUpdateId": book.update_id}),
                            "sync_ready",
                        ));
                        if sync_cycle > 1 {
                            let _ = writer.send(make_system_line(
                                market_start,
                                "binance",
                                seq.next(),
                                json!({"sync_cycle": sync_cycle, "lastUpdateId": book.update_id}),
                                "resync_completed",
                            ));
                        }
                        synced = true;
                    }
                }
                Msg::BookTicker(ticker) => {
                    emit_book_ticker_phase1(market_start, writer, seq, RawJson::from_string(raw_msg), &ticker);
                    maybe_record_strike(market_start, writer, seq, &ticker, strike_recorded, fv_feeds);
                    send_fv_bba(fv_feeds, &ticker);
                }
            }
        }

        // ── Phase 2: Steady state ─────────────────────────────────────
        let mut gap_hit = false;
        while !cancel.is_cancelled() && synced && !gap_hit {
            let raw_msg = match recv_ws_msg(read, cancel).await {
                Some(msg) => msg,
                None => return Ok(()),
            };

            let msg = match parse_msg(&raw_msg) {
                Some(m) => m,
                None => continue,
            };

            match msg {
                Msg::Trade(trade) => {
                    emit_trade(market_start, writer, seq, RawJson::from_string(raw_msg), &trade, fv_feeds);
                }
                Msg::BookTicker(ticker) => {
                    let _ = writer.send(make_line(
                        market_start,
                        "binance",
                        "book_ticker_raw",
                        seq.next(),
                        RawJson::from_string(raw_msg),
                        RawJson::Null,
                    ));

                    let (local_best_bid, local_best_ask, consistent) = if book.update_id > 0 {
                        let lbb = book.best_bid_price().map(String::from);
                        let lba = book.best_ask_price().map(String::from);
                        let c = Some(
                            book.best_bid_price() == Some(ticker.b.as_str())
                                && book.best_ask_price() == Some(ticker.a.as_str()),
                        );
                        (lbb, lba, c)
                    } else {
                        (None, None, None)
                    };

                    let _ = writer.send(make_line(
                        market_start,
                        "binance",
                        "book_ticker_normalized",
                        seq.next(),
                        RawJson::Null,
                        RawJson::from_serialize(&BookTickerNormalized {
                            bid_price: ticker.b.clone(),
                            bid_qty: ticker.bid_qty.clone(),
                            ask_price: ticker.a.clone(),
                            ask_qty: ticker.ask_qty.clone(),
                            update_id: ticker.u,
                            local_best_bid,
                            local_best_ask,
                            consistent,
                        }),
                    ));

                    maybe_record_strike(market_start, writer, seq, &ticker, strike_recorded, fv_feeds);
                    send_fv_bba(fv_feeds, &ticker);
                }
                Msg::Depth(update) => {
                    let _ = writer.send(make_line(
                        market_start,
                        "binance",
                        "depth_raw",
                        seq.next(),
                        RawJson::from_string(raw_msg),
                        json!({"U": update.first_update_id, "u": update.last_update_id, "local_update_id": book.update_id}),
                    ));

                    if update.first_update_id > book.update_id + 1 {
                        let _ = writer.send(make_system_line(
                            market_start,
                            "binance",
                            seq.next(),
                            json!({
                                "last_good_update_id": book.update_id,
                                "incoming_U": update.first_update_id,
                                "incoming_u": update.last_update_id,
                            }),
                            "gap_detected",
                        ));
                        let _ = writer.send(make_system_line(
                            market_start,
                            "binance",
                            seq.next(),
                            json!({"reason": "gap_in_update_ids"}),
                            "resync_started",
                        ));
                        gap_hit = true;
                        continue;
                    }

                    if update.last_update_id <= book.update_id {
                        continue;
                    }

                    book.apply_diff(&update);
                    let _ = writer.send(make_line(
                        market_start,
                        "binance",
                        "depth_applied",
                        seq.next(),
                        Value::Null,
                        json!({"U": update.first_update_id, "u": update.last_update_id, "local_update_id": book.update_id}),
                    ));
                    let _ = writer.send(make_line(
                        market_start,
                        "binance",
                        "book_state",
                        seq.next(),
                        Value::Null,
                        book.snapshot(update.first_update_id, update.last_update_id),
                    ));
                    send_fv_depth(fv_feeds, &book);
                }
            }
        }

        if !gap_hit {
            return Ok(());
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────

async fn recv_ws_msg(read: &mut WsRead, cancel: &CancellationToken) -> Option<String> {
    loop {
        let msg = tokio::select! {
            msg = tokio::time::timeout(Duration::from_secs(5), read.next()) => msg,
            _ = cancel.cancelled() => return None,
        };

        match msg {
            Ok(Some(Ok(m))) => {
                if let Ok(text) = m.into_text() {
                    return Some(text.to_string());
                }
            }
            Ok(Some(Err(_))) => return None, // WS error → reconnect
            Ok(None) => return None,         // stream closed
            Err(_) => continue,              // timeout → retry
        }
    }
}

fn emit_trade(
    market_start: i64,
    writer: &WriterHandle,
    seq: &SeqCounter,
    raw: RawJson,
    trade: &TradeMsg,
    fv_feeds: &Option<FairValueFeeds>,
) {
    let _ = writer.send(make_line(
        market_start,
        "binance",
        "trade_raw",
        seq.next(),
        raw,
        RawJson::Null,
    ));

    let side = if trade.m { "SELL" } else { "BUY" };

    let _ = writer.send(make_line(
        market_start,
        "binance",
        "trade_normalized",
        seq.next(),
        RawJson::Null,
        RawJson::from_serialize(&TradeNormalized {
            price: trade.p.clone(),
            qty: trade.q.clone(),
            side,
            trade_id: trade.t,
            trade_time: trade.trade_time,
            event_time: trade.event_time,
        }),
    ));

    if let Some(feeds) = fv_feeds {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let _ = feeds.trade.send(Some(FvTradeSnapshot {
            qty: trade.q.parse().unwrap_or(0.0),
            is_buy: !trade.m,
            ts_recv_ms: now_ms,
        }));
    }
}

fn emit_book_ticker_phase1(
    market_start: i64,
    writer: &WriterHandle,
    seq: &SeqCounter,
    raw: RawJson,
    ticker: &BookTickerMsg,
) {
    let _ = writer.send(make_line(
        market_start,
        "binance",
        "book_ticker_raw",
        seq.next(),
        raw,
        RawJson::Null,
    ));
    let _ = writer.send(make_line(
        market_start,
        "binance",
        "book_ticker_normalized",
        seq.next(),
        RawJson::Null,
        RawJson::from_serialize(&BookTickerNormalized {
            bid_price: ticker.b.clone(),
            bid_qty: ticker.bid_qty.clone(),
            ask_price: ticker.a.clone(),
            ask_qty: ticker.ask_qty.clone(),
            update_id: ticker.u,
            local_best_bid: None,
            local_best_ask: None,
            consistent: None,
        }),
    ));
}

fn maybe_record_strike(
    market_start: i64,
    writer: &WriterHandle,
    seq: &SeqCounter,
    ticker: &BookTickerMsg,
    strike_recorded: &mut bool,
    fv_feeds: &Option<FairValueFeeds>,
) {
    if *strike_recorded {
        return;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    if now < market_start {
        return;
    }

    let bid: Decimal = ticker.b.parse().unwrap_or_default();
    let ask: Decimal = ticker.a.parse().unwrap_or_default();
    let mid = (bid + ask) / Decimal::from(2);

    let _ = writer.send(make_system_line(
        market_start,
        "binance",
        seq.next(),
        RawJson::from_serialize(&StrikePrice {
            strike_price: mid.to_string(),
            strike_bid: ticker.b.clone(),
            strike_ask: ticker.a.clone(),
            market_start,
        }),
        "strike_price",
    ));

    if let Some(feeds) = fv_feeds {
        let market_start_ms = market_start * 1000;
        let strike_f64: f64 = mid.to_string().parse().unwrap_or(0.0);
        let _ = feeds.strike.send(Some(FvStrikeInfo {
            strike_price: strike_f64,
            market_start_ms,
            market_end_ms: market_start_ms + (MARKET_DURATION as i64) * 1000,
        }));
    }

    *strike_recorded = true;
    tracing::info!(
        market_start,
        strike = %mid,
        bid = %ticker.b,
        ask = %ticker.a,
        "Strike price recorded"
    );
}

fn send_fv_bba(fv_feeds: &Option<FairValueFeeds>, ticker: &BookTickerMsg) {
    if let Some(feeds) = fv_feeds {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let _ = feeds.bba.send(Some(BbaSnapshot {
            bid_price: ticker.b.parse().unwrap_or(0.0),
            bid_qty: ticker.bid_qty.parse().unwrap_or(0.0),
            ask_price: ticker.a.parse().unwrap_or(0.0),
            ask_qty: ticker.ask_qty.parse().unwrap_or(0.0),
            ts_recv_ms: now_ms,
        }));
    }
}

fn send_fv_depth(fv_feeds: &Option<FairValueFeeds>, book: &BinanceBook) {
    if let Some(feeds) = fv_feeds {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let (bids, asks) = book.top_levels(20);
        let _ = feeds.depth.send(Some(FvDepthSnapshot {
            bids,
            asks,
            ts_recv_ms: now_ms,
        }));
    }
}
