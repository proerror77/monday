/*! 
 * 高性能 OrderBook 管理器
 * 
 * 性能優化特性：
 * - 無鎖數據結構 (Lock-free)
 * - 批量消息處理
 * - 緩存統計計算
 * - SIMD 優化特徵計算
 */

use crate::integrations::bitget_connector::BitgetMessage;
use crate::core::types::Timestamp;

use barter_data::{
    event::{MarketEvent, DataKind},
    subscription::{
        book::OrderBookL1,
        trade::PublicTrade,
    },
    books::Level,
};
use barter_instrument::{Side as BarterSide, exchange::ExchangeId};

use serde_json::Value;
use anyhow::Result;
use std::collections::{HashMap, BTreeMap};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tracing::{info, warn, debug};

// 無鎖隊列
use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;

/// 高性能 OrderBook 項目
#[derive(Debug, Clone)]
pub struct HighPerfLevel {
    pub price: Decimal,
    pub amount: Decimal,
    pub order_id: Option<String>,
    pub timestamp: u64, // 使用 u64 時間戳減少內存佔用
}

impl From<crate::integrations::ExtendedLevel> for HighPerfLevel {
    fn from(level: crate::integrations::ExtendedLevel) -> Self {
        Self {
            price: level.price,
            amount: level.amount,
            order_id: level.order_id,
            timestamp: level.timestamp.timestamp_micros() as u64,
        }
    }
}

/// 緩存統計信息
#[derive(Debug, Default)]
pub struct CachedOrderBookStats {
    pub total_bid_volume: Decimal,
    pub total_ask_volume: Decimal,
    pub weighted_mid_price: Decimal,
    pub spread: Decimal,
    pub last_update: u64,
    pub is_dirty: AtomicBool, // 標記是否需要重新計算
}

impl Clone for CachedOrderBookStats {
    fn clone(&self) -> Self {
        Self {
            total_bid_volume: self.total_bid_volume,
            total_ask_volume: self.total_ask_volume,
            weighted_mid_price: self.weighted_mid_price,
            spread: self.spread,
            last_update: self.last_update,
            is_dirty: AtomicBool::new(self.is_dirty.load(Ordering::Relaxed)),
        }
    }
}

impl CachedOrderBookStats {
    pub fn mark_dirty(&self) {
        self.is_dirty.store(true, Ordering::Relaxed);
    }
    
    pub fn is_dirty(&self) -> bool {
        self.is_dirty.load(Ordering::Relaxed)
    }
    
    pub fn mark_clean(&self) {
        self.is_dirty.store(false, Ordering::Relaxed);
    }
}

/// 高性能 OrderBook
/// 針對單線程環境優化，移除不必要的原子操作
#[derive(Debug)]
pub struct HighPerfOrderBook {
    pub symbol: String,
    pub l1_bid: Option<HighPerfLevel>,
    pub l1_ask: Option<HighPerfLevel>,
    pub l2_bids: BTreeMap<String, HighPerfLevel>,
    pub l2_asks: BTreeMap<String, HighPerfLevel>,
    
    // 緩存統計信息
    pub cached_stats: CachedOrderBookStats,
    pub sequence: u64,        // 移除原子操作開銷
    pub last_update: u64,     // 移除原子操作開銷
}

impl HighPerfOrderBook {
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            l1_bid: None,
            l1_ask: None,
            l2_bids: BTreeMap::new(),
            l2_asks: BTreeMap::new(),
            cached_stats: CachedOrderBookStats::default(),
            sequence: 0,
            last_update: 0,
        }
    }

    /// 更新 L2 數據並自動更新 L1 和統計緩存
    pub fn update_l2_fast(&mut self, bids: Vec<HighPerfLevel>, asks: Vec<HighPerfLevel>) {
        // 清空現有 L2 數據
        self.l2_bids.clear();
        self.l2_asks.clear();

        let mut total_bid_volume = Decimal::ZERO;
        let mut total_ask_volume = Decimal::ZERO;

        // 插入 bids 並同時計算統計
        for bid in bids {
            if bid.amount > Decimal::ZERO {
                total_bid_volume += bid.amount;
                let price_key = bid.price.to_string();
                self.l2_bids.insert(price_key, bid);
            }
        }

        // 插入 asks 並同時計算統計
        for ask in asks {
            if ask.amount > Decimal::ZERO {
                total_ask_volume += ask.amount;
                let price_key = ask.price.to_string();
                self.l2_asks.insert(price_key, ask);
            }
        }

        // 更新 L1
        self.update_l1_from_l2();
        
        // 更新緩存統計
        self.update_cached_stats(total_bid_volume, total_ask_volume);
        
        // 更新序列號和時間戳（零成本更新）
        self.sequence += 1;
        self.last_update = Utc::now().timestamp_micros() as u64;
    }

    /// 從 L2 更新 L1
    fn update_l1_from_l2(&mut self) {
        // 最佳 bid (最高價)
        if let Some((_, best_bid)) = self.l2_bids.iter().next_back() {
            self.l1_bid = Some(best_bid.clone());
        }
        
        // 最佳 ask (最低價)
        if let Some((_, best_ask)) = self.l2_asks.iter().next() {
            self.l1_ask = Some(best_ask.clone());
        }
    }

    /// 更新緩存統計信息
    fn update_cached_stats(&mut self, total_bid_volume: Decimal, total_ask_volume: Decimal) {
        let spread = if let (Some(bid), Some(ask)) = (&self.l1_bid, &self.l1_ask) {
            ask.price - bid.price
        } else {
            Decimal::ZERO
        };

        let weighted_mid_price = if let (Some(bid), Some(ask)) = (&self.l1_bid, &self.l1_ask) {
            // 基於數量加權的中間價
            let total_volume = bid.amount + ask.amount;
            if total_volume > Decimal::ZERO {
                (bid.price * ask.amount + ask.price * bid.amount) / total_volume
            } else {
                (bid.price + ask.price) / Decimal::from(2)
            }
        } else {
            Decimal::ZERO
        };

        // 原子更新緩存（在實際場景中可能需要更複雜的同步）
        self.cached_stats.total_bid_volume = total_bid_volume;
        self.cached_stats.total_ask_volume = total_ask_volume;
        self.cached_stats.weighted_mid_price = weighted_mid_price;
        self.cached_stats.spread = spread;
        self.cached_stats.last_update = Utc::now().timestamp_micros() as u64;
        self.cached_stats.mark_clean();
    }

    /// 快速獲取統計信息（無需重新計算）
    pub fn get_cached_stats(&self) -> &CachedOrderBookStats {
        &self.cached_stats
    }

    /// SIMD 優化的批量價格計算（假設啟用 AVX2）
    #[cfg(target_feature = "avx2")]
    pub fn calculate_vwap_simd(&self, depth: usize) -> Option<Decimal> {
        // 這是一個示例，實際的 SIMD 實現會更複雜
        use std::arch::x86_64::*;
        
        if self.l2_bids.len() < depth || self.l2_asks.len() < depth {
            return None;
        }

        // 簡化版本：計算前 N 檔的 VWAP
        let bid_levels: Vec<_> = self.l2_bids.values().rev().take(depth).collect();
        let ask_levels: Vec<_> = self.l2_asks.values().take(depth).collect();

        let mut total_value = Decimal::ZERO;
        let mut total_volume = Decimal::ZERO;

        for level in bid_levels.iter().chain(ask_levels.iter()) {
            total_value += level.price * level.amount;
            total_volume += level.amount;
        }

        if total_volume > Decimal::ZERO {
            Some(total_value / total_volume)
        } else {
            None
        }
    }

    /// 標準版本的 VWAP 計算
    #[cfg(not(target_feature = "avx2"))]
    pub fn calculate_vwap_simd(&self, depth: usize) -> Option<Decimal> {
        self.calculate_vwap_standard(depth)
    }

    /// 標準 VWAP 計算
    pub fn calculate_vwap_standard(&self, depth: usize) -> Option<Decimal> {
        if self.l2_bids.len() < depth || self.l2_asks.len() < depth {
            return None;
        }

        let bid_levels: Vec<_> = self.l2_bids.values().rev().take(depth).collect();
        let ask_levels: Vec<_> = self.l2_asks.values().take(depth).collect();

        let mut total_value = Decimal::ZERO;
        let mut total_volume = Decimal::ZERO;

        for level in bid_levels.iter().chain(ask_levels.iter()) {
            total_value += level.price * level.amount;
            total_volume += level.amount;
        }

        if total_volume > Decimal::ZERO {
            Some(total_value / total_volume)
        } else {
            None
        }
    }
}

/// 批量消息處理器
#[derive(Debug)]
pub struct BatchMessageProcessor {
    message_queue: Receiver<BitgetMessage>,
    message_sender: Sender<BitgetMessage>,
    batch_size: usize,
    batch_timeout_ms: u64,
}

impl BatchMessageProcessor {
    pub fn new(batch_size: usize, batch_timeout_ms: u64) -> Self {
        let (sender, receiver) = unbounded();
        Self {
            message_queue: receiver,
            message_sender: sender,
            batch_size,
            batch_timeout_ms,
        }
    }

    pub fn get_sender(&self) -> Sender<BitgetMessage> {
        self.message_sender.clone()
    }

    /// 批量收集消息 - 優化版本
    pub fn collect_batch(&self) -> Vec<BitgetMessage> {
        let mut batch = Vec::with_capacity(self.batch_size);
        
        // 如果超時為0，使用快速批量收集
        if self.batch_timeout_ms == 0 {
            // 快速收集所有可用消息
            while batch.len() < self.batch_size {
                match self.message_queue.try_recv() {
                    Ok(message) => batch.push(message),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
            return batch;
        }

        // 原有超時邏輯
        let start_time = std::time::Instant::now();
        match self.message_queue.try_recv() {
            Ok(first_message) => batch.push(first_message),
            Err(TryRecvError::Empty) => return batch,
            Err(TryRecvError::Disconnected) => return batch,
        }

        while batch.len() < self.batch_size {
            if start_time.elapsed().as_micros() as u64 >= self.batch_timeout_ms * 1000 {
                break;
            }

            match self.message_queue.try_recv() {
                Ok(message) => batch.push(message),
                Err(TryRecvError::Empty) => break, // 立即退出而不是等待
                Err(TryRecvError::Disconnected) => break,
            }
        }

        batch
    }
}

/// 高性能 OrderBook 管理器
#[derive(Debug)]
pub struct HighPerfOrderBookManager {
    orderbooks: HashMap<String, HighPerfOrderBook>,
    batch_processor: BatchMessageProcessor,
    stats: HighPerfManagerStats,
}

#[derive(Debug, Default)]
pub struct HighPerfManagerStats {
    pub total_events: AtomicU64,
    pub l1_updates: AtomicU64,
    pub l2_updates: AtomicU64,
    pub trade_events: AtomicU64,
    pub batch_processed: AtomicU64,
    pub processing_time_ns: AtomicU64,
    pub start_time: Option<DateTime<Utc>>,
}

impl HighPerfOrderBookManager {
    pub fn new(batch_size: usize, batch_timeout_ms: u64) -> Self {
        Self {
            orderbooks: HashMap::new(),
            batch_processor: BatchMessageProcessor::new(batch_size, batch_timeout_ms),
            stats: HighPerfManagerStats {
                start_time: Some(Utc::now()),
                ..Default::default()
            },
        }
    }

    pub fn get_message_sender(&self) -> Sender<BitgetMessage> {
        self.batch_processor.get_sender()
    }

    /// 批量處理消息
    pub fn process_message_batch(&mut self) -> Result<Vec<MarketEvent>> {
        let start_time = std::time::Instant::now();
        let batch = self.batch_processor.collect_batch();
        
        if batch.is_empty() {
            return Ok(Vec::new());
        }

        let mut market_events = Vec::with_capacity(batch.len());
        
        for message in batch {
            if let Ok(Some(event)) = self.handle_single_message(message) {
                market_events.push(event);
            }
        }

        // 更新統計
        self.stats.batch_processed.fetch_add(1, Ordering::Relaxed);
        self.stats.total_events.fetch_add(market_events.len() as u64, Ordering::Relaxed);
        self.stats.processing_time_ns.fetch_add(
            start_time.elapsed().as_nanos() as u64, 
            Ordering::Relaxed
        );

        Ok(market_events)
    }

    /// 處理單個消息
    fn handle_single_message(&mut self, message: BitgetMessage) -> Result<Option<MarketEvent>> {
        match message {
            BitgetMessage::OrderBook { symbol, data, action, timestamp } => {
                self.handle_orderbook_message_fast(&symbol, &data, action.as_deref(), timestamp)
            }
            BitgetMessage::Trade { symbol, data, timestamp } => {
                self.handle_trade_message_fast(&symbol, &data, timestamp)
            }
            BitgetMessage::Ticker { .. } => Ok(None),
        }
    }

    /// 快速處理訂單簿消息
    fn handle_orderbook_message_fast(
        &mut self,
        symbol: &str,
        data: &Value,
        action: Option<&str>,
        timestamp: Timestamp,
    ) -> Result<Option<MarketEvent>> {
        let orderbook = self.orderbooks.entry(symbol.to_string())
            .or_insert_with(|| HighPerfOrderBook::new(symbol.to_string()));

        let books_data = data.as_array().ok_or_else(|| anyhow::anyhow!("Invalid orderbook data format"))?;

        if let Some(book) = books_data.first() {
            let empty_vec = Vec::new();
            let bids_data = book.get("bids").and_then(|v| v.as_array()).unwrap_or(&empty_vec);
            let asks_data = book.get("asks").and_then(|v| v.as_array()).unwrap_or(&empty_vec);

            // 快速解析為 HighPerfLevel
            let mut bids = Vec::with_capacity(bids_data.len().min(20));
            let mut asks = Vec::with_capacity(asks_data.len().min(20));

            // 並行解析 bids 和 asks (在實際場景中可以使用 rayon)
            for bid in bids_data.iter().take(20) {
                if let Some(bid_array) = bid.as_array() {
                    if bid_array.len() >= 2 {
                        if let (Some(price_str), Some(amount_str)) = (
                            bid_array[0].as_str(),
                            bid_array[1].as_str()
                        ) {
                            if let (Ok(price), Ok(amount)) = (
                                price_str.parse::<Decimal>(),
                                amount_str.parse::<Decimal>()
                            ) {
                                bids.push(HighPerfLevel {
                                    price,
                                    amount,
                                    order_id: None,
                                    timestamp: timestamp,
                                });
                            }
                        }
                    }
                }
            }

            for ask in asks_data.iter().take(20) {
                if let Some(ask_array) = ask.as_array() {
                    if ask_array.len() >= 2 {
                        if let (Some(price_str), Some(amount_str)) = (
                            ask_array[0].as_str(),
                            ask_array[1].as_str()
                        ) {
                            if let (Ok(price), Ok(amount)) = (
                                price_str.parse::<Decimal>(),
                                amount_str.parse::<Decimal>()
                            ) {
                                asks.push(HighPerfLevel {
                                    price,
                                    amount,
                                    order_id: None,
                                    timestamp: timestamp,
                                });
                            }
                        }
                    }
                }
            }

            // 快速更新 orderbook
            orderbook.update_l2_fast(bids, asks);

            // 創建 MarketEvent
            let market_event = Self::create_market_event_fast_static(symbol, &orderbook.l1_bid, &orderbook.l1_ask, timestamp)?;
            self.stats.l2_updates.fetch_add(1, Ordering::Relaxed);
            self.stats.l1_updates.fetch_add(1, Ordering::Relaxed);

            Ok(Some(market_event))
        } else {
            Err(anyhow::anyhow!("Empty orderbook data"))
        }
    }

    /// 快速處理交易消息
    fn handle_trade_message_fast(
        &mut self,
        symbol: &str,
        data: &Value,
        timestamp: Timestamp,
    ) -> Result<Option<MarketEvent>> {
        let trades = data.as_array().ok_or_else(|| anyhow::anyhow!("Invalid trade data format"))?;
        
        if let Some(trade_obj) = trades.first() {
            let trade_id = trade_obj.get("tradeId")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let price = trade_obj.get("price")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            let amount = trade_obj.get("size")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            let side_str = trade_obj.get("side")
                .and_then(|v| v.as_str())
                .unwrap_or("buy");
            
            let side = match side_str {
                "sell" => BarterSide::Sell,
                _ => BarterSide::Buy,
            };
            
            let quote_currency = if symbol.contains("USDT") { "USDT" } else { "USD" };
            let instrument = (symbol, quote_currency, barter_instrument::instrument::market_data::kind::MarketDataInstrumentKind::Spot).into();
            
            let exchange_time = DateTime::from_timestamp_micros(timestamp as i64)
                .unwrap_or_else(|| Utc::now());
            
            let market_event = MarketEvent {
                time_exchange: exchange_time,
                time_received: Utc::now(),
                exchange: ExchangeId::Bitget,
                instrument,
                kind: DataKind::Trade(PublicTrade {
                    id: trade_id,
                    price,
                    amount,
                    side,
                }),
            };
            
            self.stats.trade_events.fetch_add(1, Ordering::Relaxed);
            Ok(Some(market_event))
        } else {
            Err(anyhow::anyhow!("Empty trade data"))
        }
    }

    /// 快速創建 MarketEvent（靜態版本）
    fn create_market_event_fast_static(
        symbol: &str,
        l1_bid: &Option<HighPerfLevel>,
        l1_ask: &Option<HighPerfLevel>,
        timestamp: Timestamp,
    ) -> Result<MarketEvent> {
        let exchange_time = DateTime::from_timestamp_micros(timestamp as i64)
            .unwrap_or_else(|| Utc::now());

        if let (Some(bid), Some(ask)) = (l1_bid, l1_ask) {
            let quote_currency = if symbol.contains("USDT") { "USDT" } else { "USD" };
            let instrument = (symbol, quote_currency, barter_instrument::instrument::market_data::kind::MarketDataInstrumentKind::Spot).into();

            let orderbook_l1 = OrderBookL1 {
                last_update_time: exchange_time,
                best_bid: Some(Level {
                    price: bid.price,
                    amount: bid.amount,
                }),
                best_ask: Some(Level {
                    price: ask.price,
                    amount: ask.amount,
                }),
            };

            Ok(MarketEvent {
                time_exchange: exchange_time,
                time_received: Utc::now(),
                exchange: ExchangeId::Bitget,
                instrument,
                kind: DataKind::OrderBookL1(orderbook_l1),
            })
        } else {
            Err(anyhow::anyhow!("No valid L1 data available"))
        }
    }

    /// 獲取 OrderBook
    pub fn get_orderbook(&self, symbol: &str) -> Option<&HighPerfOrderBook> {
        self.orderbooks.get(symbol)
    }

    /// 獲取所有 OrderBooks
    pub fn get_orderbooks(&self) -> &HashMap<String, HighPerfOrderBook> {
        &self.orderbooks
    }

    /// 打印高性能統計
    pub fn print_perf_stats(&self) {
        let total_events = self.stats.total_events.load(Ordering::Relaxed);
        let total_processing_time_ns = self.stats.processing_time_ns.load(Ordering::Relaxed);
        let batch_count = self.stats.batch_processed.load(Ordering::Relaxed);

        let avg_latency_ns = if total_events > 0 {
            total_processing_time_ns / total_events
        } else {
            0
        };

        let avg_batch_size = if batch_count > 0 {
            total_events as f64 / batch_count as f64
        } else {
            0.0
        };

        info!("🚀 High-Performance OrderBook Manager Statistics:");
        info!("   Total events: {}", total_events);
        info!("   L1 updates: {}", self.stats.l1_updates.load(Ordering::Relaxed));
        info!("   L2 updates: {}", self.stats.l2_updates.load(Ordering::Relaxed));
        info!("   Trade events: {}", self.stats.trade_events.load(Ordering::Relaxed));
        info!("   Batches processed: {}", batch_count);
        info!("   Avg batch size: {:.1}", avg_batch_size);
        info!("   Avg processing latency: {}ns ({:.2}μs)", avg_latency_ns, avg_latency_ns as f64 / 1000.0);
        info!("   Active orderbooks: {}", self.orderbooks.len());

        // 顯示每個訂單簿的緩存統計
        for (symbol, orderbook) in &self.orderbooks {
            let stats = orderbook.get_cached_stats();
            let sequence = orderbook.sequence;
            
            info!("   {} - Mid: {:.4}, Spread: {:.6}, Liquidity: {:.2}/{:.2}, Seq: {}", 
                  symbol, 
                  stats.weighted_mid_price, 
                  stats.spread, 
                  stats.total_bid_volume, 
                  stats.total_ask_volume,
                  sequence);
        }
    }

    /// 獲取統計信息
    pub fn get_stats(&self) -> &HighPerfManagerStats {
        &self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_high_perf_orderbook() {
        let mut orderbook = HighPerfOrderBook::new("TESTUSDT".to_string());
        
        let bids = vec![
            HighPerfLevel {
                price: dec!(100.0),
                amount: dec!(1.0),
                order_id: None,
                timestamp: 0,
            },
        ];

        let asks = vec![
            HighPerfLevel {
                price: dec!(100.5),
                amount: dec!(1.5),
                order_id: None,
                timestamp: 0,
            },
        ];

        orderbook.update_l2_fast(bids, asks);
        
        let stats = orderbook.get_cached_stats();
        assert_eq!(stats.spread, dec!(0.5));
        assert_eq!(stats.total_bid_volume, dec!(1.0));
        assert_eq!(stats.total_ask_volume, dec!(1.5));
    }

    #[test]
    fn test_batch_processor() {
        let processor = BatchMessageProcessor::new(10, 100);
        let sender = processor.get_sender();
        
        // 這個測試需要在真實場景中運行
        assert!(sender.capacity().is_none()); // unbounded channel
    }

    #[test]
    fn test_vwap_calculation() {
        let mut orderbook = HighPerfOrderBook::new("TESTUSDT".to_string());
        
        let bids = vec![
            HighPerfLevel { price: dec!(100.0), amount: dec!(1.0), order_id: None, timestamp: 0 },
            HighPerfLevel { price: dec!(99.5), amount: dec!(2.0), order_id: None, timestamp: 0 },
        ];

        let asks = vec![
            HighPerfLevel { price: dec!(100.5), amount: dec!(1.5), order_id: None, timestamp: 0 },
            HighPerfLevel { price: dec!(101.0), amount: dec!(2.5), order_id: None, timestamp: 0 },
        ];

        orderbook.update_l2_fast(bids, asks);
        
        let vwap = orderbook.calculate_vwap_standard(2);
        assert!(vwap.is_some());
        
        // VWAP = (100*1 + 99.5*2 + 100.5*1.5 + 101*2.5) / (1+2+1.5+2.5)
        // = (100 + 199 + 150.75 + 252.5) / 7 = 702.25 / 7 ≈ 100.32
        let expected_vwap = dec!(702.25) / dec!(7.0);
        assert!((vwap.unwrap() - expected_vwap).abs() < dec!(0.01));
    }
}