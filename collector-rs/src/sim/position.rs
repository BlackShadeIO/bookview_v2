use super::market_data::TokenSide;
use super::order::{Fill, OrderSide};

#[derive(Debug, Clone)]
pub struct TokenInventory {
    pub sellable: f64,
    locked: Vec<(f64, i64)>,
}

impl TokenInventory {
    pub fn new() -> Self {
        Self { sellable: 0.0, locked: Vec::new() }
    }

    pub fn total(&self) -> f64 {
        self.sellable + self.locked.iter().map(|(q, _)| q).sum::<f64>()
    }

    pub fn tick(&mut self, now_ms: i64) {
        let mut newly_sellable = 0.0;
        self.locked.retain(|(qty, unlock)| {
            if *unlock <= now_ms {
                newly_sellable += qty;
                false
            } else {
                true
            }
        });
        self.sellable += newly_sellable;
    }

    pub fn add_from_split(&mut self, qty: f64) {
        self.sellable += qty;
    }

    pub fn add_from_market(&mut self, qty: f64, fill_ts_ms: i64, cooldown_ms: u64) {
        self.locked.push((qty, fill_ts_ms + cooldown_ms as i64));
    }

    pub fn can_sell(&self, qty: f64) -> bool {
        self.sellable >= qty - 1e-9
    }

    pub fn sell(&mut self, qty: f64) {
        self.sellable -= qty;
    }
}

#[derive(Debug, Clone)]
pub struct Position {
    pub cash: f64,
    pub up: TokenInventory,
    pub down: TokenInventory,
    pub fills: Vec<Fill>,
    pub total_volume: f64,
    pub orders_submitted: u64,
    pub orders_rejected: u64,
}

impl Position {
    pub fn new(capital: f64) -> Self {
        Self {
            cash: capital,
            up: TokenInventory::new(),
            down: TokenInventory::new(),
            fills: Vec::new(),
            total_volume: 0.0,
            orders_submitted: 0,
            orders_rejected: 0,
        }
    }

    pub fn apply_split(&mut self, size: f64) {
        self.cash -= size * 1.0;
        self.up.add_from_split(size);
        self.down.add_from_split(size);
    }

    pub fn apply_fill(&mut self, fill: Fill, cooldown_ms: u64) {
        let inv = match fill.token {
            TokenSide::Up => &mut self.up,
            TokenSide::Down => &mut self.down,
        };
        match fill.side {
            OrderSide::Bid => {
                self.cash -= fill.price * fill.size;
                inv.add_from_market(fill.size, fill.ts_ms, cooldown_ms);
            }
            OrderSide::Ask => {
                self.cash += fill.price * fill.size;
                inv.sell(fill.size);
            }
        }
        self.total_volume += fill.price * fill.size;
        self.fills.push(fill);
    }

    pub fn settle(&mut self, outcome_up: bool) {
        if outcome_up {
            self.cash += self.up.total() * 1.0;
        } else {
            self.cash += self.down.total() * 1.0;
        }
    }

    pub fn inventory(&self, token: TokenSide) -> &TokenInventory {
        match token {
            TokenSide::Up => &self.up,
            TokenSide::Down => &self.down,
        }
    }
}
