/*!
 * Barter-Data 兼容的 OrderBook 管理器
 * 
 * 支持多層級訂單簿：
 * - L1: 最佳買賣價 (Best Bid/Ask)
 * - L2: 多檔位價格深度 (Multiple Price Levels)
 * - L3: 完整訂單簿 (Full Order Book with Order IDs)
 */

use crate::integrations::bitget_connector::{BitgetMessage, BitgetChannel};
use crate::core::types::Timestamp;

use barter_data::{
    event::{MarketEvent, DataKind},
    subscription::{
        book::OrderBookL1,
        trade::PublicTrade,
    },
    books::Level,
    error::DataError,
};
use barter_instrument::{Side as BarterSide, exchange::ExchangeId};

use serde_json::Value;
use anyhow::Result;
use std::collections::{HashMap, BTreeMap};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tracing::{info, warn, error, debug};

/// OrderBook 層級類型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderBookLevel {
    /// L1: 最佳買賣價 (books1 - 60ms 推送)
    L1,
    /// L2: 多檔位價格深度 (books5/books15 - 200ms 推送)
    L2,
    /// L3: 完整訂單簿 (包含訂單 ID) - Bitget 不支持，預留設計
    L3,
}

/// Bitget 支援的深度類型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitgetDepthType {
    /// books1: 1檔位深度，60ms 推送頻率
    Books1,
    /// books5: 5檔位深度，200ms 推送頻率  
    Books5,
    /// books15: 15檔位深度，200ms 推送頻率
    Books15,
    /// books: 全量深度，首次 snapshot，後續 update，200ms 推送頻率
    Books,
}

/// 擴展的訂單簿項目，包含訂單 ID (L3 專用)
#[derive(Debug, Clone)]
pub struct ExtendedLevel {
    /// 價格
    pub price: Decimal,
    /// 數量
    pub amount: Decimal,
    /// 訂單 ID (L3 專用，L1/L2 為 None)
    pub order_id: Option<String>,
    /// 更新時間戳
    pub timestamp: DateTime<Utc>,
}

impl From<Level> for ExtendedLevel {
    fn from(level: Level) -> Self {
        Self {
            price: level.price,
            amount: level.amount,
            order_id: None,
            timestamp: Utc::now(),
        }
    }
}

/// 多層級訂單簿狀態
#[derive(Debug, Clone)]
pub struct MultiLevelOrderBook {
    /// 交易對符號
    pub symbol: String,
    
    /// L1 數據：最佳買賣價
    pub l1_bid: Option<ExtendedLevel>,
    pub l1_ask: Option<ExtendedLevel>,
    
    /// L2 數據：多檔位價格深度 (價格 -> 數量)
    pub l2_bids: BTreeMap<String, ExtendedLevel>, // 使用 String 作為 key 以保持精度
    pub l2_asks: BTreeMap<String, ExtendedLevel>,
    
    /// L3 數據：完整訂單簿 (訂單 ID -> 訂單詳情) - Bitget 不支持
    pub l3_bids: HashMap<String, ExtendedLevel>,
    pub l3_asks: HashMap<String, ExtendedLevel>,
    
    /// Bitget 特定字段
    pub last_checksum: Option<i32>,    // Bitget checksum 驗證
    pub last_sequence: Option<u64>,    // Bitget sequence 號
    pub is_snapshot: bool,             // 是否為 snapshot 數據
    
    /// 最後更新時間
    pub last_update: DateTime<Utc>,
    /// 更新序列號
    pub sequence: u64,
}

impl MultiLevelOrderBook {
    /// 創建新的多層級訂單簿
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            l1_bid: None,
            l1_ask: None,
            l2_bids: BTreeMap::new(),
            l2_asks: BTreeMap::new(),
            l3_bids: HashMap::new(),
            l3_asks: HashMap::new(),
            last_checksum: None,
            last_sequence: None,
            is_snapshot: false,
            last_update: Utc::now(),
            sequence: 0,
        }
    }

    /// 更新 L1 數據
    pub fn update_l1(&mut self, bid: Option<ExtendedLevel>, ask: Option<ExtendedLevel>) {
        if let Some(bid) = bid {
            self.l1_bid = Some(bid);
        }
        if let Some(ask) = ask {
            self.l1_ask = Some(ask);
        }
        self.last_update = Utc::now();
        self.sequence += 1;
    }

    /// 更新 L2 數據
    pub fn update_l2(&mut self, bids: Vec<ExtendedLevel>, asks: Vec<ExtendedLevel>) {
        // 清空現有 L2 數據 (全量更新)
        self.l2_bids.clear();
        self.l2_asks.clear();

        // 插入新的 bids (按價格排序，高到低)
        for bid in bids {
            let price_key = bid.price.to_string();
            if bid.amount > Decimal::ZERO {
                self.l2_bids.insert(price_key, bid);
            }
        }

        // 插入新的 asks (按價格排序，低到高)  
        for ask in asks {
            let price_key = ask.price.to_string();
            if ask.amount > Decimal::ZERO {
                self.l2_asks.insert(price_key, ask);
            }
        }

        // 同時更新 L1
        if let Some((_, best_bid)) = self.l2_bids.iter().next_back() {
            self.l1_bid = Some(best_bid.clone());
        }
        if let Some((_, best_ask)) = self.l2_asks.iter().next() {
            self.l1_ask = Some(best_ask.clone());
        }

        self.last_update = Utc::now();
        self.sequence += 1;
    }

    /// 更新 L3 數據 (增量更新)
    pub fn update_l3(&mut self, order_id: String, side: BarterSide, level: Option<ExtendedLevel>) {
        let target_map = match side {
            BarterSide::Buy => &mut self.l3_bids,
            BarterSide::Sell => &mut self.l3_asks,
        };

        match level {
            Some(mut level) => {
                level.order_id = Some(order_id.clone());
                target_map.insert(order_id, level);
            }
            None => {
                // 刪除訂單
                target_map.remove(&order_id);
            }
        }

        // 重新計算 L2 和 L1 數據從 L3
        self.recalculate_l2_from_l3();
        self.last_update = Utc::now();
        self.sequence += 1;
    }

    /// 從 L3 數據重新計算 L2 和 L1
    fn recalculate_l2_from_l3(&mut self) {
        // 聚合 L3 數據到 L2
        let mut l2_bids: BTreeMap<String, ExtendedLevel> = BTreeMap::new();
        let mut l2_asks: BTreeMap<String, ExtendedLevel> = BTreeMap::new();

        // 聚合 bids
        for level in self.l3_bids.values() {
            let price_key = level.price.to_string();
            l2_bids.entry(price_key)
                .and_modify(|existing| existing.amount += level.amount)
                .or_insert_with(|| level.clone());
        }

        // 聚合 asks
        for level in self.l3_asks.values() {
            let price_key = level.price.to_string();
            l2_asks.entry(price_key)
                .and_modify(|existing| existing.amount += level.amount)
                .or_insert_with(|| level.clone());
        }

        self.l2_bids = l2_bids;
        self.l2_asks = l2_asks;

        // 更新 L1
        if let Some((_, best_bid)) = self.l2_bids.iter().next_back() {
            self.l1_bid = Some(best_bid.clone());
        }
        if let Some((_, best_ask)) = self.l2_asks.iter().next() {
            self.l1_ask = Some(best_ask.clone());
        }
    }

    /// 獲取價差
    pub fn get_spread(&self) -> Option<Decimal> {
        if let (Some(bid), Some(ask)) = (&self.l1_bid, &self.l1_ask) {
            Some(ask.price - bid.price)
        } else {
            None
        }
    }

    /// 獲取中間價
    pub fn get_mid_price(&self) -> Option<Decimal> {
        if let (Some(bid), Some(ask)) = (&self.l1_bid, &self.l1_ask) {
            Some((bid.price + ask.price) / Decimal::from(2))
        } else {
            None
        }
    }

    /// 獲取 L2 深度統計
    pub fn get_l2_depth(&self) -> (usize, usize) {
        (self.l2_bids.len(), self.l2_asks.len())
    }

    /// 獲取 L3 訂單統計 (Bitget 不支持真實 L3，返回 0)
    pub fn get_l3_orders(&self) -> (usize, usize) {
        (0, 0) // Bitget 不提供 L3 數據
    }

    /// 計算 Bitget Checksum
    /// 按照 Bitget 文檔：bid1:ask1:bid2:ask2... 交替排列，最多25檔
    pub fn calculate_checksum(&self) -> i32 {
        let mut checksum_string = String::new();
        
        // 取前25檔 bids (價格降序) 和 asks (價格升序)
        let bids: Vec<_> = self.l2_bids.values()
            .collect::<Vec<_>>()
            .into_iter()
            .rev() // BTreeMap 默認升序，需要反轉為降序
            .take(25)
            .collect();
            
        let asks: Vec<_> = self.l2_asks.values()
            .take(25)
            .collect();
        
        // 交替添加 bid 和 ask
        let max_depth = std::cmp::max(bids.len(), asks.len());
        
        for i in 0..max_depth {
            if i < bids.len() {
                if !checksum_string.is_empty() {
                    checksum_string.push(':');
                }
                checksum_string.push_str(&format!("{}:{}", bids[i].price, bids[i].amount));
            }
            
            if i < asks.len() {
                if !checksum_string.is_empty() {
                    checksum_string.push(':');
                }
                checksum_string.push_str(&format!("{}:{}", asks[i].price, asks[i].amount));
            }
        }
        
        // 計算 CRC32 校驗和
        crc32fast::hash(checksum_string.as_bytes()) as i32
    }

    /// 驗證 Checksum
    pub fn verify_checksum(&self, expected_checksum: i32) -> bool {
        let calculated = self.calculate_checksum();
        calculated == expected_checksum
    }

    /// 更新 Bitget 特定數據
    pub fn update_bitget_metadata(&mut self, checksum: Option<i32>, sequence: Option<u64>, is_snapshot: bool) {
        self.last_checksum = checksum;
        self.last_sequence = sequence;
        self.is_snapshot = is_snapshot;
        self.last_update = Utc::now();
        self.sequence += 1;
    }
}

/// Barter-Data 兼容的多層級 OrderBook 管理器
pub struct BarterOrderBookManager {
    /// 所有交易對的訂單簿
    orderbooks: HashMap<String, MultiLevelOrderBook>,
    /// 統計信息
    stats: OrderBookManagerStats,
}

#[derive(Debug, Clone, Default)]
pub struct OrderBookManagerStats {
    pub total_events: u64,
    pub l1_updates: u64,
    pub l2_updates: u64,
    pub l3_updates: u64,
    pub trade_events: u64,
    pub error_count: u64,
    pub start_time: Option<DateTime<Utc>>,
}

impl BarterOrderBookManager {
    /// 創建新的管理器
    pub fn new() -> Self {
        Self {
            orderbooks: HashMap::new(),
            stats: OrderBookManagerStats {
                start_time: Some(Utc::now()),
                ..Default::default()
            },
        }
    }

    /// 處理 Bitget 消息並轉換為 Barter MarketEvent
    pub fn handle_bitget_message(&mut self, message: BitgetMessage) -> Result<Option<MarketEvent>> {
        self.stats.total_events += 1;

        match message {
            BitgetMessage::OrderBook { symbol, data, action, timestamp } => {
                self.handle_orderbook_message(&symbol, &data, action.as_deref(), timestamp)
            }
            BitgetMessage::Trade { symbol, data, timestamp } => {
                self.handle_trade_message(&symbol, &data, timestamp)
            }
            BitgetMessage::Ticker { symbol: _, data: _, timestamp: _ } => {
                // 暫時忽略 Ticker 消息
                Ok(None)
            }
        }
    }

    /// 處理訂單簿消息
    fn handle_orderbook_message(
        &mut self,
        symbol: &str,
        data: &Value,
        action: Option<&str>,
        timestamp: Timestamp,
    ) -> Result<Option<MarketEvent>> {
        let orderbook = self.orderbooks.entry(symbol.to_string())
            .or_insert_with(|| MultiLevelOrderBook::new(symbol.to_string()));

        // 解析 Bitget 訂單簿數據
        let books_data = data.as_array().ok_or_else(|| anyhow::anyhow!("Invalid orderbook data format"))?;

        if let Some(book) = books_data.first() {
            let empty_vec = Vec::new();
            let bids_data = book.get("bids").and_then(|v| v.as_array()).unwrap_or(&empty_vec);
            let asks_data = book.get("asks").and_then(|v| v.as_array()).unwrap_or(&empty_vec);
            
            // 解析 Bitget 特定字段
            let checksum = book.get("checksum").and_then(|v| v.as_i64()).map(|v| v as i32);
            let sequence = book.get("seq").and_then(|v| v.as_u64());
            let is_snapshot = action.map(|a| a == "snapshot").unwrap_or(false);

            // 解析為 ExtendedLevel
            let mut bids = Vec::new();
            let mut asks = Vec::new();

            // 解析 bids
            for bid in bids_data.iter().take(20) { // 最多取 20 檔
                if let Some(bid_array) = bid.as_array() {
                    if bid_array.len() >= 2 {
                        if let (Some(price_str), Some(amount_str)) = (
                            bid_array[0].as_str(),
                            bid_array[1].as_str()
                        ) {
                            if let (Ok(price), Ok(amount)) = (
                                Decimal::from_str_exact(price_str),
                                Decimal::from_str_exact(amount_str)
                            ) {
                                bids.push(ExtendedLevel {
                                    price,
                                    amount,
                                    order_id: None, // L2 level, no order ID
                                    timestamp: Utc::now(),
                                });
                            }
                        }
                    }
                }
            }

            // 解析 asks
            for ask in asks_data.iter().take(20) { // 最多取 20 檔
                if let Some(ask_array) = ask.as_array() {
                    if ask_array.len() >= 2 {
                        if let (Some(price_str), Some(amount_str)) = (
                            ask_array[0].as_str(),
                            ask_array[1].as_str()
                        ) {
                            if let (Ok(price), Ok(amount)) = (
                                Decimal::from_str_exact(price_str),
                                Decimal::from_str_exact(amount_str)
                            ) {
                                asks.push(ExtendedLevel {
                                    price,
                                    amount,
                                    order_id: None, // L2 level, no order ID
                                    timestamp: Utc::now(),
                                });
                            }
                        }
                    }
                }
            }

            // 更新 L2 數據 (同時會更新 L1)
            orderbook.update_l2(bids, asks);
            
            // 更新 Bitget 特定元數據
            orderbook.update_bitget_metadata(checksum, sequence, is_snapshot);
            
            // Checksum 驗證 (如果提供)
            if let Some(expected_checksum) = checksum {
                if expected_checksum != 0 && !orderbook.verify_checksum(expected_checksum) {
                    warn!("Checksum mismatch for {}: expected {}, calculated {}", 
                          symbol, expected_checksum, orderbook.calculate_checksum());
                }
            }

            // 創建 MarketEvent - 克隆必要的數據以避免借用檢查問題
            let l1_bid = orderbook.l1_bid.clone();
            let l1_ask = orderbook.l1_ask.clone();
            
            let market_event = Self::create_l1_market_event_static(symbol, l1_bid, l1_ask, timestamp)?;
            self.stats.l2_updates += 1;
            self.stats.l1_updates += 1;

            Ok(Some(market_event))
        } else {
            Err(anyhow::anyhow!("Empty orderbook data"))
        }
    }

    /// 處理交易消息
    fn handle_trade_message(
        &mut self,
        symbol: &str,
        data: &Value,
        timestamp: Timestamp,
    ) -> Result<Option<MarketEvent>> {
        // 直接在這裡實現交易數據轉換，避免循環引用
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
            
            self.stats.trade_events += 1;
            Ok(Some(market_event))
        } else {
            Err(anyhow::anyhow!("Empty trade data"))
        }
    }

    /// 創建 L1 MarketEvent (靜態版本，避免借用問題)
    fn create_l1_market_event_static(
        symbol: &str,
        l1_bid: Option<ExtendedLevel>,
        l1_ask: Option<ExtendedLevel>,
        timestamp: Timestamp,
    ) -> Result<MarketEvent> {
        let exchange_time = DateTime::from_timestamp_micros(timestamp as i64)
            .unwrap_or_else(|| Utc::now());

        if let (Some(bid), Some(ask)) = (l1_bid, l1_ask) {
            let quote_currency = if symbol.contains("USDT") { "USDT" } else { "USD" };
            let instrument = (symbol, quote_currency, barter_instrument::instrument::market_data::kind::MarketDataInstrumentKind::Spot).into();

            let orderbook_l1 = OrderBookL1 {
                last_update_time: exchange_time,
                best_bid: Some(barter_data::books::Level {
                    price: bid.price,
                    amount: bid.amount,
                }),
                best_ask: Some(barter_data::books::Level {
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

    /// 獲取特定符號的訂單簿
    pub fn get_orderbook(&self, symbol: &str) -> Option<&MultiLevelOrderBook> {
        self.orderbooks.get(symbol)
    }

    /// 獲取所有訂單簿
    pub fn get_all_orderbooks(&self) -> &HashMap<String, MultiLevelOrderBook> {
        &self.orderbooks
    }

    /// 打印統計信息
    pub fn print_stats(&self) {
        let elapsed = self.stats.start_time
            .map(|start| Utc::now().signed_duration_since(start))
            .map(|duration| std::time::Duration::from_secs(duration.num_seconds() as u64))
            .unwrap_or_default();

        let events_per_sec = if elapsed.as_secs() > 0 {
            self.stats.total_events as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        info!("📊 Barter Multi-Level OrderBook Manager Statistics:");
        info!("   Total events: {}", self.stats.total_events);
        info!("   L1 updates: {}", self.stats.l1_updates);
        info!("   L2 updates: {}", self.stats.l2_updates);
        info!("   L3 updates: {}", self.stats.l3_updates);
        info!("   Trade events: {}", self.stats.trade_events);
        info!("   Error count: {}", self.stats.error_count);
        info!("   Events/sec: {:.1}", events_per_sec);
        info!("   Active orderbooks: {}", self.orderbooks.len());

        // 顯示每個訂單簿的詳細信息
        for (symbol, orderbook) in &self.orderbooks {
            let (l2_bids, l2_asks) = orderbook.get_l2_depth();
            let (l3_bid_orders, l3_ask_orders) = orderbook.get_l3_orders();
            
            if let (Some(mid_price), Some(spread)) = (orderbook.get_mid_price(), orderbook.get_spread()) {
                info!("   {} - Mid: {:.4}, Spread: {:.6}, L2: {}x{}, L3: {}x{}", 
                      symbol, mid_price, spread, l2_bids, l2_asks, l3_bid_orders, l3_ask_orders);
            }
        }
    }

    /// 獲取統計信息
    pub fn get_stats(&self) -> &OrderBookManagerStats {
        &self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_multi_level_orderbook_creation() {
        let orderbook = MultiLevelOrderBook::new("BTCUSDT".to_string());
        assert_eq!(orderbook.symbol, "BTCUSDT");
        assert!(orderbook.l1_bid.is_none());
        assert!(orderbook.l1_ask.is_none());
        assert_eq!(orderbook.l2_bids.len(), 0);
        assert_eq!(orderbook.l2_asks.len(), 0);
    }

    #[test]
    fn test_l1_update() {
        let mut orderbook = MultiLevelOrderBook::new("BTCUSDT".to_string());
        
        let bid = ExtendedLevel {
            price: dec!(50000.0),
            amount: dec!(1.0),
            order_id: None,
            timestamp: Utc::now(),
        };
        
        let ask = ExtendedLevel {
            price: dec!(50001.0),
            amount: dec!(1.5),
            order_id: None,
            timestamp: Utc::now(),
        };

        orderbook.update_l1(Some(bid), Some(ask));
        
        assert!(orderbook.l1_bid.is_some());
        assert!(orderbook.l1_ask.is_some());
        assert_eq!(orderbook.get_spread(), Some(dec!(1.0)));
        assert_eq!(orderbook.get_mid_price(), Some(dec!(50000.5)));
    }

    #[test]
    fn test_l2_update() {
        let mut orderbook = MultiLevelOrderBook::new("BTCUSDT".to_string());
        
        let bids = vec![
            ExtendedLevel {
                price: dec!(50000.0),
                amount: dec!(1.0),
                order_id: None,
                timestamp: Utc::now(),
            },
            ExtendedLevel {
                price: dec!(49999.0),
                amount: dec!(2.0),
                order_id: None,
                timestamp: Utc::now(),
            },
        ];

        let asks = vec![
            ExtendedLevel {
                price: dec!(50001.0),
                amount: dec!(1.5),
                order_id: None,
                timestamp: Utc::now(),
            },
            ExtendedLevel {
                price: dec!(50002.0),
                amount: dec!(2.5),
                order_id: None,
                timestamp: Utc::now(),
            },
        ];

        orderbook.update_l2(bids, asks);
        
        let (bid_depth, ask_depth) = orderbook.get_l2_depth();
        assert_eq!(bid_depth, 2);
        assert_eq!(ask_depth, 2);
        
        // L1 should be automatically updated
        assert!(orderbook.l1_bid.is_some());
        assert!(orderbook.l1_ask.is_some());
    }
}