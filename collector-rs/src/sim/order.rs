use super::market_data::TokenSide;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderSide {
    Bid,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    InTransit,
    Live,
    Filled,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Order {
    pub id: u64,
    pub token: TokenSide,
    pub side: OrderSide,
    pub price: f64,
    pub size: f64,
    pub filled: f64,
    pub submitted_at_ms: i64,
    pub live_at_ms: i64,
    pub status: OrderStatus,
}

#[derive(Debug, Clone)]
pub struct Fill {
    pub order_id: u64,
    pub ts_ms: i64,
    pub token: TokenSide,
    pub side: OrderSide,
    pub price: f64,
    pub size: f64,
}

pub struct LatencyQueue {
    pending: Vec<Order>,
    min_ms: u64,
    max_ms: u64,
    rng_state: u64,
}

impl LatencyQueue {
    pub fn new(min_ms: u64, max_ms: u64, seed: u64) -> Self {
        Self { pending: Vec::new(), min_ms, max_ms, rng_state: seed }
    }

    pub fn submit(&mut self, mut order: Order, now_ms: i64) {
        let latency = self.random_latency();
        order.live_at_ms = now_ms + latency as i64;
        order.status = OrderStatus::InTransit;
        self.pending.push(order);
    }

    pub fn drain_ready(&mut self, now_ms: i64) -> Vec<Order> {
        let mut ready = Vec::new();
        self.pending.retain(|o| {
            if o.live_at_ms <= now_ms {
                ready.push(o.clone());
                false
            } else {
                true
            }
        });
        for o in &mut ready {
            o.status = OrderStatus::Live;
        }
        ready
    }

    pub fn cancel_pending(&mut self, order_id: u64) -> bool {
        if let Some(pos) = self.pending.iter().position(|o| o.id == order_id) {
            self.pending.remove(pos);
            return true;
        }
        false
    }

    fn random_latency(&mut self) -> u64 {
        self.rng_state = self.rng_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let range = self.max_ms - self.min_ms + 1;
        self.min_ms + (self.rng_state >> 33) % range
    }
}
