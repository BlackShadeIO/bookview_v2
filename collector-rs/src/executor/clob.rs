use alloy::signers::local::PrivateKeySigner;
use rust_decimal::Decimal;
use thiserror::Error;

use super::auth::AuthenticatedClobClient;

pub use polymarket_client_sdk_v2::clob::types::Side;
pub use polymarket_client_sdk_v2::clob::types::OrderType;
pub use polymarket_client_sdk_v2::clob::types::OrderStatusType;

#[derive(Error, Debug)]
pub enum ClobError {
    #[error("CLOB request failed: {0}")]
    RequestFailed(String),
    #[error("order rejected: {0}")]
    OrderRejected(String),
}

#[derive(Debug, Clone)]
pub struct LimitOrderParams {
    pub token_id: String,
    pub side: Side,
    pub price: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone)]
pub struct MarketOrderParams {
    pub token_id: String,
    pub side: Side,
    pub size: Decimal,
}

#[derive(Debug, Clone)]
pub struct OrderResponse {
    pub order_id: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct OrderBookSummary {
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
}

#[derive(Debug, Clone)]
pub struct PriceLevel {
    pub price: Decimal,
    pub size: Decimal,
}

pub async fn place_limit_order(
    client: &AuthenticatedClobClient,
    signer: &PrivateKeySigner,
    params: LimitOrderParams,
) -> Result<OrderResponse, ClobError> {
    let token_id: alloy::primitives::U256 = params
        .token_id
        .parse()
        .map_err(|e: alloy::primitives::ruint::ParseError| ClobError::RequestFailed(format!("invalid token ID: {e}")))?;

    tracing::info!(
        token_id = %params.token_id,
        side = ?params.side,
        price = %params.price,
        size = %params.size,
        "Placing limit order"
    );

    let resp = client
        .limit_order()
        .token_id(token_id)
        .side(params.side)
        .price(params.price)
        .size(params.size)
        .order_type(polymarket_client_sdk_v2::clob::types::OrderType::GTC)
        .build_sign_and_post(signer)
        .await
        .map_err(|e| ClobError::RequestFailed(e.to_string()))?;

    tracing::info!(order_id = %resp.order_id, "Limit order placed");

    Ok(OrderResponse {
        order_id: resp.order_id,
        status: format!("{:?}", resp.status),
    })
}

pub async fn place_market_order(
    client: &AuthenticatedClobClient,
    signer: &PrivateKeySigner,
    params: MarketOrderParams,
) -> Result<OrderResponse, ClobError> {
    let token_id: alloy::primitives::U256 = params
        .token_id
        .parse()
        .map_err(|e: alloy::primitives::ruint::ParseError| ClobError::RequestFailed(format!("invalid token ID: {e}")))?;

    tracing::info!(
        token_id = %params.token_id,
        side = ?params.side,
        size = %params.size,
        "Placing market order"
    );

    let amount = polymarket_client_sdk_v2::clob::types::Amount::usdc(params.size)
        .map_err(|e| ClobError::RequestFailed(format!("invalid amount: {e}")))?;

    let resp = client
        .market_order()
        .token_id(token_id)
        .side(params.side)
        .amount(amount)
        .build_sign_and_post(signer)
        .await
        .map_err(|e| ClobError::RequestFailed(e.to_string()))?;

    tracing::info!(order_id = %resp.order_id, "Market order placed");

    Ok(OrderResponse {
        order_id: resp.order_id,
        status: format!("{:?}", resp.status),
    })
}

pub async fn cancel_order(
    client: &AuthenticatedClobClient,
    order_id: &str,
) -> Result<(), ClobError> {
    tracing::info!(order_id = %order_id, "Cancelling order");

    client
        .cancel_order(order_id)
        .await
        .map_err(|e| ClobError::RequestFailed(e.to_string()))?;

    tracing::info!(order_id = %order_id, "Order cancelled");
    Ok(())
}

pub async fn cancel_all(client: &AuthenticatedClobClient) -> Result<(), ClobError> {
    tracing::info!("Cancelling all orders");

    client
        .cancel_all_orders()
        .await
        .map_err(|e| ClobError::RequestFailed(e.to_string()))?;

    tracing::info!("All orders cancelled");
    Ok(())
}

pub async fn fetch_orderbook(
    client: &AuthenticatedClobClient,
    token_id: &str,
) -> Result<OrderBookSummary, ClobError> {
    let token = token_id
        .parse()
        .map_err(|e: alloy::primitives::ruint::ParseError| {
            ClobError::RequestFailed(format!("invalid token ID: {e}"))
        })?;

    let request =
        polymarket_client_sdk_v2::clob::types::request::OrderBookSummaryRequest::builder()
            .token_id(token)
            .build();

    let resp = client
        .order_book(&request)
        .await
        .map_err(|e| ClobError::RequestFailed(e.to_string()))?;

    let bids = resp
        .bids
        .iter()
        .map(|l| PriceLevel {
            price: l.price,
            size: l.size,
        })
        .collect();

    let asks = resp
        .asks
        .iter()
        .map(|l| PriceLevel {
            price: l.price,
            size: l.size,
        })
        .collect();

    Ok(OrderBookSummary { bids, asks })
}

pub async fn fetch_balance(client: &AuthenticatedClobClient) -> Result<String, ClobError> {
    let request =
        polymarket_client_sdk_v2::clob::types::request::BalanceAllowanceRequest::default();

    let resp = client
        .balance_allowance(request)
        .await
        .map_err(|e| ClobError::RequestFailed(e.to_string()))?;

    Ok(format!("balance: {}", resp.balance))
}

pub async fn place_typed_order(
    client: &AuthenticatedClobClient,
    signer: &PrivateKeySigner,
    token_id: &str,
    side: Side,
    price: Decimal,
    size: Decimal,
    order_type: OrderType,
) -> Result<OrderResponse, ClobError> {
    let token: alloy::primitives::U256 = token_id.parse().map_err(
        |e: alloy::primitives::ruint::ParseError| {
            ClobError::RequestFailed(format!("invalid token ID: {e}"))
        },
    )?;

    tracing::info!(
        token_id = %token_id,
        side = ?side,
        price = %price,
        size = %size,
        order_type = %order_type,
        "Placing typed order"
    );

    let resp = client
        .limit_order()
        .token_id(token)
        .side(side)
        .price(price)
        .size(size)
        .order_type(order_type)
        .build_sign_and_post(signer)
        .await
        .map_err(|e| ClobError::RequestFailed(e.to_string()))?;

    tracing::info!(order_id = %resp.order_id, "Typed order placed");

    Ok(OrderResponse {
        order_id: resp.order_id,
        status: format!("{:?}", resp.status),
    })
}

pub async fn place_post_only_order(
    client: &AuthenticatedClobClient,
    signer: &PrivateKeySigner,
    params: LimitOrderParams,
) -> Result<OrderResponse, ClobError> {
    let token: alloy::primitives::U256 = params
        .token_id
        .parse()
        .map_err(|e: alloy::primitives::ruint::ParseError| {
            ClobError::RequestFailed(format!("invalid token ID: {e}"))
        })?;

    tracing::info!(
        token_id = %params.token_id,
        side = ?params.side,
        price = %params.price,
        size = %params.size,
        "Placing post-only order"
    );

    let resp = client
        .limit_order()
        .token_id(token)
        .side(params.side)
        .price(params.price)
        .size(params.size)
        .order_type(OrderType::GTC)
        .post_only(true)
        .build_sign_and_post(signer)
        .await
        .map_err(|e| ClobError::RequestFailed(e.to_string()))?;

    if !resp.success {
        let reason = resp
            .error_msg
            .unwrap_or_else(|| format!("status={:?}", resp.status));
        return Err(ClobError::OrderRejected(reason));
    }

    tracing::info!(order_id = %resp.order_id, status = ?resp.status, "Post-only order placed");

    Ok(OrderResponse {
        order_id: resp.order_id,
        status: format!("{:?}", resp.status),
    })
}

#[derive(Debug, Clone)]
pub struct OrderStatusInfo {
    pub order_id: String,
    pub status: OrderStatusType,
    pub original_size: Decimal,
    pub size_matched: Decimal,
    pub price: Decimal,
    pub side: Side,
}

pub async fn get_order_status(
    client: &AuthenticatedClobClient,
    order_id: &str,
) -> Result<OrderStatusInfo, ClobError> {
    let resp = client
        .order(order_id)
        .await
        .map_err(|e| ClobError::RequestFailed(e.to_string()))?;

    Ok(OrderStatusInfo {
        order_id: resp.id,
        status: resp.status,
        original_size: resp.original_size,
        size_matched: resp.size_matched,
        price: resp.price,
        side: resp.side,
    })
}

pub async fn batch_post_only(
    client: &AuthenticatedClobClient,
    signer: &PrivateKeySigner,
    yes_token: &str,
    no_token: &str,
    price: Decimal,
    size: Decimal,
) -> Result<Vec<OrderResponse>, ClobError> {
    let yes_id: alloy::primitives::U256 =
        yes_token.parse().map_err(
            |e: alloy::primitives::ruint::ParseError| {
                ClobError::RequestFailed(format!("invalid YES token ID: {e}"))
            },
        )?;
    let no_id: alloy::primitives::U256 =
        no_token.parse().map_err(
            |e: alloy::primitives::ruint::ParseError| {
                ClobError::RequestFailed(format!("invalid NO token ID: {e}"))
            },
        )?;

    tracing::info!(
        yes_token = %yes_token,
        no_token = %no_token,
        price = %price,
        size = %size,
        "Building batch Post-Only orders"
    );

    let yes_order = client
        .limit_order()
        .token_id(yes_id)
        .side(Side::Buy)
        .price(price)
        .size(size)
        .order_type(OrderType::GTC)
        .post_only(true)
        .build()
        .await
        .map_err(|e| ClobError::RequestFailed(format!("build YES post-only failed: {e}")))?;

    let no_order = client
        .limit_order()
        .token_id(no_id)
        .side(Side::Buy)
        .price(price)
        .size(size)
        .order_type(OrderType::GTC)
        .post_only(true)
        .build()
        .await
        .map_err(|e| ClobError::RequestFailed(format!("build NO post-only failed: {e}")))?;

    let yes_signed = client
        .sign(signer, yes_order)
        .await
        .map_err(|e| ClobError::RequestFailed(format!("sign YES post-only failed: {e}")))?;
    let no_signed = client
        .sign(signer, no_order)
        .await
        .map_err(|e| ClobError::RequestFailed(format!("sign NO post-only failed: {e}")))?;

    let responses = client
        .post_orders(vec![yes_signed, no_signed])
        .await
        .map_err(|e| ClobError::RequestFailed(format!("batch post failed: {e}")))?;

    tracing::info!(count = responses.len(), "Batch Post-Only orders placed");

    Ok(responses
        .into_iter()
        .map(|r| OrderResponse {
            order_id: r.order_id,
            status: format!("{:?}", r.status),
        })
        .collect())
}
