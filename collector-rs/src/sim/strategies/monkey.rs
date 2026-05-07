use crate::sim::market_data::{MarketState, SimEvent, TokenSide};
use crate::sim::order::{Order, OrderSide};
use crate::sim::position::Position;
use crate::sim::strategy::{Strategy, StrategyAction};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataSource {
    BtcPrice,
    BtcDepth,
    BtcTrades,
    FvAnalytical,
    FvLstm,
    PolyBook,
    ZScore,
    TradeImbalance,
    BookImbalance,
    Sigma,
    Tau,
}

impl DataSource {
    pub const ALL: &[DataSource] = &[
        Self::BtcPrice, Self::BtcDepth, Self::BtcTrades,
        Self::FvAnalytical, Self::FvLstm, Self::PolyBook,
        Self::ZScore, Self::TradeImbalance, Self::BookImbalance,
        Self::Sigma, Self::Tau,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::BtcPrice => "btc_price",
            Self::BtcDepth => "btc_depth",
            Self::BtcTrades => "btc_trades",
            Self::FvAnalytical => "fv_analytical",
            Self::FvLstm => "fv_lstm",
            Self::PolyBook => "poly_book",
            Self::ZScore => "z_score",
            Self::TradeImbalance => "trade_imbalance",
            Self::BookImbalance => "book_imbalance",
            Self::Sigma => "sigma",
            Self::Tau => "tau",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MonkeyDNA {
    pub sources: Vec<DataSource>,
    pub weights: Vec<f64>,
    pub threshold: f64,
    pub lookback_ms: u64,
    pub tick_interval_ms: u64,
    pub order_offset_ticks: u8,
    pub order_size: f64,
    pub split_size: f64,
    pub use_split_for_exit: bool,
    pub cancel_stale_ms: u64,
    pub depth_levels: usize,
}

pub struct MonkeyStrategy {
    dna: MonkeyDNA,
    last_eval_ms: i64,
    btc_price_history: Vec<(i64, f64)>,
    btc_trade_history: Vec<(i64, f64, bool)>, // (ts, qty, is_buy)
    market_end_ms: i64,
}

impl MonkeyStrategy {
    pub fn new(dna: MonkeyDNA) -> Self {
        Self {
            dna,
            last_eval_ms: 0,
            btc_price_history: Vec::new(),
            btc_trade_history: Vec::new(),
            market_end_ms: 0,
        }
    }

    fn extract_value(&self, source: DataSource, state: &MarketState) -> f64 {
        match source {
            DataSource::BtcPrice => {
                if self.btc_price_history.is_empty() || state.btc_mid == 0.0 { return 0.0; }
                let cutoff = state.ts_ms - self.dna.lookback_ms as i64;
                let old = self.btc_price_history.iter().rev()
                    .find(|(ts, _)| *ts <= cutoff)
                    .map(|(_, p)| *p)
                    .unwrap_or(self.btc_price_history[0].1);
                if old == 0.0 { return 0.0; }
                (state.btc_mid - old) / old
            }
            DataSource::BtcDepth => {
                let n = self.dna.depth_levels;
                let bid_vol: f64 = state.btc_depth_bids.iter().take(n).map(|l| l.size).sum();
                let ask_vol: f64 = state.btc_depth_asks.iter().take(n).map(|l| l.size).sum();
                let total = bid_vol + ask_vol;
                if total == 0.0 { return 0.0; }
                (bid_vol - ask_vol) / total
            }
            DataSource::BtcTrades => {
                let cutoff = state.ts_ms - self.dna.lookback_ms as i64;
                let mut buy_vol = 0.0f64;
                let mut sell_vol = 0.0f64;
                for (ts, qty, is_buy) in self.btc_trade_history.iter().rev() {
                    if *ts < cutoff { break; }
                    if *is_buy { buy_vol += qty; } else { sell_vol += qty; }
                }
                let total = buy_vol + sell_vol;
                if total == 0.0 { return 0.0; }
                (buy_vol - sell_vol) / total
            }
            DataSource::FvAnalytical => {
                state.fair_yes.unwrap_or(0.5) - 0.5
            }
            DataSource::FvLstm => {
                state.lstm_fair_yes.unwrap_or(0.5) - 0.5
            }
            DataSource::PolyBook => {
                let bid_size: f64 = state.up_book.as_ref()
                    .map(|b| b.bids.iter().take(5).map(|l| l.size).sum())
                    .unwrap_or(0.0);
                let ask_size: f64 = state.up_book.as_ref()
                    .map(|b| b.asks.iter().take(5).map(|l| l.size).sum())
                    .unwrap_or(0.0);
                let total = bid_size + ask_size;
                if total == 0.0 { return 0.0; }
                (bid_size - ask_size) / total
            }
            DataSource::ZScore => {
                state.z_score.unwrap_or(0.0)
            }
            DataSource::TradeImbalance => {
                state.trade_imbalance.unwrap_or(0.0)
            }
            DataSource::BookImbalance => {
                state.book_imbalance.unwrap_or(0.5) - 0.5
            }
            DataSource::Sigma => {
                let sig = state.sigma_returns.unwrap_or(0.0);
                (sig - 0.005).min(1.0).max(-1.0)
            }
            DataSource::Tau => {
                (state.tau - 150.0) / 150.0
            }
        }
    }

    fn compute_signal(&self, state: &MarketState) -> f64 {
        let mut signal = 0.0;
        for (src, weight) in self.dna.sources.iter().zip(self.dna.weights.iter()) {
            signal += self.extract_value(*src, state) * weight;
        }
        signal
    }
}

impl Strategy for MonkeyStrategy {
    fn name(&self) -> String {
        let sources: Vec<&str> = self.dna.sources.iter().map(|s| s.label()).collect();
        format!("monkey[{}]", sources.join("+"))
    }

    fn split_size(&self) -> f64 {
        self.dna.split_size
    }

    fn on_market_start(&mut self, _start_ms: i64, end_ms: i64) {
        self.market_end_ms = end_ms;
    }

    fn on_event(
        &mut self,
        event: &SimEvent,
        state: &MarketState,
        position: &Position,
        live_orders: &[Order],
    ) -> Vec<StrategyAction> {
        // Track BTC price/trade history for lookback signals
        match event {
            SimEvent::BtcTicker { ts_ms, mid, .. } => {
                self.btc_price_history.push((*ts_ms, *mid));
                if self.btc_price_history.len() > 200 {
                    self.btc_price_history.drain(..100);
                }
            }
            SimEvent::BtcTrade { ts_ms, qty, is_buy, .. } => {
                self.btc_trade_history.push((*ts_ms, *qty, *is_buy));
                if self.btc_trade_history.len() > 500 {
                    self.btc_trade_history.drain(..250);
                }
            }
            _ => {}
        }

        let now = state.ts_ms;
        if now - self.last_eval_ms < self.dna.tick_interval_ms as i64 {
            return Vec::new();
        }
        self.last_eval_ms = now;

        let mut actions = Vec::new();

        // Cancel stale orders
        for order in live_orders {
            if now - order.submitted_at_ms > self.dna.cancel_stale_ms as i64 {
                actions.push(StrategyAction::CancelOrder { order_id: order.id });
            }
        }

        // Don't place new orders in last 5s (let settlement happen)
        if state.tau < 5.0 {
            return actions;
        }

        let signal = self.compute_signal(state);

        if signal.abs() > self.dna.threshold {
            let offset = self.dna.order_offset_ticks as f64 * 0.01;

            if signal > 0.0 {
                // Buy UP
                if let Some(best_ask) = state.up_best_ask() {
                    let price = ((best_ask - offset) * 100.0).round() / 100.0;
                    if price > 0.0 {
                        actions.push(StrategyAction::PlaceOrder {
                            token: TokenSide::Up,
                            side: OrderSide::Bid,
                            price,
                            size: self.dna.order_size,
                        });
                    }
                }
                // Exit DOWN from inventory (Split gives us immediate liquidity)
                if position.down.sellable > self.dna.order_size {
                    if let Some(best_bid) = state.down_best_bid() {
                        let price = ((best_bid + offset) * 100.0).round() / 100.0;
                        if price < 1.0 {
                            actions.push(StrategyAction::PlaceOrder {
                                token: TokenSide::Down,
                                side: OrderSide::Ask,
                                price,
                                size: self.dna.order_size,
                            });
                        }
                    }
                }
            } else {
                // Buy DOWN
                if let Some(best_ask) = state.down_best_ask() {
                    let price = ((best_ask - offset) * 100.0).round() / 100.0;
                    if price > 0.0 {
                        actions.push(StrategyAction::PlaceOrder {
                            token: TokenSide::Down,
                            side: OrderSide::Bid,
                            price,
                            size: self.dna.order_size,
                        });
                    }
                }
                // Exit UP from inventory (Split gives us immediate liquidity)
                if position.up.sellable > self.dna.order_size {
                    if let Some(best_bid) = state.up_best_bid() {
                        let price = ((best_bid + offset) * 100.0).round() / 100.0;
                        if price < 1.0 {
                            actions.push(StrategyAction::PlaceOrder {
                                token: TokenSide::Up,
                                side: OrderSide::Ask,
                                price,
                                size: self.dna.order_size,
                            });
                        }
                    }
                }
            }
        }

        actions
    }
}

// Generate N monkeys with combinatorial data source configurations
pub fn generate_monkeys(count: usize, seed: u64) -> Vec<MonkeyStrategy> {
    let mut monkeys = Vec::new();
    let mut rng = seed;

    let sources = DataSource::ALL;
    let n_sources = sources.len();

    // Single-source monkeys (11 × 5 params = 55)
    for &src in sources {
        for _ in 0..5 {
            monkeys.push(MonkeyStrategy::new(random_dna(&[src], &mut rng)));
            if monkeys.len() >= count { return monkeys; }
        }
    }

    // Pair combos
    for i in 0..n_sources {
        for j in (i + 1)..n_sources {
            for _ in 0..3 {
                monkeys.push(MonkeyStrategy::new(random_dna(&[sources[i], sources[j]], &mut rng)));
                if monkeys.len() >= count { return monkeys; }
            }
        }
    }

    // Triple combos
    for i in 0..n_sources {
        for j in (i + 1)..n_sources {
            for k in (j + 1)..n_sources {
                monkeys.push(MonkeyStrategy::new(random_dna(&[sources[i], sources[j], sources[k]], &mut rng)));
                if monkeys.len() >= count { return monkeys; }
            }
        }
    }

    // Quad combos until we hit count
    for i in 0..n_sources {
        for j in (i + 1)..n_sources {
            for k in (j + 1)..n_sources {
                for l in (k + 1)..n_sources {
                    monkeys.push(MonkeyStrategy::new(random_dna(&[sources[i], sources[j], sources[k], sources[l]], &mut rng)));
                    if monkeys.len() >= count { return monkeys; }
                }
            }
        }
    }

    monkeys
}

fn random_dna(srcs: &[DataSource], rng: &mut u64) -> MonkeyDNA {
    let weights: Vec<f64> = srcs.iter().map(|_| rand_f64(rng) * 2.0 - 1.0).collect();
    let order_size = 5.0 + rand_f64(rng) * 45.0;
    MonkeyDNA {
        sources: srcs.to_vec(),
        weights,
        threshold: 0.01 + rand_f64(rng) * 0.09,
        lookback_ms: 500 + (rand_f64(rng) * 4500.0) as u64,
        tick_interval_ms: 200 + (rand_f64(rng) * 1800.0) as u64,
        order_offset_ticks: 1 + (rand_f64(rng) * 4.0) as u8,
        order_size,
        // Split is always meaningful — it's risk-neutral (always get $1 back per pair at settlement)
        // Gives immediate sellable inventory for exits without 8s cooldown
        split_size: 100.0 + rand_f64(rng) * 100.0, // 100-200 tokens always
        use_split_for_exit: true, // always use inventory — that's the whole point of splitting
        cancel_stale_ms: 3000 + (rand_f64(rng) * 12000.0) as u64,
        depth_levels: 3 + (rand_f64(rng) * 17.0) as usize,
    }
}

fn rand_f64(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (*state >> 11) as f64 / (1u64 << 53) as f64
}
