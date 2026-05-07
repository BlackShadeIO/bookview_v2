use rust_decimal::Decimal;
use tokio::time::Instant;

use crate::types::{FairValueSnapshot, MarketInfo, PolyBbaSnapshot};

#[derive(Debug, Default)]
pub struct ExecutorState {
    pub market_info: Option<MarketInfo>,
    pub poly_bba: Option<PolyBbaSnapshot>,
    pub fair_value: Option<FairValueSnapshot>,
    pub open_orders: Vec<OrderInfo>,
    pub position: Position,
    pub last_bba_update: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct OrderInfo {
    pub order_id: String,
    pub token_id: String,
    pub side: String,
    pub price: Decimal,
    pub size: Decimal,
    pub placed_at: Instant,
}

#[derive(Debug, Clone, Default)]
pub struct Position {
    pub yes_shares: Decimal,
    pub no_shares: Decimal,
    pub usdc_balance: Decimal,
}

impl Position {
    pub fn total_exposure(&self) -> Decimal {
        self.yes_shares + self.no_shares
    }
}
