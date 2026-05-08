use alloy::primitives::{Bytes, B256, U256};
use alloy::sol_types::SolCall;
use anyhow::Result;
use chrono::Utc;
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing_subscriber::EnvFilter;

use bookview_collector::executor;
use executor::ctf_ops::{self, CTF_ADAPTER, PUSD, USDC_E, IERC1155, IERC20, IConditionalTokens};

#[derive(Parser)]
#[command(name = "executor-cli", about = "Polymarket executor CLI utilities")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch and print deposit addresses for the configured wallet
    DepositAddresses,
    /// Discover the current active 5-minute BTC market
    DiscoverBtc5m,
    /// Place a test order (dry-run by default)
    TestOrder,
    /// Split USDC into YES+NO tokens for the current BTC 5-min market
    Split {
        #[arg(long, default_value = "5")]
        amount: u64,
    },
    /// Merge YES+NO tokens back into USDC for the current BTC 5-min market
    Merge {
        #[arg(long, default_value = "5")]
        amount: u64,
    },
    /// Redeem resolved positions back into USDC
    Redeem {
        #[arg(long)]
        condition_id: String,
    },
    /// Approve Exchange contracts to transfer CTF tokens (one-time setup)
    ApproveExchange,
    /// Integration test: split, FAK at live YES ask, FOK at live NO ask, Post-Only batch
    TestFlow,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("executor=info".parse()?))
        .init();

    dotenvy::dotenv().ok();

    let cli = Cli::parse();
    let config = executor::config::AppConfig::from_env()?;

    let http = reqwest::Client::builder()
        .tcp_nodelay(true)
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .build()?;

    match cli.command {
        Commands::DepositAddresses => cmd_deposit_addresses(&config).await,
        Commands::DiscoverBtc5m => cmd_discover_btc_5m(&http, &config).await,
        Commands::TestOrder => cmd_test_order(&http, &config).await,
        Commands::Split { amount } => cmd_split(&http, &config, amount).await,
        Commands::Merge { amount } => cmd_merge(&http, &config, amount).await,
        Commands::Redeem { condition_id } => cmd_redeem(&http, &config, &condition_id).await,
        Commands::ApproveExchange => cmd_approve_exchange(&http, &config).await,
        Commands::TestFlow => cmd_test_flow(&http, &config).await,
    }
}

// ── Deposit Addresses ────────────────────────────────────────────────

async fn cmd_deposit_addresses(config: &executor::config::AppConfig) -> Result<()> {
    let addrs =
        executor::bridge::fetch_deposit_addresses(&config.bridge_host, config.wallet_address)
            .await?;
    println!("Deposit addresses for {}:", config.wallet_address);
    if let Some(evm) = &addrs.evm {
        println!("  EVM:  {evm}");
    }
    if let Some(svm) = &addrs.svm {
        println!("  SVM:  {svm}");
    }
    if let Some(btc) = &addrs.btc {
        println!("  BTC:  {btc}");
    }
    Ok(())
}

// ── Discover ─────────────────────────────────────────────────────────

async fn cmd_discover_btc_5m(
    http: &reqwest::Client,
    config: &executor::config::AppConfig,
) -> Result<()> {
    let market = executor::gamma::discover_active_btc_5m(http, &config.gamma_host).await?;
    match market {
        Some(m) => {
            println!("Active 5-min BTC market found:");
            println!("  Question:     {}", m.question);
            println!("  Condition ID: {}", m.condition_id);
            println!("  Slug:         {}", m.slug);
            println!("  Outcomes:     {:?}", m.outcomes);
            println!("  Token IDs:    {:?}", m.clob_token_ids);
            println!("  End time:     {}", m.end_date);
        }
        None => println!("No active 5-minute BTC market found."),
    }
    Ok(())
}

// ── Test Order ───────────────────────────────────────────────────────

async fn cmd_test_order(
    http: &reqwest::Client,
    config: &executor::config::AppConfig,
) -> Result<()> {
    let market = executor::gamma::discover_active_btc_5m(http, &config.gamma_host)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No active 5-minute BTC market found"))?;

    tracing::info!(
        question = %market.question,
        condition_id = %market.condition_id,
        "Found market for test order"
    );

    if config.dry_run {
        tracing::info!("DRY_RUN=true — would place test order but not submitting");
        return Ok(());
    }

    if !config.enable_test_order {
        anyhow::bail!("ENABLE_TEST_ORDER must be true to place real orders");
    }

    let signer = executor::auth::create_signer(config)?;
    let client = executor::auth::create_authenticated_client(config, &signer).await?;

    let yes_token = market
        .clob_token_ids
        .first()
        .ok_or_else(|| anyhow::anyhow!("No YES token ID found"))?;

    let order_resp = executor::clob::place_limit_order(
        &client,
        &signer,
        executor::clob::LimitOrderParams {
            token_id: yes_token.clone(),
            side: executor::clob::Side::Buy,
            price: dec!(0.10),
            size: dec!(5.0),
        },
    )
    .await?;

    tracing::info!(order_id = %order_resp.order_id, "Test order placed, cancelling...");
    executor::clob::cancel_order(&client, &order_resp.order_id).await?;
    tracing::info!("Test order cancelled successfully");
    Ok(())
}

// ── Split ────────────────────────────────────────────────────────────

async fn cmd_split(
    http: &reqwest::Client,
    config: &executor::config::AppConfig,
    amount: u64,
) -> Result<()> {
    if config.dry_run {
        tracing::info!(amount, "DRY_RUN=true — would split but not submitting");
        return Ok(());
    }

    let market = executor::gamma::discover_active_btc_5m(http, &config.gamma_host)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No active 5-minute BTC market found"))?;

    tracing::info!(
        question = %market.question,
        condition_id = %market.condition_id,
        "Splitting on market"
    );

    let tx = do_split(http, config, &market, amount).await?;
    println!("Split complete:");
    println!("  Amount:  {} USDC → {} YES + {} NO", amount, amount, amount);
    println!("  Tx:      {}", tx);

    tracing::info!("Registering position with CLOB...");
    let signer = executor::auth::create_signer(config)?;
    let clob_client = executor::auth::create_authenticated_client(config, &signer).await?;
    let _ = do_register_positions(&clob_client, &signer, &market).await;

    Ok(())
}

// ── Merge ────────────────────────────────────────────────────────────

async fn cmd_merge(
    http: &reqwest::Client,
    config: &executor::config::AppConfig,
    amount: u64,
) -> Result<()> {
    if config.dry_run {
        tracing::info!(amount, "DRY_RUN=true — would merge but not submitting");
        return Ok(());
    }

    let market = executor::gamma::discover_active_btc_5m(http, &config.gamma_host)
        .await?
        .ok_or_else(|| anyhow::anyhow!("No active 5-minute BTC market found"))?;

    let signer = executor::auth::create_signer(config)?;
    let relayer = executor::relayer::RelayerClient::new(config, signer, http.clone())?;

    let contract_cfg = polymarket_client_sdk_v2::contract_config(config.chain_id, false)
        .ok_or_else(|| anyhow::anyhow!("No contract config for chain {}", config.chain_id))?;

    let condition_id: B256 = market
        .condition_id
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid condition ID hex"))?;

    let ctf_address = contract_cfg.conditional_tokens;
    let amount_units = U256::from(amount) * U256::from(1_000_000u64);

    let approve_ctf = IERC1155::setApprovalForAllCall {
        operator: CTF_ADAPTER,
        approved: true,
    }
    .abi_encode();

    let merge_data = IConditionalTokens::mergePositionsCall {
        collateralToken: USDC_E,
        parentCollectionId: B256::ZERO,
        conditionId: condition_id,
        partition: vec![U256::from(1), U256::from(2)],
        amount: amount_units,
    }
    .abi_encode();

    let calls = vec![
        executor::relayer::ProxyCall {
            typeCode: 1,
            to: ctf_address,
            value: U256::ZERO,
            data: Bytes::from(approve_ctf),
        },
        executor::relayer::ProxyCall {
            typeCode: 1,
            to: CTF_ADAPTER,
            value: U256::ZERO,
            data: Bytes::from(merge_data),
        },
    ];

    let result = relayer.execute_and_wait(calls, "merge positions").await?;
    let tx_hash = result.transaction_hash.as_deref().unwrap_or("pending");

    println!("Merge complete:");
    println!(
        "  Amount:  {} YES + {} NO → {} USDC",
        amount, amount, amount
    );
    println!("  Tx:      {}", tx_hash);
    Ok(())
}

// ── Redeem ───────────────────────────────────────────────────────────

async fn cmd_redeem(
    http: &reqwest::Client,
    config: &executor::config::AppConfig,
    condition_id_hex: &str,
) -> Result<()> {
    let signer = executor::auth::create_signer(config)?;
    let relayer = executor::relayer::RelayerClient::new(config, signer, http.clone())?;

    let contract_cfg = polymarket_client_sdk_v2::contract_config(config.chain_id, false)
        .ok_or_else(|| anyhow::anyhow!("No contract config for chain {}", config.chain_id))?;

    let condition_id: B256 = condition_id_hex
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid condition ID hex"))?;

    let ctf_address = contract_cfg.conditional_tokens;

    let approve_ctf = IERC1155::setApprovalForAllCall {
        operator: CTF_ADAPTER,
        approved: true,
    }
    .abi_encode();

    let redeem_data = IConditionalTokens::redeemPositionsCall {
        collateralToken: USDC_E,
        parentCollectionId: B256::ZERO,
        conditionId: condition_id,
        indexSets: vec![U256::from(1), U256::from(2)],
    }
    .abi_encode();

    let calls = vec![
        executor::relayer::ProxyCall {
            typeCode: 1,
            to: ctf_address,
            value: U256::ZERO,
            data: Bytes::from(approve_ctf),
        },
        executor::relayer::ProxyCall {
            typeCode: 1,
            to: CTF_ADAPTER,
            value: U256::ZERO,
            data: Bytes::from(redeem_data),
        },
    ];

    let result = relayer.execute_and_wait(calls, "redeem positions").await?;
    let tx_hash = result.transaction_hash.as_deref().unwrap_or("pending");

    println!("Redeem complete:");
    println!("  Condition: {}", condition_id);
    println!("  Tx:        {}", tx_hash);
    Ok(())
}

// ── Approve Exchange ─────────────────────────────────────────────────

async fn cmd_approve_exchange(
    http: &reqwest::Client,
    config: &executor::config::AppConfig,
) -> Result<()> {
    let signer = executor::auth::create_signer(config)?;
    let relayer = executor::relayer::RelayerClient::new(config, signer, http.clone())?;

    let cfg = polymarket_client_sdk_v2::contract_config(config.chain_id, false)
        .ok_or_else(|| anyhow::anyhow!("No contract config for chain {}", config.chain_id))?;
    let neg_cfg = polymarket_client_sdk_v2::contract_config(config.chain_id, true)
        .ok_or_else(|| {
            anyhow::anyhow!("No neg-risk contract config for chain {}", config.chain_id)
        })?;

    let ctf = cfg.conditional_tokens;
    let exchange_v2 = cfg
        .exchange_v2
        .ok_or_else(|| anyhow::anyhow!("No exchange_v2 address"))?;
    let neg_risk_exchange_v2 = neg_cfg
        .exchange_v2
        .ok_or_else(|| anyhow::anyhow!("No neg-risk exchange_v2 address"))?;

    let approve_pusd_adapter = IERC20::approveCall {
        spender: CTF_ADAPTER,
        amount: U256::MAX,
    }
    .abi_encode();

    let approve_ctf_adapter = IERC1155::setApprovalForAllCall {
        operator: CTF_ADAPTER,
        approved: true,
    }
    .abi_encode();

    let approve_exchange = IERC1155::setApprovalForAllCall {
        operator: exchange_v2,
        approved: true,
    }
    .abi_encode();

    let approve_neg_risk = IERC1155::setApprovalForAllCall {
        operator: neg_risk_exchange_v2,
        approved: true,
    }
    .abi_encode();

    let calls = vec![
        executor::relayer::ProxyCall {
            typeCode: 1,
            to: PUSD,
            value: U256::ZERO,
            data: Bytes::from(approve_pusd_adapter),
        },
        executor::relayer::ProxyCall {
            typeCode: 1,
            to: ctf,
            value: U256::ZERO,
            data: Bytes::from(approve_ctf_adapter),
        },
        executor::relayer::ProxyCall {
            typeCode: 1,
            to: ctf,
            value: U256::ZERO,
            data: Bytes::from(approve_exchange),
        },
        executor::relayer::ProxyCall {
            typeCode: 1,
            to: ctf,
            value: U256::ZERO,
            data: Bytes::from(approve_neg_risk),
        },
    ];

    let result = relayer.execute_and_wait(calls, "approve exchanges").await?;
    let tx_hash = result.transaction_hash.as_deref().unwrap_or("pending");

    println!("Approvals complete:");
    println!("  pUSD → Adapter:            approved ({})", CTF_ADAPTER);
    println!("  CTF → Adapter:             approved ({})", CTF_ADAPTER);
    println!("  CTF → Exchange V2:         approved ({})", exchange_v2);
    println!(
        "  CTF → NegRisk Exchange V2: approved ({})",
        neg_risk_exchange_v2
    );
    println!("  Tx: {}", tx_hash);
    Ok(())
}

// ── Test Flow ────────────────────────────────────────────────────────

struct StepResult {
    name: &'static str,
    success: bool,
    detail: String,
}

impl StepResult {
    fn ok(name: &'static str, detail: String) -> Self {
        Self {
            name,
            success: true,
            detail,
        }
    }
    fn fail(name: &'static str, detail: String) -> Self {
        Self {
            name,
            success: false,
            detail,
        }
    }
}

impl std::fmt::Display for StepResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = if self.success {
            "COMPLETED"
        } else {
            "NOT COMPLETED"
        };
        write!(f, "[{tag}] {}: {}", self.name, self.detail)
    }
}

fn print_summary(results: &[StepResult]) {
    let passed = results.iter().filter(|r| r.success).count();
    let failed = results.iter().filter(|r| !r.success).count();

    println!("\n============================================================");
    println!("  TEST FLOW SUMMARY — {passed} completed, {failed} not completed");
    println!("============================================================");
    for r in results {
        println!("  {r}");
    }
    println!();
}

async fn wait_for_fresh_market(
    http: &reqwest::Client,
    gamma_host: &str,
) -> Result<executor::gamma::BtcFiveMinMarket> {
    const MIN_REMAINING_SECS: i64 = 240;

    if let Some(current) = executor::gamma::discover_active_btc_5m(http, gamma_host).await? {
        let remaining = (current.end_date - Utc::now()).num_seconds();
        if remaining >= MIN_REMAINING_SECS {
            println!(
                "Current market '{}' has {}s remaining (>= {}s) — using it",
                current.question, remaining, MIN_REMAINING_SECS
            );
            return Ok(current);
        }
        if remaining > 0 {
            println!(
                "Current market has {}s remaining (< {}s) — waiting for next window...",
                remaining, MIN_REMAINING_SECS
            );
            let wait = remaining as u64 + 10;
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
        }
    }

    for attempt in 1..=60 {
        match executor::gamma::discover_active_btc_5m(http, gamma_host).await? {
            Some(m) => {
                let remaining = (m.end_date - Utc::now()).num_seconds();
                println!(
                    "Found fresh market: '{}' ({}s remaining)",
                    m.question, remaining
                );
                return Ok(m);
            }
            None => {
                if attempt % 6 == 1 {
                    println!("Waiting for new market (attempt {}/60)...", attempt);
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
    anyhow::bail!("Timeout waiting for fresh market (5 minutes)")
}

fn best_ask_price(book: &executor::clob::OrderBookSummary) -> Option<Decimal> {
    book.asks.iter().map(|l| l.price).min()
}

async fn cmd_test_flow(
    http: &reqwest::Client,
    config: &executor::config::AppConfig,
) -> Result<()> {
    use executor::clob::OrderType;

    if config.dry_run {
        anyhow::bail!("test-flow requires DRY_RUN=false (set in .env)");
    }

    let mut results: Vec<StepResult> = Vec::new();

    // ── Step 1: Market Discovery ─────────────────────────────────────
    println!("\n============================================================");
    println!("  STEP 1: Market Discovery");
    println!("============================================================");

    let market = wait_for_fresh_market(http, &config.gamma_host).await?;
    println!("  Market:       {}", market.question);
    println!("  Condition ID: {}", market.condition_id);
    println!(
        "  YES token:    {}",
        market.clob_token_ids.first().unwrap_or(&"?".into())
    );
    println!(
        "  NO token:     {}",
        market.clob_token_ids.get(1).unwrap_or(&"?".into())
    );

    let yes_token = market
        .clob_token_ids
        .first()
        .ok_or_else(|| anyhow::anyhow!("no YES token ID"))?;
    let no_token = market
        .clob_token_ids
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("no NO token ID"))?;

    // ── Step 2: Split $5 ─────────────────────────────────────────────
    println!("\n============================================================");
    println!("  STEP 2: Split $5 pUSD → YES + NO tokens");
    println!("============================================================");

    let split_result = do_split(http, config, &market, 5).await;
    match &split_result {
        Ok(tx) => {
            let r = StepResult::ok("Split $5", format!("tx={tx}"));
            println!("  {r}");
            results.push(r);
        }
        Err(e) => {
            let r = StepResult::fail("Split $5", format!("{e:#}"));
            println!("  {r}");
            results.push(r);
            print_summary(&results);
            return Err(anyhow::anyhow!("Split failed, cannot continue: {e}"));
        }
    }

    // ── Step 3: Register positions with CLOB ─────────────────────────
    println!("\n============================================================");
    println!("  STEP 3: Register positions with CLOB");
    println!("============================================================");

    let signer = executor::auth::create_signer(config)?;
    let clob_client = executor::auth::create_authenticated_client(config, &signer).await?;

    let reg_result = do_register_positions(&clob_client, &signer, &market).await;
    match &reg_result {
        Ok(()) => {
            let r = StepResult::ok("CLOB Registration", "position registered".into());
            println!("  {r}");
            results.push(r);
        }
        Err(e) => {
            let r = StepResult::fail("CLOB Registration", format!("{e:#}"));
            println!("  {r}");
            results.push(r);
        }
    }

    // ── Fetch live prices ────────────────────────────────────────────
    println!("\n============================================================");
    println!("  Fetching live orderbook prices...");
    println!("============================================================");

    let yes_book = executor::clob::fetch_orderbook(&clob_client, yes_token).await?;
    let no_book = executor::clob::fetch_orderbook(&clob_client, no_token).await?;

    let yes_ask = best_ask_price(&yes_book).unwrap_or(dec!(0.50));
    let no_ask = best_ask_price(&no_book).unwrap_or(dec!(0.50));

    println!("  YES best ask: {}", yes_ask);
    println!("  NO  best ask: {}", no_ask);

    // ── Step 4: FAK Buy YES at live ask ──────────────────────────────
    println!("\n============================================================");
    println!(
        "  STEP 4: FAK Buy YES — 5 shares @ {} (live ask)",
        yes_ask
    );
    println!("============================================================");

    let fak_result = executor::clob::place_typed_order(
        &clob_client,
        &signer,
        yes_token,
        executor::clob::Side::Buy,
        yes_ask,
        dec!(5.0),
        OrderType::FAK,
    )
    .await;
    match &fak_result {
        Ok(resp) => {
            let r = StepResult::ok(
                "FAK Buy YES",
                format!("order_id={}, status={}", resp.order_id, resp.status),
            );
            println!("  {r}");
            results.push(r);
        }
        Err(e) => {
            let r = StepResult::fail("FAK Buy YES", format!("{e:#}"));
            println!("  {r}");
            results.push(r);
        }
    }

    // ── Step 5: FOK Buy NO at live ask ───────────────────────────────
    println!("\n============================================================");
    println!("  STEP 5: FOK Buy NO — 5 shares @ {} (live ask)", no_ask);
    println!("============================================================");

    let fok_result = executor::clob::place_typed_order(
        &clob_client,
        &signer,
        no_token,
        executor::clob::Side::Buy,
        no_ask,
        dec!(5.0),
        OrderType::FOK,
    )
    .await;
    match &fok_result {
        Ok(resp) => {
            let r = StepResult::ok(
                "FOK Buy NO",
                format!("order_id={}, status={}", resp.order_id, resp.status),
            );
            println!("  {r}");
            results.push(r);
        }
        Err(e) => {
            let r = StepResult::fail("FOK Buy NO", format!("{e:#}"));
            println!("  {r}");
            results.push(r);
        }
    }

    // ── Step 6: Batch Post-Only ──────────────────────────────────────
    println!("\n============================================================");
    println!("  STEP 6: Batch Post-Only Buy — both tokens @ $0.20, 5 shares");
    println!("============================================================");

    let batch_result =
        executor::clob::batch_post_only(&clob_client, &signer, yes_token, no_token, dec!(0.20), dec!(5.0))
            .await;
    match &batch_result {
        Ok(resps) => {
            for (i, resp) in resps.iter().enumerate() {
                let r = StepResult::ok(
                    if i == 0 {
                        "Post-Only Buy YES"
                    } else {
                        "Post-Only Buy NO"
                    },
                    format!("order_id={}, status={}", resp.order_id, resp.status),
                );
                println!("  {r}");
                results.push(r);
            }
        }
        Err(e) => {
            let r = StepResult::fail("Batch Post-Only", format!("{e:#}"));
            println!("  {r}");
            results.push(r);
        }
    }

    // ── Summary ──────────────────────────────────────────────────────
    print_summary(&results);
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────

async fn do_split(
    http: &reqwest::Client,
    config: &executor::config::AppConfig,
    market: &executor::gamma::BtcFiveMinMarket,
    amount: u64,
) -> Result<String> {
    ctf_ops::split_position(http, config, &market.condition_id, amount).await
}

async fn do_register_positions(
    clob_client: &executor::auth::AuthenticatedClobClient,
    signer: &alloy::signers::local::PrivateKeySigner,
    market: &executor::gamma::BtcFiveMinMarket,
) -> Result<()> {
    ctf_ops::register_positions(clob_client, signer, &market.clob_token_ids).await
}
