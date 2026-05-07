use crate::sim::market_data::{MarketState, SimEvent, TokenSide};
use crate::sim::order::{Order, OrderSide};
use crate::sim::position::Position;
use crate::sim::strategy::{Strategy, StrategyAction};

pub struct TrendStrategy {
    lookback_ms: u64,
    threshold: f64,
    order_size: f64,
    split: f64,
    btc_history: Vec<(i64, f64)>,
    last_action_ms: i64,
}

impl TrendStrategy {
    pub fn new(order_size: f64, split: f64) -> Self {
        Self {
            lookback_ms: 3000,
            threshold: 0.0002,
            order_size,
            split,
            btc_history: Vec::new(),
            last_action_ms: 0,
        }
    }
}

impl Strategy for TrendStrategy {
    fn name(&self) -> String { "trend".into() }
    fn split_size(&self) -> f64 { self.split }

    fn on_event(
        &mut self,
        event: &SimEvent,
        state: &MarketState,
        position: &Position,
        _live_orders: &[Order],
    ) -> Vec<StrategyAction> {
        if let SimEvent::BtcTicker { ts_ms, mid, .. } = event {
            self.btc_history.push((*ts_ms, *mid));
            if self.btc_history.len() > 200 {
                self.btc_history.drain(..100);
            }
        }

        // Only evaluate every 500ms
        if state.ts_ms - self.last_action_ms < 500 { return Vec::new(); }
        if state.tau < 10.0 { return Vec::new(); }
        if state.btc_mid == 0.0 { return Vec::new(); }

        let cutoff = state.ts_ms - self.lookback_ms as i64;
        let old_price = self.btc_history.iter().rev()
            .find(|(ts, _)| *ts <= cutoff)
            .map(|(_, p)| *p)
            .unwrap_or(0.0);

        if old_price == 0.0 { return Vec::new(); }

        let momentum = (state.btc_mid - old_price) / old_price;
        self.last_action_ms = state.ts_ms;

        let mut actions = Vec::new();

        if momentum > self.threshold {
            // BTC rising → buy UP
            if let Some(best_ask) = state.up_best_ask() {
                let bid = ((best_ask - 0.01) * 100.0).round() / 100.0;
                if bid > 0.0 {
                    actions.push(StrategyAction::PlaceOrder {
                        token: TokenSide::Up,
                        side: OrderSide::Bid,
                        price: bid,
                        size: self.order_size,
                    });
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
        } else if momentum < -self.threshold {
            // BTC falling → buy DOWN
            if let Some(best_ask) = state.down_best_ask() {
                let bid = ((best_ask - 0.01) * 100.0).round() / 100.0;
                if bid > 0.0 {
                    actions.push(StrategyAction::PlaceOrder {
                        token: TokenSide::Down,
                        side: OrderSide::Bid,
                        price: bid,
                        size: self.order_size,
                    });
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
        }

        actions
    }
}
