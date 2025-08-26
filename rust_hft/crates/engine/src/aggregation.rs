//! 市場數據聚合：TopN SoA + BarBuilder + 多交易所 Joiner
use hft_core::*;
use ports::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

/// TopN 深度快照 - 使用 SoA (Structure of Arrays) 提升性能
/// 内部使用定点存储，边缘转换 Decimal
#[derive(Debug, Clone)]
pub struct TopNSnapshot {
    pub symbol: Symbol,
    pub timestamp: Timestamp,
    pub sequence: u64,
    
    // SoA 定点存储: 提升 SIMD/缓存性能，避免 Decimal 开销
    pub bid_prices: Vec<FixedPrice>,
    pub bid_quantities: Vec<FixedQuantity>,
    pub ask_prices: Vec<FixedPrice>,
    pub ask_quantities: Vec<FixedQuantity>,
}

impl TopNSnapshot {
    pub fn new(symbol: Symbol, top_n: usize) -> Self {
        Self {
            symbol,
            timestamp: 0,
            sequence: 0,
            bid_prices: Vec::with_capacity(top_n),
            bid_quantities: Vec::with_capacity(top_n),
            ask_prices: Vec::with_capacity(top_n),
            ask_quantities: Vec::with_capacity(top_n),
        }
    }
    
    pub fn update_from_snapshot(&mut self, snapshot: &MarketSnapshot) {
        self.timestamp = snapshot.timestamp;
        self.sequence = snapshot.sequence;
        
        // 清空並重新填充
        self.bid_prices.clear();
        self.bid_quantities.clear();
        self.ask_prices.clear();
        self.ask_quantities.clear();
        
        // 填充 bids (已按價格降序排列) - 边界转换 Decimal -> FixedPoint
        for level in &snapshot.bids {
            self.bid_prices.push(level.price.into());
            self.bid_quantities.push(level.quantity.into());
        }
        
        // 填充 asks (已按價格升序排列) - 边界转换 Decimal -> FixedPoint
        for level in &snapshot.asks {
            self.ask_prices.push(level.price.into());
            self.ask_quantities.push(level.quantity.into());
        }
    }
    
    /// 获取中间价 (边界方法：转换回 Decimal 供系统边界使用)
    pub fn get_mid_price(&self) -> Option<Price> {
        // 使用定点快速计算后转换
        let fixed_mid = self.get_mid_price_fast()?;
        Some(fixed_mid.into())
    }
    
    /// 高性能中间价计算 (热路径优化版本) - 直接使用定点数据
    #[inline]
    pub fn get_mid_price_fast(&self) -> Option<FixedPrice> {
        if self.bid_prices.is_empty() || self.ask_prices.is_empty() {
            return None;
        }
        
        let best_bid = self.bid_prices[0];
        let best_ask = self.ask_prices[0];
        
        Some(FixedPrice::mid(best_bid, best_ask))
    }
    
    /// 获取价差基点 (边界方法：转换回 Decimal 供系统边界使用)
    pub fn get_spread_bps(&self) -> Option<Bps> {
        // 使用定点快速计算后转换
        let fixed_bps = self.get_spread_bps_fast()?;
        Some(fixed_bps.into())
    }
    
    /// 高性能价差计算 (热路径优化版本) - 直接使用定点数据
    #[inline]
    pub fn get_spread_bps_fast(&self) -> Option<FixedBps> {
        if self.bid_prices.is_empty() || self.ask_prices.is_empty() {
            return None;
        }
        
        let best_bid = self.bid_prices[0];
        let best_ask = self.ask_prices[0];
        
        if best_bid == FixedPrice::ZERO {
            return None;
        }
        
        Some(FixedPrice::spread_bps(best_bid, best_ask))
    }
}

/// K線建構器 - O(1) 累計
#[derive(Debug, Clone)]
pub struct BarBuilder {
    pub symbol: Symbol,
    pub interval_ms: u64,
    pub open_time: Timestamp,
    pub close_time: Timestamp,
    pub open: Option<Price>,
    pub high: Option<Price>,
    pub low: Option<Price>,
    pub close: Option<Price>,
    pub volume: Quantity,
    pub trade_count: u32,
}

impl BarBuilder {
    pub fn new(symbol: Symbol, interval_ms: u64, start_time: Timestamp) -> Self {
        Self {
            symbol,
            interval_ms,
            open_time: start_time,
            close_time: start_time + interval_ms,
            open: None,
            high: None,
            low: None,
            close: None,
            volume: Quantity::zero(),
            trade_count: 0,
        }
    }
    
    pub fn add_trade(&mut self, trade: &Trade) {
        // 設置 OHLC
        if self.open.is_none() {
            self.open = Some(trade.price);
        }
        
        self.close = Some(trade.price);
        
        match self.high {
            None => self.high = Some(trade.price),
            Some(high) if trade.price.0 > high.0 => self.high = Some(trade.price),
            _ => {}
        }
        
        match self.low {
            None => self.low = Some(trade.price),
            Some(low) if trade.price.0 < low.0 => self.low = Some(trade.price),
            _ => {}
        }
        
        // 累計成交量
        self.volume = Quantity(self.volume.0 + trade.quantity.0);
        self.trade_count += 1;
    }
    
    pub fn is_complete(&self, current_time: Timestamp) -> bool {
        current_time >= self.close_time
    }
    
    pub fn build(&self) -> Option<AggregatedBar> {
        if let (Some(open), Some(high), Some(low), Some(close)) = 
            (self.open, self.high, self.low, self.close) {
            Some(AggregatedBar {
                symbol: self.symbol.clone(),
                interval_ms: self.interval_ms,
                open_time: self.open_time,
                close_time: self.close_time,
                open,
                high,
                low,
                close,
                volume: self.volume,
                trade_count: self.trade_count,
            })
        } else {
            None
        }
    }
}

/// 多交易所深度 Joiner - 合併來自不同交易所的同一品種
#[derive(Debug)]
pub struct CrossExchangeJoiner {
    pub symbol: Symbol,
    pub exchanges: HashMap<String, TopNSnapshot>,
    pub last_update: Timestamp,
}

impl CrossExchangeJoiner {
    pub fn new(symbol: Symbol) -> Self {
        Self {
            symbol,
            exchanges: HashMap::new(),
            last_update: 0,
        }
    }
    
    pub fn update_exchange(&mut self, exchange: String, snapshot: TopNSnapshot) {
        self.last_update = std::cmp::max(self.last_update, snapshot.timestamp);
        self.exchanges.insert(exchange, snapshot);
    }
    
    /// 檢查是否有套利機會（簡單版本：最佳買價 vs 最佳賣價）
    pub fn check_arbitrage_opportunity(&self, min_spread_bps: Decimal) -> Option<ArbitrageOpportunity> {
        if self.exchanges.len() < 2 {
            return None;
        }
        
        let mut best_bid = (FixedPrice::ZERO.raw(), String::new());
        let mut best_ask = (FixedPrice::from_f64(f64::MAX).raw(), String::new());
        
        for (exchange, snapshot) in &self.exchanges {
            if !snapshot.bid_prices.is_empty() && snapshot.bid_prices[0].raw() > best_bid.0 {
                best_bid = (snapshot.bid_prices[0].raw(), exchange.clone());
            }
            
            if !snapshot.ask_prices.is_empty() && snapshot.ask_prices[0].raw() < best_ask.0 {
                best_ask = (snapshot.ask_prices[0].raw(), exchange.clone());
            }
        }
        
        if best_bid.1 != best_ask.1 && best_bid.0 > best_ask.0 {
            let best_bid_fixed = FixedPrice::from_f64(best_bid.0 as f64 / 1_000_000.0);
            let best_ask_fixed = FixedPrice::from_f64(best_ask.0 as f64 / 1_000_000.0);
            let spread_bps_fixed = FixedPrice::spread_bps(best_bid_fixed, best_ask_fixed);
            
            let min_spread_bps_fixed = FixedBps::from_f64(min_spread_bps.to_f64().unwrap_or(0.0));
            if spread_bps_fixed.raw() >= min_spread_bps_fixed.raw() {
                // 計算最大可交易數量 (取兩邊最小值) - 转换为 Decimal 供边界使用
                let bid_qty = self.exchanges[&best_bid.1].bid_quantities.get(0).copied().unwrap_or(FixedQuantity::ZERO);
                let ask_qty = self.exchanges[&best_ask.1].ask_quantities.get(0).copied().unwrap_or(FixedQuantity::ZERO);
                let max_qty = if bid_qty.raw() < ask_qty.raw() { bid_qty } else { ask_qty };
                
                return Some(ArbitrageOpportunity {
                    leg1: Symbol(format!("{}:{}", best_bid.1, self.symbol.0)),
                    leg2: Symbol(format!("{}:{}", best_ask.1, self.symbol.0)),
                    spread_bps: spread_bps_fixed.into(),
                    max_quantity: max_qty.into(),
                    timestamp: self.last_update,
                });
            }
        }
        
        None
    }
    
    /// 高性能套利机会检查 (热路径优化版本)
    #[inline]
    pub fn check_arbitrage_opportunity_fast(&self, min_spread_bps: FixedBps) -> Option<ArbitrageOpportunity> {
        if self.exchanges.len() < 2 {
            return None;
        }
        
        let mut best_bid = (FixedPrice::ZERO, String::new());
        let mut best_ask = (FixedPrice::from_f64(f64::MAX), String::new());
        
        for (exchange, snapshot) in &self.exchanges {
            if !snapshot.bid_prices.is_empty() {
                let bid_price = snapshot.bid_prices[0]; // 已经是 FixedPrice
                if bid_price.raw() > best_bid.0.raw() {
                    best_bid = (bid_price, exchange.clone());
                }
            }
            
            if !snapshot.ask_prices.is_empty() {
                let ask_price = snapshot.ask_prices[0]; // 已经是 FixedPrice
                if ask_price.raw() < best_ask.0.raw() {
                    best_ask = (ask_price, exchange.clone());
                }
            }
        }
        
        if best_bid.1 != best_ask.1 && best_bid.0.raw() > best_ask.0.raw() {
            let spread_bps = FixedPrice::spread_bps(best_ask.0, best_bid.0); // Note: ask-bid for positive spread
            
            if spread_bps.raw() >= min_spread_bps.raw() {
                // 计算最大可交易数量
                let bid_qty = self.exchanges[&best_bid.1].bid_quantities.get(0).copied().unwrap_or(FixedQuantity::ZERO);
                let ask_qty = self.exchanges[&best_ask.1].ask_quantities.get(0).copied().unwrap_or(FixedQuantity::ZERO);
                let max_qty = if bid_qty.raw() < ask_qty.raw() { bid_qty } else { ask_qty };
                
                return Some(ArbitrageOpportunity {
                    leg1: Symbol(format!("{}:{}", best_bid.1, self.symbol.0)),
                    leg2: Symbol(format!("{}:{}", best_ask.1, self.symbol.0)),
                    spread_bps: spread_bps.into(), // 转回 Decimal 用于边界输出
                    max_quantity: max_qty.into(),
                    timestamp: self.last_update,
                });
            }
        }
        
        None
    }
}

/// 聚合引擎 - 統一管理所有聚合功能
pub struct AggregationEngine {
    pub top_n: usize,
    /// 不可变共享订单簿：只在有变更时替换 Arc
    pub orderbooks: HashMap<Symbol, Arc<TopNSnapshot>>,
    pub bar_builders: HashMap<(Symbol, u64), BarBuilder>,
    pub joiners: HashMap<Symbol, CrossExchangeJoiner>,
    pub stale_threshold_us: u64,
    /// 变更追踪：记录哪些 symbol 的订单簿发生了变化
    pub changed_symbols: std::collections::HashSet<Symbol>,
    /// 快照版本号，用于追踪变化
    pub snapshot_version: u64,
}

impl AggregationEngine {
    pub fn new() -> Self {
        Self {
            top_n: 10, // 預設 Top-10
            orderbooks: HashMap::new(),
            bar_builders: HashMap::new(),
            joiners: HashMap::new(),
            stale_threshold_us: 3000, // 預設 3ms
            changed_symbols: HashSet::new(),
            snapshot_version: 0,
        }
    }
    
    pub fn with_config(top_n: usize, stale_threshold_us: u64) -> Self {
        Self {
            top_n,
            orderbooks: HashMap::new(),
            bar_builders: HashMap::new(),
            joiners: HashMap::new(),
            stale_threshold_us,
            changed_symbols: HashSet::new(),
            snapshot_version: 0,
        }
    }
    
    pub fn process_market_event(&mut self, event: MarketEvent) -> Vec<MarketEvent> {
        let _aggregation_start = now_micros();
        let mut output_events = Vec::new();
        
        // TODO: 從 TrackedMarketEvent 中提取延遲追蹤器，並記錄聚合階段
        // let mut tracker = event.tracker;
        // tracker.record_stage(LatencyStage::Aggregation);
        
        match event {
            MarketEvent::Snapshot(snapshot) => {
                // 创建新的快照（不可变），替换旧的 Arc
                let mut new_topn = if let Some(existing) = self.orderbooks.get(&snapshot.symbol) {
                    // 复用现有容量，避免频繁分配
                    (**existing).clone()
                } else {
                    TopNSnapshot::new(snapshot.symbol.clone(), self.top_n)
                };
                
                new_topn.update_from_snapshot(&snapshot);
                
                // 替换为新的 Arc，实现 copy-on-write 语义
                self.orderbooks.insert(snapshot.symbol.clone(), Arc::new(new_topn));
                
                // 标记该 symbol 已变更
                self.changed_symbols.insert(snapshot.symbol.clone());
                
                // 更新 Joiner (假設 symbol 格式為 "EXCHANGE:SYMBOL")
                if let Some((exchange, base_symbol)) = snapshot.symbol.0.split_once(':') {
                    let base_symbol = Symbol(base_symbol.to_string());
                    let joiner = self.joiners.entry(base_symbol.clone())
                        .or_insert_with(|| CrossExchangeJoiner::new(base_symbol));
                    
                    // 获取新创建的 Arc 快照
                    if let Some(topn_arc) = self.orderbooks.get(&snapshot.symbol) {
                        joiner.update_exchange(exchange.to_string(), (**topn_arc).clone());
                        
                        // 檢查套利機會
                        if let Some(arb_opp) = joiner.check_arbitrage_opportunity(Decimal::from(5)) { // 5 bps 最小利差
                            output_events.push(MarketEvent::Arbitrage(arb_opp));
                        }
                    }
                }
            }
            
            MarketEvent::Trade(trade) => {
                // 更新 K線
                for interval_ms in [60000, 300000, 900000] { // 1m, 5m, 15m
                    let interval_start = (trade.timestamp / interval_ms) * interval_ms;
                    let key = (trade.symbol.clone(), interval_ms);
                    
                    let builder = self.bar_builders.entry(key)
                        .or_insert_with(|| BarBuilder::new(trade.symbol.clone(), interval_ms, interval_start));
                    
                    builder.add_trade(&trade);
                    
                    if builder.is_complete(trade.timestamp) {
                        if let Some(bar) = builder.build() {
                            output_events.push(MarketEvent::Bar(bar));
                        }
                        
                        // 創建新的 BarBuilder
                        *builder = BarBuilder::new(trade.symbol.clone(), interval_ms, interval_start + interval_ms);
                    }
                }
            }
            
            _ => {
                // 其他事件直接透傳
                output_events.push(event);
            }
        }
        
        output_events
    }
    
    pub fn get_market_view(&self, symbol: &Symbol) -> Option<Arc<TopNSnapshot>> {
        self.orderbooks.get(symbol).cloned()
    }
    
    pub fn cleanup_stale_data(&mut self, current_time: Timestamp) {
        // 清理過期的訂單簿
        self.orderbooks.retain(|_, snapshot| {
            current_time.saturating_sub(snapshot.timestamp) < self.stale_threshold_us
        });
        
        // 清理過期的 K線建構器
        self.bar_builders.retain(|_, builder| {
            current_time < builder.close_time + self.stale_threshold_us
        });
    }
    
    /// 新增：處理單個事件並返回生成的事件
    pub fn handle_event(&mut self, event: MarketEvent) -> Result<Vec<MarketEvent>, HftError> {
        let events = self.process_market_event(event);
        Ok(events)
    }
    
    /// 检查是否应该发布快照（增量版本）
    pub fn should_publish_snapshot(&self) -> bool {
        // 只有在有变更时才发布
        !self.changed_symbols.is_empty()
    }
    
    /// 标记快照已发布，清除变更标记并增加版本号
    pub fn mark_snapshot_published(&mut self) {
        self.changed_symbols.clear();
        self.snapshot_version += 1;
    }
    
    /// 构建市场视图快照 (零拷贝：只拷贝 Arc 指针)
    pub fn build_market_view(&self) -> MarketView {
        MarketView {
            orderbooks: self.orderbooks.clone(), // 浅拷贝：只复制 Arc 指针，不复制数据
            arbitrage_opportunities: self.get_all_arbitrage_opportunities(),
            timestamp: self.get_latest_timestamp(),
            version: self.snapshot_version,
        }
    }
    
    /// 獲取所有套利機會
    fn get_all_arbitrage_opportunities(&self) -> Vec<ArbitrageOpportunity> {
        let mut opportunities = Vec::new();
        for joiner in self.joiners.values() {
            if let Some(opp) = joiner.check_arbitrage_opportunity(Decimal::from(5)) {
                opportunities.push(opp);
            }
        }
        opportunities
    }
    
    /// 獲取最新時間戳
    fn get_latest_timestamp(&self) -> Timestamp {
        self.orderbooks.values()
            .map(|ob| ob.timestamp)
            .max()
            .unwrap_or(0)
    }
}

/// 市場視圖 - 引擎對外發佈的快照（不可变共享）
#[derive(Debug, Clone)]
pub struct MarketView {
    /// 订单簿快照：Arc 实现零拷贝共享
    pub orderbooks: HashMap<Symbol, Arc<TopNSnapshot>>,
    pub arbitrage_opportunities: Vec<ArbitrageOpportunity>,
    pub timestamp: Timestamp,
    /// 快照版本号，用于变更检测
    pub version: u64,
}

impl MarketView {
    /// 獲取指定交易對的訂單簿
    pub fn get_orderbook(&self, symbol: &Symbol) -> Option<&TopNSnapshot> {
        self.orderbooks.get(symbol).map(|arc| arc.as_ref())
    }
    
    /// 獲取最佳買價 (边界方法：转换为 Decimal)
    pub fn get_best_bid(&self, symbol: &Symbol) -> Option<(Price, Quantity)> {
        let orderbook = self.get_orderbook(symbol)?;
        let fixed_price = orderbook.bid_prices.get(0)?;
        let fixed_qty = orderbook.bid_quantities.get(0)?;
        Some(((*fixed_price).into(), (*fixed_qty).into()))
    }
    
    /// 獲取最佳賣價 (边界方法：转换为 Decimal)
    pub fn get_best_ask(&self, symbol: &Symbol) -> Option<(Price, Quantity)> {
        let orderbook = self.get_orderbook(symbol)?;
        let fixed_price = orderbook.ask_prices.get(0)?;
        let fixed_qty = orderbook.ask_quantities.get(0)?;
        Some(((*fixed_price).into(), (*fixed_qty).into()))
    }
    
    /// 獲取中間價 (边界方法：使用快速定点计算后转换)
    pub fn get_mid_price(&self, symbol: &Symbol) -> Option<Price> {
        let fixed_mid = self.get_mid_price_fast(symbol)?;
        Some(fixed_mid.into())
    }
    
    /// 高性能中间价获取 (热路径优化)
    #[inline]
    pub fn get_mid_price_fast(&self, symbol: &Symbol) -> Option<FixedPrice> {
        let orderbook = self.get_orderbook(symbol)?;
        orderbook.get_mid_price_fast()
    }
    
    /// 獲取價差 (基點) - 边界方法：使用快速定点计算后转换
    pub fn get_spread_bps(&self, symbol: &Symbol) -> Option<Bps> {
        let fixed_bps = self.get_spread_bps_fast(symbol)?;
        Some(fixed_bps.into())
    }
    
    /// 高性能价差获取 (热路径优化)
    #[inline]
    pub fn get_spread_bps_fast(&self, symbol: &Symbol) -> Option<FixedBps> {
        let orderbook = self.get_orderbook(symbol)?;
        orderbook.get_spread_bps_fast()
    }
    
    /// 檢查數據是否陳舊
    pub fn is_stale(&self, threshold_us: u64) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        now.saturating_sub(self.timestamp) > threshold_us
    }
}