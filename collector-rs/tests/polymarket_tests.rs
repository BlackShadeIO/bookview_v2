use serde_json::json;
use std::hash::{Hash, Hasher};

// ── LocalBook tests ───────────────────────────────────────────────────────

// Inline minimal LocalBook for testing (same logic as src/polymarket.rs)
use std::collections::HashMap;

struct LocalBook {
    bids: HashMap<String, String>,
    asks: HashMap<String, String>,
}

impl LocalBook {
    fn new() -> Self {
        Self {
            bids: HashMap::new(),
            asks: HashMap::new(),
        }
    }

    fn replace(&mut self, bids: &serde_json::Value, asks: &serde_json::Value) {
        self.bids.clear();
        if let Some(arr) = bids.as_array() {
            for entry in arr {
                if let Some(obj) = entry.as_object() {
                    let price = obj
                        .get("price")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let size = obj
                        .get("size")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    self.bids.insert(price, size);
                } else if let Some(arr) = entry.as_array() {
                    if arr.len() >= 2 {
                        let price = arr[0].as_str().unwrap_or("").to_string();
                        let size = arr[1].as_str().unwrap_or("").to_string();
                        self.bids.insert(price, size);
                    }
                }
            }
        }
        self.asks.clear();
        if let Some(arr) = asks.as_array() {
            for entry in arr {
                if let Some(obj) = entry.as_object() {
                    let price = obj
                        .get("price")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let size = obj
                        .get("size")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    self.asks.insert(price, size);
                } else if let Some(arr) = entry.as_array() {
                    if arr.len() >= 2 {
                        let price = arr[0].as_str().unwrap_or("").to_string();
                        let size = arr[1].as_str().unwrap_or("").to_string();
                        self.asks.insert(price, size);
                    }
                }
            }
        }
    }

    fn apply_delta(&mut self, side: &str, price: &str, size: &str) {
        let book = if side == "BUY" {
            &mut self.bids
        } else {
            &mut self.asks
        };
        if size == "0" {
            book.remove(price);
        } else {
            book.insert(price.to_string(), size.to_string());
        }
    }

    fn snapshot(&self) -> serde_json::Value {
        let mut sorted_bids: Vec<(&str, &str)> = self
            .bids
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        sorted_bids.sort_by(|a, b| {
            let fa: f64 = a.0.parse().unwrap_or(0.0);
            let fb: f64 = b.0.parse().unwrap_or(0.0);
            fb.partial_cmp(&fa).unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut sorted_asks: Vec<(&str, &str)> = self
            .asks
            .iter()
            .map(|(p, s)| (p.as_str(), s.as_str()))
            .collect();
        sorted_asks.sort_by(|a, b| {
            let fa: f64 = a.0.parse().unwrap_or(0.0);
            let fb: f64 = b.0.parse().unwrap_or(0.0);
            fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
        });
        let best_bid = sorted_bids.first().map(|(p, _)| json!(p));
        let best_ask = sorted_asks.first().map(|(p, _)| json!(p));
        json!({
            "bid_count": sorted_bids.len(),
            "ask_count": sorted_asks.len(),
            "bids": sorted_bids.iter().map(|(p, s)| json!([p, s])).collect::<Vec<_>>(),
            "asks": sorted_asks.iter().map(|(p, s)| json!([p, s])).collect::<Vec<_>>(),
            "best_bid": best_bid,
            "best_ask": best_ask,
        })
    }
}

#[test]
fn replace_with_dict_format() {
    let mut book = LocalBook::new();
    let bids = json!([
        {"price": "0.55", "size": "1200"},
        {"price": "0.54", "size": "800"},
    ]);
    let asks = json!([
        {"price": "0.56", "size": "1000"},
        {"price": "0.57", "size": "500"},
    ]);
    book.replace(&bids, &asks);

    assert_eq!(book.bids.len(), 2);
    assert_eq!(book.asks.len(), 2);
    assert_eq!(book.bids.get("0.55").unwrap(), "1200");

    let snap = book.snapshot();
    assert_eq!(snap["best_bid"], "0.55");
    assert_eq!(snap["best_ask"], "0.56");
    assert_eq!(snap["bid_count"], 2);
}

#[test]
fn replace_with_array_format() {
    let mut book = LocalBook::new();
    let bids = json!([["0.55", "1200"], ["0.54", "800"]]);
    let asks = json!([["0.56", "1000"]]);
    book.replace(&bids, &asks);

    assert_eq!(book.bids.len(), 2);
    assert_eq!(book.asks.len(), 1);
    assert_eq!(book.bids.get("0.55").unwrap(), "1200");
}

#[test]
fn apply_delta_buy_sell() {
    let mut book = LocalBook::new();
    let bids = json!([{"price": "0.55", "size": "1200"}]);
    let asks = json!([{"price": "0.56", "size": "1000"}]);
    book.replace(&bids, &asks);

    // Add bid
    book.apply_delta("BUY", "0.54", "500");
    assert_eq!(book.bids.len(), 2);
    assert_eq!(book.bids.get("0.54").unwrap(), "500");

    // Remove bid
    book.apply_delta("BUY", "0.55", "0");
    assert_eq!(book.bids.len(), 1);
    assert!(!book.bids.contains_key("0.55"));

    // Add ask
    book.apply_delta("SELL", "0.57", "300");
    assert_eq!(book.asks.len(), 2);

    // Remove ask
    book.apply_delta("SELL", "0.56", "0");
    assert_eq!(book.asks.len(), 1);
}

#[test]
fn snapshot_sort_order() {
    let mut book = LocalBook::new();
    let bids = json!([
        {"price": "0.50", "size": "100"},
        {"price": "0.55", "size": "200"},
        {"price": "0.52", "size": "150"},
    ]);
    let asks = json!([
        {"price": "0.60", "size": "100"},
        {"price": "0.56", "size": "200"},
        {"price": "0.58", "size": "150"},
    ]);
    book.replace(&bids, &asks);

    let snap = book.snapshot();

    // Bids: descending (highest first)
    let snap_bids = snap["bids"].as_array().unwrap();
    assert_eq!(snap_bids[0][0], "0.55");
    assert_eq!(snap_bids[1][0], "0.52");
    assert_eq!(snap_bids[2][0], "0.50");

    // Asks: ascending (lowest first)
    let snap_asks = snap["asks"].as_array().unwrap();
    assert_eq!(snap_asks[0][0], "0.56");
    assert_eq!(snap_asks[1][0], "0.58");
    assert_eq!(snap_asks[2][0], "0.60");
}

// ── Dedup tests ───────────────────────────────────────────────────────────

fn dedup_hash(msg: &serde_json::Value) -> u64 {
    let bytes = serde_json::to_vec(msg).unwrap_or_default();
    let mut hasher = ahash::AHasher::default();
    hasher.write(&bytes);
    hasher.finish()
}

#[test]
fn dedup_same_content_same_hash() {
    // serde_json without preserve_order uses BTreeMap → sorted keys
    let msg1 = json!({"event_type": "book", "asset_id": "abc", "price": "0.55"});
    let msg2 = json!({"asset_id": "abc", "event_type": "book", "price": "0.55"});

    let h1 = dedup_hash(&msg1);
    let h2 = dedup_hash(&msg2);
    assert_eq!(
        h1, h2,
        "Same content with different key order must produce same hash"
    );
}

#[test]
fn dedup_different_content_different_hash() {
    let msg1 = json!({"event_type": "book", "asset_id": "abc"});
    let msg2 = json!({"event_type": "book", "asset_id": "def"});

    let h1 = dedup_hash(&msg1);
    let h2 = dedup_hash(&msg2);
    assert_ne!(h1, h2);
}

// ── Gamma validation tests ────────────────────────────────────────────────

fn parse_string_or_vec(val: &serde_json::Value) -> Option<Vec<String>> {
    match val {
        serde_json::Value::Array(arr) => Some(
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
        ),
        serde_json::Value::String(s) => serde_json::from_str::<Vec<String>>(s).ok(),
        _ => None,
    }
}

fn validate_market(data: &serde_json::Value, slug: &str) -> Option<String> {
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
        Some(o) if o.len() != 2 => return Some(format!("expected 2 outcomes, got {}", o.len())),
        None => return Some(format!("cannot parse outcomes: {}", data["outcomes"])),
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

#[test]
fn validate_valid_market() {
    let data = json!({
        "slug": "btc-updown-5m-123",
        "closed": false,
        "enableOrderBook": true,
        "outcomes": ["Up", "Down"],
        "clobTokenIds": ["id1", "id2"],
    });
    assert!(validate_market(&data, "btc-updown-5m-123").is_none());
}

#[test]
fn validate_closed_market() {
    let data = json!({
        "slug": "btc-updown-5m-123",
        "closed": true,
        "enableOrderBook": true,
        "outcomes": ["Up", "Down"],
        "clobTokenIds": ["id1", "id2"],
    });
    assert_eq!(
        validate_market(&data, "btc-updown-5m-123").unwrap(),
        "market is closed"
    );
}

#[test]
fn validate_string_encoded_arrays() {
    let data = json!({
        "slug": "btc-updown-5m-123",
        "closed": false,
        "enableOrderBook": true,
        "outcomes": "[\"Up\", \"Down\"]",
        "clobTokenIds": "[\"id1\", \"id2\"]",
    });
    assert!(validate_market(&data, "btc-updown-5m-123").is_none());
}

#[test]
fn validate_wrong_outcome_count() {
    let data = json!({
        "slug": "btc-updown-5m-123",
        "closed": false,
        "enableOrderBook": true,
        "outcomes": ["Up", "Down", "Sideways"],
        "clobTokenIds": ["id1", "id2"],
    });
    assert!(
        validate_market(&data, "btc-updown-5m-123")
            .unwrap()
            .contains("expected 2 outcomes")
    );
}

#[test]
fn validate_slug_mismatch() {
    let data = json!({
        "slug": "wrong-slug",
        "closed": false,
        "enableOrderBook": true,
        "outcomes": ["Up", "Down"],
        "clobTokenIds": ["id1", "id2"],
    });
    assert!(
        validate_market(&data, "btc-updown-5m-123")
            .unwrap()
            .contains("slug mismatch")
    );
}
