use crate::sim::market_data::{MarketState, SimEvent, TokenSide};
use crate::sim::order::{Order, OrderSide};
use crate::sim::position::Position;
use crate::sim::strategy::{Strategy, StrategyAction};

pub struct MarketMakerStrategy {
    half_spread: f64,
    skew_factor: f64,
    max_position: f64,
    order_size: f64,
    split: f64,
    requote_ms: u64,
    last_quote_ms: i64,
}

impl MarketMakerStrategy {
    pub fn new(half_spread: f64, order_size: f64, split: f64) -> Self {
        Self {
            half_spread,
            skew_factor: 0.5,
            max_position: 150.0,
            order_size,
            split,
            requote_ms: 1000,
            last_quote_ms: 0,
        }
    }
}

impl Strategy for MarketMakerStrategy {
    fn name(&self) -> String { "market_maker".into() }
    fn split_size(&self) -> f64 { self.split }

    fn on_event(
        &mut self,
        event: &SimEvent,
        state: &MarketState,
        position: &Position,
        live_orders: &[Order],
    ) -> Vec<StrategyAction> {
        // Only requote on FV events or book updates
        let should_quote = matches!(event, SimEvent::FairValue { .. } | SimEvent::PolyBookState { .. });
        if !should_quote { return Vec::new(); }

        if state.ts_ms - self.last_quote_ms < self.requote_ms as i64 {
            return Vec::new();
        }

        // Flatten in last 15s
        if state.tau < 15.0 {
            if !live_orders.is_empty() {
                return vec![StrategyAction::CancelAll];
            }
            return Vec::new();
        }

        let fv = state.fair_yes.unwrap_or(0.5);
        let up_inventory = position.up.total() - position.down.total();
        let skew = -self.skew_factor * up_inventory / self.max_position;

        let theo_bid = fv - self.half_spread + skew;
        let theo_ask = fv + self.half_spread + skew;

        let bid_price = (theo_bid * 100.0).round() / 100.0;
        let ask_price = (theo_ask * 100.0).round() / 100.0;

        if bid_price >= ask_price || bid_price < 0.01 || ask_price > 0.99 {
            return Vec::new();
        }

        let mut actions = Vec::new();

        // Cancel all existing before requoting
        if !live_orders.is_empty() {
            actions.push(StrategyAction::CancelAll);
        }

        // Bid side (buy UP)
        if position.up.total() < self.max_position {
            actions.push(StrategyAction::PlaceOrder {
                token: TokenSide::Up,
                side: OrderSide::Bid,
                price: bid_price,
                size: self.order_size,
            });
        }

        // Ask side (sell UP from inventory)
        if position.up.sellable > self.order_size {
            actions.push(StrategyAction::PlaceOrder {
                token: TokenSide::Up,
                side: OrderSide::Ask,
                price: ask_price,
                size: self.order_size,
            });
        }

        // Also quote DOWN side (mirror)
        let down_bid = ((1.0 - ask_price) * 100.0).round() / 100.0;
        let down_ask = ((1.0 - bid_price) * 100.0).round() / 100.0;

        if down_bid > 0.0 && down_bid < down_ask && position.down.total() < self.max_position {
            actions.push(StrategyAction::PlaceOrder {
                token: TokenSide::Down,
                side: OrderSide::Bid,
                price: down_bid,
                size: self.order_size,
            });
        }
        if down_ask < 1.0 && position.down.sellable > self.order_size {
            actions.push(StrategyAction::PlaceOrder {
                token: TokenSide::Down,
                side: OrderSide::Ask,
                price: down_ask,
                size: self.order_size,
            });
        }

        self.last_quote_ms = state.ts_ms;
        actions
    }
}
