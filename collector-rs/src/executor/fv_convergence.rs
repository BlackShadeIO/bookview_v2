use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::config::MARKET_DURATION;

use super::config::AppConfig;
use super::state::ExecutorState;
use super::strategy::{
    Action, ActionResult, FillNotification, OrderSide, Strategy, TokenSide,
};

const FV_THRESHOLD: f64 = 0.55;
const EDGE_MIN: f64 = 0.05;
const CONVERGENCE: f64 = 0.01;
const STOP_LOSS: f64 = 0.08;
const ACTIVE_WINDOW_SECS: i64 = 180;
const MAX_SELL_ERRORS: u32 = 5;
const MIN_ORDER_SIZE: f64 = 5.0;
const MAX_SPLIT_RETRIES: u32 = 5;

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
    split_retry_count: u32,
    split_retry_after: Option<Instant>,
    register_requested: bool,
    register_confirmed: bool,

    pending_buy_order_id: Option<String>,
    pending_buy_price: f64,
    pending_buy_side: Option<TokenSide>,
    pending_sell_order_id: Option<String>,
    pending_sell_price: f64,

    held_side: Option<TokenSide>,
    held_size: f64,
    entry_price: f64,

    sell_error_count: u32,

    total_bought: f64,
    total_sold: f64,

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
            split_retry_count: 0,
            split_retry_after: None,
            register_requested: false,
            register_confirmed: false,
            pending_buy_order_id: None,
            pending_buy_price: 0.0,
            pending_buy_side: None,
            pending_sell_order_id: None,
            pending_sell_price: 0.0,
            held_side: None,
            held_size: 0.0,
            entry_price: 0.0,
            sell_error_count: 0,
            total_bought: 0.0,
            total_sold: 0.0,
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

    fn truncate_size(s: f64) -> f64 {
        (s * 100.0).floor() / 100.0
    }

    fn update_phase_with_state(&mut self, state: &ExecutorState) {
        let now = Self::now_epoch();
        let elapsed = now - self.market_start_epoch;

        match self.phase {
            Phase::WaitingForData => {}
            Phase::PreMarket => {
                if elapsed >= 0
                    && self.split_confirmed
                    && self.register_confirmed
                    && state.poly_bba.is_some()
                    && state.fair_value.is_some()
                {
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

    fn is_holding(&self) -> bool {
        self.held_side.is_some() && self.held_size > 0.0
    }

    fn tick_pre_market(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();

        if !self.split_requested && !self.split_confirmed {
            if self.split_retry_count >= MAX_SPLIT_RETRIES {
                tracing::error!(retries = self.split_retry_count, "Split failed too many times — giving up");
                self.phase = Phase::Done;
                return vec![];
            }
            if let Some(after) = self.split_retry_after {
                if Instant::now() < after {
                    return vec![];
                }
            }
            tracing::info!(amount = self.split_amount, retry = self.split_retry_count, "Requesting pre-market split");
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

    fn compute_buy_signal(&self, state: &ExecutorState) -> Option<(TokenSide, f64)> {
        let bba = state.poly_bba.as_ref()?;
        let fv = state.fair_value.as_ref()?;

        let fv_yes = fv.fair_yes;
        let fv_no = fv.fair_no;

        if fv_yes > FV_THRESHOLD {
            if let Some(ask) = bba.yes_best_ask {
                if fv_yes - ask >= EDGE_MIN {
                    let price = Self::round_price(ask - 0.01);
                    if price > 0.0 && price < 1.0 {
                        return Some((TokenSide::Yes, price));
                    }
                }
            }
        }

        if fv_no > FV_THRESHOLD {
            if let Some(ask) = bba.no_best_ask {
                if fv_no - ask >= EDGE_MIN {
                    let price = Self::round_price(ask - 0.01);
                    if price > 0.0 && price < 1.0 {
                        return Some((TokenSide::No, price));
                    }
                }
            }
        }

        None
    }

    fn make_buy_action(&self, side: TokenSide, price: f64) -> Action {
        match side {
            TokenSide::Yes => Action::PlacePostOnlyBuyYes {
                price,
                size: self.order_size,
            },
            TokenSide::No => Action::PlacePostOnlyBuyNo {
                price,
                size: self.order_size,
            },
        }
    }

    fn tick_entry(&mut self, state: &ExecutorState) -> Vec<Action> {
        if self.is_holding() {
            return vec![];
        }
        if !self.split_confirmed || !self.register_confirmed {
            return vec![];
        }

        let signal = self.compute_buy_signal(state);

        if let Some(order_id) = &self.pending_buy_order_id {
            match &signal {
                Some((side, price)) if *price != self.pending_buy_price || Some(*side) != self.pending_buy_side => {
                    let fv = state.fair_value.as_ref().unwrap();
                    let fv_val = match side {
                        TokenSide::Yes => fv.fair_yes,
                        TokenSide::No => fv.fair_no,
                    };
                    tracing::info!(
                        old_price = self.pending_buy_price,
                        new_price = price,
                        ?side,
                        fv = fv_val,
                        "Repricing buy order"
                    );
                    let cancel = Action::CancelOrder {
                        order_id: order_id.clone(),
                    };
                    self.pending_buy_order_id = None;
                    self.pending_buy_price = 0.0;
                    self.pending_buy_side = None;
                    return vec![cancel, self.make_buy_action(*side, *price)];
                }
                None => {
                    tracing::info!("Entry conditions lost — cancelling pending buy");
                    let cancel = Action::CancelOrder {
                        order_id: order_id.clone(),
                    };
                    self.pending_buy_order_id = None;
                    self.pending_buy_price = 0.0;
                    self.pending_buy_side = None;
                    return vec![cancel];
                }
                _ => return vec![],
            }
        }

        if let Some((side, price)) = signal {
            let fv = state.fair_value.as_ref().unwrap();
            let bba = state.poly_bba.as_ref().unwrap();
            let (fv_val, ask) = match side {
                TokenSide::Yes => (fv.fair_yes, bba.yes_best_ask.unwrap()),
                TokenSide::No => (fv.fair_no, bba.no_best_ask.unwrap()),
            };
            tracing::info!(
                ?side,
                fv = fv_val,
                ask,
                edge = fv_val - ask,
                buy_price = price,
                "Entry signal: BUY"
            );
            return vec![self.make_buy_action(side, price)];
        }

        vec![]
    }

    fn compute_sell_signal(&self, state: &ExecutorState) -> Option<f64> {
        let bba = state.poly_bba.as_ref()?;
        let fv = state.fair_value.as_ref()?;
        let held_side = self.held_side?;

        let (best_ask, best_bid, fair_val) = match held_side {
            TokenSide::Yes => (bba.yes_best_ask, bba.yes_best_bid, fv.fair_yes),
            TokenSide::No => (bba.no_best_ask, bba.no_best_bid, fv.fair_no),
        };

        let ask = best_ask?;
        let bid = best_bid?;
        let gap = (ask - fair_val).abs();
        if gap <= CONVERGENCE {
            let sell_price = Self::round_price(bid);
            let sell_size = Self::truncate_size(self.held_size);
            if sell_price > 0.0 && sell_price < 1.0 && sell_size > 0.0 {
                return Some(sell_price);
            }
        }
        None
    }

    fn compute_stop_loss_signal(&self, state: &ExecutorState) -> Option<f64> {
        let bba = state.poly_bba.as_ref()?;
        let held_side = self.held_side?;
        if self.entry_price <= 0.0 {
            return None;
        }

        let best_bid = match held_side {
            TokenSide::Yes => bba.yes_best_bid?,
            TokenSide::No => bba.no_best_bid?,
        };

        let unrealized = best_bid - self.entry_price;
        if unrealized < -STOP_LOSS {
            let sell_price = Self::round_price(best_bid);
            let sell_size = Self::truncate_size(self.held_size);
            if sell_price > 0.0 && sell_price < 1.0 && sell_size > 0.0 {
                return Some(sell_price);
            }
        }
        None
    }

    fn make_sell_action(&self, price: f64) -> Action {
        let sell_size = Self::truncate_size(self.held_size);
        match self.held_side.unwrap() {
            TokenSide::Yes => Action::PlaceLimitSellYes {
                price,
                size: sell_size,
            },
            TokenSide::No => Action::PlaceLimitSellNo {
                price,
                size: sell_size,
            },
        }
    }

    fn tick_exit(&mut self, state: &ExecutorState) -> Vec<Action> {
        if !self.is_holding() {
            return vec![];
        }
        if self.sell_error_count >= MAX_SELL_ERRORS {
            return vec![];
        }
        if self.pending_sell_order_id.is_some() {
            return vec![];
        }

        let held_side = self.held_side.unwrap();

        // Stop-loss: exit immediately if unrealized loss exceeds threshold
        if let Some(sell_price) = self.compute_stop_loss_signal(state) {
            let bba = state.poly_bba.as_ref().unwrap();
            let best_bid = match held_side {
                TokenSide::Yes => bba.yes_best_bid.unwrap(),
                TokenSide::No => bba.no_best_bid.unwrap(),
            };
            let sell_size = Self::truncate_size(self.held_size);
            if self.total_sold + sell_size > self.total_bought {
                return vec![];
            }
            let unrealized = best_bid - self.entry_price;
            tracing::warn!(
                ?held_side,
                entry_price = self.entry_price,
                best_bid,
                unrealized,
                sell_price,
                sell_size,
                "STOP LOSS triggered"
            );
            return vec![self.make_sell_action(sell_price)];
        }

        // Convergence exit: sell when price matches FV
        if let Some(sell_price) = self.compute_sell_signal(state) {
            let bba = state.poly_bba.as_ref().unwrap();
            let fv = state.fair_value.as_ref().unwrap();
            let (ask, bid, fair_val) = match held_side {
                TokenSide::Yes => (bba.yes_best_ask.unwrap(), bba.yes_best_bid.unwrap(), fv.fair_yes),
                TokenSide::No => (bba.no_best_ask.unwrap(), bba.no_best_bid.unwrap(), fv.fair_no),
            };
            let sell_size = Self::truncate_size(self.held_size);
            if self.total_sold + sell_size > self.total_bought {
                tracing::error!(
                    total_bought = self.total_bought,
                    total_sold = self.total_sold,
                    sell_size,
                    "Over-sell protection: would sell more than bought"
                );
                return vec![];
            }
            tracing::info!(
                ?held_side,
                best_ask = ask,
                best_bid = bid,
                fair_val,
                gap = (ask - fair_val).abs(),
                sell_price,
                sell_size,
                raw_held = self.held_size,
                "Exit signal: convergence sell at best bid"
            );
            return vec![self.make_sell_action(sell_price)];
        }

        vec![]
    }
}

impl Strategy for FvConvergenceStrategy {
    fn on_tick(&mut self, state: &ExecutorState, _config: &AppConfig) -> Vec<Action> {
        // PreMarket only needs market_info (for condition_id to split/register).
        // Active needs BBA + FV for trading signals.
        if self.phase == Phase::WaitingForData {
            if state.market_info.is_some() {
                let now = Self::now_epoch();
                if now < self.market_start_epoch {
                    tracing::info!("Market info received, entering PreMarket");
                    self.phase = Phase::PreMarket;
                } else if now < self.market_start_epoch + MARKET_DURATION as i64 {
                    tracing::info!("Market info received late, entering PreMarket to split first");
                    self.phase = Phase::PreMarket;
                } else {
                    self.phase = Phase::Done;
                }
            }
            return vec![];
        }

        self.update_phase_with_state(state);

        match self.phase {
            Phase::WaitingForData => vec![],
            Phase::PreMarket => self.tick_pre_market(),
            Phase::Active => {
                let exit_actions = self.tick_exit(state);
                if !exit_actions.is_empty() {
                    return exit_actions;
                }
                self.tick_entry(state)
            }
            Phase::WindDown => {
                if let Some(order_id) = self.pending_buy_order_id.take() {
                    tracing::info!("WindDown — cancelling pending buy");
                    self.pending_buy_price = 0.0;
                    self.pending_buy_side = None;
                    return vec![Action::CancelOrder { order_id }];
                }
                self.tick_exit(state)
            }
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
                self.split_retry_count += 1;
                let backoff_secs = if reason.contains("429") || reason.contains("quota exceeded") {
                    60
                } else {
                    (5u64).saturating_mul(self.split_retry_count as u64).min(30)
                };
                tracing::warn!(reason, retry = self.split_retry_count, backoff_secs, "Split failed, backing off");
                self.split_retry_after = Some(Instant::now() + std::time::Duration::from_secs(backoff_secs));
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
            (Action::CancelOrder { order_id }, _) => {
                tracing::info!(order_id, "Order cancelled");
            }
            (
                Action::PlacePostOnlyBuyYes { price, .. },
                ActionResult::OrderPlaced { order_id },
            ) => {
                tracing::info!(order_id, price, "Buy YES order placed");
                self.pending_buy_order_id = Some(order_id.clone());
                self.pending_buy_price = *price;
                self.pending_buy_side = Some(TokenSide::Yes);
            }
            (
                Action::PlacePostOnlyBuyNo { price, .. },
                ActionResult::OrderPlaced { order_id },
            ) => {
                tracing::info!(order_id, price, "Buy NO order placed");
                self.pending_buy_order_id = Some(order_id.clone());
                self.pending_buy_price = *price;
                self.pending_buy_side = Some(TokenSide::No);
            }
            (
                Action::PlacePostOnlyBuyYes { .. } | Action::PlacePostOnlyBuyNo { .. },
                ActionResult::Rejected { reason },
            ) => {
                tracing::info!(reason, "Buy order rejected, will retry");
                self.pending_buy_order_id = None;
                self.pending_buy_price = 0.0;
                self.pending_buy_side = None;
            }
            (
                Action::PlacePostOnlyBuyYes { .. } | Action::PlacePostOnlyBuyNo { .. },
                ActionResult::Error { reason },
            ) => {
                tracing::warn!(reason, "Buy order error");
                self.pending_buy_order_id = None;
                self.pending_buy_price = 0.0;
                self.pending_buy_side = None;
            }
            (
                Action::PlacePostOnlyBuyYes { price, .. }
                | Action::PlacePostOnlyBuyNo { price, .. },
                ActionResult::DryRun { .. },
            ) => {
                tracing::info!(price, "Buy order dry-run");
                self.pending_buy_order_id = Some("dry-run".into());
                self.pending_buy_price = *price;
            }
            (
                Action::PlaceLimitSellYes { price, .. }
                | Action::PlaceLimitSellNo { price, .. },
                ActionResult::OrderPlaced { order_id },
            ) => {
                tracing::info!(order_id, price, "Sell order placed (market)");
                self.pending_sell_order_id = Some(order_id.clone());
                self.pending_sell_price = *price;
                self.sell_error_count = 0;
            }
            (
                Action::PlaceLimitSellYes { .. } | Action::PlaceLimitSellNo { .. },
                ActionResult::Error { reason },
            ) => {
                self.sell_error_count += 1;
                tracing::warn!(reason, errors = self.sell_error_count, max = MAX_SELL_ERRORS, "Sell order error");
                self.pending_sell_order_id = None;
                self.pending_sell_price = 0.0;
                if self.sell_error_count >= MAX_SELL_ERRORS {
                    tracing::error!("Sell error limit reached — abandoning exit attempts");
                }
            }
            (
                Action::PlaceLimitSellYes { price, .. }
                | Action::PlaceLimitSellNo { price, .. },
                ActionResult::DryRun { .. },
            ) => {
                tracing::info!(price, "Sell order dry-run");
                self.pending_sell_order_id = Some("dry-run".into());
                self.pending_sell_price = *price;
            }
            _ => {}
        }
    }

    fn on_fill(&mut self, fill: &FillNotification) {
        if fill.size <= 0.0 {
            tracing::warn!(order_id = %fill.order_id, "Ignoring zero-size fill");
            return;
        }
        match fill.order_side {
            OrderSide::Buy => {
                self.total_bought += fill.size;
                let prev_size = self.held_size;
                self.held_side = Some(fill.token_side);
                self.held_size += fill.size;
                if prev_size > 0.0 {
                    self.entry_price = (self.entry_price * prev_size + fill.price * fill.size) / self.held_size;
                } else {
                    self.entry_price = fill.price;
                }
                self.pending_buy_order_id = None;
                self.pending_buy_price = 0.0;
                self.pending_buy_side = None;
                tracing::info!(
                    ?fill.token_side,
                    size = fill.size,
                    price = fill.price,
                    avg_entry = self.entry_price,
                    total_held = self.held_size,
                    "Buy fill — position opened"
                );
            }
            OrderSide::Sell => {
                self.total_sold += fill.size;
                self.held_size = (self.held_size - fill.size).max(0.0);
                self.pending_sell_order_id = None;
                self.pending_sell_price = 0.0;
                self.sell_error_count = 0;
                if self.held_size < MIN_ORDER_SIZE {
                    if self.held_size > 0.0 {
                        tracing::info!(
                            ?fill.token_side,
                            price = fill.price,
                            entry = self.entry_price,
                            dust = self.held_size,
                            "Sell fill — position closed (dust below min order size)"
                        );
                    } else {
                        tracing::info!(
                            ?fill.token_side,
                            price = fill.price,
                            entry = self.entry_price,
                            "Sell fill — position closed"
                        );
                    }
                    self.held_side = None;
                    self.held_size = 0.0;
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
