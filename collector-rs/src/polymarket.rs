use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hasher;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::{DashMap, DashSet};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use tokio::sync::watch;

use bookview_collector::config::*;
use bookview_collector::error::CollectorError;
use bookview_collector::types::*;
use bookview_collector::writer::{SeqCounter, WriterHandle};

// ── Local order book ──────────────────────────────────────────────────────

pub struct LocalBook {
    bids: BTreeMap<i64, Level>,
    asks: BTreeMap<i64, Level>,
}

struct Level {
    price: String,
    size: String,
}

fn price_size(entry: &Value) -> Option<(&str, &str)> {
    if let Some(obj) = entry.as_object() {
        return Some((
            obj.get("price").and_then(|v| v.as_str())?,
            obj.get("size").and_then(|v| v.as_str())?,
        ));
    }

    let arr = entry.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    Some((arr[0].as_str()?, arr[1].as_str()?))
}

impl LocalBook {
    pub fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
        }
    }

    pub fn replace(&mut self, bids: &Value, asks: &Value) {
        self.bids.clear();
        if let Some(arr) = bids.as_array() {
            for entry in arr {
                if let Some((price, size)) = price_size(entry) {
                    self.bids.insert(
                        parse_decimal_key(price, 1_000_000),
                        Level {
                            price: price.to_string(),
                            size: size.to_string(),
                        },
                    );
                }
            }
        }

        self.asks.clear();
        if let Some(arr) = asks.as_array() {
            for entry in arr {
                if let Some((price, size)) = price_size(entry) {
                    self.asks.insert(
                        parse_decimal_key(price, 1_000_000),
                        Level {
                            price: price.to_string(),
                            size: size.to_string(),
                        },
                    );
                }
            }
        }
    }

    pub fn apply_delta(&mut self, side: &str, price: &str, size: &str) {
        let book = if side == "BUY" {
            &mut self.bids
        } else {
            &mut self.asks
        };
        let key = parse_decimal_key(price, 1_000_000);
        if is_zero_decimal(size) {
            book.remove(&key);
        } else {
            book.insert(
                key,
                Level {
                    price: price.to_string(),
                    size: size.to_string(),
                },
            );
        }
    }

    pub fn best_bid_f64(&self) -> Option<f64> {
        self.bids.iter().next_back().and_then(|(_, l)| l.price.parse().ok())
    }

    pub fn best_ask_f64(&self) -> Option<f64> {
        self.asks.iter().next().and_then(|(_, l)| l.price.parse().ok())
    }

    pub fn snapshot_full(
        &self,
        asset_id: &str,
        last_trade_price: Option<&str>,
        trigger: &str,
    ) -> RawJson {
        RawJson::from_serialize(&PolyBookState {
            bid_count: self.bids.len(),
            ask_count: self.asks.len(),
            bids: self
                .bids
                .iter()
                .rev()
                .map(|(_, l)| [l.price.as_str(), l.size.as_str()])
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|(_, l)| [l.price.as_str(), l.size.as_str()])
                .collect(),
            best_bid: self
                .bids
                .iter()
                .next_back()
                .map(|(_, l)| l.price.as_str()),
            best_ask: self.asks.iter().next().map(|(_, l)| l.price.as_str()),
            asset_id,
            last_trade_price,
            trigger_event_type: trigger,
        })
    }
}

// ── Dedup (allocation-free) ──────────────────────────────────────────────

fn dedup_hash(msg: &Value) -> u64 {
    let mut hasher = ahash::AHasher::default();
    hash_json_value(&mut hasher, msg);
    hasher.finish()
}

fn hash_json_value(h: &mut ahash::AHasher, val: &Value) {
    match val {
        Value::Null => h.write_u8(0),
        Value::Bool(b) => {
            h.write_u8(1);
            h.write_u8(*b as u8);
        }
        Value::Number(n) => {
            h.write_u8(2);
            if let Some(i) = n.as_i64() {
                h.write_i64(i);
            } else if let Some(u) = n.as_u64() {
                h.write_u64(u);
            } else if let Some(f) = n.as_f64() {
                h.write_u64(f.to_bits());
            }
        }
        Value::String(s) => {
            h.write_u8(3);
            h.write(s.as_bytes());
        }
        Value::Array(arr) => {
            h.write_u8(4);
            for v in arr {
                hash_json_value(h, v);
            }
        }
        Value::Object(map) => {
            h.write_u8(5);
            for (k, v) in map {
                h.write(k.as_bytes());
                hash_json_value(h, v);
            }
        }
    }
}

// ── Gamma API ─────────────────────────────────────────────────────────────

async fn fetch_gamma_market(
    client: &reqwest::Client,
    slug: &str,
    market_start: i64,
    writer: &WriterHandle,
    seq: &SeqCounter,
) -> Option<Value> {
    let deadline = market_start + GAMMA_LOOKUP_DEADLINE as i64;
    let mut backoff = GAMMA_RETRY_BASE_SECS;
    let mut attempt: u32 = 0;

    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        if now >= deadline {
            break;
        }

        attempt += 1;
        let url = gamma_market_url(slug);

        match client.get(&url).send().await {
            Ok(resp) => {
                if resp.status().as_u16() == 200 {
                    if let Ok(data) = resp.json::<Value>().await {
                        return Some(data);
                    }
                } else {
                    let status = resp.status().as_u16();
                    let body = resp.text().await.unwrap_or_default();
                    let _ = writer.send(make_line(
                        market_start,
                        "polymarket",
                        "market_lookup_retry",
                        seq.next(),
                        json!({"status": status, "body": body, "attempt": attempt}),
                        json!({"slug": slug, "attempt": attempt, "status": status}),
                    ));
                }
            }
            Err(e) => {
                let _ = writer.send(make_line(
                    market_start,
                    "polymarket",
                    "market_lookup_retry",
                    seq.next(),
                    json!({"error": e.to_string(), "attempt": attempt}),
                    json!({"slug": slug, "attempt": attempt, "error": e.to_string()}),
                ));
            }
        }

        tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
        backoff = (backoff * 2.0).min(GAMMA_RETRY_MAX_SECS);
    }

    None
}

fn parse_string_or_vec(val: &Value) -> Option<Vec<String>> {
    match val {
        Value::Array(arr) => Some(
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        ),
        Value::String(s) => serde_json::from_str::<Vec<String>>(s).ok(),
        _ => None,
    }
}

fn validate_market(data: &Value, slug: &str) -> Option<String> {
    if data.is_null() || (data.is_object() && data.as_object().unwrap().is_empty()) {
        return Some("empty response".to_string());
    }
    if data["slug"].as_str() != Some(slug) {
        return Some(format!(
            "slug mismatch: expected {}, got {:?}",
            slug,
            data["slug"].as_str()
        ));
    }
    if data["closed"].as_bool() == Some(true) {
        return Some("market is closed".to_string());
    }
    if !data["enableOrderBook"].as_bool().unwrap_or(false) {
        return Some("order book not enabled".to_string());
    }

    let outcomes = parse_string_or_vec(&data["outcomes"]);
    match &outcomes {
        Some(o) if o.len() != 2 => {
            return Some(format!("expected 2 outcomes, got {}", o.len()));
        }
        None => {
            return Some(format!("cannot parse outcomes: {}", data["outcomes"]));
        }
        _ => {}
    }

    let clob_ids = parse_string_or_vec(&data["clobTokenIds"]);
    match &clob_ids {
        Some(ids) if ids.len() != 2 => {
            return Some(format!("expected 2 clobTokenIds, got {}", ids.len()));
        }
        None => {
            return Some(format!(
                "cannot parse clobTokenIds: {}",
                data["clobTokenIds"]
            ));
        }
        _ => {}
    }

    None
}

fn parse_market_meta(data: &Value) -> Value {
    let outcomes = parse_string_or_vec(&data["outcomes"]).unwrap_or_default();
    let clob_ids = parse_string_or_vec(&data["clobTokenIds"]).unwrap_or_default();

    json!({
        "market_id": data["id"],
        "condition_id": data["conditionId"],
        "slug": data["slug"],
        "question": data["question"],
        "outcomes": outcomes,
        "clob_token_ids": clob_ids,
        "end_date": data["endDate"],
        "game_start_time": data.get("gameStartTime").cloned().unwrap_or(Value::Null),
    })
}

// ── Main collector ────────────────────────────────────────────────────────

pub async fn run_polymarket(
    market_start: i64,
    writer: WriterHandle,
    cancel: CancellationToken,
    client: reqwest::Client,
    market_info_tx: Option<watch::Sender<Option<MarketInfo>>>,
    poly_bba_tx: Option<Arc<watch::Sender<Option<PolyBbaSnapshot>>>>,
) -> Result<(), CollectorError> {
    let seq = Arc::new(SeqCounter::new());
    let slug = format!("btc-updown-5m-{market_start}");

    // Gamma lookup
    let raw_market = fetch_gamma_market(&client, &slug, market_start, &writer, &seq).await;

    let raw_market = match raw_market {
        Some(data) => data,
        None => {
            let _ = writer.send(make_line(
                market_start,
                "polymarket",
                "market_lookup_failed",
                seq.next(),
                json!({"slug": &slug}),
                json!({"slug": &slug, "reason": "deadline exceeded"}),
            ));
            return Ok(());
        }
    };

    if let Some(err) = validate_market(&raw_market, &slug) {
        let _ = writer.send(make_line(
            market_start,
            "polymarket",
            "market_lookup_failed",
            seq.next(),
            raw_market,
            json!({"slug": &slug, "reason": err}),
        ));
        return Ok(());
    }

    let meta = parse_market_meta(&raw_market);
    let token_ids: Vec<String> = meta["clob_token_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    if let Some(ref tx) = market_info_tx {
        let _ = tx.send(Some(MarketInfo {
            condition_id: meta["condition_id"].as_str().unwrap_or("").to_string(),
            clob_token_ids: token_ids.clone(),
            slug: slug.clone(),
            market_start,
        }));
    }

    let _ = writer.send(make_line(
        market_start,
        "polymarket",
        "market_lookup",
        seq.next(),
        raw_market,
        meta,
    ));

    // Shared state
    let books: Arc<std::sync::Mutex<HashMap<String, LocalBook>>> = {
        let mut m = HashMap::new();
        for tid in &token_ids {
            m.insert(tid.clone(), LocalBook::new());
        }
        Arc::new(std::sync::Mutex::new(m))
    };
    let last_trade: Arc<DashMap<String, String, ahash::RandomState>> =
        Arc::new(DashMap::with_hasher(ahash::RandomState::new()));
    let seen: Arc<DashSet<u64, ahash::RandomState>> =
        Arc::new(DashSet::with_hasher(ahash::RandomState::new()));

    // Launch 10 WS connections
    let mut tasks = JoinSet::new();
    for conn_id in 0..POLY_WS_CONNECTIONS {
        let writer = writer.clone();
        let seq = seq.clone();
        let cancel = cancel.clone();
        let books = books.clone();
        let last_trade = last_trade.clone();
        let seen = seen.clone();
        let token_ids = token_ids.clone();
        let bba_tx = poly_bba_tx.clone();

        tasks.spawn(async move {
            ws_connection_loop(
                conn_id,
                market_start,
                &writer,
                &seq,
                &cancel,
                &books,
                &last_trade,
                &seen,
                &token_ids,
                bba_tx.as_deref(),
            )
            .await
        });
    }

    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result {
            tracing::warn!(error = %e, "Polymarket WS task panicked");
        }
    }

    Ok(())
}

async fn ws_connection_loop(
    conn_id: usize,
    market_start: i64,
    writer: &WriterHandle,
    seq: &SeqCounter,
    cancel: &CancellationToken,
    books: &Arc<std::sync::Mutex<HashMap<String, LocalBook>>>,
    last_trade: &Arc<DashMap<String, String, ahash::RandomState>>,
    seen: &Arc<DashSet<u64, ahash::RandomState>>,
    token_ids: &[String],
    poly_bba_tx: Option<&watch::Sender<Option<PolyBbaSnapshot>>>,
) {
    let mut reconnect_backoff = WS_RECONNECT_BASE_SECS;
    let mut msgs_received: u64 = 0;
    let mut msgs_deduped: u64 = 0;

    while !cancel.is_cancelled() {
        let _ = writer.send(make_system_line(
            market_start,
            "polymarket",
            seq.next(),
            json!({"url": POLYMARKET_WS_URL, "conn_id": conn_id}),
            "ws_connecting",
        ));

        match connect_and_stream(
            conn_id,
            market_start,
            writer,
            seq,
            cancel,
            books,
            last_trade,
            seen,
            token_ids,
            &mut msgs_received,
            &mut msgs_deduped,
            &mut reconnect_backoff,
            poly_bba_tx,
        )
        .await
        {
            Ok(true) => break, // cancelled / clean stop
            Ok(false) => {
                // disconnected, will reconnect at top of loop
            }
            Err(e) => {
                let _ = writer.send(make_line(
                    market_start,
                    "polymarket",
                    "error",
                    seq.next(),
                    json!({"error": e.to_string(), "type": "WsConnectionError", "conn_id": conn_id}),
                    json!({"reason": "ws_connection_error", "conn_id": conn_id}),
                ));

                if cancel.is_cancelled() {
                    break;
                }

                let _ = writer.send(make_system_line(
                    market_start,
                    "polymarket",
                    seq.next(),
                    json!({"backoff": reconnect_backoff, "conn_id": conn_id}),
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
}

#[allow(clippy::too_many_arguments)]
async fn connect_and_stream(
    conn_id: usize,
    market_start: i64,
    writer: &WriterHandle,
    seq: &SeqCounter,
    cancel: &CancellationToken,
    books: &Arc<std::sync::Mutex<HashMap<String, LocalBook>>>,
    last_trade: &Arc<DashMap<String, String, ahash::RandomState>>,
    seen: &Arc<DashSet<u64, ahash::RandomState>>,
    token_ids: &[String],
    msgs_received: &mut u64,
    msgs_deduped: &mut u64,
    reconnect_backoff: &mut f64,
    poly_bba_tx: Option<&watch::Sender<Option<PolyBbaSnapshot>>>,
) -> Result<bool, CollectorError> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(POLYMARKET_WS_URL).await?;
    if let tokio_tungstenite::MaybeTlsStream::Rustls(tls) = ws_stream.get_ref() {
        let _ = tls.get_ref().0.set_nodelay(true);
    }
    let (mut write, mut read) = ws_stream.split();

    let _ = writer.send(make_system_line(
        market_start,
        "polymarket",
        seq.next(),
        json!({"conn_id": conn_id}),
        "ws_connected",
    ));
    *reconnect_backoff = WS_RECONNECT_BASE_SECS;

    // Subscribe
    let sub_payload = json!({
        "assets_ids": token_ids,
        "type": "market",
        "custom_feature_enabled": true,
    });
    write
        .send(Message::Text(sub_payload.to_string().into()))
        .await?;

    let _ = writer.send(make_system_line(
        market_start,
        "polymarket",
        seq.next(),
        json!({"token_ids": token_ids, "conn_id": conn_id}),
        "ws_subscribed",
    ));

    // Message loop
    loop {
        let msg = tokio::select! {
            msg = tokio::time::timeout(Duration::from_secs(5), read.next()) => msg,
            _ = cancel.cancelled() => {
                return Ok(true);
            }
        };

        match msg {
            Err(_) => continue,        // timeout
            Ok(None) => break,         // stream closed
            Ok(Some(Err(_))) => break, // WS error
            Ok(Some(Ok(m))) => {
                let text = match m.into_text() {
                    Ok(t) => t.to_string(),
                    Err(_) => continue,
                };

                let parsed: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = writer.send(make_line(
                            market_start,
                            "polymarket",
                            "error",
                            seq.next(),
                            json!({"raw": text, "conn_id": conn_id}),
                            json!({"reason": "json_decode_error", "conn_id": conn_id}),
                        ));
                        continue;
                    }
                };

                let msgs = if parsed.is_array() {
                    parsed.as_array().unwrap().clone()
                } else {
                    vec![parsed]
                };

                for msg_val in &msgs {
                    *msgs_received += 1;
                    let key = dedup_hash(msg_val);
                    if !seen.insert(key) {
                        *msgs_deduped += 1;
                        continue;
                    }
                    process_poly_message(
                        msg_val,
                        market_start,
                        writer,
                        seq,
                        books,
                        last_trade,
                        token_ids,
                        poly_bba_tx,
                    );
                }
            }
        }
    }

    // Disconnected
    let _ = writer.send(make_system_line(
        market_start,
        "polymarket",
        seq.next(),
        json!({
            "conn_id": conn_id,
            "msgs_received": msgs_received,
            "msgs_deduped": msgs_deduped,
        }),
        "ws_disconnected",
    ));

    if cancel.is_cancelled() {
        return Ok(true);
    }

    let _ = writer.send(make_system_line(
        market_start,
        "polymarket",
        seq.next(),
        json!({"backoff": reconnect_backoff, "conn_id": conn_id}),
        "ws_reconnecting",
    ));

    Ok(false)
}

// ── BBA publishing ───────────────────────────────────────────────────────

fn publish_poly_bba(
    books: &HashMap<String, LocalBook>,
    token_ids: &[String],
    tx: &watch::Sender<Option<PolyBbaSnapshot>>,
) {
    let yes_book = token_ids.first().and_then(|id| books.get(id));
    let no_book = token_ids.get(1).and_then(|id| books.get(id));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let _ = tx.send(Some(PolyBbaSnapshot {
        yes_best_bid: yes_book.and_then(|b| b.best_bid_f64()),
        yes_best_ask: yes_book.and_then(|b| b.best_ask_f64()),
        no_best_bid: no_book.and_then(|b| b.best_bid_f64()),
        no_best_ask: no_book.and_then(|b| b.best_ask_f64()),
        ts_recv_ms: now,
    }));
}

// ── Message processing ────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn process_poly_message(
    msg: &Value,
    market_start: i64,
    writer: &WriterHandle,
    seq: &SeqCounter,
    books: &Arc<std::sync::Mutex<HashMap<String, LocalBook>>>,
    last_trade: &Arc<DashMap<String, String, ahash::RandomState>>,
    token_ids: &[String],
    poly_bba_tx: Option<&watch::Sender<Option<PolyBbaSnapshot>>>,
) {
    let event_type = msg["event_type"].as_str().unwrap_or("unknown");
    let asset_id = msg["asset_id"].as_str().unwrap_or("");

    match event_type {
        "book" => {
            let _ = writer.send(make_line(
                market_start,
                "polymarket",
                "book_raw",
                seq.next(),
                RawJson::from_serialize(msg),
                json!({"asset_id": asset_id, "event_type": event_type}),
            ));

            let ltp = msg.get("last_trade_price").and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    Some(v.to_string().trim_matches('"').to_string())
                }
            });
            if let Some(ref price) = ltp {
                if !asset_id.is_empty() {
                    last_trade.insert(asset_id.to_string(), price.clone());
                }
            }

            let mut books_guard = books.lock().expect("polymarket books mutex poisoned");
            if let Some(book) = books_guard.get_mut(asset_id) {
                let bids = &msg["bids"];
                let asks = &msg["asks"];
                book.replace(bids, asks);
                let ltp_ref = last_trade.get(asset_id);
                let snap =
                    book.snapshot_full(asset_id, ltp_ref.as_deref().map(|s| s.as_str()), "book");
                let _ = writer.send(make_line(
                    market_start,
                    "polymarket",
                    "book_state",
                    seq.next(),
                    RawJson::Null,
                    snap,
                ));
            }
            if let Some(tx) = poly_bba_tx {
                publish_poly_bba(&books_guard, token_ids, tx);
            }
        }

        "price_change" => {
            let _ = writer.send(make_line(
                market_start,
                "polymarket",
                "price_change_raw",
                seq.next(),
                RawJson::from_serialize(msg),
                json!({"asset_id": asset_id, "event_type": event_type}),
            ));

            let changes = msg
                .get("price_changes")
                .or_else(|| msg.get("changes"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let mut affected: HashSet<String> = HashSet::new();
            {
                let mut books_guard = books.lock().expect("polymarket books mutex poisoned");
                for change in &changes {
                    let ch_asset = change["asset_id"].as_str().unwrap_or(asset_id).to_string();
                    let side = change["side"].as_str().unwrap_or("");
                    let price = change["price"]
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| change["price"].to_string());
                    let size = change["size"]
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| change["size"].to_string());

                    if let Some(book) = books_guard.get_mut(&ch_asset) {
                        book.apply_delta(side, &price, &size);
                        affected.insert(ch_asset);
                    }
                }

                for aid in &affected {
                    let _ = writer.send(make_line(
                        market_start,
                        "polymarket",
                        "price_change_applied",
                        seq.next(),
                        Value::Null,
                        json!({"asset_id": aid, "changes_count": changes.len()}),
                    ));

                    if let Some(book) = books_guard.get(aid) {
                        let ltp_ref = last_trade.get(aid.as_str());
                        let snap = book.snapshot_full(
                            aid,
                            ltp_ref.as_deref().map(|s| s.as_str()),
                            "price_change",
                        );
                        let _ = writer.send(make_line(
                            market_start,
                            "polymarket",
                            "book_state",
                            seq.next(),
                            RawJson::Null,
                            snap,
                        ));
                    }
                }
                if let Some(tx) = poly_bba_tx {
                    publish_poly_bba(&books_guard, token_ids, tx);
                }
            }
        }

        "last_trade_price" => {
            let price = msg["price"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| msg["price"].to_string());
            last_trade.insert(asset_id.to_string(), price.clone());
            let _ = writer.send(make_line(
                market_start,
                "polymarket",
                "last_trade_price_raw",
                seq.next(),
                RawJson::from_serialize(msg),
                json!({"asset_id": asset_id, "price": price}),
            ));
        }

        "best_bid_ask" => {
            let _ = writer.send(make_line(
                market_start,
                "polymarket",
                "best_bid_ask_raw",
                seq.next(),
                RawJson::from_serialize(msg),
                json!({"asset_id": asset_id}),
            ));
        }

        "tick_size_change" => {
            let _ = writer.send(make_line(
                market_start,
                "polymarket",
                "tick_size_change_raw",
                seq.next(),
                RawJson::from_serialize(msg),
                json!({"asset_id": asset_id}),
            ));
        }

        "market_resolved" => {
            let _ = writer.send(make_line(
                market_start,
                "polymarket",
                "market_resolved_raw",
                seq.next(),
                RawJson::from_serialize(msg),
                json!({"asset_id": asset_id}),
            ));
        }

        _ => {
            let _ = writer.send(make_line(
                market_start,
                "polymarket",
                format!("{event_type}_raw"),
                seq.next(),
                RawJson::from_serialize(msg),
                json!({"asset_id": asset_id, "event_type": event_type}),
            ));
        }
    }
}
