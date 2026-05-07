use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use clap::Parser;
use rayon::prelude::*;

use bookview_collector::sim::engine::{simulate_market, SimResult};
use bookview_collector::sim::market_data::{discover_markets, load_market};
use bookview_collector::sim::output::{print_csv, print_summary};
use bookview_collector::sim::strategies::fair_value::FairValueStrategy;
use bookview_collector::sim::strategies::market_maker::MarketMakerStrategy;
use bookview_collector::sim::strategies::mean_revert::MeanRevertStrategy;
use bookview_collector::sim::strategies::monkey::generate_monkeys;
use bookview_collector::sim::strategies::trend::TrendStrategy;
use bookview_collector::sim::{FillModel, SimConfig};

#[derive(Parser)]
#[command(name = "sim", about = "Trading strategy backtesting simulator")]
struct Cli {
    /// Path to data directory
    #[arg(default_value = "../data")]
    data_dir: PathBuf,

    /// Strategies to run (comma-separated): monkeys,fv,mm,trend,meanrev
    #[arg(long, default_value = "monkeys,fv,mm,trend,meanrev")]
    strategies: String,

    /// Number of combinatorial monkey instances
    #[arg(long, default_value = "300")]
    monkey_count: usize,

    /// Initial capital per strategy (USDC)
    #[arg(long, default_value = "1000.0")]
    capital: f64,

    /// Default split size for intelligent strategies
    #[arg(long, default_value = "100.0")]
    split_size: f64,

    /// Fill model: aggressive, conservative, queue
    #[arg(long, default_value = "queue")]
    fill_model: String,

    /// Minimum latency in ms
    #[arg(long, default_value = "300")]
    min_latency_ms: u64,

    /// Maximum latency in ms
    #[arg(long, default_value = "500")]
    max_latency_ms: u64,

    /// Hold cooldown in ms (time before market-purchased tokens can be sold)
    #[arg(long, default_value = "8000")]
    hold_cooldown_ms: u64,

    /// Order size for fixed-size strategies
    #[arg(long, default_value = "50.0")]
    order_size: f64,

    /// Output format: summary, csv
    #[arg(long, default_value = "summary")]
    output: String,

    /// Random seed
    #[arg(long, default_value = "42")]
    seed: u64,

    /// Only simulate specific epoch(s)
    #[arg(long)]
    epoch: Option<Vec<i64>>,

    /// Show top N monkeys in leaderboard
    #[arg(long, default_value = "20")]
    top_n: usize,

    /// FV edge threshold for directional strategy
    #[arg(long, default_value = "0.03")]
    fv_edge: f64,

    /// Market maker half-spread
    #[arg(long, default_value = "0.02")]
    mm_spread: f64,
}

fn main() {
    let cli = Cli::parse();

    let config = SimConfig {
        capital: cli.capital,
        default_split_size: cli.split_size,
        min_latency_ms: cli.min_latency_ms,
        max_latency_ms: cli.max_latency_ms,
        hold_cooldown_ms: cli.hold_cooldown_ms,
        order_size: cli.order_size,
        fill_model: FillModel::from_str(&cli.fill_model),
        seed: cli.seed,
    };

    let data_dir = &cli.data_dir;
    if !data_dir.is_dir() {
        eprintln!("Data directory not found: {}", data_dir.display());
        std::process::exit(1);
    }

    let epochs = match &cli.epoch {
        Some(ep) => ep.clone(),
        None => discover_markets(data_dir),
    };

    if epochs.is_empty() {
        eprintln!("No markets found.");
        return;
    }

    eprintln!("Found {} markets in {}", epochs.len(), data_dir.display());

    // Determine which strategies to run
    let strat_names: Vec<&str> = cli.strategies.split(',').map(|s| s.trim()).collect();
    let run_monkeys = strat_names.contains(&"monkeys");
    let run_fv = strat_names.contains(&"fv");
    let run_mm = strat_names.contains(&"mm");
    let run_trend = strat_names.contains(&"trend");
    let run_meanrev = strat_names.contains(&"meanrev");

    // Generate monkey DNA configs upfront (deterministic from seed)
    let monkey_configs: Vec<_> = if run_monkeys {
        eprintln!("Generating {} combinatorial monkeys...", cli.monkey_count);
        generate_monkeys(cli.monkey_count, cli.seed)
            .into_iter()
            .map(|m| m)
            .collect()
    } else {
        Vec::new()
    };

    let n_strategies = monkey_configs.len()
        + if run_fv { 2 } else { 0 }  // analytical + lstm
        + if run_mm { 1 } else { 0 }
        + if run_trend { 1 } else { 0 }
        + if run_meanrev { 1 } else { 0 };

    eprintln!("Running {} strategies across {} markets ({} total sims)",
        n_strategies, epochs.len(), n_strategies * epochs.len());

    let start = Instant::now();
    let counter = AtomicUsize::new(0);
    let total = epochs.len();

    let all_results: Vec<Vec<SimResult>> = epochs.par_iter().map(|&epoch| {
        let market = match load_market(data_dir, epoch) {
            Ok(m) => m,
            Err(e) => {
                let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
                if done % 50 == 0 { eprintln!("  [{done}/{total}] (skipped {epoch}: {e})"); }
                return Vec::new();
            }
        };

        let mut results = Vec::new();

        // Run monkeys
        if run_monkeys {
            let monkey_strats = generate_monkeys(cli.monkey_count, cli.seed);
            for mut monkey in monkey_strats {
                results.push(simulate_market(&market, &mut monkey, &config));
            }
        }

        // Run intelligent strategies
        if run_fv {
            let mut fv_a = FairValueStrategy::new(cli.fv_edge, cli.order_size, cli.split_size, false);
            results.push(simulate_market(&market, &mut fv_a, &config));

            let mut fv_l = FairValueStrategy::new(cli.fv_edge, cli.order_size, cli.split_size, true);
            results.push(simulate_market(&market, &mut fv_l, &config));
        }
        if run_mm {
            let mut mm = MarketMakerStrategy::new(cli.mm_spread, cli.order_size, cli.split_size);
            results.push(simulate_market(&market, &mut mm, &config));
        }
        if run_trend {
            let mut tr = TrendStrategy::new(cli.order_size, cli.split_size);
            results.push(simulate_market(&market, &mut tr, &config));
        }
        if run_meanrev {
            let mut mr = MeanRevertStrategy::new(cli.order_size, cli.split_size);
            results.push(simulate_market(&market, &mut mr, &config));
        }

        let done = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if done % 20 == 0 || done == total {
            let elapsed = start.elapsed().as_secs_f64();
            let rate = elapsed / done as f64;
            let eta = rate * (total - done) as f64;
            eprintln!("  [{done}/{total}] {epoch} ({:.1}s/mkt, ETA {:.0}s)", rate, eta);
        }

        results
    }).collect();

    let flat_results: Vec<SimResult> = all_results.into_iter().flatten().collect();
    let elapsed = start.elapsed().as_secs_f64();

    eprintln!("\nCompleted {} simulations in {:.1}s ({:.0} sims/sec)",
        flat_results.len(), elapsed, flat_results.len() as f64 / elapsed);

    match cli.output.as_str() {
        "csv" => print_csv(&flat_results),
        _ => print_summary(&flat_results, cli.top_n),
    }
}
