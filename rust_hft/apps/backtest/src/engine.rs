use std::collections::{BTreeMap, HashMap, VecDeque};
use std::mem;

use anyhow::Result;
use itertools::Itertools;
use ordered_float::OrderedFloat;
use serde::Serialize;
use tracing::{debug, warn};

use crate::config::{BacktestConfig, ExecutionConfig, RiskConfig, StrategyConfig};
use crate::event::{open_event_stream, EventEnvelope, EventPayload, Level, TradeSide};

const MICROS_IN_SECOND: f64 = 1_000_000.0;

pub struct BacktestEngine {
    cfg: BacktestConfig,
    order_book: OrderBook,
    liquidity: LiquidityMap,
    flow: FlowTracker,
    execution: ExecutionManager,
    stats: BacktestStats,
    last_ts: Option<i64>,
}

impl BacktestEngine {
    pub fn new(cfg: BacktestConfig) -> Self {
        let max_levels = cfg.data.max_depth_levels;
        let tick_size = cfg.data.tick_size.max(1e-6);
        let strategy = cfg.strategy.clone();
        let execution_cfg = cfg.execution.clone();
        let risk_cfg = cfg.risk.clone();
        Self {
            cfg,
            order_book: OrderBook::new(max_levels),
            liquidity: LiquidityMap::new(strategy, tick_size, max_levels),
            flow: FlowTracker::new(),
            execution: ExecutionManager::new(execution_cfg, risk_cfg, tick_size),
            stats: BacktestStats::default(),
            last_ts: None,
        }
    }

    pub fn run(&mut self) -> Result<BacktestResult> {
        if self.cfg.data.format.to_lowercase() != "ndjson" {
            warn!(
                "資料格式 {} 尚未實作專用解析器，將以 ndjson 模式處理",
                self.cfg.data.format
            );
        }
        let stream = open_event_stream(
            &self.cfg.data.path,
            self.cfg.data.start_ts,
            self.cfg.data.end_ts,
        )?;
        self.run_with_stream(stream)
    }

    pub fn run_with_stream<I>(&mut self, stream: I) -> Result<BacktestResult>
    where
        I: Iterator<Item = anyhow::Result<EventEnvelope>>,
    {
        for evt in stream {
            let event = evt?;
            self.process_event(&event)?;
        }

        // 平倉殘餘持倉
        if self.execution.has_position() {
            if let Some(mid) = self.order_book.mid_price() {
                let ts = self
                    .last_ts
                    .map(|t| t as f64 / MICROS_IN_SECOND)
                    .unwrap_or(0.0);
                self.execution
                    .exit_position(ts, mid, ExitReason::SessionEnd, &mut self.stats);
            }
        }

        Ok(self.finish())
    }

    fn process_event(&mut self, event: &EventEnvelope) -> Result<()> {
        self.last_ts = Some(event.ts);
        let ts_sec = event.ts as f64 / MICROS_IN_SECOND;
        match &event.payload {
            EventPayload::Snapshot { bids, asks } => {
                let ofi = self.order_book.apply_snapshot(event.ts, bids, asks);
                self.liquidity.update(
                    event.ts,
                    &self.order_book.snapshot(self.cfg.data.max_depth_levels),
                );
                self.flow.update_ofi(ts_sec, ofi);
                self.evaluate_signals(event.ts)?;
            }
            EventPayload::L2Update { bids, asks } => {
                let ofi = self.order_book.apply_delta(event.ts, bids, asks);
                self.liquidity.update(
                    event.ts,
                    &self.order_book.snapshot(self.cfg.data.max_depth_levels),
                );
                self.flow.update_ofi(ts_sec, ofi);
                self.evaluate_signals(event.ts)?;
            }
            EventPayload::Trade {
                side,
                price,
                quantity,
            } => {
                self.flow.update_trade(ts_sec, *side, *quantity);
                self.stats.last_trade_price = Some(*price);
            }
        }
        Ok(())
    }

    fn evaluate_signals(&mut self, ts: i64) -> Result<()> {
        let mid = match self.order_book.mid_price() {
            Some(m) => m,
            None => return Ok(()),
        };
        let ts_sec = ts as f64 / MICROS_IN_SECOND;

        // 支撐／壓力
        let supports = self
            .liquidity
            .support_levels(mid, self.cfg.strategy.support_count);
        let resistances = self
            .liquidity
            .resistance_levels(mid, self.cfg.strategy.resistance_count);

        // 計算流動性特徵
        let depth_support = supports.first().cloned();
        let depth_resistance = resistances.first().cloned();
        let tt_vol_down = self
            .flow
            .tt_sell_volume(self.cfg.strategy.breakout_window_secs);
        let tt_vol_up = self
            .flow
            .tt_buy_volume(self.cfg.strategy.breakout_window_secs);
        let cvd_delta = self.flow.cvd_delta(self.cfg.strategy.breakout_window_secs);
        let ofi = self.flow.ofi(self.cfg.strategy.breakout_window_secs);

        // 短向破位
        if let Some(level) = depth_support {
            let price_delta = self.cfg.strategy.price_delta_ticks * self.cfg.data.tick_size;
            let price_condition = mid <= level.price - price_delta;
            let depth_condition =
                level.depth > 0.0 && tt_vol_down >= self.cfg.strategy.volume_factor * level.depth;
            let cvd_condition = cvd_delta <= -self.cfg.strategy.cvd_threshold.abs().max(1e-9);
            let ofi_condition = ofi <= -self.cfg.strategy.ofi_threshold.abs().max(1e-9);

            if price_condition && depth_condition && cvd_condition && ofi_condition {
                if let Some((bid_price, _)) = self.order_book.best_bid() {
                    let entry_price =
                        bid_price - self.cfg.execution.max_slippage_ticks * self.cfg.data.tick_size;
                    let qty = self.execution.calc_short_qty(
                        level.depth,
                        tt_vol_down,
                        self.cfg.execution.base_qty,
                    );
                    if qty > 0.0 && self.execution.can_enter(qty) {
                        self.execution.enter_short(
                            ts_sec,
                            entry_price,
                            qty,
                            level.price,
                            level.depth,
                            &mut self.stats,
                        );
                    }
                }
            }
        }

        // 多向破位（可選）
        if let Some(level) = depth_resistance {
            let price_delta = self.cfg.strategy.price_delta_ticks * self.cfg.data.tick_size;
            let price_condition = mid >= level.price + price_delta;
            let depth_condition =
                level.depth > 0.0 && tt_vol_up >= self.cfg.strategy.volume_factor * level.depth;
            let cvd_condition = cvd_delta >= self.cfg.strategy.cvd_threshold.abs().max(1e-9);
            let ofi_condition = ofi >= self.cfg.strategy.ofi_threshold.abs().max(1e-9);

            if price_condition && depth_condition && cvd_condition && ofi_condition {
                if let Some((ask_price, _)) = self.order_book.best_ask() {
                    let entry_price =
                        ask_price + self.cfg.execution.max_slippage_ticks * self.cfg.data.tick_size;
                    let qty = self.execution.calc_long_qty(
                        level.depth,
                        tt_vol_up,
                        self.cfg.execution.base_qty,
                    );
                    if qty > 0.0 && self.execution.can_enter(qty) {
                        self.execution.enter_long(
                            ts_sec,
                            entry_price,
                            qty,
                            level.price,
                            level.depth,
                            &mut self.stats,
                        );
                    }
                }
            }
        }

        self.execution
            .evaluate_exit(ts_sec, mid, ofi, cvd_delta, &mut self.stats);

        Ok(())
    }

    fn finish(&mut self) -> BacktestResult {
        BacktestResult {
            trades: mem::take(&mut self.execution.trades),
            summary: self.stats.clone_into_summary(),
        }
    }
}

pub struct BacktestResult {
    pub trades: Vec<TradeRecord>,
    pub summary: SummaryMetrics,
}

#[derive(Debug, Clone)]
struct LiquidityLevel {
    price: f64,
    depth: f64,
}

// ----- Order Book -----
#[derive(Default)]
struct OrderBook {
    bids: BTreeMap<OrderedFloat<f64>, f64>,
    asks: BTreeMap<OrderedFloat<f64>, f64>,
    max_levels: usize,
    last_best_bid_qty: f64,
    last_best_ask_qty: f64,
    last_ts: Option<i64>,
}

impl OrderBook {
    fn new(max_levels: usize) -> Self {
        Self {
            max_levels,
            ..Default::default()
        }
    }

    fn apply_snapshot(&mut self, ts: i64, bids: &[Level], asks: &[Level]) -> (f64, f64) {
        self.bids.clear();
        self.asks.clear();
        for level in bids.iter().take(self.max_levels) {
            if level.quantity > 0.0 {
                self.bids.insert(OrderedFloat(level.price), level.quantity);
            }
        }
        for level in asks.iter().take(self.max_levels) {
            if level.quantity > 0.0 {
                self.asks.insert(OrderedFloat(level.price), level.quantity);
            }
        }
        let delta = self.update_best_sizes();
        self.last_ts = Some(ts);
        delta
    }

    fn apply_delta(&mut self, ts: i64, bids: &[Level], asks: &[Level]) -> (f64, f64) {
        for level in bids.iter().take(self.max_levels) {
            let key = OrderedFloat(level.price);
            if level.quantity <= 0.0 {
                self.bids.remove(&key);
            } else {
                self.bids.insert(key, level.quantity);
            }
        }
        for level in asks.iter().take(self.max_levels) {
            let key = OrderedFloat(level.price);
            if level.quantity <= 0.0 {
                self.asks.remove(&key);
            } else {
                self.asks.insert(key, level.quantity);
            }
        }
        let delta = self.update_best_sizes();
        self.last_ts = Some(ts);
        delta
    }

    fn best_bid(&self) -> Option<(f64, f64)> {
        self.bids
            .iter()
            .next_back()
            .map(|(p, q)| (p.into_inner(), *q))
    }

    fn best_ask(&self) -> Option<(f64, f64)> {
        self.asks.iter().next().map(|(p, q)| (p.into_inner(), *q))
    }

    fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some((bid, _)), Some((ask, _))) if ask >= bid => Some((bid + ask) / 2.0),
            _ => None,
        }
    }

    fn snapshot(&self, max_levels: usize) -> DepthSnapshot {
        let bids = self
            .bids
            .iter()
            .rev()
            .take(max_levels)
            .map(|(p, q)| Level {
                price: p.into_inner(),
                quantity: *q,
            })
            .collect_vec();
        let asks = self
            .asks
            .iter()
            .take(max_levels)
            .map(|(p, q)| Level {
                price: p.into_inner(),
                quantity: *q,
            })
            .collect_vec();
        DepthSnapshot { bids, asks }
    }

    fn update_best_sizes(&mut self) -> (f64, f64) {
        let bid_qty = self.best_bid().map(|(_, q)| q).unwrap_or(0.0);
        let ask_qty = self.best_ask().map(|(_, q)| q).unwrap_or(0.0);
        let delta_bid = bid_qty - self.last_best_bid_qty;
        let delta_ask = ask_qty - self.last_best_ask_qty;
        self.last_best_bid_qty = bid_qty;
        self.last_best_ask_qty = ask_qty;
        (delta_bid, delta_ask)
    }
}

#[derive(Clone)]
struct DepthSnapshot {
    bids: Vec<Level>,
    asks: Vec<Level>,
}

// ----- Liquidity Map -----
struct LiquidityMap {
    cfg: StrategyConfig,
    tick_size: f64,
    max_levels: usize,
    total_duration: f64,
    contributions: VecDeque<Contribution>,
    bid_stats: HashMap<OrderedFloat<f64>, LevelStat>,
    ask_stats: HashMap<OrderedFloat<f64>, LevelStat>,
    last_snapshot: Option<DepthSnapshot>,
    last_ts: Option<i64>,
}

#[derive(Clone)]
struct Contribution {
    duration: f64,
    bids: Vec<Level>,
    asks: Vec<Level>,
}

#[derive(Default)]
struct LevelStat {
    weighted_qty: f64,
    duration: f64,
}

impl LiquidityMap {
    fn new(cfg: StrategyConfig, tick_size: f64, max_levels: usize) -> Self {
        Self {
            cfg,
            tick_size,
            max_levels,
            total_duration: 0.0,
            contributions: VecDeque::new(),
            bid_stats: HashMap::new(),
            ask_stats: HashMap::new(),
            last_snapshot: None,
            last_ts: None,
        }
    }

    fn update(&mut self, ts: i64, snapshot: &DepthSnapshot) {
        if let (Some(prev), Some(prev_ts)) = (&self.last_snapshot, self.last_ts) {
            let dt = ((ts - prev_ts) as f64 / MICROS_IN_SECOND).max(0.0);
            if dt > 0.0 {
                let contrib = Contribution {
                    duration: dt,
                    bids: prev.bids.clone(),
                    asks: prev.asks.clone(),
                };
                self.add_contribution(&contrib);
                self.contributions.push_back(contrib);
                self.total_duration += dt;
                self.trim();
            }
        }
        self.last_snapshot = Some(snapshot.clone());
        self.last_ts = Some(ts);
    }

    fn add_contribution(&mut self, contrib: &Contribution) {
        for level in contrib.bids.iter().take(self.max_levels) {
            if level.quantity <= 0.0 {
                continue;
            }
            let key = OrderedFloat(self.round_price(level.price));
            let stat = self.bid_stats.entry(key).or_default();
            stat.weighted_qty += level.quantity * contrib.duration;
            stat.duration += contrib.duration;
        }
        for level in contrib.asks.iter().take(self.max_levels) {
            if level.quantity <= 0.0 {
                continue;
            }
            let key = OrderedFloat(self.round_price(level.price));
            let stat = self.ask_stats.entry(key).or_default();
            stat.weighted_qty += level.quantity * contrib.duration;
            stat.duration += contrib.duration;
        }
    }

    fn remove_contribution(&mut self, contrib: &Contribution, portion: f64) {
        if portion <= 0.0 {
            return;
        }
        for level in contrib.bids.iter().take(self.max_levels) {
            if level.quantity <= 0.0 {
                continue;
            }
            let key = OrderedFloat(self.round_price(level.price));
            if let Some(stat) = self.bid_stats.get_mut(&key) {
                stat.weighted_qty -= level.quantity * portion;
                stat.duration -= portion;
                if stat.weighted_qty <= 1e-9 || stat.duration <= 1e-9 {
                    self.bid_stats.remove(&key);
                }
            }
        }
        for level in contrib.asks.iter().take(self.max_levels) {
            if level.quantity <= 0.0 {
                continue;
            }
            let key = OrderedFloat(self.round_price(level.price));
            if let Some(stat) = self.ask_stats.get_mut(&key) {
                stat.weighted_qty -= level.quantity * portion;
                stat.duration -= portion;
                if stat.weighted_qty <= 1e-9 || stat.duration <= 1e-9 {
                    self.ask_stats.remove(&key);
                }
            }
        }
    }

    fn trim(&mut self) {
        let window = self.cfg.liquidity_window_secs.max(1.0);
        while self.total_duration > window && !self.contributions.is_empty() {
            let overflow = self.total_duration - window;
            if let Some(front) = self.contributions.front() {
                if front.duration <= overflow + 1e-9 {
                    let front = self.contributions.pop_front().unwrap();
                    self.remove_contribution(&front, front.duration);
                    self.total_duration -= front.duration;
                } else {
                    let mut partial = front.clone();
                    partial.duration = overflow;
                    self.remove_contribution(&partial, overflow);
                    if let Some(front_mut) = self.contributions.front_mut() {
                        front_mut.duration -= overflow;
                    }
                    self.total_duration -= overflow;
                }
            }
        }
    }

    fn round_price(&self, price: f64) -> f64 {
        let steps = (price / self.tick_size).round();
        steps * self.tick_size
    }

    fn support_levels(&self, mid: f64, count: usize) -> Vec<LiquidityLevel> {
        let window = self
            .cfg
            .liquidity_window_secs
            .max(self.total_duration)
            .max(1.0);
        let alpha = self.cfg.smoothing_alpha.clamp(0.0, 1.0);
        let mut levels = self
            .bid_stats
            .iter()
            .filter_map(|(price, stat)| {
                let p = price.into_inner();
                if p <= mid && stat.duration > 0.0 {
                    let avg = stat.weighted_qty / window;
                    let inst = if stat.duration > 0.0 {
                        stat.weighted_qty / stat.duration.max(1e-9)
                    } else {
                        avg
                    };
                    let depth = alpha * avg + (1.0 - alpha) * inst;
                    if depth > 0.0 {
                        return Some(LiquidityLevel { price: p, depth });
                    }
                }
                None
            })
            .collect_vec();
        levels.sort_by(|a, b| {
            b.depth
                .partial_cmp(&a.depth)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        levels.truncate(count);
        levels
    }

    fn resistance_levels(&self, mid: f64, count: usize) -> Vec<LiquidityLevel> {
        let window = self
            .cfg
            .liquidity_window_secs
            .max(self.total_duration)
            .max(1.0);
        let alpha = self.cfg.smoothing_alpha.clamp(0.0, 1.0);
        let mut levels = self
            .ask_stats
            .iter()
            .filter_map(|(price, stat)| {
                let p = price.into_inner();
                if p >= mid && stat.duration > 0.0 {
                    let avg = stat.weighted_qty / window;
                    let inst = if stat.duration > 0.0 {
                        stat.weighted_qty / stat.duration.max(1e-9)
                    } else {
                        avg
                    };
                    let depth = alpha * avg + (1.0 - alpha) * inst;
                    if depth > 0.0 {
                        return Some(LiquidityLevel { price: p, depth });
                    }
                }
                None
            })
            .collect_vec();
        levels.sort_by(|a, b| {
            b.depth
                .partial_cmp(&a.depth)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        levels.truncate(count);
        levels
    }
}

// ----- Flow Tracker -----
struct FlowTracker {
    trades: VecDeque<TradeSample>,
    ofi: VecDeque<OfiSample>,
    cvd: VecDeque<CvdSample>,
    cvd_value: f64,
    sell_volume: f64,
    buy_volume: f64,
}

#[derive(Clone)]
struct TradeSample {
    ts: f64,
    side: TradeSide,
    quantity: f64,
}

#[derive(Clone)]
struct OfiSample {
    ts: f64,
    value: f64,
}

#[derive(Clone)]
struct CvdSample {
    ts: f64,
    value: f64,
}

impl FlowTracker {
    fn new() -> Self {
        Self {
            trades: VecDeque::new(),
            ofi: VecDeque::new(),
            cvd: VecDeque::new(),
            cvd_value: 0.0,
            sell_volume: 0.0,
            buy_volume: 0.0,
        }
    }

    fn update_trade(&mut self, ts: f64, side: TradeSide, qty: f64) {
        let sample = TradeSample {
            ts,
            side,
            quantity: qty.max(0.0),
        };
        match side {
            TradeSide::Buy => self.buy_volume += sample.quantity,
            TradeSide::Sell => self.sell_volume += sample.quantity,
        }
        self.trades.push_back(sample);

        match side {
            TradeSide::Buy => self.cvd_value += qty,
            TradeSide::Sell => self.cvd_value -= qty,
        }
        self.cvd.push_back(CvdSample {
            ts,
            value: self.cvd_value,
        });
    }

    fn update_ofi(&mut self, ts: f64, delta: (f64, f64)) {
        let value = delta.0 - delta.1;
        if value.abs() < 1e-9 {
            return;
        }
        self.ofi.push_back(OfiSample { ts, value });
    }

    fn tt_sell_volume(&mut self, window: f64) -> f64 {
        self.prune(window);
        self.sell_volume
    }

    fn tt_buy_volume(&mut self, window: f64) -> f64 {
        self.prune(window);
        self.buy_volume
    }

    fn ofi(&mut self, window: f64) -> f64 {
        self.prune(window);
        self.ofi.iter().map(|s| s.value).sum()
    }

    fn cvd_delta(&mut self, window: f64) -> f64 {
        self.prune(window);
        if let Some(first) = self.cvd.front() {
            self.cvd_value - first.value
        } else {
            0.0
        }
    }

    fn prune(&mut self, window: f64) {
        if window <= 0.0 {
            return;
        }
        let cutoff = match self
            .trades
            .back()
            .map(|s| s.ts)
            .or_else(|| self.ofi.back().map(|s| s.ts))
            .or_else(|| self.cvd.back().map(|s| s.ts))
        {
            Some(ts) => ts - window,
            None => return,
        };

        while let Some(sample) = self.trades.front() {
            if sample.ts >= cutoff {
                break;
            }
            let sample = self.trades.pop_front().unwrap();
            match sample.side {
                TradeSide::Buy => self.buy_volume -= sample.quantity,
                TradeSide::Sell => self.sell_volume -= sample.quantity,
            }
        }

        while let Some(sample) = self.ofi.front() {
            if sample.ts >= cutoff {
                break;
            }
            self.ofi.pop_front();
        }

        while let Some(sample) = self.cvd.front() {
            if sample.ts >= cutoff {
                break;
            }
            self.cvd.pop_front();
        }
    }
}

// ----- Execution -----
struct ExecutionManager {
    cfg: ExecutionConfig,
    risk: RiskConfig,
    tick_size: f64,
    position: PositionState,
    trades: Vec<TradeRecord>,
    pnl: f64,
    equity_curve: Vec<(f64, f64)>,
    consecutive_losses: usize,
    disabled: bool,
}

#[derive(Default)]
struct PositionState {
    side: Option<PositionSide>,
    qty: f64,
    entry_price: f64,
    entry_ts: f64,
    reference_level: f64,
    reference_depth: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionSide {
    Long,
    Short,
}

#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub entry_ts: f64,
    pub exit_ts: f64,
    pub side: PositionSide,
    pub qty: f64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub pnl: f64,
    pub reason: ExitReason,
    pub reference_level: f64,
    pub reference_depth: f64,
}

#[derive(Debug, Clone, Copy)]
pub enum ExitReason {
    PriceReversion,
    StopLoss,
    TakeProfit,
    HoldTimeout,
    SessionEnd,
    RiskStop,
}

impl PositionState {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

impl ExecutionManager {
    fn new(cfg: ExecutionConfig, risk: RiskConfig, tick_size: f64) -> Self {
        Self {
            cfg,
            risk,
            tick_size,
            position: PositionState::default(),
            trades: Vec::new(),
            pnl: 0.0,
            equity_curve: Vec::new(),
            consecutive_losses: 0,
            disabled: false,
        }
    }

    fn has_position(&self) -> bool {
        self.position.side.is_some()
    }

    fn can_enter(&self, qty: f64) -> bool {
        if self.disabled {
            return false;
        }
        let projected = self.position.qty.abs() + qty.abs();
        projected <= self.risk.inventory_limit + 1e-9
    }

    fn calc_short_qty(&self, depth: f64, vol: f64, base_qty: f64) -> f64 {
        if depth <= 0.0 {
            return 0.0;
        }
        let ratio = (vol / depth).min(self.cfg.max_position / base_qty);
        (base_qty * ratio).min(self.cfg.max_position).max(0.0)
    }

    fn calc_long_qty(&self, depth: f64, vol: f64, base_qty: f64) -> f64 {
        if depth <= 0.0 {
            return 0.0;
        }
        let ratio = (vol / depth).min(self.cfg.max_position / base_qty);
        (base_qty * ratio).min(self.cfg.max_position).max(0.0)
    }

    fn enter_short(
        &mut self,
        ts: f64,
        price: f64,
        qty: f64,
        reference_level: f64,
        reference_depth: f64,
        stats: &mut BacktestStats,
    ) {
        if qty <= 0.0 {
            return;
        }
        if self.position.side == Some(PositionSide::Short) {
            // 累加倉位
            let total_qty = self.position.qty + qty;
            let new_entry =
                (self.position.entry_price * self.position.qty + price * qty) / total_qty;
            self.position.qty = total_qty;
            self.position.entry_price = new_entry;
            self.position.entry_ts = ts;
            self.position.reference_level = reference_level;
            self.position.reference_depth = reference_depth;
        } else {
            self.position.side = Some(PositionSide::Short);
            self.position.qty = qty;
            self.position.entry_price = price;
            self.position.entry_ts = ts;
            self.position.reference_level = reference_level;
            self.position.reference_depth = reference_depth;
        }
        stats.max_position = stats.max_position.max(self.position.qty.abs());
        debug!(
            "enter short qty={:.4} price={:.4} ref={:.4}",
            qty, price, reference_level
        );
    }

    fn enter_long(
        &mut self,
        ts: f64,
        price: f64,
        qty: f64,
        reference_level: f64,
        reference_depth: f64,
        stats: &mut BacktestStats,
    ) {
        if qty <= 0.0 {
            return;
        }
        if self.position.side == Some(PositionSide::Long) {
            let total_qty = self.position.qty + qty;
            let new_entry =
                (self.position.entry_price * self.position.qty + price * qty) / total_qty;
            self.position.qty = total_qty;
            self.position.entry_price = new_entry;
            self.position.entry_ts = ts;
            self.position.reference_level = reference_level;
            self.position.reference_depth = reference_depth;
        } else {
            self.position.side = Some(PositionSide::Long);
            self.position.qty = qty;
            self.position.entry_price = price;
            self.position.entry_ts = ts;
            self.position.reference_level = reference_level;
            self.position.reference_depth = reference_depth;
        }
        stats.max_position = stats.max_position.max(self.position.qty.abs());
        debug!(
            "enter long qty={:.4} price={:.4} ref={:.4}",
            qty, price, reference_level
        );
    }

    fn evaluate_exit(
        &mut self,
        ts: f64,
        mid: f64,
        ofi: f64,
        cvd_delta: f64,
        stats: &mut BacktestStats,
    ) {
        if self.position.side.is_none() {
            return;
        }
        let elapsed = ts - self.position.entry_ts;
        let stop_loss_price = match self.position.side {
            Some(PositionSide::Short) => {
                self.position.entry_price + self.cfg.stop_loss_ticks * self.tick_size
            }
            Some(PositionSide::Long) => {
                self.position.entry_price - self.cfg.stop_loss_ticks * self.tick_size
            }
            None => 0.0,
        };
        let take_profit_price = match self.position.side {
            Some(PositionSide::Short) => {
                self.position.entry_price - self.cfg.take_profit_ticks * self.tick_size
            }
            Some(PositionSide::Long) => {
                self.position.entry_price + self.cfg.take_profit_ticks * self.tick_size
            }
            None => 0.0,
        };

        let (stop_hit, tp_hit, reversion) = match self.position.side {
            Some(PositionSide::Short) => (
                mid >= stop_loss_price,
                mid <= take_profit_price,
                mid >= self.position.reference_level || (cvd_delta >= 0.0 && ofi >= 0.0),
            ),
            Some(PositionSide::Long) => (
                mid <= stop_loss_price,
                mid >= take_profit_price,
                mid <= self.position.reference_level || (cvd_delta <= 0.0 && ofi <= 0.0),
            ),
            None => (false, false, false),
        };

        let hold_timeout = self
            .cfg
            .hold_secs
            .map(|hold| elapsed >= hold)
            .unwrap_or(false);

        let reason = if stop_hit {
            Some(ExitReason::StopLoss)
        } else if tp_hit {
            Some(ExitReason::TakeProfit)
        } else if hold_timeout {
            Some(ExitReason::HoldTimeout)
        } else if reversion {
            Some(ExitReason::PriceReversion)
        } else {
            None
        };

        if let Some(reason) = reason {
            self.exit_position(ts, mid, reason, stats);
        }
    }

    fn exit_position(
        &mut self,
        ts: f64,
        price: f64,
        reason: ExitReason,
        stats: &mut BacktestStats,
    ) {
        if self.position.side.is_none() || self.position.qty == 0.0 {
            return;
        }
        let qty = self.position.qty;
        let entry_price = self.position.entry_price;
        let side = self.position.side.unwrap();
        let pnl = match side {
            PositionSide::Short => (entry_price - price) * qty,
            PositionSide::Long => (price - entry_price) * qty,
        };
        self.pnl += pnl;
        self.equity_curve.push((ts, self.pnl));
        stats.update(pnl, self.pnl);

        if pnl < 0.0 {
            self.consecutive_losses += 1;
            if self.consecutive_losses >= self.risk.max_consecutive_losses {
                self.disabled = true;
                warn!(
                    "停機：連續虧損達 {} 筆，停止進場",
                    self.risk.max_consecutive_losses
                );
            }
        } else {
            self.consecutive_losses = 0;
        }

        let mut exit_reason = reason;
        if let Some(limit) = self.risk.daily_loss_limit {
            if self.pnl <= -limit {
                self.disabled = true;
                exit_reason = ExitReason::RiskStop;
                warn!("停機：日損達 {:.4}，停止進場", limit);
            }
        }

        self.trades.push(TradeRecord {
            entry_ts: self.position.entry_ts,
            exit_ts: ts,
            side,
            qty,
            entry_price,
            exit_price: price,
            pnl,
            reason: exit_reason,
            reference_level: self.position.reference_level,
            reference_depth: self.position.reference_depth,
        });

        self.position.reset();
    }
}

// ----- Backtest Stats -----
#[derive(Default)]
struct BacktestStats {
    pub total_pnl: f64,
    pub wins: usize,
    pub losses: usize,
    pub max_drawdown: f64,
    pub peak_equity: f64,
    pub max_position: f64,
    pub last_trade_price: Option<f64>,
}

impl BacktestStats {
    fn update(&mut self, pnl: f64, equity: f64) {
        self.total_pnl += pnl;
        if pnl >= 0.0 {
            self.wins += 1;
        } else {
            self.losses += 1;
        }
        self.peak_equity = self.peak_equity.max(equity);
        let drawdown = self.peak_equity - equity;
        self.max_drawdown = self.max_drawdown.max(drawdown);
    }

    fn clone_into_summary(&self) -> SummaryMetrics {
        let total_trades = self.wins + self.losses;
        let win_rate = if total_trades > 0 {
            self.wins as f64 / total_trades as f64
        } else {
            0.0
        };
        SummaryMetrics {
            total_pnl: self.total_pnl,
            trades: total_trades,
            win_rate,
            max_drawdown: self.max_drawdown,
            max_position: self.max_position,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryMetrics {
    pub total_pnl: f64,
    pub trades: usize,
    pub win_rate: f64,
    pub max_drawdown: f64,
    pub max_position: f64,
}
