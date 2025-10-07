//! 市場數據聚合：TopN SoA + BarBuilder + 多交易所 Joiner
use hft_core::*;
use ports::*;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

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
    pub source_venue: Option<VenueId>,
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
            source_venue: None,
        }
    }

    pub fn add_trade(&mut self, trade: &Trade) {
        // 設置來源交易所（Phase 1 重構：顯式 venue 語義）
        if self.source_venue.is_none() {
            self.source_venue = trade.source_venue;
        }

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
            (self.open, self.high, self.low, self.close)
        {
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
                source_venue: self.source_venue,
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
    pub exchanges: FxHashMap<VenueId, TopNSnapshot>,
    pub last_update: Timestamp,
}

impl CrossExchangeJoiner {
    pub fn new(symbol: Symbol) -> Self {
        Self {
            symbol,
            exchanges: FxHashMap::default(),
            last_update: 0,
        }
    }

    pub fn update_exchange(&mut self, exchange: VenueId, snapshot: TopNSnapshot) {
        self.last_update = std::cmp::max(self.last_update, snapshot.timestamp);
        self.exchanges.insert(exchange, snapshot);
    }

    /// 檢查是否有套利機會（簡單版本：最佳買價 vs 最佳賣價）
    pub fn check_arbitrage_opportunity(
        &self,
        min_spread_bps: Decimal,
    ) -> Option<ArbitrageOpportunity> {
        if self.exchanges.len() < 2 {
            return None;
        }

        let mut best_bid = (FixedPrice::ZERO.raw(), VenueId::BINANCE); // 默認值，實際會被覆蓋
        let mut best_ask = (FixedPrice::from_f64(f64::MAX).raw(), VenueId::BINANCE); // 默認值，實際會被覆蓋

        for (&exchange, snapshot) in &self.exchanges {
            if !snapshot.bid_prices.is_empty() && snapshot.bid_prices[0].raw() > best_bid.0 {
                best_bid = (snapshot.bid_prices[0].raw(), exchange);
            }

            if !snapshot.ask_prices.is_empty() && snapshot.ask_prices[0].raw() < best_ask.0 {
                best_ask = (snapshot.ask_prices[0].raw(), exchange);
            }
        }

        if best_bid.1 != best_ask.1 && best_bid.0 > best_ask.0 {
            let best_bid_fixed = FixedPrice::from_f64(best_bid.0 as f64 / 1_000_000.0);
            let best_ask_fixed = FixedPrice::from_f64(best_ask.0 as f64 / 1_000_000.0);
            let spread_bps_fixed = FixedPrice::spread_bps(best_bid_fixed, best_ask_fixed);

            let min_spread_bps_fixed = FixedBps::from_f64(min_spread_bps.to_f64().unwrap_or(0.0));
            if spread_bps_fixed.raw() >= min_spread_bps_fixed.raw() {
                // 計算最大可交易數量 (取兩邊最小值) - 转换为 Decimal 供边界使用
                let bid_qty = self.exchanges[&best_bid.1]
                    .bid_quantities
                    .get(0)
                    .copied()
                    .unwrap_or(FixedQuantity::ZERO);
                let ask_qty = self.exchanges[&best_ask.1]
                    .ask_quantities
                    .get(0)
                    .copied()
                    .unwrap_or(FixedQuantity::ZERO);
                let max_qty = if bid_qty.raw() < ask_qty.raw() {
                    bid_qty
                } else {
                    ask_qty
                };

                return Some(ArbitrageOpportunity {
                    symbol: self.symbol.clone(),
                    bid_venue: best_bid.1,
                    ask_venue: best_ask.1,
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
    pub fn check_arbitrage_opportunity_fast(
        &self,
        min_spread_bps: FixedBps,
    ) -> Option<ArbitrageOpportunity> {
        if self.exchanges.len() < 2 {
            return None;
        }

        let mut best_bid = (FixedPrice::ZERO, VenueId::BINANCE);
        let mut best_ask = (FixedPrice::from_f64(f64::MAX), VenueId::BINANCE);

        for (&exchange, snapshot) in &self.exchanges {
            if !snapshot.bid_prices.is_empty() {
                let bid_price = snapshot.bid_prices[0]; // 已经是 FixedPrice
                if bid_price.raw() > best_bid.0.raw() {
                    best_bid = (bid_price, exchange);
                }
            }

            if !snapshot.ask_prices.is_empty() {
                let ask_price = snapshot.ask_prices[0]; // 已经是 FixedPrice
                if ask_price.raw() < best_ask.0.raw() {
                    best_ask = (ask_price, exchange);
                }
            }
        }

        if best_bid.1 != best_ask.1 && best_bid.0.raw() > best_ask.0.raw() {
            let spread_bps = FixedPrice::spread_bps(best_ask.0, best_bid.0); // Note: ask-bid for positive spread

            if spread_bps.raw() >= min_spread_bps.raw() {
                // 计算最大可交易数量
                let bid_qty = self.exchanges[&best_bid.1]
                    .bid_quantities
                    .get(0)
                    .copied()
                    .unwrap_or(FixedQuantity::ZERO);
                let ask_qty = self.exchanges[&best_ask.1]
                    .ask_quantities
                    .get(0)
                    .copied()
                    .unwrap_or(FixedQuantity::ZERO);
                let max_qty = if bid_qty.raw() < ask_qty.raw() {
                    bid_qty
                } else {
                    ask_qty
                };

                return Some(ArbitrageOpportunity {
                    symbol: self.symbol.clone(),
                    bid_venue: best_bid.1,
                    ask_venue: best_ask.1,
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
    /// 不可变共享订单簿（per-venue）：只在有变更时替换 Arc
    pub orderbooks: FxHashMap<VenueSymbol, Arc<TopNSnapshot>>,
    pub bar_builders: FxHashMap<(Symbol, u64), BarBuilder>,
    pub joiners: FxHashMap<Symbol, CrossExchangeJoiner>,
    pub stale_threshold_us: u64,
    /// 变更追踪：记录哪些 symbol 的订单簿发生了变化
    pub changed_symbols: FxHashSet<Symbol>,
    /// 快照版本号，用于追踪变化
    pub snapshot_version: u64,
}

impl AggregationEngine {
    pub fn new() -> Self {
        Self {
            top_n: 10, // 預設 Top-10
            orderbooks: FxHashMap::with_capacity_and_hasher(64, Default::default()),
            bar_builders: FxHashMap::with_capacity_and_hasher(128, Default::default()),
            joiners: FxHashMap::with_capacity_and_hasher(32, Default::default()),
            stale_threshold_us: 3000, // 預設 3ms
            changed_symbols: FxHashSet::with_capacity_and_hasher(64, Default::default()),
            snapshot_version: 0,
        }
    }

    pub fn with_config(top_n: usize, stale_threshold_us: u64) -> Self {
        Self {
            top_n,
            orderbooks: FxHashMap::with_capacity_and_hasher(64, Default::default()),
            bar_builders: FxHashMap::with_capacity_and_hasher(128, Default::default()),
            joiners: FxHashMap::with_capacity_and_hasher(32, Default::default()),
            stale_threshold_us,
            changed_symbols: FxHashSet::with_capacity_and_hasher(64, Default::default()),
            snapshot_version: 0,
        }
    }

    pub fn process_market_event_into(
        &mut self,
        event: MarketEvent,
        output_events: &mut Vec<MarketEvent>,
    ) {
        let _aggregation_start = now_micros();

        // TODO: 從 TrackedMarketEvent 中提取延遲追蹤器，並記錄聚合階段
        // let mut tracker = event.tracker;
        // tracker.record_stage(LatencyStage::Aggregation);

        match event {
            MarketEvent::Snapshot(snapshot) => {
                // 创建新的快照（不可变），替换旧的 Arc
                let mut new_topn = if let Some(venue) = snapshot.source_venue {
                    if let Some(existing) = self
                        .orderbooks
                        .get(&VenueSymbol::new(venue, snapshot.symbol.clone()))
                    {
                        // 复用现有容量，避免频繁分配
                        (**existing).clone()
                    } else {
                        TopNSnapshot::new(snapshot.symbol.clone(), self.top_n)
                    }
                } else {
                    TopNSnapshot::new(snapshot.symbol.clone(), self.top_n)
                };

                new_topn.update_from_snapshot(&snapshot);

                // 替换为新的 Arc（需要 source_venue 才能寫入 per-venue 訂單簿）
                if let Some(venue) = snapshot.source_venue {
                    let key = VenueSymbol::new(venue, snapshot.symbol.clone());
                    self.orderbooks.insert(key, Arc::new(new_topn));
                }

                // 标记该 symbol 已变更
                self.changed_symbols.insert(snapshot.symbol.clone());

                // 將原始快照事件透傳給策略（允許策略基於 LOB 做決策，如 OBI）
                output_events.push(MarketEvent::Snapshot(snapshot.clone()));

                // 更新 Joiner：改用 source_venue（不再依賴 symbol 前綴）
                if let Some(venue_id) = snapshot.source_venue {
                    let base_symbol = snapshot.symbol.clone(); // 當前事件使用純 base symbol
                    let joiner = self
                        .joiners
                        .entry(base_symbol.clone())
                        .or_insert_with(|| CrossExchangeJoiner::new(base_symbol));
                    if let Some(topn_arc) = self
                        .orderbooks
                        .get(&VenueSymbol::new(venue_id, snapshot.symbol.clone()))
                    {
                        joiner.update_exchange(venue_id, (**topn_arc).clone());
                        if let Some(arb_opp) = joiner.check_arbitrage_opportunity(Decimal::from(5))
                        {
                            output_events.push(MarketEvent::Arbitrage(arb_opp));
                        }
                    }
                }
            }

            MarketEvent::Trade(trade) => {
                // 更新 K線
                for interval_ms in [60000, 300000, 900000] {
                    // 1m, 5m, 15m
                    let interval_start = (trade.timestamp / interval_ms) * interval_ms;
                    let key = (trade.symbol.clone(), interval_ms);

                    let builder = self.bar_builders.entry(key).or_insert_with(|| {
                        BarBuilder::new(trade.symbol.clone(), interval_ms, interval_start)
                    });

                    builder.add_trade(&trade);

                    if builder.is_complete(trade.timestamp) {
                        if let Some(bar) = builder.build() {
                            output_events.push(MarketEvent::Bar(bar));
                        }

                        // 創建新的 BarBuilder
                        *builder = BarBuilder::new(
                            trade.symbol.clone(),
                            interval_ms,
                            interval_start + interval_ms,
                        );
                    }
                }
            }

            _ => {
                // 其他事件直接透傳
                output_events.push(event);
            }
        }
    }

    pub fn get_market_view(&self, symbol: &Symbol) -> Option<Arc<TopNSnapshot>> {
        // 返回任一場所該 symbol 的 TopN（若存在）
        for (vs, snap) in &self.orderbooks {
            if &vs.symbol == symbol {
                return Some(snap.clone());
            }
        }
        None
    }

    pub fn cleanup_stale_data(&mut self, current_time: Timestamp) {
        // 清理過期的訂單簿
        self.orderbooks.retain(|_, snapshot| {
            current_time.saturating_sub(snapshot.timestamp) < self.stale_threshold_us
        });

        // 清理過期的 K線建構器
        self.bar_builders
            .retain(|_, builder| current_time < builder.close_time + self.stale_threshold_us);
    }

    /// 新增：處理單個事件並返回生成的事件
    pub fn handle_event(&mut self, event: MarketEvent) -> Result<Vec<MarketEvent>, HftError> {
        let mut events = Vec::with_capacity(4);
        self.handle_event_into(event, &mut events)?;
        Ok(events)
    }

    /// 新增：處理事件並將結果寫入提供的緩衝
    pub fn handle_event_into(
        &mut self,
        event: MarketEvent,
        output_events: &mut Vec<MarketEvent>,
    ) -> Result<(), HftError> {
        self.process_market_event_into(event, output_events);
        Ok(())
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
        let mut opportunities = Vec::with_capacity(self.joiners.len());
        for joiner in self.joiners.values() {
            if let Some(opp) = joiner.check_arbitrage_opportunity(Decimal::from(5)) {
                opportunities.push(opp);
            }
        }
        opportunities
    }

    /// 獲取最新時間戳
    fn get_latest_timestamp(&self) -> Timestamp {
        self.orderbooks
            .values()
            .map(|ob| ob.timestamp)
            .max()
            .unwrap_or(0)
    }
}

/// 市場視圖 - 引擎對外發佈的快照（不可变共享）
#[derive(Debug, Clone)]
pub struct MarketView {
    /// 订单簿快照（per-venue）：Arc 实现零拷贝共享
    pub orderbooks: FxHashMap<VenueSymbol, Arc<TopNSnapshot>>,
    pub arbitrage_opportunities: Vec<ArbitrageOpportunity>,
    pub timestamp: Timestamp,
    /// 快照版本号，用于变更检测
    pub version: u64,
}

impl MarketView {
    /// 獲取指定交易對的訂單簿
    pub fn get_orderbook(&self, key: &VenueSymbol) -> Option<&TopNSnapshot> {
        self.orderbooks.get(key).map(|arc| arc.as_ref())
    }

    /// 獲取最佳買價 (边界方法：转换为 Decimal)
    pub fn get_best_bid_for_venue(&self, key: &VenueSymbol) -> Option<(Price, Quantity)> {
        let orderbook = self.get_orderbook(key)?;
        let fixed_price = orderbook.bid_prices.get(0)?;
        let fixed_qty = orderbook.bid_quantities.get(0)?;
        Some(((*fixed_price).into(), (*fixed_qty).into()))
    }

    /// 獲取最佳賣價 (边界方法：转换为 Decimal)
    pub fn get_best_ask_for_venue(&self, key: &VenueSymbol) -> Option<(Price, Quantity)> {
        let orderbook = self.get_orderbook(key)?;
        let fixed_price = orderbook.ask_prices.get(0)?;
        let fixed_qty = orderbook.ask_quantities.get(0)?;
        Some(((*fixed_price).into(), (*fixed_qty).into()))
    }

    /// 獲取中間價 (边界方法：使用快速定点计算后转换)
    pub fn get_mid_price_for_venue(&self, key: &VenueSymbol) -> Option<Price> {
        let fixed_mid = self.get_mid_price_fast_for_venue(key)?;
        Some(fixed_mid.into())
    }

    /// 高性能中间价获取 (热路径优化)
    #[inline]
    pub fn get_mid_price_fast_for_venue(&self, key: &VenueSymbol) -> Option<FixedPrice> {
        let orderbook = self.get_orderbook(key)?;
        orderbook.get_mid_price_fast()
    }

    /// 獲取價差 (基點) - 边界方法：使用快速定点计算后转换
    pub fn get_spread_bps_for_venue(&self, key: &VenueSymbol) -> Option<Bps> {
        let fixed_bps = self.get_spread_bps_fast_for_venue(key)?;
        Some(fixed_bps.into())
    }

    /// 高性能价差获取 (热路径优化)
    #[inline]
    pub fn get_spread_bps_fast_for_venue(&self, key: &VenueSymbol) -> Option<FixedBps> {
        let orderbook = self.get_orderbook(key)?;
        orderbook.get_spread_bps_fast()
    }

    /// 便利方法：取得任一場所的最佳賣價（僅供兼容舊接口使用）
    pub fn get_best_ask_any(&self, symbol: &Symbol) -> Option<(Price, Quantity)> {
        for (k, _) in &self.orderbooks {
            if &k.symbol == symbol {
                return self.get_best_ask_for_venue(k);
            }
        }
        None
    }
    /// 便利方法：取得任一場所的最佳買價（僅供兼容舊接口使用）
    pub fn get_best_bid_any(&self, symbol: &Symbol) -> Option<(Price, Quantity)> {
        for (k, _) in &self.orderbooks {
            if &k.symbol == symbol {
                return self.get_best_bid_for_venue(k);
            }
        }
        None
    }
    /// 便利方法：任一場所中間價
    pub fn get_mid_price_any(&self, symbol: &Symbol) -> Option<Price> {
        for (k, _) in &self.orderbooks {
            if &k.symbol == symbol {
                return self.get_mid_price_for_venue(k);
            }
        }
        None
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
