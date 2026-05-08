use super::config::AppConfig;
use super::state::ExecutorState;

#[derive(Debug, Clone)]
pub enum Action {
    PlaceLimitBuyYes { price: f64, size: f64 },
    PlaceLimitBuyNo { price: f64, size: f64 },
    PlaceLimitSellYes { price: f64, size: f64 },
    PlaceLimitSellNo { price: f64, size: f64 },
    PlacePostOnlyBuyYes { price: f64, size: f64 },
    PlacePostOnlyBuyNo { price: f64, size: f64 },
    PlacePostOnlySellYes { price: f64, size: f64 },
    PlacePostOnlySellNo { price: f64, size: f64 },
    SplitUsdc { amount: u64 },
    RegisterPositions,
    CancelOrder { order_id: String },
    CancelAll,
}

#[derive(Debug, Clone)]
pub enum ActionResult {
    OrderPlaced { order_id: String },
    Rejected { reason: String },
    Success { detail: String },
    Error { reason: String },
    DryRun { description: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSide {
    Yes,
    No,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct FillNotification {
    pub order_id: String,
    pub token_side: TokenSide,
    pub order_side: OrderSide,
    pub price: f64,
    pub size: f64,
}

pub trait Strategy: Send + Sync {
    fn on_tick(&mut self, state: &ExecutorState, config: &AppConfig) -> Vec<Action>;

    fn on_action_result(&mut self, _action: &Action, _result: &ActionResult) {}

    fn on_fill(&mut self, _fill: &FillNotification) {}
}

pub struct NoopStrategy;

impl Strategy for NoopStrategy {
    fn on_tick(&mut self, state: &ExecutorState, _config: &AppConfig) -> Vec<Action> {
        if let (Some(bba), Some(fv)) = (&state.poly_bba, &state.fair_value) {
            tracing::info!(
                yes_bid = ?bba.yes_best_bid,
                yes_ask = ?bba.yes_best_ask,
                fair_yes = fv.fair_yes,
                lstm_fair_yes = ?fv.lstm_fair_yes,
                tau = fv.tau,
                "Executor tick"
            );
        }
        vec![]
    }
}
