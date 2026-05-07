use std::collections::HashMap;
use super::engine::SimResult;
use super::strategies::monkey::DataSource;

pub fn print_summary(results: &[SimResult], top_n: usize) {
    // Separate monkeys from intelligent strategies
    let mut monkey_results: Vec<&SimResult> = Vec::new();
    let mut strat_results: HashMap<String, Vec<&SimResult>> = HashMap::new();

    for r in results {
        if r.strategy_name.starts_with("monkey[") {
            monkey_results.push(r);
        } else {
            strat_results.entry(r.strategy_name.clone()).or_default().push(r);
        }
    }

    // Print intelligent strategies
    if !strat_results.is_empty() {
        let n_markets = strat_results.values().next().map(|v| v.len()).unwrap_or(0);
        println!("\n=== Intelligent Strategies ({} markets) ===", n_markets);
        println!("{:<16} {:>8} {:>7} {:>6} {:>10} {:>10} {:>6}",
            "Strategy", "Avg PnL", "Sharpe", "Win%", "Fills/Mkt", "Fill Rate", "Split");
        println!("{}", "─".repeat(72));

        let mut entries: Vec<(String, f64, f64, f64, f64, f64, f64)> = strat_results.iter().map(|(name, rs)| {
            let stats = compute_stats(rs);
            (name.clone(), stats.avg_pnl, stats.sharpe, stats.win_pct, stats.avg_fills, stats.fill_rate, stats.avg_split)
        }).collect();
        entries.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        for (name, avg_pnl, sharpe, win_pct, fills, fill_rate, split) in &entries {
            println!("{:<16} {:>8.2} {:>7.2} {:>5.1}% {:>10.1} {:>9.1}% {:>6.0}",
                name, avg_pnl, sharpe, win_pct, fills, fill_rate * 100.0, split);
        }
    }

    // Print monkey leaderboard
    if !monkey_results.is_empty() {
        // Group by monkey name
        let mut monkey_groups: HashMap<String, Vec<&SimResult>> = HashMap::new();
        for r in &monkey_results {
            monkey_groups.entry(r.strategy_name.clone()).or_default().push(r);
        }

        let mut monkey_stats: Vec<(String, StratStats)> = monkey_groups.iter()
            .map(|(name, rs)| (name.clone(), compute_stats(rs)))
            .collect();
        monkey_stats.sort_by(|a, b| b.1.sharpe.partial_cmp(&a.1.sharpe).unwrap_or(std::cmp::Ordering::Equal));

        let total_monkeys = monkey_stats.len();
        let show = top_n.min(total_monkeys);

        println!("\n=== Monkey Leaderboard (Top {} of {}) ===", show, total_monkeys);
        println!("{:<4} {:<40} {:>7} {:>8} {:>6} {:>6}",
            "Rank", "DNA", "Sharpe", "Avg PnL", "Win%", "Fills");
        println!("{}", "─".repeat(80));

        for (i, (name, stats)) in monkey_stats.iter().take(show).enumerate() {
            let short_name = name.trim_start_matches("monkey[").trim_end_matches(']');
            println!("{:<4} {:<40} {:>7.2} {:>8.2} {:>5.1}% {:>6.1}",
                i + 1, truncate(short_name, 40), stats.sharpe, stats.avg_pnl, stats.win_pct, stats.avg_fills);
        }

        // Source attribution
        println!("\n=== Data Source Attribution ===");
        println!("{:<18} {:>10} {:>16} {:>12}",
            "Source", "Avg Sharpe", "In Top-20", "Signal");
        println!("{}", "─".repeat(60));

        let top_20_names: Vec<&str> = monkey_stats.iter().take(20)
            .map(|(n, _)| n.as_str()).collect();

        for src in DataSource::ALL {
            let label = src.label();
            let relevant: Vec<f64> = monkey_stats.iter()
                .filter(|(name, _)| name.contains(label))
                .map(|(_, s)| s.sharpe)
                .collect();

            if relevant.is_empty() { continue; }
            let avg_sharpe: f64 = relevant.iter().sum::<f64>() / relevant.len() as f64;
            let in_top_20 = top_20_names.iter().filter(|n| n.contains(label)).count();

            let signal = if avg_sharpe > 0.5 { "HIGH" }
                else if avg_sharpe > 0.2 { "MEDIUM" }
                else if avg_sharpe > 0.0 { "LOW" }
                else { "NOISE" };

            println!("{:<18} {:>10.3} {:>12}/{:<3} {:>12}",
                label, avg_sharpe, in_top_20, 20, signal);
        }

        // Bottom monkeys for reference
        println!("\n=== Worst 5 Monkeys ===");
        for (name, stats) in monkey_stats.iter().rev().take(5) {
            let short = name.trim_start_matches("monkey[").trim_end_matches(']');
            println!("  {:<40} sharpe={:.2} pnl={:.2}", truncate(short, 40), stats.sharpe, stats.avg_pnl);
        }
    }
}

pub fn print_csv(results: &[SimResult]) {
    println!("epoch,strategy,outcome,pnl,return_pct,fills,orders,rejected,volume,split");
    for r in results {
        println!("{},{},{},{:.4},{:.4},{},{},{},{:.2},{:.0}",
            r.epoch, r.strategy_name,
            if r.outcome { "UP" } else { "DOWN" },
            r.pnl, r.return_pct, r.num_fills,
            r.orders_submitted, r.orders_rejected,
            r.total_volume, r.split_size);
    }
}

struct StratStats {
    avg_pnl: f64,
    sharpe: f64,
    win_pct: f64,
    avg_fills: f64,
    fill_rate: f64,
    avg_split: f64,
}

fn compute_stats(results: &[&SimResult]) -> StratStats {
    let n = results.len() as f64;
    if n == 0.0 {
        return StratStats { avg_pnl: 0.0, sharpe: 0.0, win_pct: 0.0, avg_fills: 0.0, fill_rate: 0.0, avg_split: 0.0 };
    }

    let pnls: Vec<f64> = results.iter().map(|r| r.pnl).collect();
    let avg_pnl = pnls.iter().sum::<f64>() / n;
    let variance = pnls.iter().map(|p| (p - avg_pnl).powi(2)).sum::<f64>() / n;
    let std_dev = variance.sqrt();
    let sharpe = if std_dev > 0.0 { avg_pnl / std_dev * (288.0f64).sqrt() } else { 0.0 };

    let wins = results.iter().filter(|r| r.pnl > 0.0).count() as f64;
    let win_pct = wins / n * 100.0;

    let avg_fills = results.iter().map(|r| r.num_fills as f64).sum::<f64>() / n;
    let total_orders: f64 = results.iter().map(|r| r.orders_submitted as f64).sum();
    let total_fills: f64 = results.iter().map(|r| r.num_fills as f64).sum();
    let fill_rate = if total_orders > 0.0 { total_fills / total_orders } else { 0.0 };
    let avg_split = results.iter().map(|r| r.split_size).sum::<f64>() / n;

    StratStats { avg_pnl, sharpe, win_pct, avg_fills, fill_rate, avg_split }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}
