//! NBBO (National Best Bid Offer) 市場數據聚合器
//!
//! 實時聚合多個交易所的訂單簿數據，計算跨交易所最佳買賣價格

use std::collections::{HashMap, BTreeMap};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock, Mutex};
use tokio::time::{Duration, Instant};
use tracing::{info, warn, error, debug};
use serde::{Serialize, Deserialize};

use super::message_types::{MarketEvent, OrderBookLevel};
use crate::core::types::*;

/// NBBO 數據結構
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NBBOData {
    /// 交易對
    pub symbol: String,
    
    /// 最佳買價信息
    pub best_bid: Option<NBBOLevel>,
    
    /// 最佳賣價信息  
    pub best_ask: Option<NBBOLevel>,
    
    /// 買價深度（按交易所分組）
    pub bid_depth: BTreeMap<String, Vec<OrderBookLevel>>, // exchange -> levels
    
    /// 賣價深度（按交易所分組）
    pub ask_depth: BTreeMap<String, Vec<OrderBookLevel>>, // exchange -> levels
    
    /// 更新時間戳
    pub timestamp: u64,
    
    /// 聚合統計
    pub stats: NBBOStats,
}

/// NBBO 層級信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NBBOLevel {
    /// 價格
    pub price: f64,
    
    /// 總數量（跨交易所聚合）
    pub total_quantity: f64,
    
    /// 交易所分佈
    pub exchanges: Vec<ExchangeLevel>,
    
    /// 更新時間戳
    pub timestamp: u64,
}

/// 交易所層級
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeLevel {
    /// 交易所名稱
    pub exchange: String,
    
    /// 該交易所在此價格的數量
    pub quantity: f64,
    
    /// 訂單數量
    pub order_count: u32,
    
    /// 延遲（毫秒）
    pub latency_ms: Option<f64>,
}

/// NBBO 統計信息
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NBBOStats {
    /// 跨交易所價差（基點）
    pub spread_bps: f64,
    
    /// 買賣價差（基點）
    pub bid_ask_spread_bps: f64,
    
    /// 流動性深度（前10檔總量）
    pub bid_liquidity_depth: f64,
    pub ask_liquidity_depth: f64,
    
    /// 活躍交易所數量
    pub active_exchanges: u32,
    
    /// 數據新鮮度（毫秒）
    pub data_freshness_ms: f64,
    
    /// 更新頻率
    pub update_frequency: f64,
}

/// NBBO 聚合器
pub struct NBBOAggregator {
    /// 訂閱的交易對
    symbols: Arc<RwLock<Vec<String>>>,
    
    /// 各交易所的訂單簿數據
    orderbooks: Arc<RwLock<HashMap<String, HashMap<String, OrderBookSnapshot>>>>>, // symbol -> exchange -> snapshot
    
    /// 聚合的NBBO數據
    nbbo_data: Arc<RwLock<HashMap<String, NBBOData>>>, // symbol -> NBBO
    
    /// 事件發佈器
    nbbo_sender: mpsc::UnboundedSender<NBBOUpdate>,
    
    /// 配置
    config: NBBOConfig,
    
    /// 統計信息
    stats: Arc<RwLock<NBBOAggregatorStats>>,
    
    /// 最後更新時間
    last_update: Arc<RwLock<HashMap<String, Instant>>>, // symbol -> last_update
}

/// 訂單簿快照
#[derive(Debug, Clone)]
pub struct OrderBookSnapshot {
    pub exchange: String,
    pub symbol: String,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub timestamp: u64,
    pub sequence: u64,
    pub is_stale: bool,
}

/// NBBO 更新事件
#[derive(Debug, Clone)]
pub enum NBBOUpdate {
    /// 新的NBBO數據
    DataUpdate(NBBOData),
    
    /// 套利機會
    ArbitrageOpportunity {
        symbol: String,
        buy_exchange: String,
        sell_exchange: String,
        profit_bps: f64,
        timestamp: u64,
    },
    
    /// 流動性告警
    LiquidityAlert {
        symbol: String,
        side: OrderSide,
        remaining_depth: f64,
        threshold_breached: f64,
        timestamp: u64,
    },
    
    /// 數據陳舊告警
    StaleDataAlert {
        symbol: String,
        exchange: String,
        age_ms: f64,
        timestamp: u64,
    },
}

/// NBBO 配置
#[derive(Debug, Clone)]
pub struct NBBOConfig {
    /// 數據陳舊閾值（毫秒）
    pub stale_threshold_ms: u64,
    
    /// 聚合間隔（毫秒）
    pub aggregation_interval_ms: u64,
    
    /// 套利機會最小利潤（基點）
    pub min_arbitrage_profit_bps: f64,
    
    /// 流動性告警閾值
    pub liquidity_alert_threshold: f64,
    
    /// 保留深度層數
    pub depth_levels: u32,
    
    /// 價格精度
    pub price_precision: u32,
}

impl Default for NBBOConfig {
    fn default() -> Self {
        Self {
            stale_threshold_ms: 5000,    // 5秒
            aggregation_interval_ms: 100, // 100毫秒
            min_arbitrage_profit_bps: 5.0, // 5個基點
            liquidity_alert_threshold: 10000.0, // $10,000
            depth_levels: 10,
            price_precision: 8,
        }
    }
}

/// NBBO 聚合器統計
#[derive(Debug, Clone, Default)]
pub struct NBBOAggregatorStats {
    pub total_updates: u64,
    pub arbitrage_opportunities: u64,
    pub stale_data_alerts: u64,
    pub liquidity_alerts: u64,
    pub avg_aggregation_time_us: f64,
    pub symbols_tracked: u32,
    pub exchanges_connected: u32,
}

impl NBBOAggregator {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<NBBOUpdate>) {
        let (nbbo_sender, nbbo_receiver) = mpsc::unbounded_channel();
        
        let aggregator = Self {
            symbols: Arc::new(RwLock::new(Vec::new())),
            orderbooks: Arc::new(RwLock::new(HashMap::new())),
            nbbo_data: Arc::new(RwLock::new(HashMap::new())),
            nbbo_sender,
            config: NBBOConfig::default(),
            stats: Arc::new(RwLock::new(NBBOAggregatorStats::default())),
            last_update: Arc::new(RwLock::new(HashMap::new())),
        };
        
        (aggregator, nbbo_receiver)
    }
    
    pub fn with_config(config: NBBOConfig) -> (Self, mpsc::UnboundedReceiver<NBBOUpdate>) {
        let (mut aggregator, receiver) = Self::new();
        aggregator.config = config;
        (aggregator, receiver)
    }
    
    /// 啟動NBBO聚合器
    pub async fn start(&self) -> Result<(), String> {
        info!("Starting NBBO Aggregator");
        
        // 啟動聚合任務
        self.start_aggregation_task().await;
        
        // 啟動陳舊數據檢查任務
        self.start_stale_data_checker().await;
        
        // 啟動套利機會檢測任務
        self.start_arbitrage_detector().await;
        
        info!("NBBO Aggregator started successfully");
        Ok(())
    }
    
    /// 添加交易對
    pub async fn add_symbol(&self, symbol: String) {
        let mut symbols = self.symbols.write().await;
        if !symbols.contains(&symbol) {
            symbols.push(symbol.clone());
            info!("Added symbol to NBBO tracking: {}", symbol);
        }
    }
    
    /// 處理市場事件
    pub async fn handle_market_event(&self, event: MarketEvent) {
        match event {
            MarketEvent::OrderBookUpdate { symbol, exchange, bids, asks, timestamp, sequence, is_snapshot } => {
                self.update_orderbook(symbol, exchange, bids, asks, timestamp, sequence, is_snapshot).await;
            },
            _ => {
                // 忽略其他類型的事件
            }
        }
    }
    
    /// 更新訂單簿
    async fn update_orderbook(
        &self,
        symbol: String,
        exchange: String,
        bids: Vec<OrderBookLevel>,
        asks: Vec<OrderBookLevel>,
        timestamp: u64,
        sequence: u64,
        _is_snapshot: bool,
    ) {
        let snapshot = OrderBookSnapshot {
            exchange: exchange.clone(),
            symbol: symbol.clone(),
            bids,
            asks,
            timestamp,
            sequence,
            is_stale: false,
        };
        
        // 更新訂單簿數據
        {
            let mut orderbooks = self.orderbooks.write().await;
            let symbol_books = orderbooks.entry(symbol.clone()).or_insert_with(HashMap::new);
            symbol_books.insert(exchange, snapshot);
        }
        
        // 更新最後更新時間
        {
            let mut last_update = self.last_update.write().await;
            last_update.insert(symbol.clone(), Instant::now());
        }
        
        // 觸發聚合
        self.aggregate_symbol(&symbol).await;
    }
    
    /// 聚合單個交易對的NBBO數據
    async fn aggregate_symbol(&self, symbol: &str) {
        let start_time = Instant::now();
        
        let orderbooks = {
            let orderbooks_lock = self.orderbooks.read().await;
            if let Some(symbol_books) = orderbooks_lock.get(symbol) {
                symbol_books.clone()
            } else {
                return;
            }
        };
        
        if orderbooks.is_empty() {
            return;
        }
        
        // 聚合所有交易所的買賣數據
        let mut all_bids: BTreeMap<String, Vec<OrderBookLevel>> = BTreeMap::new();
        let mut all_asks: BTreeMap<String, Vec<OrderBookLevel>> = BTreeMap::new();
        let mut active_exchanges = 0;
        
        for (exchange, snapshot) in &orderbooks {
            if !snapshot.is_stale {
                all_bids.insert(exchange.clone(), snapshot.bids.clone());
                all_asks.insert(exchange.clone(), snapshot.asks.clone());
                active_exchanges += 1;
            }
        }
        
        if active_exchanges == 0 {
            return;
        }
        
        // 計算最佳買賣價
        let best_bid = self.calculate_best_level(&all_bids, true).await;
        let best_ask = self.calculate_best_level(&all_asks, false).await;
        
        // 計算統計數據
        let stats = self.calculate_stats(&best_bid, &best_ask, &all_bids, &all_asks, active_exchanges).await;
        
        // 創建NBBO數據
        let nbbo_data = NBBOData {
            symbol: symbol.to_string(),
            best_bid,
            best_ask,
            bid_depth: all_bids,
            ask_depth: all_asks,
            timestamp: chrono::Utc::now().timestamp_nanos() as u64,
            stats,
        };
        
        // 存儲NBBO數據
        {
            let mut nbbo_lock = self.nbbo_data.write().await;
            nbbo_lock.insert(symbol.to_string(), nbbo_data.clone());
        }
        
        // 發送更新事件
        let _ = self.nbbo_sender.send(NBBOUpdate::DataUpdate(nbbo_data));
        
        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.total_updates += 1;
            let elapsed = start_time.elapsed();
            stats.avg_aggregation_time_us = (stats.avg_aggregation_time_us * (stats.total_updates - 1) as f64 + elapsed.as_micros() as f64) / stats.total_updates as f64;
        }
    }
    
    /// 計算最佳層級
    async fn calculate_best_level(&self, levels_by_exchange: &BTreeMap<String, Vec<OrderBookLevel>>, is_bid: bool) -> Option<NBBOLevel> {
        if levels_by_exchange.is_empty() {
            return None;
        }
        
        // 創建價格聚合映射 price -> Vec<ExchangeLevel>
        let mut price_levels: BTreeMap<OrderedFloat<f64>, Vec<ExchangeLevel>> = BTreeMap::new();
        
        for (exchange, levels) in levels_by_exchange {
            for level in levels.iter().take(self.config.depth_levels as usize) {
                let price = OrderedFloat(level.price);
                let exchange_level = ExchangeLevel {
                    exchange: exchange.clone(),
                    quantity: level.quantity,
                    order_count: level.order_count,
                    latency_ms: None, // TODO: 實現延遲測量
                };
                
                price_levels.entry(price).or_insert_with(Vec::new).push(exchange_level);
            }
        }
        
        if price_levels.is_empty() {
            return None;
        }
        
        // 根據買賣方向選擇最佳價格
        let best_price_entry = if is_bid {
            price_levels.last_key_value() // 買價選最高
        } else {
            price_levels.first_key_value() // 賣價選最低
        };
        
        if let Some((price, exchanges)) = best_price_entry {
            let total_quantity = exchanges.iter().map(|e| e.quantity).sum();
            
            Some(NBBOLevel {
                price: price.0,
                total_quantity,
                exchanges: exchanges.clone(),
                timestamp: chrono::Utc::now().timestamp_nanos() as u64,
            })
        } else {
            None
        }
    }
    
    /// 計算統計數據
    async fn calculate_stats(
        &self,
        best_bid: &Option<NBBOLevel>,
        best_ask: &Option<NBBOLevel>,
        all_bids: &BTreeMap<String, Vec<OrderBookLevel>>,
        all_asks: &BTreeMap<String, Vec<OrderBookLevel>>,
        active_exchanges: u32,
    ) -> NBBOStats {
        let mut stats = NBBOStats::default();
        stats.active_exchanges = active_exchanges;
        
        // 計算買賣價差
        if let (Some(bid), Some(ask)) = (best_bid, best_ask) {
            if bid.price > 0.0 && ask.price > 0.0 {
                let spread = ask.price - bid.price;
                stats.bid_ask_spread_bps = (spread / bid.price) * 10000.0;
            }
        }
        
        // 計算流動性深度
        stats.bid_liquidity_depth = all_bids.values()
            .flat_map(|levels| levels.iter().take(10))
            .map(|level| level.quantity * level.price)
            .sum();
            
        stats.ask_liquidity_depth = all_asks.values()
            .flat_map(|levels| levels.iter().take(10))
            .map(|level| level.quantity * level.price)
            .sum();
        
        // TODO: 計算跨交易所價差、數據新鮮度等
        
        stats
    }
    
    /// 獲取NBBO數據
    pub async fn get_nbbo(&self, symbol: &str) -> Option<NBBOData> {
        self.nbbo_data.read().await.get(symbol).cloned()
    }
    
    /// 獲取統計信息
    pub async fn get_stats(&self) -> NBBOAggregatorStats {
        self.stats.read().await.clone()
    }
    
    /// 啟動聚合任務
    async fn start_aggregation_task(&self) {
        let symbols = self.symbols.clone();
        let config = self.config.clone();
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(config.aggregation_interval_ms));
            
            loop {
                interval.tick().await;
                
                let symbols_to_process = symbols.read().await.clone();
                for symbol in symbols_to_process {
                    // 定期聚合邏輯在 handle_market_event 中實現
                    debug!("Periodic aggregation for symbol: {}", symbol);
                }
            }
        });
    }
    
    /// 啟動陳舊數據檢查任務
    async fn start_stale_data_checker(&self) {
        let orderbooks = self.orderbooks.clone();
        let nbbo_sender = self.nbbo_sender.clone();
        let config = self.config.clone();
        let stats = self.stats.clone();
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(config.stale_threshold_ms / 2));
            
            loop {
                interval.tick().await;
                
                let mut orderbooks_lock = orderbooks.write().await;
                let now = Instant::now();
                
                for (symbol, exchanges) in orderbooks_lock.iter_mut() {
                    for (exchange, snapshot) in exchanges.iter_mut() {
                        let age_ms = now.duration_since(
                            Instant::now() - Duration::from_nanos((chrono::Utc::now().timestamp_nanos() as u64 - snapshot.timestamp) / 1_000_000)
                        ).as_millis() as f64;
                        
                        if age_ms > config.stale_threshold_ms as f64 {
                            if !snapshot.is_stale {
                                snapshot.is_stale = true;
                                
                                let _ = nbbo_sender.send(NBBOUpdate::StaleDataAlert {
                                    symbol: symbol.clone(),
                                    exchange: exchange.clone(),
                                    age_ms,
                                    timestamp: chrono::Utc::now().timestamp_nanos() as u64,
                                });
                                
                                // 更新統計
                                {
                                    let mut stats_lock = stats.write().await;
                                    stats_lock.stale_data_alerts += 1;
                                }
                            }
                        }
                    }
                }
            }
        });
    }
    
    /// 啟動套利機會檢測任務  
    async fn start_arbitrage_detector(&self) {
        let nbbo_data = self.nbbo_data.clone();
        let nbbo_sender = self.nbbo_sender.clone();
        let config = self.config.clone();
        let stats = self.stats.clone();
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(config.aggregation_interval_ms));
            
            loop {
                interval.tick().await;
                
                let nbbo_lock = nbbo_data.read().await;
                
                for (symbol, data) in nbbo_lock.iter() {
                    // 檢測跨交易所套利機會
                    if let (Some(best_bid), Some(best_ask)) = (&data.best_bid, &data.best_ask) {
                        // 尋找最高買價和最低賣價來自不同交易所的情況
                        for bid_exchange in &best_bid.exchanges {
                            for ask_exchange in &best_ask.exchanges {
                                if bid_exchange.exchange != ask_exchange.exchange {
                                    let profit = best_bid.price - best_ask.price;
                                    let profit_bps = (profit / best_ask.price) * 10000.0;
                                    
                                    if profit_bps > config.min_arbitrage_profit_bps {
                                        let _ = nbbo_sender.send(NBBOUpdate::ArbitrageOpportunity {
                                            symbol: symbol.clone(),
                                            buy_exchange: ask_exchange.exchange.clone(),
                                            sell_exchange: bid_exchange.exchange.clone(),
                                            profit_bps,
                                            timestamp: chrono::Utc::now().timestamp_nanos() as u64,
                                        });
                                        
                                        // 更新統計
                                        {
                                            let mut stats_lock = stats.write().await;
                                            stats_lock.arbitrage_opportunities += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}

// 用於價格排序的包裝器
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy)]
struct OrderedFloat<T>(T);

impl PartialEq for OrderedFloat<f64> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for OrderedFloat<f64> {}

impl PartialOrd for OrderedFloat<f64> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

impl Ord for OrderedFloat<f64> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Equal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_nbbo_aggregator_creation() {
        let (aggregator, _receiver) = NBBOAggregator::new();
        aggregator.add_symbol("BTCUSDT".to_string()).await;
        
        let symbols = aggregator.symbols.read().await;
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0], "BTCUSDT");
    }
    
    #[tokio::test]
    async fn test_orderbook_update() {
        let (aggregator, _receiver) = NBBOAggregator::new();
        aggregator.add_symbol("BTCUSDT".to_string()).await;
        
        let bids = vec![
            OrderBookLevel { price: 50000.0, quantity: 1.0, order_count: 1 },
            OrderBookLevel { price: 49999.0, quantity: 2.0, order_count: 2 },
        ];
        
        let asks = vec![
            OrderBookLevel { price: 50001.0, quantity: 1.5, order_count: 1 },
            OrderBookLevel { price: 50002.0, quantity: 2.5, order_count: 2 },
        ];
        
        aggregator.update_orderbook(
            "BTCUSDT".to_string(),
            "binance".to_string(),
            bids,
            asks,
            chrono::Utc::now().timestamp_nanos() as u64,
            1,
            true,
        ).await;
        
        let nbbo = aggregator.get_nbbo("BTCUSDT").await;
        assert!(nbbo.is_some());
        
        let nbbo_data = nbbo.unwrap();
        assert!(nbbo_data.best_bid.is_some());
        assert!(nbbo_data.best_ask.is_some());
        assert_eq!(nbbo_data.best_bid.unwrap().price, 50000.0);
        assert_eq!(nbbo_data.best_ask.unwrap().price, 50001.0);
    }
}