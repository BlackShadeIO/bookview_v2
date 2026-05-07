pub mod engine;
pub mod market_data;
pub mod order;
pub mod output;
pub mod position;
pub mod strategies;
pub mod strategy;

#[derive(Debug, Clone)]
pub struct SimConfig {
    pub capital: f64,
    pub default_split_size: f64,
    pub min_latency_ms: u64,
    pub max_latency_ms: u64,
    pub hold_cooldown_ms: u64,
    pub order_size: f64,
    pub fill_model: FillModel,
    pub seed: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            capital: 1000.0,
            default_split_size: 100.0,
            min_latency_ms: 300,
            max_latency_ms: 500,
            hold_cooldown_ms: 8000,
            order_size: 50.0,
            fill_model: FillModel::QueueAware,
            seed: 42,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillModel {
    Aggressive,
    Conservative,
    QueueAware,
}

impl FillModel {
    pub fn from_str(s: &str) -> Self {
        match s {
            "aggressive" => Self::Aggressive,
            "conservative" => Self::Conservative,
            _ => Self::QueueAware,
        }
    }
}
