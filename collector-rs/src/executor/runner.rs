use std::time::Duration;

use serde_json::json;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::auth;
use super::clob;
use super::config::AppConfig;
use super::state::ExecutorState;
use super::strategy::{NoopStrategy, Strategy};
use crate::types::{FairValueSnapshot, MarketInfo, PolyBbaSnapshot, make_line, make_system_line};
use crate::writer::{SeqCounter, WriterHandle};

const STRATEGY_TICK_MS: u64 = 500;
const MARKET_INFO_TIMEOUT_SECS: u64 = 30;

pub async fn run_executor(
    market_start: i64,
    mut market_info_rx: watch::Receiver<Option<MarketInfo>>,
    poly_bba_rx: watch::Receiver<Option<PolyBbaSnapshot>>,
    fv_rx: watch::Receiver<Option<FairValueSnapshot>>,
    writer: WriterHandle,
    cancel: CancellationToken,
    config: AppConfig,
) {
    let seq = SeqCounter::new();

    let _ = writer.send(make_system_line(
        market_start,
        "executor",
        seq.next(),
        json!({"dry_run": config.dry_run, "wallet": format!("{}", config.wallet_address)}),
        "executor_started",
    ));

    // Wait for market info from polymarket collector
    let market_info = tokio::select! {
        _ = cancel.cancelled() => {
            tracing::info!("Executor cancelled before market info received");
            return;
        }
        result = wait_for_market_info(&mut market_info_rx) => {
            match result {
                Some(info) => info,
                None => {
                    tracing::warn!("Executor timed out waiting for market info");
                    let _ = writer.send(make_system_line(
                        market_start,
                        "executor",
                        seq.next(),
                        json!({"reason": "market_info_timeout"}),
                        "executor_stopped",
                    ));
                    return;
                }
            }
        }
    };

    tracing::info!(
        condition_id = %market_info.condition_id,
        tokens = market_info.clob_token_ids.len(),
        "Executor received market info"
    );

    // Create CLOB client for order placement
    let signer = match auth::create_signer(&config) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "Failed to create signer");
            return;
        }
    };

    let clob_client = match auth::create_authenticated_client(&config, &signer).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "Failed to authenticate CLOB client");
            return;
        }
    };

    let _ = writer.send(make_system_line(
        market_start,
        "executor",
        seq.next(),
        json!({"condition_id": market_info.condition_id}),
        "executor_active",
    ));

    // Strategy loop
    let mut state = ExecutorState {
        market_info: Some(market_info),
        ..Default::default()
    };

    let mut strategy: Box<dyn Strategy> = Box::new(NoopStrategy);
    let mut interval = tokio::time::interval(Duration::from_millis(STRATEGY_TICK_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = cancel.cancelled() => break,
        }

        // Drain latest data from channels
        if let Some(bba) = poly_bba_rx.borrow().clone() {
            state.last_bba_update = Some(tokio::time::Instant::now());
            state.poly_bba = Some(bba);
        }
        if let Some(fv) = fv_rx.borrow().clone() {
            state.fair_value = Some(fv);
        }

        let actions = strategy.on_tick(&state, &config);

        for action in actions {
            let _ = writer.send(make_line(
                market_start,
                "executor",
                "strategy_action",
                seq.next(),
                json!({"action": format!("{action:?}")}),
                json!({"action": format!("{action:?}")}),
            ));
        }
    }

    // Graceful shutdown: cancel open orders
    tracing::info!("Executor shutting down, cancelling open orders");
    if !config.dry_run {
        if let Err(e) = clob::cancel_all(&clob_client).await {
            tracing::warn!(error = %e, "Failed to cancel orders on shutdown");
        }
    }

    let _ = writer.send(make_system_line(
        market_start,
        "executor",
        seq.next(),
        json!({"reason": "market_ended"}),
        "executor_stopped",
    ));
}

async fn wait_for_market_info(
    rx: &mut watch::Receiver<Option<MarketInfo>>,
) -> Option<MarketInfo> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(MARKET_INFO_TIMEOUT_SECS);

    loop {
        if let Some(info) = rx.borrow().clone() {
            return Some(info);
        }

        tokio::select! {
            result = rx.changed() => {
                if result.is_err() {
                    return None;
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                return None;
            }
        }
    }
}
