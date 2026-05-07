use rust_decimal::Decimal;
use thiserror::Error;

use super::config::AppConfig;
use super::state::ExecutorState;
use crate::config::MARKET_DURATION;

use std::time::Duration;

#[derive(Error, Debug)]
pub enum RiskError {
    #[error("dry run active — order not submitted")]
    DryRunActive,
    #[error("max order size exceeded: {size} > {max}")]
    MaxOrderSizeExceeded { size: Decimal, max: Decimal },
    #[error("max position size exceeded: {total} > {max}")]
    MaxPositionSizeExceeded { total: Decimal, max: Decimal },
    #[error("price out of bounds: {price} not in [{min}, {max}]")]
    PriceOutOfBounds {
        price: Decimal,
        min: Decimal,
        max: Decimal,
    },
    #[error("orderbook stale ({elapsed_secs:.1}s since last update)")]
    StaleOrderbook { elapsed_secs: f64 },
    #[error("too close to market expiry ({remaining_secs}s remaining)")]
    TooCloseToExpiry { remaining_secs: i64 },
    #[error("no active market")]
    NoActiveMarket,
}

const STALE_THRESHOLD: Duration = Duration::from_secs(10);
const MIN_SECONDS_BEFORE_CLOSE: i64 = 30;

pub struct OrderParams {
    pub price: Decimal,
    pub size: Decimal,
}

pub fn validate_order(
    params: &OrderParams,
    state: &ExecutorState,
    config: &AppConfig,
) -> Result<(), RiskError> {
    if config.dry_run {
        return Err(RiskError::DryRunActive);
    }

    let market = state
        .market_info
        .as_ref()
        .ok_or(RiskError::NoActiveMarket)?;

    if params.size > config.max_order_size {
        return Err(RiskError::MaxOrderSizeExceeded {
            size: params.size,
            max: config.max_order_size,
        });
    }

    let total_exposure = state.position.total_exposure() + params.size;
    if total_exposure > config.max_position_size {
        return Err(RiskError::MaxPositionSizeExceeded {
            total: total_exposure,
            max: config.max_position_size,
        });
    }

    if params.price < config.order_price_min || params.price > config.order_price_max {
        return Err(RiskError::PriceOutOfBounds {
            price: params.price,
            min: config.order_price_min,
            max: config.order_price_max,
        });
    }

    if let Some(last_update) = state.last_bba_update {
        if last_update.elapsed() > STALE_THRESHOLD {
            return Err(RiskError::StaleOrderbook {
                elapsed_secs: last_update.elapsed().as_secs_f64(),
            });
        }
    }

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let market_end = market.market_start + MARKET_DURATION as i64;
    let remaining = market_end - now_secs;
    if remaining < MIN_SECONDS_BEFORE_CLOSE {
        return Err(RiskError::TooCloseToExpiry {
            remaining_secs: remaining,
        });
    }

    Ok(())
}
