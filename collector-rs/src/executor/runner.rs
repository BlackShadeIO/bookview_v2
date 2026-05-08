use std::time::{Duration, Instant};

use rust_decimal::Decimal;
use serde_json::json;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::auth;
use super::clob::{self, OrderStatusType, Side};
use super::config::AppConfig;
use super::ctf_ops;
use super::fv_convergence::FvConvergenceStrategy;
use super::state::ExecutorState;
use super::strategy::{
    Action, ActionResult, FillNotification, NoopStrategy, OrderSide, Strategy, TokenSide,
};
use crate::config::DATA_DIR;
use crate::types::{FairValueSnapshot, MarketInfo, PolyBbaSnapshot, make_line, make_system_line};
use crate::writer::{SeqCounter, WriterHandle};

const STRATEGY_TICK_MS: u64 = 500;
const MARKET_INFO_TIMEOUT_SECS: u64 = 30;
const FILL_CHECK_INTERVAL: u64 = 2; // check fills every N ticks
const TOGGLE_CHECK_INTERVAL: u64 = 4; // check toggle file every N ticks (~2s)

pub async fn run_executor(
    market_start: i64,
    mut market_info_rx: watch::Receiver<Option<MarketInfo>>,
    poly_bba_rx: watch::Receiver<Option<PolyBbaSnapshot>>,
    fv_rx: watch::Receiver<Option<FairValueSnapshot>>,
    writer: WriterHandle,
    cancel: CancellationToken,
    config: AppConfig,
    http: reqwest::Client,
) {
    let seq = SeqCounter::new();

    let _ = writer.send(make_system_line(
        market_start,
        "executor",
        seq.next(),
        json!({
            "dry_run": config.dry_run,
            "wallet": format!("{}", config.wallet_address),
            "strategy": &config.strategy_name,
        }),
        "executor_started",
    ));

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
        json!({
            "condition_id": market_info.condition_id,
            "strategy": &config.strategy_name,
        }),
        "executor_active",
    ));

    let mut state = ExecutorState {
        market_info: Some(market_info.clone()),
        ..Default::default()
    };

    let mut strategy: Box<dyn Strategy> = match config.strategy_name.as_str() {
        "fv_convergence" => {
            tracing::info!("Using FvConvergence strategy");
            Box::new(FvConvergenceStrategy::new(market_start, &config))
        }
        _ => {
            tracing::info!("Using Noop strategy");
            Box::new(NoopStrategy)
        }
    };

    let mut interval = tokio::time::interval(Duration::from_millis(STRATEGY_TICK_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut tick_count: u64 = 0;
    let mut tracked_buy_order: Option<TrackedOrder> = None;
    let mut tracked_sell_order: Option<TrackedOrder> = None;

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = cancel.cancelled() => break,
        }

        tick_count += 1;

        // Check toggle file — immediate stop when removed
        if tick_count % TOGGLE_CHECK_INTERVAL == 0 && !DATA_DIR.join(".executor-enabled").exists() {
            tracing::info!("Toggle file removed — executor stopping immediately");
            let _ = writer.send(make_system_line(
                market_start,
                "executor",
                seq.next(),
                json!({"reason": "toggle_disabled"}),
                "executor_stopped",
            ));
            break;
        }

        // Drain latest data from channels
        if let Some(bba) = poly_bba_rx.borrow().clone() {
            state.last_bba_update = Some(tokio::time::Instant::now());
            state.poly_bba = Some(bba);
        }
        if let Some(fv) = fv_rx.borrow().clone() {
            state.fair_value = Some(fv);
        }

        // Fill detection — poll tracked orders
        if tick_count % FILL_CHECK_INTERVAL == 0 {
            check_order_fill(
                &clob_client,
                &mut tracked_buy_order,
                &market_info,
                strategy.as_mut(),
                &mut state,
                &writer,
                market_start,
                &seq,
                &config,
            )
            .await;
            check_order_fill(
                &clob_client,
                &mut tracked_sell_order,
                &market_info,
                strategy.as_mut(),
                &mut state,
                &writer,
                market_start,
                &seq,
                &config,
            )
            .await;
        }

        let actions = strategy.on_tick(&state, &config);

        for action in actions {
            let t0 = Instant::now();
            let result = execute_action(
                &action,
                &clob_client,
                &signer,
                &http,
                &market_info,
                &state,
                &config,
            )
            .await;
            let latency_ms = t0.elapsed().as_millis() as u64;

            let _ = writer.send(make_line(
                market_start,
                "executor",
                "strategy_action",
                seq.next(),
                json!({
                    "action": format!("{action:?}"),
                    "result": format!("{result:?}"),
                    "latency_ms": latency_ms,
                }),
                action_log(&action, &result, latency_ms),
            ));

            // Track orders for fill detection
            match (&action, &result) {
                (
                    Action::PlacePostOnlyBuyYes { price, size }
                    | Action::PlacePostOnlyBuyNo { price, size },
                    ActionResult::OrderPlaced { order_id },
                ) => {
                    let token_side = match &action {
                        Action::PlacePostOnlyBuyYes { .. } => TokenSide::Yes,
                        _ => TokenSide::No,
                    };
                    tracked_buy_order = Some(TrackedOrder {
                        order_id: order_id.clone(),
                        token_side,
                        order_side: OrderSide::Buy,
                        price: *price,
                        size: *size,
                    });
                }
                (
                    Action::PlacePostOnlySellYes { price, size }
                    | Action::PlacePostOnlySellNo { price, size },
                    ActionResult::OrderPlaced { order_id },
                ) => {
                    let token_side = match &action {
                        Action::PlacePostOnlySellYes { .. } => TokenSide::Yes,
                        _ => TokenSide::No,
                    };
                    tracked_sell_order = Some(TrackedOrder {
                        order_id: order_id.clone(),
                        token_side,
                        order_side: OrderSide::Sell,
                        price: *price,
                        size: *size,
                    });
                }
                _ => {}
            }

            strategy.on_action_result(&action, &result);
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

#[derive(Debug, Clone)]
struct TrackedOrder {
    order_id: String,
    token_side: TokenSide,
    order_side: OrderSide,
    price: f64,
    size: f64,
}

async fn check_order_fill(
    clob_client: &auth::AuthenticatedClobClient,
    tracked: &mut Option<TrackedOrder>,
    _market_info: &MarketInfo,
    strategy: &mut dyn Strategy,
    state: &mut ExecutorState,
    writer: &WriterHandle,
    market_start: i64,
    seq: &SeqCounter,
    config: &AppConfig,
) {
    let order = match tracked.as_ref() {
        Some(o) => o.clone(),
        None => return,
    };

    if order.order_id == "dry-run" {
        // Auto-fill dry-run orders on the next check
        let fill = FillNotification {
            order_id: order.order_id.clone(),
            token_side: order.token_side,
            order_side: order.order_side,
            price: order.price,
            size: order.size,
        };

        let _ = writer.send(make_line(
            market_start,
            "executor",
            "dry_run_fill",
            seq.next(),
            json!({"order_id": "dry-run", "side": format!("{:?}", order.order_side)}),
            json!({"order_id": "dry-run", "side": format!("{:?}", order.order_side)}),
        ));

        update_position(state, &fill);
        strategy.on_fill(&fill);
        *tracked = None;
        return;
    }

    if config.dry_run {
        return;
    }

    let t0 = Instant::now();
    let status_result = clob::get_order_status(clob_client, &order.order_id).await;
    let poll_latency_ms = t0.elapsed().as_millis() as u64;

    match status_result {
        Ok(status) => match status.status {
            OrderStatusType::Matched => {
                let fill_price = status.price.try_into().unwrap_or(order.price);
                let fill_size: f64 = status
                    .size_matched
                    .try_into()
                    .unwrap_or(order.size);

                let fill = FillNotification {
                    order_id: order.order_id.clone(),
                    token_side: order.token_side,
                    order_side: order.order_side,
                    price: fill_price,
                    size: fill_size,
                };

                let _ = writer.send(make_line(
                    market_start,
                    "executor",
                    "order_fill",
                    seq.next(),
                    json!({
                        "order_id": order.order_id,
                        "token_side": format!("{:?}", order.token_side),
                        "order_side": format!("{:?}", order.order_side),
                        "fill_price": fill_price,
                        "fill_size": fill_size,
                        "poll_latency_ms": poll_latency_ms,
                    }),
                    json!({
                        "order_id": order.order_id,
                        "fill_price": fill_price,
                        "fill_size": fill_size,
                        "poll_latency_ms": poll_latency_ms,
                    }),
                ));

                update_position(state, &fill);
                strategy.on_fill(&fill);
                *tracked = None;
            }
            OrderStatusType::Canceled => {
                tracing::info!(order_id = %order.order_id, "Tracked order was cancelled");
                *tracked = None;
            }
            OrderStatusType::Live => {
                // Still open — check for partial fill
                let matched: f64 = status.size_matched.try_into().unwrap_or(0.0);
                if matched > 0.0 {
                    tracing::info!(
                        order_id = %order.order_id,
                        matched,
                        "Partial fill detected"
                    );
                }
            }
            _ => {}
        },
        Err(e) => {
            tracing::debug!(order_id = %order.order_id, error = %e, "Failed to check order status");
        }
    }
}

fn update_position(state: &mut ExecutorState, fill: &FillNotification) {
    let size = Decimal::try_from(fill.size).unwrap_or_default();
    match (fill.token_side, fill.order_side) {
        (TokenSide::Yes, OrderSide::Buy) => state.position.yes_shares += size,
        (TokenSide::Yes, OrderSide::Sell) => {
            state.position.yes_shares = (state.position.yes_shares - size).max(Decimal::ZERO)
        }
        (TokenSide::No, OrderSide::Buy) => state.position.no_shares += size,
        (TokenSide::No, OrderSide::Sell) => {
            state.position.no_shares = (state.position.no_shares - size).max(Decimal::ZERO)
        }
    }
}

async fn execute_action(
    action: &Action,
    clob_client: &auth::AuthenticatedClobClient,
    signer: &alloy::signers::local::PrivateKeySigner,
    http: &reqwest::Client,
    market_info: &MarketInfo,
    state: &ExecutorState,
    config: &AppConfig,
) -> ActionResult {
    match action {
        Action::SplitUsdc { amount } => {
            if config.dry_run {
                return ActionResult::DryRun {
                    description: format!("would split {} USDC", amount),
                };
            }
            match ctf_ops::split_position(http, config, &market_info.condition_id, *amount).await {
                Ok(tx) => {
                    // Register after successful split
                    if let Err(e) =
                        ctf_ops::register_positions(clob_client, signer, &market_info.clob_token_ids)
                            .await
                    {
                        tracing::warn!(error = %e, "Post-split registration failed");
                    }
                    ActionResult::Success {
                        detail: format!("split tx={tx}"),
                    }
                }
                Err(e) => ActionResult::Error {
                    reason: e.to_string(),
                },
            }
        }

        Action::RegisterPositions => {
            if config.dry_run {
                return ActionResult::DryRun {
                    description: "would register positions".into(),
                };
            }
            match ctf_ops::register_positions(clob_client, signer, &market_info.clob_token_ids)
                .await
            {
                Ok(()) => ActionResult::Success {
                    detail: "registered".into(),
                },
                Err(e) => ActionResult::Error {
                    reason: e.to_string(),
                },
            }
        }

        Action::PlacePostOnlyBuyYes { price, size }
        | Action::PlacePostOnlyBuyNo { price, size } => {
            let is_yes = matches!(action, Action::PlacePostOnlyBuyYes { .. });
            let token_idx = if is_yes { 0 } else { 1 };
            let token_id = match market_info.clob_token_ids.get(token_idx) {
                Some(id) => id.clone(),
                None => {
                    return ActionResult::Error {
                        reason: "missing token ID".into(),
                    }
                }
            };

            let price_dec = Decimal::try_from(*price).unwrap_or_default();
            let size_dec = Decimal::try_from(*size).unwrap_or_default();

            // Risk validation
            let risk_params = super::risk::OrderParams {
                price: price_dec,
                size: size_dec,
            };
            match super::risk::validate_order(&risk_params, state, config) {
                Ok(()) => {}
                Err(super::risk::RiskError::DryRunActive) => {
                    return ActionResult::DryRun {
                        description: format!(
                            "would post-only buy {} at {} x{}",
                            if is_yes { "YES" } else { "NO" },
                            price,
                            size
                        ),
                    };
                }
                Err(e) => {
                    return ActionResult::Error {
                        reason: e.to_string(),
                    }
                }
            }

            let params = clob::LimitOrderParams {
                token_id,
                side: Side::Buy,
                price: price_dec,
                size: size_dec,
            };

            match clob::place_post_only_order(clob_client, signer, params).await {
                Ok(resp) => ActionResult::OrderPlaced {
                    order_id: resp.order_id,
                },
                Err(clob::ClobError::OrderRejected(reason)) => {
                    ActionResult::Rejected { reason }
                }
                Err(e) => ActionResult::Error {
                    reason: e.to_string(),
                },
            }
        }

        Action::PlacePostOnlySellYes { price, size }
        | Action::PlacePostOnlySellNo { price, size } => {
            let is_yes = matches!(action, Action::PlacePostOnlySellYes { .. });
            let token_idx = if is_yes { 0 } else { 1 };
            let token_id = match market_info.clob_token_ids.get(token_idx) {
                Some(id) => id.clone(),
                None => {
                    return ActionResult::Error {
                        reason: "missing token ID".into(),
                    }
                }
            };

            let price_dec = Decimal::try_from(*price).unwrap_or_default();
            let size_dec = Decimal::try_from(*size).unwrap_or_default();

            let risk_params = super::risk::OrderParams {
                price: price_dec,
                size: size_dec,
            };
            match super::risk::validate_order(&risk_params, state, config) {
                Ok(()) => {}
                Err(super::risk::RiskError::DryRunActive) => {
                    return ActionResult::DryRun {
                        description: format!(
                            "would post-only sell {} at {} x{}",
                            if is_yes { "YES" } else { "NO" },
                            price,
                            size
                        ),
                    };
                }
                Err(e) => {
                    return ActionResult::Error {
                        reason: e.to_string(),
                    }
                }
            }

            let params = clob::LimitOrderParams {
                token_id,
                side: Side::Sell,
                price: price_dec,
                size: size_dec,
            };

            match clob::place_post_only_order(clob_client, signer, params).await {
                Ok(resp) => ActionResult::OrderPlaced {
                    order_id: resp.order_id,
                },
                Err(clob::ClobError::OrderRejected(reason)) => {
                    ActionResult::Rejected { reason }
                }
                Err(e) => ActionResult::Error {
                    reason: e.to_string(),
                },
            }
        }

        Action::CancelOrder { order_id } => {
            if config.dry_run {
                return ActionResult::DryRun {
                    description: format!("would cancel order {order_id}"),
                };
            }
            match clob::cancel_order(clob_client, order_id).await {
                Ok(()) => ActionResult::Success {
                    detail: format!("cancelled {order_id}"),
                },
                Err(e) => ActionResult::Error {
                    reason: e.to_string(),
                },
            }
        }

        Action::CancelAll => {
            if config.dry_run {
                return ActionResult::DryRun {
                    description: "would cancel all orders".into(),
                };
            }
            match clob::cancel_all(clob_client).await {
                Ok(()) => ActionResult::Success {
                    detail: "all orders cancelled".into(),
                },
                Err(e) => ActionResult::Error {
                    reason: e.to_string(),
                },
            }
        }

        Action::PlaceLimitBuyYes { price, size }
        | Action::PlaceLimitBuyNo { price, size } => {
            let is_yes = matches!(action, Action::PlaceLimitBuyYes { .. });
            let token_idx = if is_yes { 0 } else { 1 };
            let token_id = match market_info.clob_token_ids.get(token_idx) {
                Some(id) => id.clone(),
                None => {
                    return ActionResult::Error {
                        reason: "missing token ID".into(),
                    }
                }
            };

            let price_dec = Decimal::try_from(*price).unwrap_or_default();
            let size_dec = Decimal::try_from(*size).unwrap_or_default();

            let risk_params = super::risk::OrderParams {
                price: price_dec,
                size: size_dec,
            };
            match super::risk::validate_order(&risk_params, state, config) {
                Ok(()) => {}
                Err(super::risk::RiskError::DryRunActive) => {
                    return ActionResult::DryRun {
                        description: format!("would limit buy at {} x{}", price, size),
                    };
                }
                Err(e) => {
                    return ActionResult::Error {
                        reason: e.to_string(),
                    }
                }
            }

            let params = clob::LimitOrderParams {
                token_id,
                side: Side::Buy,
                price: price_dec,
                size: size_dec,
            };

            match clob::place_limit_order(clob_client, signer, params).await {
                Ok(resp) => ActionResult::OrderPlaced {
                    order_id: resp.order_id,
                },
                Err(e) => ActionResult::Error {
                    reason: e.to_string(),
                },
            }
        }

        Action::PlaceLimitSellYes { price, size }
        | Action::PlaceLimitSellNo { price, size } => {
            let is_yes = matches!(action, Action::PlaceLimitSellYes { .. });
            let token_idx = if is_yes { 0 } else { 1 };
            let token_id = match market_info.clob_token_ids.get(token_idx) {
                Some(id) => id.clone(),
                None => {
                    return ActionResult::Error {
                        reason: "missing token ID".into(),
                    }
                }
            };

            let price_dec = Decimal::try_from(*price).unwrap_or_default();
            let size_dec = Decimal::try_from(*size).unwrap_or_default();

            let risk_params = super::risk::OrderParams {
                price: price_dec,
                size: size_dec,
            };
            match super::risk::validate_order(&risk_params, state, config) {
                Ok(()) => {}
                Err(super::risk::RiskError::DryRunActive) => {
                    return ActionResult::DryRun {
                        description: format!("would limit sell at {} x{}", price, size),
                    };
                }
                Err(e) => {
                    return ActionResult::Error {
                        reason: e.to_string(),
                    }
                }
            }

            let params = clob::LimitOrderParams {
                token_id,
                side: Side::Sell,
                price: price_dec,
                size: size_dec,
            };

            match clob::place_limit_order(clob_client, signer, params).await {
                Ok(resp) => ActionResult::OrderPlaced {
                    order_id: resp.order_id,
                },
                Err(e) => ActionResult::Error {
                    reason: e.to_string(),
                },
            }
        }
    }
}

fn action_log(action: &Action, result: &ActionResult, latency_ms: u64) -> serde_json::Value {
    let (action_type, price, size) = match action {
        Action::PlacePostOnlyBuyYes { price, size } => ("post_only_buy_yes", Some(*price), Some(*size)),
        Action::PlacePostOnlyBuyNo { price, size } => ("post_only_buy_no", Some(*price), Some(*size)),
        Action::PlacePostOnlySellYes { price, size } => ("post_only_sell_yes", Some(*price), Some(*size)),
        Action::PlacePostOnlySellNo { price, size } => ("post_only_sell_no", Some(*price), Some(*size)),
        Action::PlaceLimitBuyYes { price, size } => ("limit_buy_yes", Some(*price), Some(*size)),
        Action::PlaceLimitBuyNo { price, size } => ("limit_buy_no", Some(*price), Some(*size)),
        Action::PlaceLimitSellYes { price, size } => ("limit_sell_yes", Some(*price), Some(*size)),
        Action::PlaceLimitSellNo { price, size } => ("limit_sell_no", Some(*price), Some(*size)),
        Action::SplitUsdc { amount } => ("split_usdc", Some(*amount as f64), None),
        Action::RegisterPositions => ("register_positions", None, None),
        Action::CancelOrder { .. } => ("cancel_order", None, None),
        Action::CancelAll => ("cancel_all", None, None),
    };

    let (result_type, order_id, reason) = match result {
        ActionResult::OrderPlaced { order_id } => ("placed", Some(order_id.as_str()), None),
        ActionResult::Rejected { reason } => ("rejected", None, Some(reason.as_str())),
        ActionResult::Success { detail } => ("success", None, Some(detail.as_str())),
        ActionResult::Error { reason } => ("error", None, Some(reason.as_str())),
        ActionResult::DryRun { description } => ("dry_run", None, Some(description.as_str())),
    };

    json!({
        "action_type": action_type,
        "result_type": result_type,
        "latency_ms": latency_ms,
        "price": price,
        "size": size,
        "order_id": order_id,
        "reason": reason,
    })
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
