use super::market_data::{MarketReplay, MarketState, SimEvent, TokenSide, TradeSide};
use super::order::{Fill, LatencyQueue, Order, OrderSide, OrderStatus};
use super::position::Position;
use super::strategy::{Strategy, StrategyAction};
use super::{FillModel, SimConfig};

pub struct SimResult {
    pub strategy_name: String,
    pub epoch: i64,
    pub outcome: bool,
    pub initial_capital: f64,
    pub final_capital: f64,
    pub pnl: f64,
    pub return_pct: f64,
    pub num_fills: usize,
    pub orders_submitted: u64,
    pub orders_rejected: u64,
    pub total_volume: f64,
    pub split_size: f64,
}

pub fn simulate_market(
    market: &MarketReplay,
    strategy: &mut dyn Strategy,
    config: &SimConfig,
) -> SimResult {
    let split = strategy.split_size();
    let initial_capital = config.capital;
    let mut position = Position::new(initial_capital);

    if split > 0.0 {
        position.apply_split(split);
    }

    let mut latency_queue = LatencyQueue::new(config.min_latency_ms, config.max_latency_ms, config.seed);
    let mut live_orders: Vec<Order> = Vec::new();
    let mut state = MarketState::new();
    let mut next_order_id: u64 = 0;
    let mut pending_cancels: Vec<(u64, i64)> = Vec::new();

    strategy.on_market_start(market.market_start_ms, market.market_end_ms);

    for event in &market.events {
        let now = event.ts_ms();

        // 0. Tick inventory cooldowns
        position.up.tick(now);
        position.down.tick(now);

        // 1. Promote orders from latency queue
        for mut order in latency_queue.drain_ready(now) {
            order.status = OrderStatus::Live;
            live_orders.push(order);
        }

        // 1b. Process pending cancellations
        pending_cancels.retain(|(id, arrive_at)| {
            if *arrive_at <= now {
                if let Some(o) = live_orders.iter_mut().find(|o| o.id == *id && o.status == OrderStatus::Live) {
                    o.status = OrderStatus::Cancelled;
                }
                false
            } else {
                true
            }
        });

        // 2. Update market state
        state.update(event, market.market_end_ms);

        // 3. Try fills on trades
        if let SimEvent::PolyTrade { token, price, side, size, .. } = event {
            try_fills(&mut live_orders, &mut position, *token, *price, *side, *size, now, config);
        }

        // 4. Only invoke strategy during market window
        if now < market.market_start_ms || now > market.market_end_ms {
            continue;
        }

        // 5. Get strategy actions
        let live_only: Vec<Order> = live_orders.iter()
            .filter(|o| o.status == OrderStatus::Live)
            .cloned()
            .collect();
        let actions = strategy.on_event(event, &state, &position, &live_only);

        // 6. Process actions
        for action in actions {
            match action {
                StrategyAction::PlaceOrder { token, side, price, size } => {
                    if !validate_maker_only(side, price, &state, token) {
                        position.orders_rejected += 1;
                        continue;
                    }
                    if side == OrderSide::Ask && !position.inventory(token).can_sell(size) {
                        position.orders_rejected += 1;
                        continue;
                    }
                    if price < 0.01 || price > 0.99 || size <= 0.0 {
                        position.orders_rejected += 1;
                        continue;
                    }

                    let order = Order {
                        id: next_order_id,
                        token,
                        side,
                        price,
                        size,
                        filled: 0.0,
                        submitted_at_ms: now,
                        live_at_ms: 0,
                        status: OrderStatus::InTransit,
                    };
                    next_order_id += 1;
                    position.orders_submitted += 1;
                    latency_queue.submit(order, now);
                }
                StrategyAction::CancelOrder { order_id } => {
                    if !latency_queue.cancel_pending(order_id) {
                        let latency = config.min_latency_ms as i64 +
                            ((config.max_latency_ms - config.min_latency_ms) / 2) as i64;
                        pending_cancels.push((order_id, now + latency));
                    }
                }
                StrategyAction::CancelAll => {
                    let latency = config.min_latency_ms as i64 +
                        ((config.max_latency_ms - config.min_latency_ms) / 2) as i64;
                    for o in live_orders.iter().filter(|o| o.status == OrderStatus::Live) {
                        pending_cancels.push((o.id, now + latency));
                    }
                }
            }
        }

        // Clean up filled/cancelled orders periodically
        if live_orders.len() > 100 {
            live_orders.retain(|o| o.status == OrderStatus::Live);
        }
    }

    // Settlement
    position.settle(market.outcome);
    let final_capital = position.cash;
    let pnl = final_capital - initial_capital;

    SimResult {
        strategy_name: strategy.name(),
        epoch: market.epoch,
        outcome: market.outcome,
        initial_capital,
        final_capital,
        pnl,
        return_pct: if initial_capital > 0.0 { pnl / initial_capital * 100.0 } else { 0.0 },
        num_fills: position.fills.len(),
        orders_submitted: position.orders_submitted,
        orders_rejected: position.orders_rejected,
        total_volume: position.total_volume,
        split_size: split,
    }
}

fn try_fills(
    live_orders: &mut [Order],
    position: &mut Position,
    trade_token: TokenSide,
    trade_price: f64,
    trade_side: TradeSide,
    trade_size: f64,
    now_ms: i64,
    config: &SimConfig,
) {
    for order in live_orders.iter_mut() {
        if order.status != OrderStatus::Live { continue; }
        if order.token != trade_token { continue; }

        let should_fill = match config.fill_model {
            FillModel::Aggressive => match order.side {
                OrderSide::Bid => trade_side == TradeSide::Sell && trade_price <= order.price,
                OrderSide::Ask => trade_side == TradeSide::Buy && trade_price >= order.price,
            },
            FillModel::Conservative => match order.side {
                OrderSide::Bid => trade_side == TradeSide::Sell && trade_price < order.price,
                OrderSide::Ask => trade_side == TradeSide::Buy && trade_price > order.price,
            },
            FillModel::QueueAware => match order.side {
                OrderSide::Bid => trade_side == TradeSide::Sell && trade_price <= order.price,
                OrderSide::Ask => trade_side == TradeSide::Buy && trade_price >= order.price,
            },
        };

        if !should_fill { continue; }

        let fill_size = match config.fill_model {
            FillModel::QueueAware => {
                // Trade must go through our price, or be large enough to eat queue
                if trade_price == order.price {
                    // At-price: only fill 30% of trade size (rough queue model)
                    (trade_size * 0.3).min(order.size - order.filled)
                } else {
                    (order.size - order.filled).min(trade_size)
                }
            }
            _ => (order.size - order.filled).min(trade_size),
        };

        if fill_size <= 0.0 { continue; }

        // Validate inventory for ask fills
        if order.side == OrderSide::Ask && !position.inventory(order.token).can_sell(fill_size) {
            continue;
        }

        order.filled += fill_size;
        if order.filled >= order.size - 1e-9 {
            order.status = OrderStatus::Filled;
        }

        let fill = Fill {
            order_id: order.id,
            ts_ms: now_ms,
            token: order.token,
            side: order.side,
            price: order.price,
            size: fill_size,
        };
        position.apply_fill(fill, config.hold_cooldown_ms);
    }
}

fn validate_maker_only(side: OrderSide, price: f64, state: &MarketState, token: TokenSide) -> bool {
    match side {
        OrderSide::Bid => {
            let best_ask = match token {
                TokenSide::Up => state.up_best_ask(),
                TokenSide::Down => state.down_best_ask(),
            };
            match best_ask {
                Some(ask) => price < ask,
                None => true,
            }
        }
        OrderSide::Ask => {
            let best_bid = match token {
                TokenSide::Up => state.up_best_bid(),
                TokenSide::Down => state.down_best_bid(),
            };
            match best_bid {
                Some(bid) => price > bid,
                None => true,
            }
        }
    }
}
