/*!
 * Barter-rs 標準化訂閱管理器（簡化版本）
 * 
 * 基於 barter-rs 文檔的最佳實踐實現：
 * - 統一的 Subscription 配置
 * - 標準化的事件轉換
 * - 高性能的數據流處理
 */

use crate::integrations::unified_bitget_connector::{UnifiedBitgetConnector, UnifiedBitgetConfig, ConnectionMode, BitgetChannel, BitgetMessage};
use crate::core::types::Timestamp;

use barter_data::{
    event::{MarketEvent, DataKind},
    subscription::{
        book::OrderBookL1,
        trade::PublicTrade,
        Subscription, SubKind,
    },
    books::Level,
};
use barter_instrument::{
    Side as BarterSide, 
    exchange::ExchangeId,
};

use serde_json::Value;
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tracing::{info, warn, error, debug};
use tokio_stream::{Stream, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};

/// 簡化的重連事件
#[derive(Debug, Clone)]
pub struct ReconnectEvent {
    pub reason: String,
    pub timestamp: DateTime<Utc>,
}

/// Barter-rs 標準化訂閱配置
#[derive(Debug, Clone)]
pub struct BarterSubscriptionConfig {
    pub exchange: ExchangeId,
    pub symbol: String,
    pub quote: String,
    pub subscription_kinds: Vec<SubKind>,
}

impl BarterSubscriptionConfig {
    pub fn new(symbol: &str, quote: &str, kinds: Vec<SubKind>) -> Self {
        Self {
            exchange: ExchangeId::Bitget,
            symbol: symbol.to_string(),
            quote: quote.to_string(),
            subscription_kinds: kinds,
        }
    }

    /// 獲取完整符號
    pub fn full_symbol(&self) -> String {
        self.symbol.clone()
    }
}

/// 標準化的市場數據流
pub struct BarterMarketStream {
    receiver: tokio::sync::mpsc::UnboundedReceiver<Result<MarketEvent, ReconnectEvent>>,
}

impl BarterMarketStream {
    pub fn new() -> (Self, tokio::sync::mpsc::UnboundedSender<Result<MarketEvent, ReconnectEvent>>) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let stream = Self { receiver };
        (stream, sender)
    }
}

impl Stream for BarterMarketStream {
    type Item = Result<MarketEvent, ReconnectEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

/// Barter-rs 標準化訂閱管理器
pub struct BarterStandardSubscriptionManager {
    subscriptions: Vec<BarterSubscriptionConfig>,
    bitget_connector: Option<UnifiedBitgetConnector>,
    market_stream_sender: tokio::sync::mpsc::UnboundedSender<Result<MarketEvent, ReconnectEvent>>,
    stats: SubscriptionManagerStats,
    symbol_cache: HashMap<String, (String, String)>, // symbol -> (base, quote)
}

#[derive(Debug, Default)]
pub struct SubscriptionManagerStats {
    pub total_subscriptions: usize,
    pub active_instruments: usize,
    pub events_processed: u64,
    pub normalization_hits: u64,
    pub normalization_misses: u64,
    pub reconnection_attempts: u64,
    pub error_count: u64,
    pub start_time: Option<DateTime<Utc>>,
}

impl BarterStandardSubscriptionManager {
    /// 創建新的標準化訂閱管理器
    pub fn new() -> (Self, BarterMarketStream) {
        let (market_stream, sender) = BarterMarketStream::new();
        
        let manager = Self {
            subscriptions: Vec::new(),
            bitget_connector: None,
            market_stream_sender: sender,
            stats: SubscriptionManagerStats {
                start_time: Some(Utc::now()),
                ..Default::default()
            },
            symbol_cache: HashMap::new(),
        };

        (manager, market_stream)
    }

    /// 添加訂閱配置
    pub fn add_subscription(&mut self, config: BarterSubscriptionConfig) {
        // 緩存符號信息
        let symbol = config.full_symbol();
        let base = symbol.replace(&config.quote, "");
        self.symbol_cache.insert(symbol, (base, config.quote.clone()));
        
        self.subscriptions.push(config);
        self.stats.total_subscriptions += 1;
        self.stats.active_instruments = self.symbol_cache.len();
    }

    /// 批量添加標準訂閱
    pub fn add_standard_subscriptions(&mut self, symbols: Vec<(&str, &str)>) {
        for (symbol, quote) in symbols {
            let config = BarterSubscriptionConfig::new(
                symbol,
                quote,
                vec![
                    SubKind::OrderBooksL1,
                    SubKind::PublicTrades,
                ]
            );
            self.add_subscription(config);
        }
    }

    /// 初始化 Bitget 連接器
    pub async fn initialize_connector(&mut self, config: UnifiedBitgetConfig) -> Result<()> {
        let mut connector = UnifiedBitgetConnector::new(config);

        // 根據 barter-rs 訂閱配置設置 Bitget 訂閱
        for sub_config in &self.subscriptions {
            let symbol = sub_config.full_symbol();
            
            for kind in &sub_config.subscription_kinds {
                let bitget_channel = match kind {
                    SubKind::OrderBooksL1 => BitgetChannel::OrderBook,
                    SubKind::PublicTrades => BitgetChannel::Trades,
                    _ => continue,
                };
                
                debug!("Added Bitget subscription: {} -> {:?}", symbol, bitget_channel);
                connector.subscribe(&symbol, bitget_channel).await?;
            }
        }

        self.bitget_connector = Some(connector);
        Ok(())
    }

    /// 啟動標準化數據流
    pub async fn start_normalized_stream(&mut self) -> Result<()> {
        let connector = self.bitget_connector.take()
            .ok_or_else(|| anyhow!("Bitget connector not initialized"))?;

        let sender = self.market_stream_sender.clone();
        let symbol_cache = self.symbol_cache.clone();
        
        info!("🚀 Starting barter-rs standardized market data stream");
        info!("📊 Active subscriptions: {}", self.stats.total_subscriptions);
        info!("🔧 Instruments cached: {}", self.stats.active_instruments);

        // 創建標準化消息處理器
        let message_handler = move |bitget_message: BitgetMessage| {
            match Self::normalize_bitget_message(bitget_message, &symbol_cache) {
                Ok(market_event) => {
                    if let Err(e) = sender.send(Ok(market_event)) {
                        error!("❌ Failed to send normalized market event: {}", e);
                    }
                }
                Err(e) => {
                    warn!("⚠️ Failed to normalize Bitget message: {}", e);
                    // 發送重連事件（如果需要）
                    let reconnect_event = ReconnectEvent {
                        reason: e.to_string(),
                        timestamp: Utc::now(),
                    };
                    if let Err(e) = sender.send(Err(reconnect_event)) {
                        error!("❌ Failed to send reconnect event: {}", e);
                    }
                }
            }
        };

        // 啟動連接器
        tokio::spawn(async move {
            match connector.start().await {
                Ok(mut receiver) => {
                    while let Some(message) = receiver.recv().await {
                        message_handler(message);
                    }
                }
                Err(e) => {
                    error!("❌ Bitget connector failed: {}", e);
                }
            }
        });

        Ok(())
    }

    /// 標準化 Bitget 消息為 barter-rs MarketEvent
    fn normalize_bitget_message(
        message: BitgetMessage,
        cache: &HashMap<String, (String, String)>
    ) -> Result<MarketEvent> {
        match message.channel {
            BitgetChannel::OrderBook => {
                Self::normalize_orderbook_message(message.symbol, message.data, message.timestamp, cache)
            }
            BitgetChannel::Trades => {
                Self::normalize_trade_message(message.symbol, message.data, message.timestamp, cache)
            }
            BitgetChannel::Ticker => {
                Err(anyhow!("Ticker messages not yet supported in normalization"))
            }
            _ => {
                Err(anyhow!("Unsupported channel: {:?}", message.channel))
            }
        }
    }

    /// 標準化訂單簿消息
    fn normalize_orderbook_message(
        symbol: String,
        data: Value,
        timestamp: Timestamp,
        cache: &HashMap<String, (String, String)>
    ) -> Result<MarketEvent> {
        let (_base, quote) = cache.get(&symbol)
            .ok_or_else(|| anyhow!("Symbol {} not found in cache", symbol))?;

        let books_data = data.as_array()
            .ok_or_else(|| anyhow!("Invalid orderbook data format"))?;

        if let Some(book) = books_data.first() {
            let empty_vec = Vec::new();
            let bids_data = book.get("bids")
                .and_then(|v| v.as_array())
                .unwrap_or(&empty_vec);
            let asks_data = book.get("asks")
                .and_then(|v| v.as_array())
                .unwrap_or(&empty_vec);

            let exchange_time = DateTime::from_timestamp_micros(timestamp as i64)
                .unwrap_or_else(|| Utc::now());

            // 創建 L1 OrderBook 事件
            let best_bid = Self::parse_level(bids_data.first())?;
            let best_ask = Self::parse_level(asks_data.first())?;

            let orderbook_l1 = OrderBookL1 {
                last_update_time: exchange_time,
                best_bid,
                best_ask,
            };

            // 創建工具
            let instrument = (symbol.as_str(), quote.as_str(), barter_instrument::instrument::market_data::kind::MarketDataInstrumentKind::Spot).into();

            Ok(MarketEvent {
                time_exchange: exchange_time,
                time_received: Utc::now(),
                exchange: ExchangeId::Bitget,
                instrument,
                kind: DataKind::OrderBookL1(orderbook_l1),
            })
        } else {
            Err(anyhow!("Empty orderbook data"))
        }
    }

    /// 標準化交易消息
    fn normalize_trade_message(
        symbol: String,
        data: Value,
        timestamp: Timestamp,
        cache: &HashMap<String, (String, String)>
    ) -> Result<MarketEvent> {
        let (_base, quote) = cache.get(&symbol)
            .ok_or_else(|| anyhow!("Symbol {} not found in cache", symbol))?;

        let trades = data.as_array()
            .ok_or_else(|| anyhow!("Invalid trade data format"))?;

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

            let exchange_time = DateTime::from_timestamp_micros(timestamp as i64)
                .unwrap_or_else(|| Utc::now());

            let public_trade = PublicTrade {
                id: trade_id,
                price,
                amount,
                side,
            };

            // 創建工具
            let instrument = (symbol.as_str(), quote.as_str(), barter_instrument::instrument::market_data::kind::MarketDataInstrumentKind::Spot).into();

            Ok(MarketEvent {
                time_exchange: exchange_time,
                time_received: Utc::now(),
                exchange: ExchangeId::Bitget,
                instrument,
                kind: DataKind::Trade(public_trade),
            })
        } else {
            Err(anyhow!("Empty trade data"))
        }
    }

    /// 解析價格檔位
    fn parse_level(level_data: Option<&Value>) -> Result<Option<Level>> {
        if let Some(level_array) = level_data.and_then(|v| v.as_array()) {
            if level_array.len() >= 2 {
                if let (Some(price_str), Some(amount_str)) = (
                    level_array[0].as_str(),
                    level_array[1].as_str()
                ) {
                    let price = price_str.parse::<Decimal>()
                        .map_err(|e| anyhow!("Failed to parse price: {}", e))?;
                    let amount = amount_str.parse::<Decimal>()
                        .map_err(|e| anyhow!("Failed to parse amount: {}", e))?;

                    return Ok(Some(Level { price, amount }));
                }
            }
        }
        Ok(None)
    }

    /// 打印訂閱統計
    pub fn print_subscription_stats(&self) {
        info!("📋 Barter-rs Standard Subscription Manager Statistics:");
        info!("   Total subscription configs: {}", self.stats.total_subscriptions);
        info!("   Active instruments: {}", self.stats.active_instruments);
        info!("   Events processed: {}", self.stats.events_processed);
        info!("   Normalization cache hits: {}", self.stats.normalization_hits);
        info!("   Normalization cache misses: {}", self.stats.normalization_misses);
        info!("   Reconnection attempts: {}", self.stats.reconnection_attempts);
        info!("   Error count: {}", self.stats.error_count);

        // 顯示所有符號
        info!("🔍 Active symbols ({}):", self.symbol_cache.len());
        for (symbol, (base, quote)) in &self.symbol_cache {
            info!("   {} | {} -> {}/{}", ExchangeId::Bitget, symbol, base, quote);
        }
    }

    /// 獲取統計信息
    pub fn get_stats(&self) -> &SubscriptionManagerStats {
        &self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscription_config_creation() {
        let config = BarterSubscriptionConfig::new(
            "BTCUSDT",
            "USDT",
            vec![SubKind::OrderBooksL1, SubKind::PublicTrades]
        );

        assert_eq!(config.exchange, ExchangeId::Bitget);
        assert_eq!(config.symbol, "BTCUSDT");
        assert_eq!(config.quote, "USDT");
        assert_eq!(config.subscription_kinds.len(), 2);
    }

    #[test]
    fn test_manager_initialization() {
        let (mut manager, _stream) = BarterStandardSubscriptionManager::new();
        
        manager.add_standard_subscriptions(vec![
            ("BTCUSDT", "USDT"),
            ("ETHUSDT", "USDT"),
        ]);

        assert_eq!(manager.stats.total_subscriptions, 2);
        assert_eq!(manager.stats.active_instruments, 2);
    }

    #[test]
    fn test_symbol_cache() {
        let (mut manager, _stream) = BarterStandardSubscriptionManager::new();
        
        let config = BarterSubscriptionConfig::new(
            "ADAUSDT",
            "USDT",
            vec![SubKind::OrderBooksL1]
        );
        
        manager.add_subscription(config);
        
        assert!(manager.symbol_cache.contains_key("ADAUSDT"));
        assert_eq!(manager.symbol_cache.get("ADAUSDT").unwrap().0, "ADA");
    }
}