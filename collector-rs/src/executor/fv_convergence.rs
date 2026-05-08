use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::MARKET_DURATION;

use super::config::AppConfig;
use super::state::ExecutorState;
use super::strategy::{
    Action, ActionResult, FillNotification, OrderSide, Strategy, TokenSide,
};

const FV_THRESHOLD: f64 = 0.55;
const EDGE_MIN: f64 = 0.05;
const CONVERGENCE: f64 = 0.01;
const ACTIVE_WINDOW_SECS: i64 = 180;

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    WaitingForData,
    PreMarket,
    Active,
    WindDown,
    Done,
}

pub struct FvConvergenceStrategy {
    phase: Phase,
    market_start_epoch: i64,

    split_requested: bool,
    split_confirmed: bool,
    register_requested: bool,
    register_confirmed: bool,

    pending_buy_order_id: Option<String>,
    pending_sell_order_id: Option<String>,

    held_side: Option<TokenSide>,
    held_size: f64,
    entry_price: f64,

    split_amount: u64,
    order_size: f64,
}

impl FvConvergenceStrategy {
    pub fn new(market_start: i64, config: &AppConfig) -> Self {
        Self {
            phase: Phase::WaitingForData,
            market_start_epoch: market_start,
            split_requested: false,
            split_confirmed: false,
            register_requested: false,
            register_confirmed: false,
            pending_buy_order_id: None,
            pending_sell_order_id: None,
            held_side: None,
            held_size: 0.0,
            entry_price: 0.0,
            split_amount: config.split_amount,
            order_size: config.strategy_order_size,
        }
    }

    fn now_epoch() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn round_price(p: f64) -> f64 {
        (p * 100.0).round() / 100.0
    }

    fn update_phase(&mut self) {
        let now = Self::now_epoch();
        let elapsed = now - self.market_start_epoch;

        match self.phase {
            Phase::WaitingForData => {}
            Phase::PreMarket => {
                if elapsed >= 0 {
                    tracing::info!(elapsed, "Transitioning to Active phase");
                    self.phase = Phase::Active;
                }
            }
            Phase::Active => {
                if elapsed >= ACTIVE_WINDOW_SECS {
                    tracing::info!(elapsed, "Transitioning to WindDown phase");
                    self.phase = Phase::WindDown;
                }
            }
            Phase::WindDown => {
                if elapsed >= MARKET_DURATION as i64 {
                    tracing::info!(elapsed, "Transitioning to Done phase");
                    self.phase = Phase::Done;
                }
            }
            Phase::Done => {}
        }
    }

    fn has_pending_buy(&self) -> bool {
        self.pending_buy_order_id.is_some()
    }

    fn has_pending_sell(&self) -> bool {
        self.pending_sell_order_id.is_some()
    }

    fn is_holding(&self) -> bool {
        self.held_side.is_some() && self.held_size > 0.0
    }

    fn tick_pre_market(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();

        if !self.split_requested && !self.split_confirmed {
            tracing::info!(amount = self.split_amount, "Requesting pre-market split");
            actions.push(Action::SplitUsdc {
                amount: self.split_amount,
            });
            self.split_requested = true;
        }

        if self.split_confirmed && !self.register_requested && !self.register_confirmed {
            tracing::info!("Requesting position registration");
            actions.push(Action::RegisterPositions);
            self.register_requested = true;
        }

        actions
    }

    fn tick_entry(&mut self, state: &ExecutorState) -> Vec<Action> {
        if self.is_holding() || self.has_pending_buy() {
            return vec![];
        }
        if !self.split_confirmed || !self.register_confirmed {
            return vec![];
        }

        let bba = match &state.poly_bba {
            Some(b) => b,
            None => return vec![],
        };
        let fv = match &state.fair_value {
            Some(f) => f,
            None => return vec![],
        };

        // Check YES side
        if fv.fair_yes > FV_THRESHOLD {
            if let Some(ask) = bba.yes_best_ask {
                let edge = fv.fair_yes - ask;
                if edge >= EDGE_MIN {
                    let price = Self::round_price(ask - 0.01);
                    if price > 0.0 && price < 1.0 {
                        tracing::info!(
                            fair_yes = fv.fair_yes,
                            yes_ask = ask,
                            edge,
                            buy_price = price,
                            "Entry signal: BUY YES"
                        );
                        return vec![Action::PlacePostOnlyBuyYes {
                            price,
                            size: self.order_size,
                        }];
                    }
                }
            }
        }

        // Check NO side
        if fv.fair_no > FV_THRESHOLD {
            if let Some(ask) = bba.no_best_ask {
                let edge = fv.fair_no - ask;
                if edge >= EDGE_MIN {
                    let price = Self::round_price(ask - 0.01);
                    if price > 0.0 && price < 1.0 {
                        tracing::info!(
                            fair_no = fv.fair_no,
                            no_ask = ask,
                            edge,
                            buy_price = price,
                            "Entry signal: BUY NO"
                        );
                        return vec![Action::PlacePostOnlyBuyNo {
                            price,
                            size: self.order_size,
                        }];
                    }
                }
            }
        }

        vec![]
    }

    fn tick_exit(&mut self, state: &ExecutorState) -> Vec<Action> {
        if !self.is_holding() || self.has_pending_sell() {
            return vec![];
        }

        let bba = match &state.poly_bba {
            Some(b) => b,
            None => return vec![],
        };
        let fv = match &state.fair_value {
            Some(f) => f,
            None => return vec![],
        };

        let held_side = self.held_side.unwrap();

        let (best_ask, fair_val) = match held_side {
            TokenSide::Yes => (bba.yes_best_ask, fv.fair_yes),
            TokenSide::No => (bba.no_best_ask, fv.fair_no),
        };

        let ask = match best_ask {
            Some(a) => a,
            None => return vec![],
        };

        let gap = (ask - fair_val).abs();
        if gap <= CONVERGENCE {
            let sell_price = Self::round_price(ask);
            if sell_price > 0.0 && sell_price < 1.0 {
                tracing::info!(
                    ?held_side,
                    best_ask = ask,
                    fair_val,
                    gap,
                    sell_price,
                    size = self.held_size,
                    "Exit signal: convergence"
                );
                let action = match held_side {
                    TokenSide::Yes => Action::PlacePostOnlySellYes {
                        price: sell_price,
                        size: self.held_size,
                    },
                    TokenSide::No => Action::PlacePostOnlySellNo {
                        price: sell_price,
                        size: self.held_size,
                    },
                };
                return vec![action];
            }
        }

        vec![]
    }
}

impl Strategy for FvConvergenceStrategy {
    fn on_tick(&mut self, state: &ExecutorState, _config: &AppConfig) -> Vec<Action> {
        // Transition phase based on time
        if self.phase == Phase::WaitingForData {
            if state.poly_bba.is_some() && state.fair_value.is_some() && state.market_info.is_some()
            {
                let now = Self::now_epoch();
                if now < self.market_start_epoch {
                    tracing::info!("Data available, entering PreMarket");
                    self.phase = Phase::PreMarket;
                } else if now < self.market_start_epoch + ACTIVE_WINDOW_SECS {
                    tracing::info!("Data available, entering Active (market already started)");
                    self.phase = Phase::Active;
                } else if now < self.market_start_epoch + MARKET_DURATION as i64 {
                    tracing::info!("Data available, entering WindDown");
                    self.phase = Phase::WindDown;
                } else {
                    self.phase = Phase::Done;
                }
            }
            return vec![];
        }

        self.update_phase();

        match self.phase {
            Phase::WaitingForData => vec![],
            Phase::PreMarket => self.tick_pre_market(),
            Phase::Active => {
                let mut actions = self.tick_exit(state);
                if actions.is_empty() {
                    actions = self.tick_entry(state);
                }
                actions
            }
            Phase::WindDown => self.tick_exit(state),
            Phase::Done => vec![],
        }
    }

    fn on_action_result(&mut self, action: &Action, result: &ActionResult) {
        match (action, result) {
            (Action::SplitUsdc { .. }, ActionResult::Success { detail }) => {
                tracing::info!(tx = %detail, "Split confirmed");
                self.split_confirmed = true;
            }
            (Action::SplitUsdc { .. }, ActionResult::DryRun { .. }) => {
                tracing::info!("Split dry-run — treating as success");
                self.split_confirmed = true;
            }
            (Action::SplitUsdc { .. }, ActionResult::Error { reason }) => {
                tracing::warn!(reason, "Split failed, will retry");
                self.split_requested = false;
            }
            (Action::RegisterPositions, ActionResult::Success { .. })
            | (Action::RegisterPositions, ActionResult::DryRun { .. }) => {
                tracing::info!("Position registration confirmed");
                self.register_confirmed = true;
            }
            (Action::RegisterPositions, ActionResult::Error { reason }) => {
                tracing::warn!(reason, "Registration failed, will retry");
                self.register_requested = false;
            }
            (
                Action::PlacePostOnlyBuyYes { price, .. }
                | Action::PlacePostOnlyBuyNo { price, .. },
                ActionResult::OrderPlaced { order_id },
            ) => {
                tracing::info!(order_id, price, "Buy order placed");
                self.pending_buy_order_id = Some(order_id.clone());
                self.entry_price = *price;
            }
            (
                Action::PlacePostOnlyBuyYes { .. } | Action::PlacePostOnlyBuyNo { .. },
                ActionResult::Rejected { reason },
            ) => {
                tracing::info!(reason, "Buy order rejected, will retry");
                self.pending_buy_order_id = None;
            }
            (
                Action::PlacePostOnlyBuyYes { .. } | Action::PlacePostOnlyBuyNo { .. },
                ActionResult::Error { reason },
            ) => {
                tracing::warn!(reason, "Buy order error");
                self.pending_buy_order_id = None;
            }
            (
                Action::PlacePostOnlyBuyYes { .. } | Action::PlacePostOnlyBuyNo { .. },
                ActionResult::DryRun { .. },
            ) => {
                tracing::info!("Buy order dry-run");
                self.pending_buy_order_id = Some("dry-run".into());
            }
            (
                Action::PlacePostOnlySellYes { .. } | Action::PlacePostOnlySellNo { .. },
                ActionResult::OrderPlaced { order_id },
            ) => {
                tracing::info!(order_id, "Sell order placed");
                self.pending_sell_order_id = Some(order_id.clone());
            }
            (
                Action::PlacePostOnlySellYes { .. } | Action::PlacePostOnlySellNo { .. },
                ActionResult::Rejected { reason },
            ) => {
                tracing::info!(reason, "Sell order rejected, will retry");
                self.pending_sell_order_id = None;
            }
            (
                Action::PlacePostOnlySellYes { .. } | Action::PlacePostOnlySellNo { .. },
                ActionResult::Error { reason },
            ) => {
                tracing::warn!(reason, "Sell order error");
                self.pending_sell_order_id = None;
            }
            (
                Action::PlacePostOnlySellYes { .. } | Action::PlacePostOnlySellNo { .. },
                ActionResult::DryRun { .. },
            ) => {
                tracing::info!("Sell order dry-run");
                self.pending_sell_order_id = Some("dry-run".into());
            }
            _ => {}
        }
    }

    fn on_fill(&mut self, fill: &FillNotification) {
        match fill.order_side {
            OrderSide::Buy => {
                self.held_side = Some(fill.token_side);
                self.held_size += fill.size;
                self.entry_price = fill.price;
                self.pending_buy_order_id = None;
                tracing::info!(
                    ?fill.token_side,
                    size = fill.size,
                    price = fill.price,
                    total_held = self.held_size,
                    "Buy fill — position opened"
                );
            }
            OrderSide::Sell => {
                self.held_size = (self.held_size - fill.size).max(0.0);
                self.pending_sell_order_id = None;
                if self.held_size <= 0.0 {
                    tracing::info!(
                        ?fill.token_side,
                        price = fill.price,
                        entry = self.entry_price,
                        "Sell fill — position closed"
                    );
                    self.held_side = None;
                    self.entry_price = 0.0;
                } else {
                    tracing::info!(
                        ?fill.token_side,
                        size = fill.size,
                        remaining = self.held_size,
                        "Partial sell fill"
                    );
                }
            }
        }
    }
}
