use std::collections::VecDeque;

use hft_core::{
    HftResult, OrderType, Price, Quantity, Side, Symbol, TimeInForce, Timestamp, VenueId,
};
use ports::{
    AccountView, ExecutionEvent, L2BookView, MarketEvent, OrderIntent, Strategy, StrategyContext,
    VenueScope,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

const MICROS_PER_SEC: u64 = 1_000_000;

/// LOB Flow Grid 策略配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobFlowGridConfig {
    /// 目標交易所
    #[serde(default = "default_venue")]
    pub venue: VenueId,
    /// 订单基准数量 (基础 lot)
    pub base_order_size: f64,
    /// 最大多頭部位
    pub max_long_position: f64,
    /// 最大空頭部位 (負數)
    pub max_short_position: f64,
    /// maker 費率 (bps)
    pub maker_fee_bps: f64,
    /// 預估單腿滑點 (bps)
    pub slippage_bps: f64,
    /// 安全邊際 (bps)
    pub safety_bps: f64,
    /// 最小額外邊際 (bps)
    pub min_margin_bps: f64,
    /// 波動係數 α
    pub volatility_alpha: f64,
    /// ASR 放大係數 γ
    pub asr_gamma: f64,
    /// Microprice 系數 β1
    pub asr_micro_coeff: f64,
    /// Aggressor imbalance 系數 β2
    pub asr_ai_coeff: f64,
    /// Order flow imbalance 系數 β3
    pub asr_ofi_coeff: f64,
    /// OBI 計算使用的檔位
    #[serde(default = "default_top_levels")]
    pub top_levels: usize,
    /// 貿易流窗口 (秒)
    pub ai_window_secs: u64,
    /// OFI 平滑半衰期 (秒)
    pub ofi_halflife_secs: f64,
    /// Mid return 半衰期 (秒)
    pub mid_return_halflife_secs: f64,
    /// 刷新間隔 (秒)
    pub refresh_interval_secs: f64,
    /// Tick Size
    pub tick_size: f64,
    /// Lot size 最小下單量
    pub lot_size: f64,
    /// Core 層數
    pub core_levels: usize,
    /// Buffer 層數
    pub buffer_levels: usize,
    /// Tail 層數
    pub tail_levels: usize,
    /// Core 權重
    pub core_weight: f64,
    /// Buffer 權重
    pub buffer_weight: f64,
    /// Tail 權重
    pub tail_weight: f64,
    /// Core spacing 倍數
    pub core_spacing_multiplier: f64,
    /// Buffer spacing 倍數
    pub buffer_spacing_multiplier: f64,
    /// Tail spacing 倍數
    pub tail_spacing_multiplier: f64,
    /// 目標深度 (USD)
    pub target_depth_usd: f64,
    /// 掃描書本最大檔數
    pub max_depth_steps: usize,
    /// 最小頂部深度 (USD)
    pub min_top_depth_usd: f64,
    /// 最小允許點差 (bps)
    pub min_spread_bps: f64,
    /// 最大允許點差 (bps)
    pub max_spread_bps: f64,
    /// Microprice 偏移閾值 (bps)
    pub bias_micro_threshold_bps: f64,
    /// Aggressor imbalance 偏移閾值
    pub bias_ai_threshold: f64,
    /// 遇到趨勢時額外退讓的 ticks 數
    pub bias_extra_ticks: i32,
}

fn default_venue() -> VenueId {
    VenueId::ASTERDEX
}

fn default_top_levels() -> usize {
    5
}

impl Default for LobFlowGridConfig {
    fn default() -> Self {
        Self {
            venue: default_venue(),
            base_order_size: 0.01,
            max_long_position: 1.5,
            max_short_position: -1.5,
            maker_fee_bps: 1.0,
            slippage_bps: 0.5,
            safety_bps: 0.3,
            min_margin_bps: 0.5,
            volatility_alpha: 0.7,
            asr_gamma: 0.7,
            asr_micro_coeff: 0.6,
            asr_ai_coeff: 20.0,
            asr_ofi_coeff: 15.0,
            top_levels: default_top_levels(),
            ai_window_secs: 30,
            ofi_halflife_secs: 20.0,
            mid_return_halflife_secs: 600.0,
            refresh_interval_secs: 8.0,
            tick_size: 0.1,
            lot_size: 0.001,
            core_levels: 3,
            buffer_levels: 2,
            tail_levels: 2,
            core_weight: 0.5,
            buffer_weight: 0.3,
            tail_weight: 0.2,
            core_spacing_multiplier: 1.0,
            buffer_spacing_multiplier: 1.6,
            tail_spacing_multiplier: 2.5,
            target_depth_usd: 50_000.0,
            max_depth_steps: 5,
            min_top_depth_usd: 10_000.0,
            min_spread_bps: 2.0,
            max_spread_bps: 120.0,
            bias_micro_threshold_bps: 1.5,
            bias_ai_threshold: 0.1,
            bias_extra_ticks: 1,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TopLevel {
    bid_price: f64,
    bid_qty: f64,
    ask_price: f64,
    ask_qty: f64,
}

#[derive(Debug, Clone)]
struct TradeRecord {
    timestamp: Timestamp,
    side: Side,
    quantity: f64,
}

#[derive(Debug, Default)]
struct LobFlowState {
    bids: Vec<(f64, f64)>,
    asks: Vec<(f64, f64)>,
    last_top: Option<TopLevel>,
    last_mid: Option<(f64, Timestamp)>,
    ewma_abs_return: f64,
    trade_window: VecDeque<TradeRecord>,
    ofi_value: f64,
    last_event_ts: Option<Timestamp>,
    last_refresh_ts: Option<Timestamp>,
}

impl LobFlowState {
    fn update_depth(&mut self, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>) {
        let prev = self.current_top();
        self.bids = bids;
        self.asks = asks;
        self.last_top = match (prev, self.current_top()) {
            (Some(_), Some(curr)) => Some(curr),
            _ => prev,
        };
    }

    fn current_top(&self) -> Option<TopLevel> {
        if self.bids.is_empty() || self.asks.is_empty() {
            return None;
        }
        Some(TopLevel {
            bid_price: self.bids[0].0,
            bid_qty: self.bids[0].1,
            ask_price: self.asks[0].0,
            ask_qty: self.asks[0].1,
        })
    }

    fn mid(&self) -> Option<f64> {
        self.current_top()
            .map(|t| (t.bid_price + t.ask_price) * 0.5)
            .filter(|m| *m > 0.0)
    }

    fn depth_sums(&self, levels: usize) -> (f64, f64) {
        let bid_sum = self
            .bids
            .iter()
            .take(levels)
            .map(|(_, qty)| *qty)
            .sum::<f64>();
        let ask_sum = self
            .asks
            .iter()
            .take(levels)
            .map(|(_, qty)| *qty)
            .sum::<f64>();
        (bid_sum, ask_sum)
    }

    fn reset_refresh(&mut self, ts: Timestamp) {
        self.last_refresh_ts = Some(ts);
    }
}

/// LOB Flow 自適應網格策略
pub struct LobFlowGridStrategy {
    symbol: Symbol,
    config: LobFlowGridConfig,
    state: LobFlowState,
    strategy_id: String,
}

impl LobFlowGridStrategy {
    pub fn new(symbol: Symbol, config: LobFlowGridConfig) -> Self {
        let strategy_id = format!("lob_grid_{}", symbol.as_str());
        Self::with_name(symbol, config, strategy_id)
    }

    pub fn with_name(symbol: Symbol, config: LobFlowGridConfig, strategy_name: String) -> Self {
        Self {
            symbol,
            config,
            state: LobFlowState::default(),
            strategy_id: strategy_name,
        }
    }

    fn handle_snapshot(
        &mut self,
        bids: &[ports::BookLevel],
        asks: &[ports::BookLevel],
        timestamp: Timestamp,
    ) {
        let bids_converted = convert_levels(bids, self.config.top_levels);
        let asks_converted = convert_levels(asks, self.config.top_levels);
        if bids_converted.is_empty() || asks_converted.is_empty() {
            return;
        }
        let prev_top = self.state.current_top();
        self.state.update_depth(bids_converted, asks_converted);
        self.update_mid_state(timestamp);
        self.update_ofi(prev_top, timestamp);
    }

    fn handle_book_view(&mut self, book: L2BookView<'_>) {
        if book.bid_prices.is_empty() || book.ask_prices.is_empty() {
            return;
        }

        let levels = self.config.top_levels;
        let prev_top = self.state.current_top();
        self.state.bids.clear();
        self.state.asks.clear();
        if self.state.bids.capacity() < levels {
            self.state.bids.reserve(levels);
        }
        if self.state.asks.capacity() < levels {
            self.state.asks.reserve(levels);
        }
        self.state.bids.extend(
            book.bid_prices
                .iter()
                .zip(book.bid_quantities)
                .take(levels)
                .map(|(price, quantity)| (price.to_f64(), quantity.to_f64())),
        );
        self.state.asks.extend(
            book.ask_prices
                .iter()
                .zip(book.ask_quantities)
                .take(levels)
                .map(|(price, quantity)| (price.to_f64(), quantity.to_f64())),
        );
        self.state.last_top = self.state.current_top().or(prev_top);
        self.update_mid_state(book.timestamp);
        self.update_ofi(prev_top, book.timestamp);
    }

    fn handle_bar(&mut self, bar: &ports::AggregatedBar) {
        // A bar may seed a non-LOB research run, but it must never replace a synchronized
        // exchange book in production.
        if !self.state.bids.is_empty() && !self.state.asks.is_empty() {
            return;
        }
        let close = match bar.close.to_f64() {
            Some(v) if v.is_finite() && v > 0.0 => v,
            _ => return,
        };
        let tick = self.config.tick_size.max(1e-6);
        let levels = std::cmp::max(1, self.config.top_levels);
        let qty = self.config.base_order_size.max(self.config.lot_size);

        let mut bids = Vec::with_capacity(levels);
        let mut asks = Vec::with_capacity(levels);
        for level in 0..levels {
            let offset = tick * (level as f64 + 1.0);
            bids.push((close - offset, qty));
            asks.push((close + offset, qty));
        }

        let prev_top = self.state.current_top();
        self.state.update_depth(bids, asks);
        self.update_mid_state(bar.close_time);
        self.update_ofi(prev_top, bar.close_time);
    }

    fn handle_update(
        &mut self,
        bids: &[ports::BookLevel],
        asks: &[ports::BookLevel],
        timestamp: Timestamp,
    ) {
        // Raw compatibility path. Production receives the canonical book through
        // `StrategyContext`; direct callers still apply deltas by price, not array index.
        if self.state.bids.is_empty() || self.state.asks.is_empty() {
            return;
        }

        let prev_top = self.state.current_top();
        apply_price_deltas(&mut self.state.bids, bids, self.config.top_levels, true);
        apply_price_deltas(&mut self.state.asks, asks, self.config.top_levels, false);
        self.state.last_top = self.state.current_top().or(prev_top);
        self.update_mid_state(timestamp);
        self.update_ofi(prev_top, timestamp);
    }

    fn update_mid_state(&mut self, timestamp: Timestamp) {
        if let Some(mid) = self.state.mid() {
            if let Some((prev_mid, prev_ts)) = self.state.last_mid {
                if prev_mid > 0.0 {
                    let ret = ((mid / prev_mid) - 1.0).abs();
                    let dt_secs = ((timestamp - prev_ts) as f64) / MICROS_PER_SEC as f64;
                    let halflife = self.config.mid_return_halflife_secs.max(1.0);
                    let decay = 2_f64.powf(-dt_secs / halflife);
                    self.state.ewma_abs_return =
                        self.state.ewma_abs_return * decay + ret * (1.0 - decay);
                }
            }
            self.state.last_mid = Some((mid, timestamp));
        }
    }

    fn update_ofi(&mut self, prev_top: Option<TopLevel>, timestamp: Timestamp) {
        let current_top = self.state.current_top();
        if let (Some(prev), Some(curr)) = (prev_top, current_top) {
            let dt_secs = if let Some(last_ts) = self.state.last_event_ts {
                ((timestamp - last_ts) as f64) / MICROS_PER_SEC as f64
            } else {
                0.0
            };
            let halflife = self.config.ofi_halflife_secs.max(1.0);
            let decay = 2_f64.powf(-(dt_secs.max(0.0)) / halflife);
            self.state.ofi_value *= decay;

            let mut delta = 0.0;
            // Bid side
            if curr.bid_price > prev.bid_price {
                delta += curr.bid_qty;
            } else if (curr.bid_price - prev.bid_price).abs() < f64::EPSILON {
                delta += curr.bid_qty - prev.bid_qty;
            } else {
                delta -= prev.bid_qty;
            }
            // Ask side
            if curr.ask_price < prev.ask_price {
                delta -= curr.ask_qty;
            } else if (curr.ask_price - prev.ask_price).abs() < f64::EPSILON {
                delta -= curr.ask_qty - prev.ask_qty;
            } else {
                delta += prev.ask_qty;
            }

            self.state.ofi_value += delta;
        }
        self.state.last_event_ts = Some(timestamp);
    }

    fn record_trade(&mut self, trade: &ports::Trade) {
        if trade.symbol != self.symbol {
            return;
        }
        let quantity = match trade.quantity.to_f64() {
            Some(q) if q.is_finite() => q,
            _ => return,
        };
        self.state.trade_window.push_back(TradeRecord {
            timestamp: trade.timestamp,
            side: trade.side,
            quantity,
        });
    }

    fn purge_old_trades(&mut self, now: Timestamp) {
        let window = self.config.ai_window_secs.max(1) as i64;
        while let Some(front) = self.state.trade_window.front() {
            let age_secs =
                ((now as i128 - front.timestamp as i128) / MICROS_PER_SEC as i128) as i64;
            if age_secs > window {
                self.state.trade_window.pop_front();
            } else {
                break;
            }
        }
    }

    fn refresh_due(&self, ts: Timestamp) -> bool {
        let interval = (self.config.refresh_interval_secs * MICROS_PER_SEC as f64) as u64;
        match self.state.last_refresh_ts {
            None => true,
            Some(last) => ts.saturating_sub(last) >= interval,
        }
    }

    fn compute_signals(&mut self, now: Timestamp) -> Option<SignalSnapshot> {
        self.purge_old_trades(now);
        let mid = self.state.mid()?;
        let (bid_sum, ask_sum) = self.state.depth_sums(self.config.top_levels);
        let obi = if (bid_sum + ask_sum) > 0.0 {
            (bid_sum - ask_sum) / (bid_sum + ask_sum)
        } else {
            0.0
        };

        let micro_price = if let (Some(b_top), Some(a_top)) =
            (self.state.bids.first(), self.state.asks.first())
        {
            let vb = bid_sum.max(1e-8);
            let va = ask_sum.max(1e-8);
            Some((va * b_top.0 + vb * a_top.0) / (va + vb))
        } else {
            None
        };

        let micro_delta_bps = match micro_price {
            Some(mu) if mid > 0.0 => ((mu - mid) / mid) * 10_000.0,
            _ => 0.0,
        };

        let mut buy_vol = 0.0;
        let mut sell_vol = 0.0;
        for trade in &self.state.trade_window {
            match trade.side {
                Side::Buy => buy_vol += trade.quantity,
                Side::Sell => sell_vol += trade.quantity,
            }
        }
        let ai = if (buy_vol + sell_vol) > 0.0 {
            (buy_vol - sell_vol) / (buy_vol + sell_vol)
        } else {
            0.0
        };

        let top = self.state.current_top()?;
        let depth_total = top.bid_qty + top.ask_qty + 1e-6;
        let ofi = self.state.ofi_value / depth_total;

        Some(SignalSnapshot {
            timestamp: now,
            mid,
            obi,
            micro_delta_bps,
            ai,
            ofi,
            best_bid: top.bid_price,
            best_ask: top.ask_price,
        })
    }

    fn compute_spacing_bps(&self, signals: &SignalSnapshot) -> f64 {
        let maker_cost =
            2.0 * (self.config.maker_fee_bps + self.config.slippage_bps) + self.config.safety_bps;
        let fee_floor = maker_cost + self.config.min_margin_bps;
        let vol_component = self.state.ewma_abs_return * 10_000.0 * self.config.volatility_alpha;
        let asr = self.config.asr_micro_coeff * signals.micro_delta_bps
            + self.config.asr_ai_coeff * signals.ai
            + self.config.asr_ofi_coeff * signals.ofi;
        let adaptive = vol_component + self.config.asr_gamma * asr.abs();
        fee_floor.max(adaptive)
    }

    fn build_levels(&self) -> Vec<GridLevelSpec> {
        let mut specs = Vec::new();
        let mut level_counter = 0;
        if self.config.core_levels > 0 {
            let per_level_weight = self.config.core_weight / self.config.core_levels as f64;
            for i in 0..self.config.core_levels {
                level_counter += 1;
                specs.push(GridLevelSpec {
                    level_index: level_counter,
                    spacing_multiplier: self.config.core_spacing_multiplier * (i + 1) as f64,
                    size_multiplier: per_level_weight,
                });
            }
        }
        if self.config.buffer_levels > 0 {
            let per_level_weight = self.config.buffer_weight / self.config.buffer_levels as f64;
            for i in 0..self.config.buffer_levels {
                level_counter += 1;
                specs.push(GridLevelSpec {
                    level_index: level_counter,
                    spacing_multiplier: self.config.buffer_spacing_multiplier * (i + 1) as f64,
                    size_multiplier: per_level_weight,
                });
            }
        }
        if self.config.tail_levels > 0 {
            let per_level_weight = self.config.tail_weight / self.config.tail_levels as f64;
            for i in 0..self.config.tail_levels {
                level_counter += 1;
                specs.push(GridLevelSpec {
                    level_index: level_counter,
                    spacing_multiplier: self.config.tail_spacing_multiplier * (i + 1) as f64,
                    size_multiplier: per_level_weight,
                });
            }
        }
        specs
    }

    fn book_depth_usd(levels: &[(f64, f64)], max_steps: usize) -> f64 {
        let mut cum = 0.0;
        for (idx, (price, qty)) in levels.iter().enumerate() {
            if idx >= max_steps {
                break;
            }
            cum += price * qty;
        }
        cum
    }

    fn price_at_depth(&self, levels: &[(f64, f64)], target_usd: f64) -> Option<f64> {
        if levels.is_empty() {
            return None;
        }
        if target_usd <= 0.0 {
            return Some(levels[0].0);
        }
        let mut cum = 0.0;
        let mut last_price = levels[0].0;
        for (idx, (price, qty)) in levels.iter().enumerate() {
            if idx >= self.config.max_depth_steps {
                break;
            }
            cum += price * qty;
            last_price = *price;
            if cum >= target_usd {
                return Some(*price);
            }
        }
        Some(last_price)
    }

    fn market_filters_passed(&self, signals: &SignalSnapshot) -> bool {
        if signals.best_ask <= signals.best_bid || signals.mid <= 0.0 {
            debug!(symbol = %self.symbol.as_str(), "filters: 無有效點差/中價");
            return false;
        }
        let spread = signals.best_ask - signals.best_bid;
        let spread_bps = (spread / signals.mid) * 10_000.0;
        if spread_bps < self.config.min_spread_bps || spread_bps > self.config.max_spread_bps {
            debug!(symbol = %self.symbol.as_str(), spread_bps, "filters: 點差超出可接受範圍");
            return false;
        }
        let bid_depth = Self::book_depth_usd(&self.state.bids, self.config.max_depth_steps);
        let ask_depth = Self::book_depth_usd(&self.state.asks, self.config.max_depth_steps);
        if bid_depth < self.config.min_top_depth_usd || ask_depth < self.config.min_top_depth_usd {
            debug!(symbol = %self.symbol.as_str(), bid_depth, ask_depth, "filters: 書內深度不足");
            return false;
        }
        true
    }

    fn generate_orders(
        &mut self,
        signals: SignalSnapshot,
        account: &AccountView,
        spacing_bps: f64,
    ) -> Vec<OrderIntent> {
        let mut orders = Vec::new();
        let levels = self.build_levels();

        if !self.market_filters_passed(&signals) {
            debug!(symbol = %self.symbol.as_str(), "市場過濾條件未滿足，跳過掛單");
            return orders;
        }

        let bid_anchor = self
            .price_at_depth(&self.state.bids, self.config.target_depth_usd)
            .unwrap_or(signals.best_bid);
        let ask_anchor = self
            .price_at_depth(&self.state.asks, self.config.target_depth_usd)
            .unwrap_or(signals.best_ask);
        let effective_bid_anchor = bid_anchor.min(signals.best_bid);
        let effective_ask_anchor = ask_anchor.max(signals.best_ask);
        let tick = self.config.tick_size.max(1e-9);

        let mut inventory = account
            .positions
            .get(&self.symbol)
            .and_then(|p| p.quantity.to_f64())
            .unwrap_or(0.0);
        let inventory_before = inventory;

        let bias_up = signals.micro_delta_bps > self.config.bias_micro_threshold_bps
            && signals.ai > self.config.bias_ai_threshold;
        let bias_down = signals.micro_delta_bps < -self.config.bias_micro_threshold_bps
            && signals.ai < -self.config.bias_ai_threshold;

        let extra_ticks_buy = if bias_up {
            self.config.bias_extra_ticks.max(0) as f64
        } else {
            0.0
        };
        let extra_ticks_sell = if bias_down {
            self.config.bias_extra_ticks.max(0) as f64
        } else {
            0.0
        };

        for level in &levels {
            let base_spacing = spacing_bps * level.spacing_multiplier;
            let price_offset = signals.mid * base_spacing / 10_000.0;
            if price_offset <= 0.0 {
                continue;
            }

            // Buy side
            if inventory < self.config.max_long_position {
                let mut target_size = self.config.base_order_size * level.size_multiplier;
                let long_room = self.config.max_long_position - inventory;
                if target_size > long_room {
                    target_size = long_room.max(0.0);
                }
                if target_size >= self.config.lot_size {
                    let mut raw_price = effective_bid_anchor - price_offset;
                    raw_price -= extra_ticks_buy * tick;
                    let price = round_down_to_tick(raw_price, tick);
                    if price > 0.0 && price < signals.best_ask {
                        if let (Ok(price_decimal), Ok(quantity_decimal)) =
                            (Price::from_f64(price), Quantity::from_f64(target_size))
                        {
                            orders.push(OrderIntent {
                                symbol: self.symbol.clone(),
                                asset_class: hft_core::AssetClass::Crypto,
                                product_type: hft_core::ProductType::Spot,
                                compliance_context: hft_core::ComplianceContext::default(),
                                side: Side::Buy,
                                quantity: quantity_decimal,
                                order_type: OrderType::Limit,
                                price: Some(price_decimal),
                                time_in_force: TimeInForce::GTC,
                                strategy_id: self.strategy_id.clone(),
                                target_venue: Some(self.config.venue),
                            });
                            inventory += target_size;
                        }
                    }
                }
            }

            // Sell side
            if inventory > self.config.max_short_position {
                let mut target_size = self.config.base_order_size * level.size_multiplier;
                let short_room = inventory - self.config.max_short_position;
                if target_size > short_room {
                    target_size = short_room.max(0.0);
                }
                if target_size >= self.config.lot_size {
                    let mut raw_price = effective_ask_anchor + price_offset;
                    raw_price += extra_ticks_sell * tick;
                    let price = round_up_to_tick(raw_price, tick);
                    if price > signals.best_bid {
                        if let (Ok(price_decimal), Ok(quantity_decimal)) =
                            (Price::from_f64(price), Quantity::from_f64(target_size))
                        {
                            orders.push(OrderIntent {
                                symbol: self.symbol.clone(),
                                asset_class: hft_core::AssetClass::Crypto,
                                product_type: hft_core::ProductType::Spot,
                                compliance_context: hft_core::ComplianceContext::default(),
                                side: Side::Sell,
                                quantity: quantity_decimal,
                                order_type: OrderType::Limit,
                                price: Some(price_decimal),
                                time_in_force: TimeInForce::GTC,
                                strategy_id: self.strategy_id.clone(),
                                target_venue: Some(self.config.venue),
                            });
                            inventory -= target_size;
                        }
                    }
                }
            }
        }

        inventory = account
            .positions
            .get(&self.symbol)
            .and_then(|p| p.quantity.to_f64())
            .unwrap_or(0.0);
        if orders.is_empty() {
            debug!(
                symbol = %self.symbol.as_str(),
                spacing_bps,
                inventory_before,
                inventory_after = inventory,
                bias_up,
                bias_down,
                micro_delta_bps = signals.micro_delta_bps,
                ai = signals.ai,
                ofi = signals.ofi,
                bid_anchor = effective_bid_anchor,
                ask_anchor = effective_ask_anchor,
                "LOB grid：本輪未生成掛單"
            );
        } else {
            info!(
                symbol = %self.symbol.as_str(),
                order_count = orders.len(),
                spacing_bps,
                inventory_before,
                inventory_after = inventory,
                bias_up,
                bias_down,
                micro_delta_bps = signals.micro_delta_bps,
                ai = signals.ai,
                ofi = signals.ofi,
                bid_anchor = effective_bid_anchor,
                ask_anchor = effective_ask_anchor,
                "LOB grid：生成限價掛單"
            );
        }
        orders
    }
}

impl Strategy for LobFlowGridStrategy {
    fn on_market_event(&mut self, event: &MarketEvent, account: &AccountView) -> Vec<OrderIntent> {
        match event {
            MarketEvent::Snapshot(s) if s.symbol == self.symbol => {
                self.handle_snapshot(&s.bids, &s.asks, s.timestamp);
                if !self.refresh_due(s.timestamp) {
                    return Vec::new();
                }
                if let Some(signals) = self.compute_signals(s.timestamp) {
                    let spacing = self.compute_spacing_bps(&signals);
                    let orders = self.generate_orders(signals, account, spacing);
                    self.state.reset_refresh(s.timestamp);
                    return orders;
                }
                Vec::new()
            }
            MarketEvent::Update(u) if u.symbol == self.symbol => {
                self.handle_update(&u.bids, &u.asks, u.timestamp);
                if !self.refresh_due(u.timestamp) {
                    return Vec::new();
                }
                if let Some(signals) = self.compute_signals(u.timestamp) {
                    let spacing = self.compute_spacing_bps(&signals);
                    let orders = self.generate_orders(signals, account, spacing);
                    self.state.reset_refresh(u.timestamp);
                    return orders;
                }
                Vec::new()
            }
            MarketEvent::Trade(t) if t.symbol == self.symbol => {
                self.record_trade(t);
                Vec::new()
            }
            MarketEvent::Bar(bar) if bar.symbol == self.symbol => {
                self.handle_bar(bar);
                if !self.refresh_due(bar.close_time) {
                    return Vec::new();
                }
                if let Some(signals) = self.compute_signals(bar.close_time) {
                    let spacing = self.compute_spacing_bps(&signals);
                    let orders = self.generate_orders(signals, account, spacing);
                    self.state.reset_refresh(bar.close_time);
                    return orders;
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn on_market_event_with_context(
        &mut self,
        event: &MarketEvent,
        context: &StrategyContext<'_>,
    ) -> Vec<OrderIntent> {
        match event {
            MarketEvent::Snapshot(snapshot) if snapshot.symbol == self.symbol => {
                let Some(book) = context
                    .book
                    .filter(|book| book.symbol == &self.symbol && book.venue == self.config.venue)
                else {
                    return Vec::new();
                };
                self.handle_book_view(book);
                self.orders_if_refresh_due(book.timestamp, context.account)
            }
            MarketEvent::Update(update) if update.symbol == self.symbol => {
                let Some(book) = context
                    .book
                    .filter(|book| book.symbol == &self.symbol && book.venue == self.config.venue)
                else {
                    return Vec::new();
                };
                self.handle_book_view(book);
                self.orders_if_refresh_due(book.timestamp, context.account)
            }
            MarketEvent::Quote(quote) if quote.symbol == self.symbol => {
                let Some(book) = context
                    .book
                    .filter(|book| book.symbol == &self.symbol && book.venue == self.config.venue)
                else {
                    return Vec::new();
                };
                self.handle_book_view(book);
                self.orders_if_refresh_due(book.timestamp, context.account)
            }
            _ => self.on_market_event(event, context.account),
        }
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn name(&self) -> &str {
        &self.strategy_id
    }

    fn id(&self) -> &str {
        &self.strategy_id
    }

    fn venue_scope(&self) -> VenueScope {
        VenueScope::Single
    }

    fn initialize(&mut self) -> HftResult<()> {
        let sym = &self.symbol.as_str();
        info!(symbol = %sym, "initializing LOB Flow Grid strategy");
        Ok(())
    }
}

impl LobFlowGridStrategy {
    fn orders_if_refresh_due(
        &mut self,
        timestamp: Timestamp,
        account: &AccountView,
    ) -> Vec<OrderIntent> {
        if !self.refresh_due(timestamp) {
            return Vec::new();
        }
        let Some(signals) = self.compute_signals(timestamp) else {
            return Vec::new();
        };
        let spacing = self.compute_spacing_bps(&signals);
        let orders = self.generate_orders(signals, account, spacing);
        self.state.reset_refresh(timestamp);
        orders
    }
}

#[allow(dead_code)]
#[derive(Debug)]
struct SignalSnapshot {
    timestamp: Timestamp,
    mid: f64,
    obi: f64,
    micro_delta_bps: f64,
    ai: f64,
    ofi: f64,
    best_bid: f64,
    best_ask: f64,
}

#[allow(dead_code)]
#[derive(Debug)]
struct GridLevelSpec {
    level_index: usize,
    spacing_multiplier: f64,
    size_multiplier: f64,
}

fn convert_levels(levels: &[ports::BookLevel], top_levels: usize) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(top_levels.min(levels.len()));
    for level in levels.iter().take(top_levels) {
        if let (Some(price), Some(qty)) = (level.price.to_f64(), level.quantity.to_f64()) {
            out.push((price, qty));
        }
    }
    out
}

fn apply_price_deltas(
    book: &mut Vec<(f64, f64)>,
    deltas: &[ports::BookLevel],
    top_levels: usize,
    descending: bool,
) {
    for level in deltas {
        let (Some(price), Some(quantity)) = (level.price.to_f64(), level.quantity.to_f64()) else {
            continue;
        };
        let existing = book
            .iter()
            .position(|(candidate, _)| candidate.total_cmp(&price).is_eq());
        match (existing, quantity > 0.0) {
            (Some(index), true) => book[index].1 = quantity,
            (Some(index), false) => {
                book.remove(index);
            }
            (None, true) => book.push((price, quantity)),
            (None, false) => {}
        }
    }
    book.sort_unstable_by(|left, right| {
        if descending {
            right.0.total_cmp(&left.0)
        } else {
            left.0.total_cmp(&right.0)
        }
    });
    book.truncate(top_levels);
}

fn round_down_to_tick(price: f64, tick: f64) -> f64 {
    if tick <= 0.0 {
        return price;
    }
    (price / tick).floor() * tick
}

fn round_up_to_tick(price: f64, tick: f64) -> f64 {
    if tick <= 0.0 {
        return price;
    }
    (price / tick).ceil() * tick
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{FixedPrice, FixedQuantity};
    use ports::{BookLevel, L2BookView, MarketSnapshot, StrategyContext};

    fn create_level(price: f64, qty: f64) -> BookLevel {
        BookLevel {
            price: Price::from_f64(price).unwrap(),
            quantity: Quantity::from_f64(qty).unwrap(),
        }
    }

    #[test]
    fn test_rounding_helpers() {
        assert!((round_down_to_tick(100.37, 0.1) - 100.3).abs() < 1e-9);
        assert!((round_up_to_tick(100.37, 0.1) - 100.4).abs() < 1e-9);
    }

    #[test]
    fn test_signal_generation() {
        let symbol = Symbol::new("BTCUSDT");
        let config = LobFlowGridConfig::default();
        let mut strategy = LobFlowGridStrategy::new(symbol.clone(), config);

        let snapshot = MarketSnapshot {
            symbol: symbol.clone(),
            timestamp: 1,
            bids: vec![
                create_level(100.0, 5.0),
                create_level(99.9, 4.0),
                create_level(99.8, 4.0),
            ],
            asks: vec![
                create_level(100.1, 5.0),
                create_level(100.2, 4.0),
                create_level(100.3, 4.0),
            ],
            sequence: 1,
            source_venue: Some(VenueId::ASTERDEX),
        };

        strategy.handle_snapshot(&snapshot.bids, &snapshot.asks, snapshot.timestamp);
        let signals = strategy.compute_signals(1).unwrap();
        assert!(signals.mid > 0.0);
        assert!(signals.best_ask > signals.best_bid);
    }

    #[test]
    fn delta_updates_price_levels_and_removes_zero_quantity() {
        let symbol = Symbol::new("BTCUSDT");
        let mut config = LobFlowGridConfig::default();
        config.top_levels = 3;
        let mut strategy = LobFlowGridStrategy::new(symbol, config);
        strategy.handle_snapshot(
            &[create_level(100.0, 2.0), create_level(99.0, 1.0)],
            &[create_level(101.0, 2.0), create_level(102.0, 1.0)],
            1,
        );

        strategy.handle_update(
            &[create_level(100.0, 0.0), create_level(99.5, 3.0)],
            &[create_level(101.0, 0.0), create_level(100.5, 4.0)],
            2,
        );

        assert_eq!(strategy.state.bids, vec![(99.5, 3.0), (99.0, 1.0)]);
        assert_eq!(strategy.state.asks, vec![(100.5, 4.0), (102.0, 1.0)]);
    }

    #[test]
    fn production_context_consumes_canonical_rebuilt_book() {
        let symbol = Symbol::new("BTCUSDT");
        let mut config = LobFlowGridConfig::default();
        config.venue = VenueId::BINANCE;
        config.top_levels = 2;
        let mut strategy = LobFlowGridStrategy::new(symbol.clone(), config);
        let bid_prices = [FixedPrice::from_f64(100.5), FixedPrice::from_f64(100.0)];
        let bid_quantities = [FixedQuantity::from_f64(5.0), FixedQuantity::from_f64(2.0)];
        let ask_prices = [FixedPrice::from_f64(100.75), FixedPrice::from_f64(101.0)];
        let ask_quantities = [FixedQuantity::from_f64(4.0), FixedQuantity::from_f64(3.0)];
        let account = AccountView::default();
        let context = StrategyContext {
            account: &account,
            book: Some(L2BookView {
                symbol: &symbol,
                venue: VenueId::BINANCE,
                timestamp: 2,
                sequence: 11,
                bid_prices: &bid_prices,
                bid_quantities: &bid_quantities,
                ask_prices: &ask_prices,
                ask_quantities: &ask_quantities,
            }),
        };
        let event = MarketEvent::Update(ports::BookUpdate {
            symbol: symbol.clone(),
            timestamp: 2,
            bids: vec![create_level(100.5, 5.0)],
            asks: Vec::new(),
            first_sequence: Some(11),
            sequence: 11,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
        });

        let _ = strategy.on_market_event_with_context(&event, &context);

        assert_eq!(strategy.state.bids, vec![(100.5, 5.0), (100.0, 2.0)]);
        assert_eq!(strategy.state.asks, vec![(100.75, 4.0), (101.0, 3.0)]);
    }

    #[test]
    fn bar_does_not_replace_an_initialized_exchange_book() {
        let symbol = Symbol::new("BTCUSDT");
        let mut strategy = LobFlowGridStrategy::new(symbol.clone(), LobFlowGridConfig::default());
        strategy.handle_snapshot(&[create_level(100.0, 2.0)], &[create_level(101.0, 2.0)], 1);
        strategy.handle_bar(&ports::AggregatedBar {
            symbol,
            interval_ms: 60_000,
            open_time: 1,
            close_time: 2,
            open: Price::from_f64(200.0).unwrap(),
            high: Price::from_f64(201.0).unwrap(),
            low: Price::from_f64(199.0).unwrap(),
            close: Price::from_f64(200.0).unwrap(),
            volume: Quantity::from_f64(1.0).unwrap(),
            trade_count: 1,
            source_venue: Some(VenueId::ASTERDEX),
        });

        assert_eq!(strategy.state.bids, vec![(100.0, 2.0)]);
        assert_eq!(strategy.state.asks, vec![(101.0, 2.0)]);
    }
}
