use alloy::signers::local::PrivateKeySigner;
use rust_decimal::Decimal;

use super::auth::AuthenticatedClobClient;
use super::clob::{self, LimitOrderParams, MarketOrderParams, OrderResponse, Side};
use super::config::AppConfig;
use super::risk::{self, OrderParams};
use super::state::ExecutorState;

pub async fn place_limit(
    client: &AuthenticatedClobClient,
    signer: &PrivateKeySigner,
    state: &ExecutorState,
    config: &AppConfig,
    params: LimitOrderParams,
) -> anyhow::Result<OrderResponse> {
    let risk_params = OrderParams {
        price: params.price,
        size: params.size,
    };

    match risk::validate_order(&risk_params, state, config) {
        Ok(()) => {}
        Err(risk::RiskError::DryRunActive) => {
            tracing::info!(
                token_id = %params.token_id,
                side = ?params.side,
                price = %params.price,
                size = %params.size,
                "DRY RUN: would place limit order"
            );
            return Ok(OrderResponse {
                order_id: "dry-run".into(),
                status: "simulated".into(),
            });
        }
        Err(e) => return Err(e.into()),
    }

    clob::place_limit_order(client, signer, params)
        .await
        .map_err(Into::into)
}

pub async fn buy_yes_market(
    client: &AuthenticatedClobClient,
    signer: &PrivateKeySigner,
    state: &ExecutorState,
    config: &AppConfig,
    size: Decimal,
) -> anyhow::Result<OrderResponse> {
    let market = state
        .market_info
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no active market"))?;

    let token_id = market
        .clob_token_ids
        .first()
        .ok_or_else(|| anyhow::anyhow!("no YES token ID"))?
        .clone();

    let midpoint = state
        .poly_bba
        .as_ref()
        .and_then(|bba| {
            let bid = bba.yes_best_bid?;
            let ask = bba.yes_best_ask?;
            Some(Decimal::try_from((bid + ask) / 2.0).unwrap_or(Decimal::new(50, 2)))
        })
        .unwrap_or(Decimal::new(50, 2));

    let risk_params = OrderParams {
        price: midpoint,
        size,
    };

    match risk::validate_order(&risk_params, state, config) {
        Ok(()) => {}
        Err(risk::RiskError::DryRunActive) => {
            tracing::info!(token_id = %token_id, size = %size, "DRY RUN: would buy YES market");
            return Ok(OrderResponse {
                order_id: "dry-run".into(),
                status: "simulated".into(),
            });
        }
        Err(e) => return Err(e.into()),
    }

    let params = MarketOrderParams {
        token_id,
        side: Side::Buy,
        size,
    };

    clob::place_market_order(client, signer, params)
        .await
        .map_err(Into::into)
}

pub async fn buy_no_market(
    client: &AuthenticatedClobClient,
    signer: &PrivateKeySigner,
    state: &ExecutorState,
    config: &AppConfig,
    size: Decimal,
) -> anyhow::Result<OrderResponse> {
    let market = state
        .market_info
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no active market"))?;

    let token_id = market
        .clob_token_ids
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("no NO token ID"))?
        .clone();

    let midpoint = state
        .poly_bba
        .as_ref()
        .and_then(|bba| {
            let bid = bba.no_best_bid?;
            let ask = bba.no_best_ask?;
            Some(Decimal::try_from((bid + ask) / 2.0).unwrap_or(Decimal::new(50, 2)))
        })
        .unwrap_or(Decimal::new(50, 2));

    let risk_params = OrderParams {
        price: midpoint,
        size,
    };

    match risk::validate_order(&risk_params, state, config) {
        Ok(()) => {}
        Err(risk::RiskError::DryRunActive) => {
            tracing::info!(token_id = %token_id, size = %size, "DRY RUN: would buy NO market");
            return Ok(OrderResponse {
                order_id: "dry-run".into(),
                status: "simulated".into(),
            });
        }
        Err(e) => return Err(e.into()),
    }

    let params = MarketOrderParams {
        token_id,
        side: Side::Buy,
        size,
    };

    clob::place_market_order(client, signer, params)
        .await
        .map_err(Into::into)
}

pub async fn cancel_all_orders(client: &AuthenticatedClobClient) -> anyhow::Result<()> {
    clob::cancel_all(client).await.map_err(Into::into)
}
