use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use clap::Parser;
use rayon::prelude::*;

use bookview_collector::config::{MARKET_DURATION, TAIL_SECONDS};
use bookview_collector::fair_value::{FairValueEngine, FairValueParams, VolTimescale};

#[derive(Parser)]
#[command(name = "backtest", about = "Fair value backtester and calibrator")]
struct Cli {
    /// Path to data directory
    #[arg(default_value = "../data")]
    data_dir: PathBuf,

    /// Run parameter calibration (grid search with train/test split)
    #[arg(long)]
    calibrate: bool,

    /// Generate fair-value.jsonl for markets that don't have one
    #[arg(long)]
    generate_fv: bool,
}

fn main() {
    let cli = Cli::parse();
    let data_dir = &cli.data_dir;

    if !data_dir.is_dir() {
        eprintln!("Data directory not found: {}", data_dir.display());
        std::process::exit(1);
    }

    let mut market_epochs = discover_markets(data_dir);
    market_epochs.sort();
    eprintln!("Found {} markets in {}", market_epochs.len(), data_dir.display());

    if market_epochs.is_empty() {
        eprintln!("No markets found.");
        return;
    }

    if cli.generate_fv {
        generate_fv_files(data_dir, &market_epochs);
    } else if cli.calibrate {
        run_calibration(data_dir, &market_epochs);
    } else {
        run_single(data_dir, &market_epochs, &FairValueParams::default());
    }
}

// ── Generate fair-value.jsonl files ────────────────────────────────────

fn generate_fv_files(data_dir: &Path, epochs: &[i64]) {
    use std::io::Write;

    let params = FairValueParams::default();
    let skip_existing = true;
    let counter = AtomicUsize::new(0);
    let total = epochs.len();

    epochs.par_iter().for_each(|&epoch| {
        let fv_path = data_dir.join(epoch.to_string()).join("fair-value.jsonl");
        if skip_existing && fv_path.exists() {
            let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
            if done % 50 == 0 {
                eprintln!("  [{done}/{total}] skipped (exists)");
            }
            return;
        }

        let market = match preload_market(data_dir, epoch) {
            Ok(m) => m,
            Err(_) => {
                counter.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };

        let market_start_ms = epoch * 1000;
        let market_end_ms = market_start_ms + (MARKET_DURATION as i64) * 1000;
        let mut engine = FairValueEngine::new(params.clone(), None);
        let mut lines: Vec<String> = Vec::new();
        let mut last_sample_ms: i64 = 0;
        let mut seq = 0u64;

        for event in &market.events {
            match event {
                BinanceEvent::BookTicker { ts_ms, bid, bid_qty, ask, ask_qty } => {
                    engine.on_book_ticker(*ts_ms, *bid, *bid_qty, *ask, *ask_qty);
                }
                BinanceEvent::Trade { ts_ms, qty, is_buy } => {
                    engine.on_trade(*ts_ms, *qty, *is_buy);
                }
                BinanceEvent::Depth { bids, asks } => {
                    engine.on_depth(bids, asks);
                }
            }

            let event_ts = match event {
                BinanceEvent::BookTicker { ts_ms, .. } | BinanceEvent::Trade { ts_ms, .. } => *ts_ms,
                BinanceEvent::Depth { .. } => continue,
            };

            if event_ts < market_start_ms || event_ts > market_end_ms { continue; }
            if event_ts - last_sample_ms < 100 { continue; }

            if let Some(snap) = engine.sample(event_ts, market.strike, market_end_ms) {
                seq += 1;
                let normalized = serde_json::to_value(&snap).unwrap_or_default();
                let line = serde_json::json!({
                    "ts_recv_ms": event_ts,
                    "ts_recv_unix": event_ts as f64 / 1000.0,
                    "market_start": epoch,
                    "source": "fair_value",
                    "event_type": "fv_sample",
                    "seq": seq,
                    "normalized": normalized,
                });
                lines.push(serde_json::to_string(&line).unwrap_or_default());
                last_sample_ms = event_ts;
            }
        }

        if !lines.is_empty() {
            if let Ok(mut f) = File::create(&fv_path) {
                for line in &lines {
                    let _ = writeln!(f, "{line}");
                }
            }
        }

        let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if done % 10 == 0 || done == total {
            eprintln!("  [{done}/{total}] generated {epoch} ({} samples)", lines.len());
        }
    });

    eprintln!("Done generating fair-value.jsonl files.");
}

// ── Single run (CSV output) ────────────────────────────────────────────

fn run_single(data_dir: &Path, epochs: &[i64], params: &FairValueParams) {
    println!("market_start,ts_ms,tau,fair_yes,fair_no,poly_up_mid,poly_down_mid,btc_mid,strike,outcome");

    let mut total_brier = 0.0;
    let mut total_samples = 0u64;
    let mut total_rmse_sum = 0.0;
    let mut total_rmse_n = 0u64;

    for (i, &epoch) in epochs.iter().enumerate() {
        let market = match preload_market(data_dir, epoch) {
            Ok(m) => m,
            Err(e) => { eprintln!("  SKIP {}: {}", epoch, e); continue; }
        };

        let rows = evaluate_market(&market, params);
        let outcome_f = if market.outcome { 1.0 } else { 0.0 };

        for row in &rows {
            println!(
                "{},{},{:.3},{:.6},{:.6},{},{},{:.2},{:.2},{}",
                epoch, row.ts_ms, row.tau, row.fair_yes, row.fair_no,
                row.poly_up_mid.map_or("".to_string(), |v| format!("{:.4}", v)),
                row.poly_down_mid.map_or("".to_string(), |v| format!("{:.4}", v)),
                row.btc_mid, market.strike,
                if market.outcome { "UP" } else { "DOWN" },
            );

            let diff = row.fair_yes - outcome_f;
            total_brier += diff * diff;
            total_samples += 1;

            if let Some(poly_mid) = row.poly_up_mid {
                let rmse_diff = row.fair_yes - poly_mid;
                total_rmse_sum += rmse_diff * rmse_diff;
                total_rmse_n += 1;
            }
        }

        eprintln!("  market {} ({}/{}): {} samples", epoch, i + 1, epochs.len(), rows.len());
    }

    eprintln!("\n=== Results ===");
    if total_samples > 0 {
        eprintln!("Brier score:  {:.6} (random=0.25)", total_brier / total_samples as f64);
    }
    if total_rmse_n > 0 {
        eprintln!("RMSE vs Poly: {:.6}", (total_rmse_sum / total_rmse_n as f64).sqrt());
    }
}

// ── Calibration ────────────────────────────────────────────────────────
// Stream one market at a time: load → evaluate all param combos → discard.
// Memory stays bounded regardless of dataset size.

fn run_calibration(data_dir: &Path, epochs: &[i64]) {
    let n = epochs.len();
    let train_end = (n as f64 * 0.7).ceil() as usize;
    let train_epochs = &epochs[..train_end];
    let test_epochs = &epochs[train_end..];

    eprintln!("Split: {} train, {} test", train_epochs.len(), test_epochs.len());

    let grid = build_grid();
    let grid_len = grid.len();
    eprintln!("Grid: {} parameter combinations", grid_len);

    let start = Instant::now();

    // Accumulate per-combo: (brier_sum, brier_n, rmse_sum, rmse_n)
    let accum: Vec<Mutex<(f64, u64, f64, u64)>> =
        (0..grid_len).map(|_| Mutex::new((0.0, 0, 0.0, 0))).collect();

    let markets_done = AtomicUsize::new(0);
    let markets_skipped = AtomicUsize::new(0);

    for &epoch in train_epochs {
        let market = match preload_market(data_dir, epoch) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("  SKIP {}: {}", epoch, e);
                markets_skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        let outcome_f = if market.outcome { 1.0 } else { 0.0 };

        // Evaluate all param combos on this market in parallel
        grid.par_iter().enumerate().for_each(|(idx, params)| {
            let rows = evaluate_market(&market, params);
            let mut brier_sum = 0.0;
            let mut brier_n = 0u64;
            let mut rmse_sum = 0.0;
            let mut rmse_n = 0u64;

            for row in &rows {
                let diff = row.fair_yes - outcome_f;
                brier_sum += diff * diff;
                brier_n += 1;

                if let Some(poly_mid) = row.poly_up_mid {
                    let d = row.fair_yes - poly_mid;
                    rmse_sum += d * d;
                    rmse_n += 1;
                }
            }

            let mut a = accum[idx].lock().unwrap();
            a.0 += brier_sum;
            a.1 += brier_n;
            a.2 += rmse_sum;
            a.3 += rmse_n;
        });

        let done = markets_done.fetch_add(1, Ordering::Relaxed) + 1;
        let elapsed = start.elapsed().as_secs_f64();
        let rate = elapsed / done as f64;
        let remaining = train_epochs.len() - done - markets_skipped.load(Ordering::Relaxed);
        eprintln!(
            "  [{}/{}] market {} ({:.1}s/market, ETA {:.0}s)",
            done, train_epochs.len(), epoch, rate, rate * remaining as f64,
        );
    }

    // Compute final scores
    let mut results: Vec<(usize, f64, f64)> = accum
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let a = m.lock().unwrap();
            let brier = if a.1 > 0 { a.0 / a.1 as f64 } else { 1.0 };
            let rmse = if a.3 > 0 { (a.2 / a.3 as f64).sqrt() } else { 1.0 };
            (idx, brier, rmse)
        })
        .collect();

    results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    eprintln!("\n=== Top 10 on TRAIN set (by Brier) ===");
    eprintln!("{:<6} {:<8} {:<8} {:<6} {:<6} {:<6} {:<6} {:<5}",
        "Rank", "Brier", "RMSE", "μW", "drift", "hlMs", "vol", "dLvl");

    for (rank, &(idx, brier, rmse)) in results.iter().take(10).enumerate() {
        let p = &grid[idx];
        eprintln!("{:<6} {:<8.6} {:<8.6} {:<6.2} {:<6.3} {:<6.0} {:<6} {:<5}",
            rank + 1, brier, rmse,
            p.microprice_weight, p.drift_coeff, p.trade_halflife_ms,
            vol_label(p.vol_timescale), p.depth_levels,
        );
    }

    // Evaluate best on test set (stream one market at a time)
    let best_idx = results[0].0;
    let best_params = &grid[best_idx];
    let train_brier = results[0].1;

    eprintln!("\nEvaluating on test set...");
    let test_brier = score_on_set_streaming(data_dir, test_epochs, best_params);
    let test_rmse = rmse_on_set_streaming(data_dir, test_epochs, best_params);
    let default_train_brier = score_from_accum(&accum, &grid, &FairValueParams::default());
    let default_test_brier = score_on_set_streaming(data_dir, test_epochs, &FairValueParams::default());

    eprintln!("\n=== Best parameters ===");
    eprintln!("  microprice_weight: {}", best_params.microprice_weight);
    eprintln!("  depth_weight:      {}", best_params.depth_weight);
    eprintln!("  drift_coeff:       {}", best_params.drift_coeff);
    eprintln!("  trade_halflife_ms: {}", best_params.trade_halflife_ms);
    eprintln!("  vol_timescale:     {}", vol_label(best_params.vol_timescale));
    eprintln!("  depth_levels:      {}", best_params.depth_levels);
    eprintln!("  imbalance_levels:  {}", best_params.imbalance_levels);

    eprintln!("\n=== Comparison ===");
    eprintln!("                  Train    Test");
    eprintln!("  Default Brier:  {:<8.6} {:<8.6}", default_train_brier, default_test_brier);
    eprintln!("  Best Brier:     {:<8.6} {:<8.6}", train_brier, test_brier);
    eprintln!("  Best RMSE:      {:<8.6} {:<8.6}", results[0].2, test_rmse);
    let improve = ((default_test_brier - test_brier) / default_test_brier * 100.0).max(-999.0);
    eprintln!("  Improvement:    {:.1}% on test set", improve);

    eprintln!("\n=== Config update (paste into config.rs) ===");
    eprintln!("pub const FV_MICROPRICE_WEIGHT: f64 = {};", best_params.microprice_weight);
    eprintln!("pub const FV_DEPTH_WEIGHT: f64 = {};", best_params.depth_weight);
    eprintln!("pub const FV_DRIFT_COEFF: f64 = {};", best_params.drift_coeff);
    eprintln!("pub const FV_TRADE_HALFLIFE_MS: f64 = {};", best_params.trade_halflife_ms);
    eprintln!("pub const FV_DEPTH_LEVELS: usize = {};", best_params.depth_levels);
    eprintln!("pub const FV_IMBALANCE_LEVELS: usize = {};", best_params.imbalance_levels);
    eprintln!("// vol_timescale: {} (update FairValueParams::default())", vol_label(best_params.vol_timescale));

    eprintln!("\nCalibration completed in {:.1}s", start.elapsed().as_secs_f64());
}

fn score_from_accum(
    accum: &[Mutex<(f64, u64, f64, u64)>],
    grid: &[FairValueParams],
    target: &FairValueParams,
) -> f64 {
    for (idx, p) in grid.iter().enumerate() {
        if (p.microprice_weight - target.microprice_weight).abs() < 0.001
            && (p.drift_coeff - target.drift_coeff).abs() < 0.001
            && (p.trade_halflife_ms - target.trade_halflife_ms).abs() < 1.0
            && p.vol_timescale == target.vol_timescale
            && p.depth_levels == target.depth_levels
        {
            let a = accum[idx].lock().unwrap();
            return if a.1 > 0 { a.0 / a.1 as f64 } else { 1.0 };
        }
    }
    // Default not in grid, compute from scratch — but this is expensive.
    // Return a sentinel; caller should handle.
    f64::NAN
}

fn score_on_set_streaming(data_dir: &Path, epochs: &[i64], params: &FairValueParams) -> f64 {
    let mut total = 0.0;
    let mut n = 0u64;
    for &epoch in epochs {
        let market = match preload_market(data_dir, epoch) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let outcome_f = if market.outcome { 1.0 } else { 0.0 };
        for row in evaluate_market(&market, params) {
            let diff = row.fair_yes - outcome_f;
            total += diff * diff;
            n += 1;
        }
    }
    if n > 0 { total / n as f64 } else { 1.0 }
}

fn rmse_on_set_streaming(data_dir: &Path, epochs: &[i64], params: &FairValueParams) -> f64 {
    let mut total = 0.0;
    let mut n = 0u64;
    for &epoch in epochs {
        let market = match preload_market(data_dir, epoch) {
            Ok(m) => m,
            Err(_) => continue,
        };
        for row in evaluate_market(&market, params) {
            if let Some(poly_mid) = row.poly_up_mid {
                let diff = row.fair_yes - poly_mid;
                total += diff * diff;
                n += 1;
            }
        }
    }
    if n > 0 { (total / n as f64).sqrt() } else { 1.0 }
}

fn build_grid() -> Vec<FairValueParams> {
    let microprice_weights = [0.5, 0.6, 0.7, 0.8, 0.9, 1.0];
    let drift_coeffs = [0.0, 0.05, 0.1, 0.15, 0.2, 0.3];
    let trade_halflives = [1500.0, 2500.0, 4000.0];
    let vol_timescales = [VolTimescale::Rv1s, VolTimescale::Rv5s, VolTimescale::Rv15s];
    let depth_levels_opts = [3, 5, 10];

    let mut grid = Vec::new();
    for &mw in &microprice_weights {
        for &dc in &drift_coeffs {
            for &th in &trade_halflives {
                for &vt in &vol_timescales {
                    for &dl in &depth_levels_opts {
                        grid.push(FairValueParams {
                            microprice_weight: mw,
                            depth_weight: 1.0 - mw,
                            depth_levels: dl,
                            imbalance_levels: 10,
                            trade_halflife_ms: th,
                            drift_coeff: dc,
                            vol_floor: 1e-8,
                            vol_timescale: vt,
                        });
                    }
                }
            }
        }
    }
    grid
}

fn vol_label(v: VolTimescale) -> &'static str {
    match v {
        VolTimescale::Rv1s => "rv1s",
        VolTimescale::Rv5s => "rv5s",
        VolTimescale::Rv15s => "rv15s",
    }
}

// ── Market evaluation ──────────────────────────────────────────────────

struct MarketData {
    epoch: i64,
    strike: f64,
    outcome: bool,
    events: Vec<BinanceEvent>,
    poly_mids: BTreeMap<i64, f64>,
}

enum BinanceEvent {
    BookTicker { ts_ms: i64, bid: f64, bid_qty: f64, ask: f64, ask_qty: f64 },
    Trade { ts_ms: i64, qty: f64, is_buy: bool },
    Depth { bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)> },
}

struct SampleRow {
    ts_ms: i64,
    tau: f64,
    fair_yes: f64,
    fair_no: f64,
    poly_up_mid: Option<f64>,
    poly_down_mid: Option<f64>,
    btc_mid: f64,
}

fn evaluate_market(market: &MarketData, params: &FairValueParams) -> Vec<SampleRow> {
    let market_start_ms = market.epoch * 1000;
    let market_end_ms = market_start_ms + (MARKET_DURATION as i64) * 1000;

    let mut engine = FairValueEngine::new(params.clone(), None);
    let mut rows = Vec::new();
    let mut last_sample_ms: i64 = 0;
    let mut last_btc_mid = 0.0;

    for event in &market.events {
        match event {
            BinanceEvent::BookTicker { ts_ms, bid, bid_qty, ask, ask_qty } => {
                engine.on_book_ticker(*ts_ms, *bid, *bid_qty, *ask, *ask_qty);
                last_btc_mid = (bid + ask) / 2.0;
            }
            BinanceEvent::Trade { ts_ms, qty, is_buy } => {
                engine.on_trade(*ts_ms, *qty, *is_buy);
            }
            BinanceEvent::Depth { bids, asks } => {
                engine.on_depth(bids, asks);
            }
        }

        let event_ts = match event {
            BinanceEvent::BookTicker { ts_ms, .. } | BinanceEvent::Trade { ts_ms, .. } => *ts_ms,
            BinanceEvent::Depth { .. } => continue,
        };

        if event_ts < market_start_ms || event_ts > market_end_ms { continue; }
        if event_ts - last_sample_ms < 100 { continue; }

        if let Some(snap) = engine.sample(event_ts, market.strike, market_end_ms) {
            let poly_up_mid = find_nearest_poly_mid(&market.poly_mids, event_ts);
            rows.push(SampleRow {
                ts_ms: event_ts,
                tau: snap.tau,
                fair_yes: snap.fair_yes,
                fair_no: snap.fair_no,
                poly_up_mid,
                poly_down_mid: poly_up_mid.map(|p| 1.0 - p),
                btc_mid: last_btc_mid,
            });
            last_sample_ms = event_ts;
        }
    }
    rows
}

// ── Data loading ───────────────────────────────────────────────────────

fn discover_markets(data_dir: &Path) -> Vec<i64> {
    let mut markets = Vec::new();
    let Ok(entries) = std::fs::read_dir(data_dir) else { return markets };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if let Ok(epoch) = name_str.parse::<i64>() {
            if entry.path().join("depth-trade.jsonl").exists() {
                markets.push(epoch);
            }
        }
    }
    markets
}

fn preload_market(data_dir: &Path, epoch: i64) -> Result<MarketData, String> {
    let folder = data_dir.join(epoch.to_string());
    let depth_path = folder.join("depth-trade.jsonl");
    let clob_path = folder.join("clob.jsonl");
    let market_end_ms = epoch * 1000 + (MARKET_DURATION as i64) * 1000;
    let collection_end_ms = market_end_ms + (TAIL_SECONDS as i64) * 1000;

    let (events, strike) = parse_binance_events(&depth_path)?;
    let strike = strike.ok_or("No strike price")?;
    let poly_mids = if clob_path.exists() { parse_poly_mids(&clob_path)? } else { BTreeMap::new() };

    let mut final_btc_mid = 0.0;
    for event in &events {
        if let BinanceEvent::BookTicker { ts_ms, bid, ask, .. } = event {
            if *ts_ms <= collection_end_ms {
                final_btc_mid = (bid + ask) / 2.0;
            }
        }
    }

    Ok(MarketData {
        epoch,
        strike,
        outcome: final_btc_mid > strike,
        events,
        poly_mids,
    })
}

fn parse_binance_events(path: &Path) -> Result<(Vec<BinanceEvent>, Option<f64>), String> {
    let file = File::open(path).map_err(|e| format!("Cannot open {}: {}", path.display(), e))?;
    let reader = BufReader::with_capacity(1024 * 1024, file);
    let mut events = Vec::new();
    let mut strike = None;

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Read error: {e}"))?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let ts_ms = v["ts_recv_ms"].as_i64().unwrap_or(0);
        let event_type = v["event_type"].as_str().unwrap_or("");

        match event_type {
            "book_ticker_normalized" => {
                let n = &v["normalized"];
                let bid = parse_str_f64(n, "bid_price");
                let bid_qty = parse_str_f64(n, "bid_qty");
                let ask = parse_str_f64(n, "ask_price");
                let ask_qty = parse_str_f64(n, "ask_qty");
                if bid > 0.0 && ask > 0.0 {
                    events.push(BinanceEvent::BookTicker { ts_ms, bid, bid_qty, ask, ask_qty });
                }
            }
            "trade_normalized" => {
                let n = &v["normalized"];
                let qty = parse_str_f64(n, "qty");
                let side = n["side"].as_str().unwrap_or("");
                if qty > 0.0 {
                    events.push(BinanceEvent::Trade { ts_ms, qty, is_buy: side == "BUY" });
                }
            }
            "book_state" => {
                let n = &v["normalized"];
                let bids = parse_levels(&n["bids"], 20);
                let asks = parse_levels(&n["asks"], 20);
                if !bids.is_empty() && !asks.is_empty() {
                    events.push(BinanceEvent::Depth { bids, asks });
                }
            }
            "system" => {
                if v["system_type"].as_str() == Some("strike_price") {
                    let sp = parse_str_f64(&v["normalized"], "strike_price");
                    if sp > 0.0 { strike = Some(sp); }
                }
            }
            _ => {}
        }
    }
    Ok((events, strike))
}

fn parse_poly_mids(path: &Path) -> Result<BTreeMap<i64, f64>, String> {
    let file = File::open(path).map_err(|e| format!("Cannot open {}: {}", path.display(), e))?;
    let reader = BufReader::with_capacity(1024 * 1024, file);
    let mut mids: BTreeMap<i64, f64> = BTreeMap::new();
    let mut up_token_id: Option<String> = None;

    for line in reader.lines() {
        let line = line.map_err(|e| format!("Read error: {e}"))?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let event_type = v["event_type"].as_str().unwrap_or("");

        if event_type == "market_lookup" {
            if let Some(raw) = v["raw"].as_object().or_else(|| v["normalized"].as_object()) {
                if let Some(ids_str) = raw.get("clobTokenIds").and_then(|v| v.as_str()) {
                    if let Ok(ids) = serde_json::from_str::<Vec<String>>(ids_str) {
                        if !ids.is_empty() { up_token_id = Some(ids[0].clone()); }
                    }
                }
                if let Some(ids) = raw.get("clobTokenIds").and_then(|v| v.as_array()) {
                    if let Some(first) = ids.first().and_then(|v| v.as_str()) {
                        up_token_id = Some(first.to_string());
                    }
                }
            }
            continue;
        }

        if event_type != "book_state" { continue; }
        let n = &v["normalized"];
        let asset_id = n["asset_id"].as_str().unwrap_or("");
        if let Some(ref up_id) = up_token_id {
            if asset_id != up_id.as_str() { continue; }
        } else { continue; }

        let best_bid = n["best_bid"].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        let best_ask = n["best_ask"].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        if best_bid > 0.0 && best_ask > 0.0 {
            mids.insert(v["ts_recv_ms"].as_i64().unwrap_or(0), (best_bid + best_ask) / 2.0);
        }
    }
    Ok(mids)
}

fn find_nearest_poly_mid(mids: &BTreeMap<i64, f64>, ts_ms: i64) -> Option<f64> {
    let mut best: Option<(i64, f64)> = None;
    for (&k, &v) in mids.range(ts_ms - 500..=ts_ms + 500) {
        let dist = (k - ts_ms).abs();
        match best {
            None => best = Some((dist, v)),
            Some((bd, _)) if dist < bd => best = Some((dist, v)),
            _ => {}
        }
    }
    best.map(|(_, v)| v)
}

fn parse_str_f64(v: &serde_json::Value, key: &str) -> f64 {
    v[key].as_str().and_then(|s| s.parse().ok()).or_else(|| v[key].as_f64()).unwrap_or(0.0)
}

fn parse_levels(arr: &serde_json::Value, max: usize) -> Vec<(f64, f64)> {
    let Some(arr) = arr.as_array() else { return Vec::new() };
    arr.iter().take(max).filter_map(|entry| {
        let a = entry.as_array()?;
        let price: f64 = a.first()?.as_str()?.parse().ok()?;
        let qty: f64 = a.get(1)?.as_str()?.parse().ok()?;
        Some((price, qty))
    }).collect()
}
