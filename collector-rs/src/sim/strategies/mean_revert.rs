use crate::sim::market_data::{MarketState, SimEvent, TokenSide};
use crate::sim::order::{Order, OrderSide};
use crate::sim::position::Position;
use crate::sim::strategy::{Strategy, StrategyAction};

pub struct MeanRevertStrategy {
    z_entry: f64,
    z_exit: f64,
    order_size: f64,
    split: f64,
    last_action_ms: i64,
    in_position: Option<TokenSide>,
}

impl MeanRevertStrategy {
    pub fn new(order_size: f64, split: f64) -> Self {
        Self {
            z_entry: 1.5,
            z_exit: 0.5,
            order_size,
            split,
            last_action_ms: 0,
            in_position: None,
        }
    }
}

impl Strategy for MeanRevertStrategy {
    fn name(&self) -> String { "mean_revert".into() }
    fn split_size(&self) -> f64 { self.split }

    fn on_event(
        &mut self,
        event: &SimEvent,
        state: &MarketState,
        position: &Position,
        _live_orders: &[Order],
    ) -> Vec<StrategyAction> {
        let SimEvent::FairValue { .. } = event else { return Vec::new() };
        if state.tau < 10.0 { return Vec::new(); }
        if state.ts_ms - self.last_action_ms < 1000 { return Vec::new(); }

        let z = state.z_score.unwrap_or(0.0);
        let mut actions = Vec::new();

        // Exit condition
        if let Some(held_token) = self.in_position {
            if z.abs() < self.z_exit {
                // Exit position by selling from inventory
                let inv = position.inventory(held_token);
                if inv.sellable > self.order_size {
                    let best_bid = match held_token {
                        TokenSide::Up => state.up_best_bid(),
                        TokenSide::Down => state.down_best_bid(),
                    };
                    if let Some(bid) = best_bid {
                        let ask = ((bid + 0.01) * 100.0).round() / 100.0;
                        actions.push(StrategyAction::PlaceOrder {
                            token: held_token,
                            side: OrderSide::Ask,
                            price: ask,
                            size: self.order_size,
                        });
                        self.in_position = None;
                        self.last_action_ms = state.ts_ms;
                    }
                }
                return actions;
            }
        }

        // Entry condition
        if z > self.z_entry {
            // UP overpriced → buy DOWN (mean revert expects z to come back toward 0)
            if let Some(best_ask) = state.down_best_ask() {
                let bid = ((best_ask - 0.01) * 100.0).round() / 100.0;
                if bid > 0.0 {
                    actions.push(StrategyAction::PlaceOrder {
                        token: TokenSide::Down,
                        side: OrderSide::Bid,
                        price: bid,
                        size: self.order_size,
                    });
                    self.in_position = Some(TokenSide::Down);
                    self.last_action_ms = state.ts_ms;
                }
            }
            // Sell UP from inventory
            if position.up.sellable > self.order_size {
                if let Some(bid) = state.up_best_bid() {
                    let ask = ((bid + 0.01) * 100.0).round() / 100.0;
                    actions.push(StrategyAction::PlaceOrder {
                        token: TokenSide::Up,
                        side: OrderSide::Ask,
                        price: ask,
                        size: self.order_size,
                    });
                }
            }
        } else if z < -self.z_entry {
            // DOWN overpriced → buy UP
            if let Some(best_ask) = state.up_best_ask() {
                let bid = ((best_ask - 0.01) * 100.0).round() / 100.0;
                if bid > 0.0 {
                    actions.push(StrategyAction::PlaceOrder {
                        token: TokenSide::Up,
                        side: OrderSide::Bid,
                        price: bid,
                        size: self.order_size,
                    });
                    self.in_position = Some(TokenSide::Up);
                    self.last_action_ms = state.ts_ms;
                }
            }
            // Sell DOWN from inventory
            if position.down.sellable > self.order_size {
                if let Some(bid) = state.down_best_bid() {
                    let ask = ((bid + 0.01) * 100.0).round() / 100.0;
                    actions.push(StrategyAction::PlaceOrder {
                        token: TokenSide::Down,
                        side: OrderSide::Ask,
                        price: ask,
                        size: self.order_size,
                    });
                }
            }
        }

        actions
    }
}
