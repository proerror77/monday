//! Cross-venue orderbook arbitrage strategy template implementing `ports::Strategy`
//! - Joins multi-venue snapshots and emits OrderIntent on opportunities
//! - Detects cross-exchange price discrepancies for simultaneous buy/sell

use hft_core::{HftResult, OrderType, Price, Quantity, Side, Symbol, TimeInForce, VenueId};
use num_traits::{FromPrimitive, ToPrimitive};
use ports::{
    AccountView, ExecutionEvent, MarketEvent, MarketSnapshot, OrderIntent, Strategy, VenueScope,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 跨所套利策略配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageStrategyConfig {
    /// 最小利差 (基點)
    pub min_spread_bps: f64,
    /// 最大單筆交易量
    pub max_position_size: f64,
    /// 最大總倉位
    pub max_total_position: f64,
    /// 資料陳舊度容忍 (微秒)
    pub max_stale_us: u64,
    /// 最小利潤 (美元)
    pub min_profit_usd: f64,
    /// 交易手續費率 (taker)
    pub taker_fee_rate: f64,
}

impl Default for ArbitrageStrategyConfig {
    fn default() -> Self {
        Self {
            min_spread_bps: 10.0,    // 10 基點
            max_position_size: 0.1,  // 0.1 BTC
            max_total_position: 1.0, // 1 BTC
            max_stale_us: 3000,      // 3ms
            min_profit_usd: 5.0,     // 5 美元
            taker_fee_rate: 0.001,   // 0.1%
        }
    }
}

/// 交易所訂單簿快照
#[derive(Debug, Clone)]
pub struct VenueSnapshot {
    pub venue: String,
    pub symbol: Symbol,
    pub timestamp: u64,
    pub best_bid: Option<(Price, Quantity)>,
    pub best_ask: Option<(Price, Quantity)>,
    pub sequence: u64,
}

impl VenueSnapshot {
    pub fn from_market_snapshot(venue: String, snapshot: &MarketSnapshot) -> Self {
        let best_bid = snapshot
            .bids
            .first()
            .map(|level| (level.price, level.quantity));
        let best_ask = snapshot
            .asks
            .first()
            .map(|level| (level.price, level.quantity));

        Self {
            venue,
            symbol: snapshot.symbol.clone(),
            timestamp: snapshot.timestamp,
            best_bid,
            best_ask,
            sequence: snapshot.sequence,
        }
    }

    pub fn is_stale(&self, current_time: u64, threshold_us: u64) -> bool {
        current_time.saturating_sub(self.timestamp) > threshold_us
    }
}

/// 套利機會計算結果
#[derive(Debug, Clone)]
pub struct ArbitrageSignal {
    pub buy_venue: String,
    pub sell_venue: String,
    pub buy_price: Price,
    pub sell_price: Price,
    pub quantity: Quantity,
    pub gross_profit_usd: f64,
    pub net_profit_usd: f64,
    pub spread_bps: f64,
}

/// 跨所套利策略實現
pub struct ArbitrageStrategy {
    symbol: Symbol,
    config: ArbitrageStrategyConfig,
    venue_snapshots: HashMap<String, VenueSnapshot>,
    strategy_id: String,
    last_signal_time: u64,
}

impl ArbitrageStrategy {
    /// 創建新的套利策略（舊版，保持向後兼容性）
    pub fn new(symbol: Symbol, config: ArbitrageStrategyConfig) -> Self {
        // 使用基於符號的穩定 ID，而非動態時間戳
        let strategy_id = format!("arbitrage_{}", symbol.as_str());
        Self::with_name(symbol, config, strategy_id)
    }

    /// 🔥 Phase 1.4: 創建帶穩定名稱的套利策略
    pub fn with_name(
        symbol: Symbol,
        config: ArbitrageStrategyConfig,
        strategy_name: String,
    ) -> Self {
        Self {
            symbol,
            config,
            venue_snapshots: HashMap::new(),
            strategy_id: strategy_name,
            last_signal_time: 0,
        }
    }

    /// 更新交易所快照
    fn update_venue_snapshot(&mut self, venue: String, snapshot: &MarketSnapshot) {
        if snapshot.symbol == self.symbol {
            let venue_snapshot = VenueSnapshot::from_market_snapshot(venue, snapshot);
            self.venue_snapshots
                .insert(venue_snapshot.venue.clone(), venue_snapshot);
        }
    }

    /// 清理陳舊資料
    fn cleanup_stale_data(&mut self, current_time: u64) {
        self.venue_snapshots
            .retain(|_, snapshot| !snapshot.is_stale(current_time, self.config.max_stale_us));
    }

    /// 檢測套利機會
    fn detect_arbitrage_opportunity(&self, current_time: u64) -> Option<ArbitrageSignal> {
        if self.venue_snapshots.len() < 2 {
            return None;
        }

        let mut best_opportunity: Option<ArbitrageSignal> = None;
        let mut max_profit = self.config.min_profit_usd;

        // 比較所有交易所組合
        let venues: Vec<_> = self.venue_snapshots.keys().cloned().collect();
        for i in 0..venues.len() {
            for j in (i + 1)..venues.len() {
                let venue_a = &venues[i];
                let venue_b = &venues[j];

                if let (Some(snap_a), Some(snap_b)) = (
                    self.venue_snapshots.get(venue_a),
                    self.venue_snapshots.get(venue_b),
                ) {
                    // 檢查資料新鮮度
                    if snap_a.is_stale(current_time, self.config.max_stale_us)
                        || snap_b.is_stale(current_time, self.config.max_stale_us)
                    {
                        continue;
                    }

                    // 檢查 A 買 B 賣的機會
                    if let Some(opp) = self.calculate_arbitrage(venue_a, snap_a, venue_b, snap_b) {
                        if opp.net_profit_usd > max_profit {
                            max_profit = opp.net_profit_usd;
                            best_opportunity = Some(opp);
                        }
                    }

                    // 檢查 B 買 A 賣的機會
                    if let Some(opp) = self.calculate_arbitrage(venue_b, snap_b, venue_a, snap_a) {
                        if opp.net_profit_usd > max_profit {
                            max_profit = opp.net_profit_usd;
                            best_opportunity = Some(opp);
                        }
                    }
                }
            }
        }

        best_opportunity
    }

    /// 計算具體套利機會
    fn calculate_arbitrage(
        &self,
        buy_venue: &str,
        buy_snap: &VenueSnapshot,
        sell_venue: &str,
        sell_snap: &VenueSnapshot,
    ) -> Option<ArbitrageSignal> {
        let (buy_price, buy_qty) = buy_snap.best_ask?; // 在買入所賣出價位買入
        let (sell_price, sell_qty) = sell_snap.best_bid?; // 在賣出所買入價位賣出

        // 檢查價差是否足夠
        let buy_price_f64 = buy_price.0.to_f64()?;
        let sell_price_f64 = sell_price.0.to_f64()?;

        if sell_price_f64 <= buy_price_f64 {
            return None; // 無套利空間
        }

        // 計算價差基點
        let spread_bps = ((sell_price_f64 - buy_price_f64) / buy_price_f64) * 10000.0;
        if spread_bps < self.config.min_spread_bps {
            return None;
        }

        // 計算可交易數量 (取最小值)
        let max_quantity = buy_qty
            .0
            .min(sell_qty.0)
            .min(rust_decimal::Decimal::from_f64(
                self.config.max_position_size,
            )?);

        if max_quantity <= rust_decimal::Decimal::ZERO {
            return None;
        }

        let quantity = Quantity(max_quantity);
        let quantity_f64 = max_quantity.to_f64()?;

        // 計算毛利潤
        let gross_profit = (sell_price_f64 - buy_price_f64) * quantity_f64;

        // 計算手續費
        let buy_fee = buy_price_f64 * quantity_f64 * self.config.taker_fee_rate;
        let sell_fee = sell_price_f64 * quantity_f64 * self.config.taker_fee_rate;
        let total_fees = buy_fee + sell_fee;

        // 計算淨利潤
        let net_profit = gross_profit - total_fees;

        if net_profit < self.config.min_profit_usd {
            return None;
        }

        Some(ArbitrageSignal {
            buy_venue: buy_venue.to_string(),
            sell_venue: sell_venue.to_string(),
            buy_price,
            sell_price,
            quantity,
            gross_profit_usd: gross_profit,
            net_profit_usd: net_profit,
            spread_bps,
        })
    }

    /// 根據套利信號生成訂單意圖
    fn signal_to_order_intents(
        &self,
        signal: ArbitrageSignal,
        account: &AccountView,
    ) -> Vec<OrderIntent> {
        let mut orders = Vec::new();

        // 檢查帳戶餘額和倉位限制
        let current_position = account
            .positions
            .get(&self.symbol)
            .map(|pos| pos.quantity.0.to_f64().unwrap_or(0.0))
            .unwrap_or(0.0);

        if current_position.abs() + signal.quantity.0.to_f64().unwrap_or(0.0)
            > self.config.max_total_position
        {
            return orders; // 超過總倉位限制
        }

        // 生成買入訂單 (在買入所)
        orders.push(OrderIntent {
            symbol: Symbol::from(format!("{}:{}", signal.buy_venue, self.symbol.as_str())),
            side: Side::Buy,
            quantity: signal.quantity,
            order_type: OrderType::Market, // 使用市價單確保成交
            price: None,
            time_in_force: TimeInForce::IOC,
            strategy_id: self.strategy_id.clone(),
            target_venue: VenueId::from_str(&signal.buy_venue), // 明確指定買入交易所
        });

        // 生成賣出訂單 (在賣出所)
        orders.push(OrderIntent {
            symbol: Symbol::from(format!("{}:{}", signal.sell_venue, self.symbol.as_str())),
            side: Side::Sell,
            quantity: signal.quantity,
            order_type: OrderType::Market, // 使用市價單確保成交
            price: None,
            time_in_force: TimeInForce::IOC,
            strategy_id: self.strategy_id.clone(),
            target_venue: VenueId::from_str(&signal.sell_venue), // 明確指定賣出交易所
        });

        orders
    }

    /// 解析交易所前綴
    fn parse_venue_symbol(symbol: &Symbol) -> (Option<String>, Symbol) {
        if let Some((venue, base_symbol)) = symbol.as_str().split_once(':') {
            (Some(venue.to_string()), Symbol::new(base_symbol))
        } else {
            (None, symbol.clone())
        }
    }
}

impl Strategy for ArbitrageStrategy {
    fn id(&self) -> &str {
        &self.strategy_id
    }
    fn venue_scope(&self) -> VenueScope {
        VenueScope::Cross
    }
    fn on_market_event(&mut self, event: &MarketEvent, account: &AccountView) -> Vec<OrderIntent> {
        match event {
            MarketEvent::Snapshot(snapshot) => {
                let (venue, base_symbol) = Self::parse_venue_symbol(&snapshot.symbol);

                // 只處理目標品種
                if base_symbol != self.symbol {
                    return Vec::new();
                }

                // 更新交易所快照
                if let Some(venue_name) = venue {
                    self.update_venue_snapshot(venue_name, snapshot);
                }

                // 清理陳舊資料
                let current_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;

                self.cleanup_stale_data(current_time);

                // 檢測套利機會
                if let Some(signal) = self.detect_arbitrage_opportunity(current_time) {
                    self.last_signal_time = current_time;
                    return self.signal_to_order_intents(signal, account);
                }

                Vec::new()
            }
            _ => {
                // 其他事件不處理
                Vec::new()
            }
        }
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        // 執行事件暫不處理，可後續擴展實現風控
        Vec::new()
    }

    fn name(&self) -> &str {
        "ArbitrageStrategy"
    }

    fn initialize(&mut self) -> HftResult<()> {
        log::info!(
            "套利策略初始化: 品種={}, 最小利差={}bps, 最大倉位={}",
            self.symbol.as_str(),
            self.config.min_spread_bps,
            self.config.max_total_position
        );
        Ok(())
    }

    fn shutdown(&mut self) -> HftResult<()> {
        log::info!("套利策略關閉: {}", self.symbol.as_str());
        Ok(())
    }
}

/// 策略工廠函數
pub fn create_arbitrage_strategy(
    symbol: Symbol,
    config: Option<ArbitrageStrategyConfig>,
) -> ArbitrageStrategy {
    let config = config.unwrap_or_default();
    ArbitrageStrategy::new(symbol, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ports::{BookLevel, MarketSnapshot};

    #[test]
    fn test_venue_snapshot_creation() {
        let snapshot = MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 1234567890,
            bids: vec![BookLevel::new_unchecked(50000.0, 1.0)],
            asks: vec![BookLevel::new_unchecked(50100.0, 1.0)],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE),
        };

        let venue_snap = VenueSnapshot::from_market_snapshot("BINANCE".to_string(), &snapshot);

        assert_eq!(venue_snap.venue, "BINANCE");
        assert_eq!(venue_snap.symbol.as_str(), "BTCUSDT");
        assert!(venue_snap.best_bid.is_some());
        assert!(venue_snap.best_ask.is_some());
    }

    #[test]
    fn test_arbitrage_strategy_creation() {
        let symbol = Symbol::new("BTCUSDT");
        let config = ArbitrageStrategyConfig::default();
        let strategy = ArbitrageStrategy::new(symbol, config);

        assert_eq!(strategy.name(), "ArbitrageStrategy");
        assert_eq!(strategy.symbol.as_str(), "BTCUSDT");
    }

    #[test]
    fn test_parse_venue_symbol() {
        let (venue, symbol) =
            ArbitrageStrategy::parse_venue_symbol(&Symbol::new("BINANCE:BTCUSDT"));
        assert_eq!(venue, Some("BINANCE".to_string()));
        assert_eq!(symbol.as_str(), "BTCUSDT");

        let (venue, symbol) = ArbitrageStrategy::parse_venue_symbol(&Symbol::new("BTCUSDT"));
        assert_eq!(venue, None);
        assert_eq!(symbol.as_str(), "BTCUSDT");
    }
}
