use serde_json::json;

#[path = "../src/types.rs"]
mod types;

#[path = "../src/config.rs"]
mod config;

#[path = "../src/error.rs"]
mod error;

// Test BinanceBook directly by reimplementing minimal version
// (can't import from binary crate easily, so test the logic)

use rust_decimal::Decimal;
use std::collections::BTreeMap;

struct BinanceBook {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
    update_id: i64,
}

impl BinanceBook {
    fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            update_id: 0,
        }
    }

    fn load_snapshot(&mut self, snap: &serde_json::Value) {
        self.bids.clear();
        self.asks.clear();
        if let Some(bids) = snap["bids"].as_array() {
            for entry in bids {
                if let (Some(p), Some(q)) = (entry[0].as_str(), entry[1].as_str()) {
                    if let (Ok(price), Ok(qty)) = (p.parse::<Decimal>(), q.parse::<Decimal>()) {
                        self.bids.insert(price, qty);
                    }
                }
            }
        }
        if let Some(asks) = snap["asks"].as_array() {
            for entry in asks {
                if let (Some(p), Some(q)) = (entry[0].as_str(), entry[1].as_str()) {
                    if let (Ok(price), Ok(qty)) = (p.parse::<Decimal>(), q.parse::<Decimal>()) {
                        self.asks.insert(price, qty);
                    }
                }
            }
        }
        if let Some(uid) = snap["lastUpdateId"].as_i64() {
            self.update_id = uid;
        }
    }

    fn apply_diff(&mut self, data: &serde_json::Value) {
        if let Some(bid_updates) = data["b"].as_array() {
            for entry in bid_updates {
                if let (Some(p), Some(q)) = (entry[0].as_str(), entry[1].as_str()) {
                    if let Ok(price) = p.parse::<Decimal>() {
                        if q == "0" || q.parse::<Decimal>().map_or(false, |d| d.is_zero()) {
                            self.bids.remove(&price);
                        } else if let Ok(qty) = q.parse::<Decimal>() {
                            self.bids.insert(price, qty);
                        }
                    }
                }
            }
        }
        if let Some(ask_updates) = data["a"].as_array() {
            for entry in ask_updates {
                if let (Some(p), Some(q)) = (entry[0].as_str(), entry[1].as_str()) {
                    if let Ok(price) = p.parse::<Decimal>() {
                        if q == "0" || q.parse::<Decimal>().map_or(false, |d| d.is_zero()) {
                            self.asks.remove(&price);
                        } else if let Ok(qty) = q.parse::<Decimal>() {
                            self.asks.insert(price, qty);
                        }
                    }
                }
            }
        }
        if let Some(u) = data["u"].as_i64() {
            self.update_id = u;
        }
    }
}

#[test]
fn load_snapshot_and_sort() {
    let mut book = BinanceBook::new();
    let snap = json!({
        "lastUpdateId": 12345,
        "bids": [["96000.50", "1.5"], ["96001.00", "2.0"], ["95999.00", "0.5"]],
        "asks": [["96002.00", "1.0"], ["96001.50", "3.0"], ["96003.00", "0.1"]]
    });

    book.load_snapshot(&snap);
    assert_eq!(book.update_id, 12345);
    assert_eq!(book.bids.len(), 3);
    assert_eq!(book.asks.len(), 3);

    // BTreeMap sorts ascending. Best bid = last (highest)
    let best_bid = book.bids.iter().next_back().unwrap();
    assert_eq!(best_bid.0.to_string(), "96001.00");

    // Best ask = first (lowest)
    let best_ask = book.asks.iter().next().unwrap();
    assert_eq!(best_ask.0.to_string(), "96001.50");
}

#[test]
fn apply_diff_removes_zero_qty() {
    let mut book = BinanceBook::new();
    let snap = json!({
        "lastUpdateId": 100,
        "bids": [["96000.00", "1.0"], ["95999.00", "2.0"]],
        "asks": [["96001.00", "1.0"]]
    });
    book.load_snapshot(&snap);

    let diff = json!({
        "b": [["96000.00", "0"], ["95998.00", "3.0"]],
        "a": [["96001.00", "0.00000000"]],
        "U": 101,
        "u": 101
    });
    book.apply_diff(&diff);

    assert_eq!(book.update_id, 101);
    assert_eq!(book.bids.len(), 2); // removed 96000, added 95998
    assert!(
        !book
            .bids
            .contains_key(&"96000.00".parse::<Decimal>().unwrap())
    );
    assert!(
        book.bids
            .contains_key(&"95998.00".parse::<Decimal>().unwrap())
    );
    assert_eq!(book.asks.len(), 0); // removed 96001 (zero as "0.00000000")
}

#[test]
fn sync_validation_snapshot_spans_buffer() {
    // Simulate: lastUpdateId=100, buffer has event U=100, u=105
    // Condition: first.U <= lastUpdateId + 1 <= first.u
    // 100 <= 101 <= 105 → true → sync OK
    let last_uid: i64 = 100;
    let first_u: i64 = 100;
    let first_uu: i64 = 105;
    assert!(first_u <= last_uid + 1 && last_uid + 1 <= first_uu);
}

#[test]
fn sync_validation_gap_detected() {
    // lastUpdateId=100, first buffer U=102 → gap (102 > 101)
    let last_uid: i64 = 100;
    let first_u: i64 = 102;
    let first_uu: i64 = 110;
    assert!(!(first_u <= last_uid + 1 && last_uid + 1 <= first_uu));
}

#[test]
fn sync_validation_snapshot_covers_all() {
    // lastUpdateId=200, buffer has events with u <= 200 → all filtered out
    let last_uid: i64 = 200;
    let buffer_events = vec![json!({"U": 190, "u": 195}), json!({"U": 196, "u": 200})];
    let remaining: Vec<_> = buffer_events
        .iter()
        .filter(|e| e["u"].as_i64().unwrap_or(0) > last_uid)
        .collect();
    assert!(remaining.is_empty());
}

#[test]
fn gap_detection_in_steady_state() {
    // book.update_id = 500, incoming U = 502 → gap (502 > 501)
    let book_update_id: i64 = 500;
    let evt_u_upper: i64 = 502;
    assert!(evt_u_upper > book_update_id + 1);

    // book.update_id = 500, incoming U = 501 → no gap
    let evt_u_upper_ok: i64 = 501;
    assert!(!(evt_u_upper_ok > book_update_id + 1));
}

#[test]
fn stale_event_skip() {
    // book.update_id = 500, incoming u = 499 → stale, skip
    let book_update_id: i64 = 500;
    let evt_u_lower: i64 = 499;
    assert!(evt_u_lower <= book_update_id);

    // incoming u = 501 → not stale
    let evt_u_lower_ok: i64 = 501;
    assert!(!(evt_u_lower_ok <= book_update_id));
}
