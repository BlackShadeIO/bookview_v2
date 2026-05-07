// Test next_market_start matches Python's math.ceil((now + 70) / 300) * 300

fn next_market_start(now_secs: f64) -> i64 {
    let adjusted = now_secs + 70.0;
    let boundary = (adjusted / 300.0).ceil() as i64;
    boundary * 300
}

#[test]
fn basic_market_boundary() {
    // now = 100.5 → (100.5 + 70) / 300 = 0.568 → ceil = 1 → 300
    assert_eq!(next_market_start(100.5), 300);
}

#[test]
fn near_boundary() {
    // now = 229.0 → (229 + 70) / 300 = 0.996 → ceil = 1 → 300
    assert_eq!(next_market_start(229.0), 300);
}

#[test]
fn exactly_70s_before_boundary() {
    // now = 230.0 → (230 + 70) / 300 = 1.0 → ceil(1.0) = 1 → 300
    // This is the critical edge case: we have exactly 70s, so this market is valid
    assert_eq!(next_market_start(230.0), 300);
}

#[test]
fn just_past_70s_threshold() {
    // now = 230.001 → (230.001 + 70) / 300 = 1.0000033... → ceil = 2 → 600
    // Less than 70s before 300, so we target 600
    assert_eq!(next_market_start(230.001), 600);
}

#[test]
fn exactly_at_boundary() {
    // now = 300.0 → (300 + 70) / 300 = 1.233... → ceil = 2 → 600
    assert_eq!(next_market_start(300.0), 600);
}

#[test]
fn large_epoch_values() {
    // Realistic timestamp: 1777506000 is a 5-min boundary
    // now = 1777505929.0 → 70s before 1777505999... need to target 1777506000
    // (1777505929 + 70) / 300 = 1777505999 / 300 = 5925019.996... → ceil = 5925020 → 5925020 * 300 = 1777506000
    assert_eq!(next_market_start(1777505929.0), 1777506000);
}

#[test]
fn large_epoch_just_past() {
    // now = 1777505931.0 → (1777505931 + 70) / 300 = 1777506001 / 300 = 5925020.003... → ceil = 5925021 → 1777506300
    assert_eq!(next_market_start(1777505931.0), 1777506300);
}

#[test]
fn boundary_alignment() {
    // Every result must be a multiple of 300
    for i in 0..100 {
        let now = 1777500000.0 + (i as f64 * 3.7);
        let result = next_market_start(now);
        assert_eq!(
            result % 300,
            0,
            "Result {} not aligned to 300 for now={}",
            result,
            now
        );
    }
}
