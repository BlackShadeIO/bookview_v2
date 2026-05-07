use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::config::*;
use crate::lstm_inference::LstmPredictor;
use crate::types::*;
use crate::writer::{SeqCounter, WriterHandle};

// ── Channel infrastructure ───────────────────────────────────────────────

pub struct FairValueFeeds {
    pub bba: watch::Sender<Option<BbaSnapshot>>,
    pub depth: watch::Sender<Option<FvDepthSnapshot>>,
    pub trade: watch::Sender<Option<FvTradeSnapshot>>,
    pub strike: watch::Sender<Option<FvStrikeInfo>>,
}

pub struct FairValueReceivers {
    pub bba: watch::Receiver<Option<BbaSnapshot>>,
    pub depth: watch::Receiver<Option<FvDepthSnapshot>>,
    pub trade: watch::Receiver<Option<FvTradeSnapshot>>,
    pub strike: watch::Receiver<Option<FvStrikeInfo>>,
}

impl FairValueFeeds {
    pub fn create() -> (Self, FairValueReceivers) {
        let (bba_tx, bba_rx) = watch::channel(None);
        let (depth_tx, depth_rx) = watch::channel(None);
        let (trade_tx, trade_rx) = watch::channel(None);
        let (strike_tx, strike_rx) = watch::channel(None);
        (
            Self {
                bba: bba_tx,
                depth: depth_tx,
                trade: trade_tx,
                strike: strike_tx,
            },
            FairValueReceivers {
                bba: bba_rx,
                depth: depth_rx,
                trade: trade_rx,
                strike: strike_rx,
            },
        )
    }
}

// ── Parameters ───────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct FairValueParams {
    pub microprice_weight: f64,
    pub depth_weight: f64,
    pub depth_levels: usize,
    pub imbalance_levels: usize,
    pub trade_halflife_ms: f64,
    pub drift_coeff: f64,
    pub vol_floor: f64,
    pub vol_timescale: VolTimescale,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VolTimescale {
    Rv1s,
    Rv5s,
    Rv15s,
}

impl Default for FairValueParams {
    fn default() -> Self {
        Self {
            microprice_weight: FV_MICROPRICE_WEIGHT,
            depth_weight: FV_DEPTH_WEIGHT,
            depth_levels: FV_DEPTH_LEVELS,
            imbalance_levels: FV_IMBALANCE_LEVELS,
            trade_halflife_ms: FV_TRADE_HALFLIFE_MS,
            drift_coeff: FV_DRIFT_COEFF,
            vol_floor: FV_VOL_FLOOR,
            vol_timescale: VolTimescale::Rv15s,
        }
    }
}

// ── Normal CDF (Abramowitz-Stegun) ──────────────────────────────────────

pub fn normal_cdf(x: f64) -> f64 {
    if x < -8.0 {
        return 0.0;
    }
    if x > 8.0 {
        return 1.0;
    }

    let abs_x = x.abs();
    // A&S 7.1.26 for erfc: erfc(u) ≈ poly(t)*exp(-u²) where t = 1/(1+p*u)
    // Φ(z) = 0.5*erfc(-z/√2), so we evaluate erfc(|z|/√2)
    let u = abs_x * std::f64::consts::FRAC_1_SQRT_2;
    let t = 1.0 / (1.0 + 0.3275911 * u);
    let poly = ((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t
        + 0.254829592)
        * t;
    let tail = 0.5 * poly * (-abs_x * abs_x / 2.0).exp();

    if x >= 0.0 {
        1.0 - tail
    } else {
        tail
    }
}

// ── Pure engine (no async, reusable in backtester) ───────────────────────

pub struct FairValueEngine {
    bid_price: f64,
    bid_qty: f64,
    ask_price: f64,
    ask_qty: f64,
    microprice: f64,

    depth_microprice: f64,
    depth_best_bid: f64,
    depth_best_ask: f64,
    depth_stale: bool,
    book_imbalance: f64,

    prev_mid: f64,
    prev_mid_ts: i64,
    vol_ewma_1s: f64,
    vol_ewma_5s: f64,
    vol_ewma_15s: f64,

    trade_imbalance_ewma: f64,
    last_trade_ts: i64,

    params: FairValueParams,
    ready: bool,
    lstm: Option<LstmPredictor>,
}

impl FairValueEngine {
    pub fn new(params: FairValueParams, lstm: Option<LstmPredictor>) -> Self {
        Self {
            bid_price: 0.0,
            bid_qty: 0.0,
            ask_price: 0.0,
            ask_qty: 0.0,
            microprice: 0.0,

            depth_microprice: 0.0,
            depth_best_bid: 0.0,
            depth_best_ask: 0.0,
            depth_stale: false,
            book_imbalance: 0.5,

            prev_mid: 0.0,
            prev_mid_ts: 0,
            vol_ewma_1s: params.vol_floor,
            vol_ewma_5s: params.vol_floor,
            vol_ewma_15s: params.vol_floor,

            trade_imbalance_ewma: 0.0,
            last_trade_ts: 0,

            params,
            ready: false,
            lstm,
        }
    }

    pub fn on_book_ticker(
        &mut self,
        ts_ms: i64,
        bid: f64,
        bid_qty: f64,
        ask: f64,
        ask_qty: f64,
    ) {
        self.bid_price = bid;
        self.bid_qty = bid_qty;
        self.ask_price = ask;
        self.ask_qty = ask_qty;

        let total_qty = bid_qty + ask_qty;
        self.microprice = if total_qty > 0.0 {
            (bid * ask_qty + ask * bid_qty) / total_qty
        } else {
            (bid + ask) / 2.0
        };

        if self.depth_best_bid > 0.0 && self.depth_best_ask > 0.0 {
            self.depth_stale = bid < self.depth_best_bid
                || ask > self.depth_best_ask
                || bid > self.depth_best_ask
                || ask < self.depth_best_bid;
        }

        let mid = (bid + ask) / 2.0;
        if self.prev_mid > 0.0 && self.prev_mid_ts > 0 {
            let dt = (ts_ms - self.prev_mid_ts) as f64 / 1000.0;
            if dt > 0.0 && dt < 5.0 {
                let ret = (mid / self.prev_mid).ln();
                let instant_var = (ret * ret) / dt;

                let alpha_1 = 1.0 - (-dt / 1.0).exp();
                let alpha_5 = 1.0 - (-dt / 5.0).exp();
                let alpha_15 = 1.0 - (-dt / 15.0).exp();

                self.vol_ewma_1s = self.vol_ewma_1s * (1.0 - alpha_1) + instant_var * alpha_1;
                self.vol_ewma_5s = self.vol_ewma_5s * (1.0 - alpha_5) + instant_var * alpha_5;
                self.vol_ewma_15s = self.vol_ewma_15s * (1.0 - alpha_15) + instant_var * alpha_15;
            }
        }
        self.prev_mid = mid;
        self.prev_mid_ts = ts_ms;
        self.ready = true;
    }

    pub fn on_depth(&mut self, bids: &[(f64, f64)], asks: &[(f64, f64)]) {
        if let Some(&(p, _)) = bids.first() {
            self.depth_best_bid = p;
        }
        if let Some(&(p, _)) = asks.first() {
            self.depth_best_ask = p;
        }
        self.depth_stale = false;

        let top_bids = &bids[..bids.len().min(self.params.depth_levels)];
        let top_asks = &asks[..asks.len().min(self.params.depth_levels)];

        let mut total_bid_qty = 0.0;
        let mut total_ask_qty = 0.0;
        let mut vwap_bid_num = 0.0;
        let mut vwap_ask_num = 0.0;

        for &(price, qty) in top_bids {
            total_bid_qty += qty;
            vwap_bid_num += price * qty;
        }
        for &(price, qty) in top_asks {
            total_ask_qty += qty;
            vwap_ask_num += price * qty;
        }

        if total_bid_qty > 0.0 && total_ask_qty > 0.0 {
            let vwap_bid = vwap_bid_num / total_bid_qty;
            let vwap_ask = vwap_ask_num / total_ask_qty;
            self.depth_microprice =
                (vwap_bid * total_ask_qty + vwap_ask * total_bid_qty)
                    / (total_bid_qty + total_ask_qty);
        }

        let imb_bids = &bids[..bids.len().min(self.params.imbalance_levels)];
        let imb_asks = &asks[..asks.len().min(self.params.imbalance_levels)];
        let bid_depth: f64 = imb_bids.iter().map(|&(_, q)| q).sum();
        let ask_depth: f64 = imb_asks.iter().map(|&(_, q)| q).sum();
        let total_depth = bid_depth + ask_depth;
        self.book_imbalance = if total_depth > 0.0 {
            bid_depth / total_depth
        } else {
            0.5
        };
    }

    pub fn on_trade(&mut self, ts_ms: i64, qty: f64, is_buy: bool) {
        let signed = if is_buy { qty } else { -qty };
        if self.last_trade_ts > 0 {
            let dt = (ts_ms - self.last_trade_ts) as f64;
            let decay = (-dt / self.params.trade_halflife_ms).exp();
            self.trade_imbalance_ewma = self.trade_imbalance_ewma * decay + signed;
        } else {
            self.trade_imbalance_ewma = signed;
        }
        self.last_trade_ts = ts_ms;
    }

    pub fn sample(
        &mut self,
        ts_ms: i64,
        strike: f64,
        expiry_ms: i64,
    ) -> Option<FairValueSnapshot> {
        if !self.ready || self.microprice <= 0.0 {
            return None;
        }

        let s_now = if self.depth_stale || self.depth_microprice == 0.0 {
            self.microprice
        } else {
            self.params.microprice_weight * self.microprice
                + self.params.depth_weight * self.depth_microprice
        };

        let tau = ((expiry_ms - ts_ms) as f64 / 1000.0).max(0.0);
        if tau <= 0.0 {
            return None;
        }

        let vol = match self.params.vol_timescale {
            VolTimescale::Rv1s => self.vol_ewma_1s,
            VolTimescale::Rv5s => self.vol_ewma_5s,
            VolTimescale::Rv15s => self.vol_ewma_15s,
        }
        .max(self.params.vol_floor);
        let sigma_returns = (vol * tau.max(0.001)).sqrt();
        let sigma_dollars = s_now * sigma_returns;
        let mu = self.params.drift_coeff * self.trade_imbalance_ewma;

        let z = ((strike / s_now).ln() - mu / s_now) / sigma_returns;
        let fair_yes = (1.0 - normal_cdf(z)).clamp(0.0, 1.0);

        let (lstm_fair_yes, lstm_fair_no) = if self.lstm.is_some() {
            let features = self.build_feature_vector(
                tau, s_now, strike, sigma_returns, sigma_dollars, z, fair_yes,
            );
            let pred = self.lstm.as_mut().unwrap().predict(&features) as f64;
            (Some(pred), Some(1.0 - pred))
        } else {
            (None, None)
        };

        Some(FairValueSnapshot {
            tau,
            microprice: s_now,
            depth_microprice: self.depth_microprice,
            blended_spot: s_now,
            strike_price: strike,
            rv_1s: self.vol_ewma_1s,
            rv_5s: self.vol_ewma_5s,
            rv_15s: self.vol_ewma_15s,
            sigma_returns,
            sigma_dollars,
            trade_imbalance: self.trade_imbalance_ewma,
            book_imbalance: self.book_imbalance,
            mu,
            z_score: z,
            fair_yes,
            fair_no: 1.0 - fair_yes,
            lstm_fair_yes,
            lstm_fair_no,
        })
    }

    fn build_feature_vector(
        &self,
        tau: f64,
        s_now: f64,
        strike: f64,
        sigma_returns: f64,
        sigma_dollars: f64,
        z_score: f64,
        fair_yes: f64,
    ) -> [f32; 14] {
        let moneyness = if strike > 0.0 {
            (s_now - strike) / strike
        } else {
            0.0
        };
        let depth_spread = if s_now > 0.0 {
            (self.depth_microprice - s_now) / s_now
        } else {
            0.0
        };
        let rv_ratio = self.vol_ewma_1s / self.vol_ewma_5s.max(1e-20);

        [
            tau as f32,
            moneyness as f32,
            self.vol_ewma_1s as f32,
            self.vol_ewma_5s as f32,
            self.vol_ewma_15s as f32,
            sigma_returns as f32,
            self.trade_imbalance_ewma as f32,
            self.book_imbalance as f32,
            (self.params.drift_coeff * self.trade_imbalance_ewma) as f32,
            z_score as f32,
            depth_spread as f32,
            sigma_dollars as f32,
            rv_ratio as f32,
            fair_yes as f32,
        ]
    }
}

// ── Async task ───────────────────────────────────────────────────────────

pub async fn run_fair_value(
    market_start: i64,
    bba_rx: watch::Receiver<Option<BbaSnapshot>>,
    depth_rx: watch::Receiver<Option<FvDepthSnapshot>>,
    trade_rx: watch::Receiver<Option<FvTradeSnapshot>>,
    strike_rx: watch::Receiver<Option<FvStrikeInfo>>,
    writer: WriterHandle,
    cancel: CancellationToken,
    params: FairValueParams,
    model_dir: Option<PathBuf>,
    fv_snapshot_tx: Option<watch::Sender<Option<FairValueSnapshot>>>,
) {
    let seq = Arc::new(SeqCounter::new());
    let lstm = model_dir.and_then(|dir| crate::lstm_inference::load_model_if_available(&dir));
    let mut engine = FairValueEngine::new(params, lstm);
    let mut interval = tokio::time::interval(Duration::from_millis(FV_TICK_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let market_start_ms = market_start * 1000;
    let market_end_ms = market_start_ms + (MARKET_DURATION as i64) * 1000;

    let mut last_bba_ts: i64 = 0;
    let mut last_depth_ts: i64 = 0;
    let mut last_trade_ts: i64 = 0;
    let mut active_logged = false;

    let _ = writer.send(make_system_line(
        market_start,
        "fair_value",
        seq.next(),
        json!({"market_start": market_start, "market_end_ms": market_end_ms}),
        "fv_warmup_started",
    ));

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = cancel.cancelled() => break,
        }

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        if now_ms > market_end_ms {
            let _ = writer.send(make_system_line(
                market_start,
                "fair_value",
                seq.next(),
                json!({"reason": "market_expired"}),
                "fv_stopped",
            ));
            break;
        }

        // Drain latest data from channels
        if let Some(snap) = bba_rx.borrow().as_ref() {
            if snap.ts_recv_ms > last_bba_ts {
                engine.on_book_ticker(
                    snap.ts_recv_ms,
                    snap.bid_price,
                    snap.bid_qty,
                    snap.ask_price,
                    snap.ask_qty,
                );
                last_bba_ts = snap.ts_recv_ms;
            }
        }

        if let Some(snap) = depth_rx.borrow().as_ref() {
            if snap.ts_recv_ms > last_depth_ts {
                engine.on_depth(&snap.bids, &snap.asks);
                last_depth_ts = snap.ts_recv_ms;
            }
        }

        if let Some(snap) = trade_rx.borrow().as_ref() {
            if snap.ts_recv_ms > last_trade_ts {
                engine.on_trade(snap.ts_recv_ms, snap.qty, snap.is_buy);
                last_trade_ts = snap.ts_recv_ms;
            }
        }

        // Pre-start: warm up only
        if now_ms < market_start_ms {
            continue;
        }

        // Active: need strike to compute FV
        let strike_info = match strike_rx.borrow().clone() {
            Some(info) => info,
            None => continue,
        };

        if !active_logged {
            let _ = writer.send(make_system_line(
                market_start,
                "fair_value",
                seq.next(),
                json!({"strike_price": strike_info.strike_price}),
                "fv_active",
            ));
            active_logged = true;
        }

        if let Some(snap) = engine.sample(now_ms, strike_info.strike_price, market_end_ms) {
            if let Some(ref tx) = fv_snapshot_tx {
                let _ = tx.send(Some(snap.clone()));
            }
            let _ = writer.send(make_line(
                market_start,
                "fair_value",
                "fv_sample",
                seq.next(),
                serde_json::Value::Null,
                RawJson::from_serialize(&snap),
            ));
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_cdf_known_values() {
        let cases = [
            (-3.0, 0.00135),
            (-2.0, 0.02275),
            (-1.0, 0.15866),
            (0.0, 0.5),
            (1.0, 0.84134),
            (2.0, 0.97725),
            (3.0, 0.99865),
        ];
        for (x, expected) in cases {
            let got = normal_cdf(x);
            assert!(
                (got - expected).abs() < 1e-4,
                "normalCDF({x}) = {got}, expected ~{expected}"
            );
        }
    }

    #[test]
    fn test_normal_cdf_symmetry() {
        for x in [0.5, 1.0, 2.0, 3.0] {
            let sum = normal_cdf(x) + normal_cdf(-x);
            assert!(
                (sum - 1.0).abs() < 1e-10,
                "CDF({x}) + CDF(-{x}) = {sum}, should be 1.0"
            );
        }
    }

    #[test]
    fn test_microprice() {
        let params = FairValueParams::default();
        let mut engine = FairValueEngine::new(params, None);
        engine.on_book_ticker(1000, 100.0, 2.0, 102.0, 3.0);
        // microprice = (100*3 + 102*2) / 5 = (300+204)/5 = 504/5 = 100.8
        assert!((engine.microprice - 100.8).abs() < 1e-10);
    }

    #[test]
    fn test_at_the_money_fair_yes() {
        let params = FairValueParams::default();
        let mut engine = FairValueEngine::new(params, None);
        engine.on_book_ticker(0, 100.0, 1.0, 100.0, 1.0);

        let snap = engine.sample(0, 100.0, 300_000).unwrap();
        assert!(
            (snap.fair_yes - 0.5).abs() < 0.05,
            "ATM fair_yes={}, expected ~0.5",
            snap.fair_yes
        );
        assert!(
            (snap.fair_yes + snap.fair_no - 1.0).abs() < 1e-15,
            "fair_yes + fair_no must equal 1.0"
        );
    }

    #[test]
    fn test_deep_itm_fair_yes() {
        let params = FairValueParams::default();
        let mut engine = FairValueEngine::new(params, None);
        // Spot way above strike
        engine.on_book_ticker(0, 110.0, 1.0, 110.0, 1.0);

        let snap = engine.sample(0, 100.0, 300_000).unwrap();
        assert!(
            snap.fair_yes > 0.9,
            "Deep ITM fair_yes={}, expected >0.9",
            snap.fair_yes
        );
    }

    #[test]
    fn test_deep_otm_fair_yes() {
        let params = FairValueParams::default();
        let mut engine = FairValueEngine::new(params, None);
        // Spot way below strike
        engine.on_book_ticker(0, 90.0, 1.0, 90.0, 1.0);

        let snap = engine.sample(0, 100.0, 300_000).unwrap();
        assert!(
            snap.fair_yes < 0.1,
            "Deep OTM fair_yes={}, expected <0.1",
            snap.fair_yes
        );
    }

    #[test]
    fn test_expired_returns_none() {
        let params = FairValueParams::default();
        let mut engine = FairValueEngine::new(params, None);
        engine.on_book_ticker(300_000, 100.0, 1.0, 100.0, 1.0);

        let snap = engine.sample(300_001, 100.0, 300_000);
        assert!(snap.is_none(), "Expired market should return None");
    }

    #[test]
    fn test_vol_ewma_convergence() {
        let params = FairValueParams::default();
        let mut engine = FairValueEngine::new(params, None);

        // Simulate 100 ticks at 10ms intervals with known volatility
        let base_price = 100.0;
        for i in 0..100 {
            let ts = i * 10; // 10ms intervals
            let noise = if i % 2 == 0 { 0.01 } else { -0.01 };
            let price = base_price + noise;
            engine.on_book_ticker(ts, price, 1.0, price, 1.0);
        }

        assert!(engine.vol_ewma_1s > 0.0, "vol_1s should be positive");
        assert!(engine.vol_ewma_5s > 0.0, "vol_5s should be positive");
        assert!(engine.vol_ewma_15s > 0.0, "vol_15s should be positive");
        assert!(
            engine.vol_ewma_1s >= engine.vol_ewma_5s,
            "Faster EWMA should react more to recent vol"
        );
    }

    #[test]
    fn test_sum_always_one() {
        let params = FairValueParams::default();
        let mut engine = FairValueEngine::new(params, None);

        for price in [95.0, 99.0, 100.0, 101.0, 105.0] {
            engine.on_book_ticker(0, price, 1.0, price + 0.01, 1.0);
            if let Some(snap) = engine.sample(0, 100.0, 300_000) {
                assert!(
                    (snap.fair_yes + snap.fair_no - 1.0).abs() < 1e-15,
                    "Sum != 1.0 at price={price}: yes={} no={}",
                    snap.fair_yes,
                    snap.fair_no
                );
            }
        }
    }
}
