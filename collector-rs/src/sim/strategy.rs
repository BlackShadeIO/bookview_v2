use super::market_data::{MarketState, SimEvent, TokenSide};
use super::order::{Order, OrderSide};
use super::position::Position;

#[derive(Debug, Clone)]
pub enum StrategyAction {
    PlaceOrder { token: TokenSide, side: OrderSide, price: f64, size: f64 },
    CancelOrder { order_id: u64 },
    CancelAll,
}

pub trait Strategy: Send {
    fn name(&self) -> String;
    fn split_size(&self) -> f64 { 0.0 }

    fn on_event(
        &mut self,
        event: &SimEvent,
        state: &MarketState,
        position: &Position,
        live_orders: &[Order],
    ) -> Vec<StrategyAction>;

    fn on_market_start(&mut self, _start_ms: i64, _end_ms: i64) {}
    fn on_market_end(&mut self) -> Vec<StrategyAction> { Vec::new() }
}
