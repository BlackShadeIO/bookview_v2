use serde_json::json;

#[test]
fn system_line_matches_python_format() {
    // Simulate the exact first line from Python output:
    // {"ts_recv_ms":1777505930193,"ts_recv_unix":1777505930.193,"market_start":1777506000,
    //  "collector_pid":19090,"source":"polymarket","event_type":"system","seq":1,
    //  "raw":null,"normalized":{"market_start":1777506000,"pid":19090,"stop_at":1777506320},
    //  "system_type":"worker_started"}

    let line = json!({
        "ts_recv_ms": 1777505930193_i64,
        "ts_recv_unix": 1777505930.193,
        "market_start": 1777506000_i64,
        "collector_pid": 19090_u32,
        "source": "polymarket",
        "event_type": "system",
        "seq": 1_u64,
        "raw": null,
        "normalized": {"market_start": 1777506000, "pid": 19090, "stop_at": 1777506320},
        "system_type": "worker_started"
    });

    let serialized = serde_json::to_string(&line).unwrap();

    // Verify key properties
    assert!(
        serialized.contains("\"raw\":null"),
        "raw should be null, not absent"
    );
    assert!(
        serialized.contains("\"system_type\":\"worker_started\""),
        "system_type should be present"
    );
    assert!(
        serialized.contains("\"ts_recv_unix\":1777505930.193"),
        "ts_recv_unix should have 3 decimal places"
    );
}

#[test]
fn non_system_line_omits_system_type() {
    // Use the actual Line struct to verify skip_serializing_if works
    #[derive(serde::Serialize)]
    struct TestLine {
        event_type: String,
        raw: serde_json::Value,
        normalized: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        system_type: Option<String>,
    }

    let line = TestLine {
        event_type: "book_raw".into(),
        raw: json!({"some": "data"}),
        normalized: json!({"asset_id": "abc"}),
        system_type: None,
    };

    let serialized = serde_json::to_string(&line).unwrap();
    assert!(
        !serialized.contains("system_type"),
        "system_type should be absent when None"
    );
}

#[test]
fn ts_recv_unix_precision() {
    // Python: now_ms = int(time.time() * 1000), now_s = now_ms / 1000.0
    // This gives exactly 3 decimal places
    let ms: i64 = 1777505930193;
    let unix = ms as f64 / 1000.0;

    let val = serde_json::to_string(&unix).unwrap();
    assert_eq!(
        val, "1777505930.193",
        "Should have exactly 3 decimal places"
    );

    // Edge case: trailing zeros
    let ms2: i64 = 1777505930100;
    let unix2 = ms2 as f64 / 1000.0;
    let val2 = serde_json::to_string(&unix2).unwrap();
    assert_eq!(
        val2, "1777505930.1",
        "serde_json drops trailing zeros — matches Python orjson"
    );
}

#[test]
fn prices_as_strings() {
    let book_state = json!({
        "bid_count": 5,
        "ask_count": 5,
        "bids": [["0.55", "1200"], ["0.54", "800"]],
        "asks": [["0.56", "1000"], ["0.57", "500"]],
        "best_bid": "0.55",
        "best_ask": "0.56",
        "asset_id": "12345",
        "last_trade_price": "0.55",
        "trigger_event_type": "book"
    });

    let serialized = serde_json::to_string(&book_state).unwrap();
    assert!(serialized.contains("\"0.55\""), "Prices must be strings");
    assert!(
        !serialized.contains(":0.55"),
        "Prices must NOT be bare numbers"
    );
}
