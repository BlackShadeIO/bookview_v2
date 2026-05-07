use crate::sim::market_data::{MarketState, SimEvent, TokenSide};
use crate::sim::order::{Order, OrderSide};
use crate::sim::position::Position;
use crate::sim::strategy::{Strategy, StrategyAction};

pub struct FairValueStrategy {
    edge_threshold: f64,
    order_size: f64,
    split: f64,
    max_position: f64,
    use_lstm: bool,
    last_order_ms: i64,
}

impl FairValueStrategy {
    pub fn new(edge_threshold: f64, order_size: f64, split: f64, use_lstm: bool) -> Self {
        Self {
            edge_threshold,
            order_size,
            split,
            max_position: 200.0,
            use_lstm,
            last_order_ms: 0,
        }
    }
}

impl Strategy for FairValueStrategy {
    fn name(&self) -> String {
        if self.use_lstm { "fv_lstm".into() } else { "fv_analytical".into() }
    }

    fn split_size(&self) -> f64 { self.split }

    fn on_event(
        &mut self,
        event: &SimEvent,
        state: &MarketState,
        position: &Position,
        live_orders: &[Order],
    ) -> Vec<StrategyAction> {
        // Only act on FV updates
        let SimEvent::FairValue { .. } = event else { return Vec::new() };

        if state.tau < 5.0 { return Vec::new(); }

        // Rate limit: one decision per 500ms
        if state.ts_ms - self.last_order_ms < 500 { return Vec::new(); }

        let fv = if self.use_lstm {
            state.lstm_fair_yes.unwrap_or_else(|| state.fair_yes.unwrap_or(0.5))
        } else {
            state.fair_yes.unwrap_or(0.5)
        };

        let mut actions = Vec::new();

        // Cancel existing orders if edge flipped
        if !live_orders.is_empty() {
            actions.push(StrategyAction::CancelAll);
        }

        let up_ask = state.up_best_ask().unwrap_or(1.0);
        let up_bid = state.up_best_bid().unwrap_or(0.0);
        let poly_mid = (up_ask + up_bid) / 2.0;

        let edge = fv - poly_mid;

        if edge > self.edge_threshold {
            // FV says UP is underpriced → buy UP
            if position.up.total() < self.max_position {
                let price = ((up_bid + 0.01) * 100.0).round() / 100.0;
                actions.push(StrategyAction::PlaceOrder {
                    token: TokenSide::Up,
                    side: OrderSide::Bid,
                    price,
                    size: self.order_size,
                });
                self.last_order_ms = state.ts_ms;
            }
            // Sell DOWN from inventory
            if position.down.sellable > self.order_size {
                let down_bid = state.down_best_bid().unwrap_or(0.0);
                if down_bid > 0.0 {
                    let price = ((down_bid + 0.01) * 100.0).round() / 100.0;
                    actions.push(StrategyAction::PlaceOrder {
                        token: TokenSide::Down,
                        side: OrderSide::Ask,
                        price,
                        size: self.order_size,
                    });
                }
            }
        } else if edge < -self.edge_threshold {
            // FV says UP is overpriced → buy DOWN
            if position.down.total() < self.max_position {
                let down_bid = state.down_best_bid().unwrap_or(0.0);
                let price = ((down_bid + 0.01) * 100.0).round() / 100.0;
                actions.push(StrategyAction::PlaceOrder {
                    token: TokenSide::Down,
                    side: OrderSide::Bid,
                    price,
                    size: self.order_size,
                });
                self.last_order_ms = state.ts_ms;
            }
            // Sell UP from inventory
            if position.up.sellable > self.order_size {
                if up_bid > 0.0 {
                    let price = ((up_bid + 0.01) * 100.0).round() / 100.0;
                    actions.push(StrategyAction::PlaceOrder {
                        token: TokenSide::Up,
                        side: OrderSide::Ask,
                        price,
                        size: self.order_size,
                    });
                }
            }
        }

        actions
    }
}
