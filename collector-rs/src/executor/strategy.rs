use super::state::ExecutorState;
use super::config::AppConfig;

#[derive(Debug, Clone)]
pub enum Action {
    PlaceLimitBuyYes { price: f64, size: f64 },
    PlaceLimitBuyNo { price: f64, size: f64 },
    PlaceLimitSellYes { price: f64, size: f64 },
    PlaceLimitSellNo { price: f64, size: f64 },
    CancelAll,
}

pub trait Strategy: Send + Sync {
    fn on_tick(
        &mut self,
        state: &ExecutorState,
        config: &AppConfig,
    ) -> Vec<Action>;
}

pub struct NoopStrategy;

impl Strategy for NoopStrategy {
    fn on_tick(
        &mut self,
        state: &ExecutorState,
        _config: &AppConfig,
    ) -> Vec<Action> {
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
