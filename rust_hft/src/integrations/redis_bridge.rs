//! Redis數據橋接模組
//! 
//! 實現Bitget WebSocket -> Redis Pub/Sub -> 前端的完整數據流
//! 基於CLAUDE.md中定義的Redis Channel契約

use anyhow::Result;
use redis::AsyncCommands;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

use crate::exchanges::{OrderBookLevel, MarketEvent};
use crate::integrations::unified_bitget_connector::{UnifiedBitgetConnector, UnifiedBitgetConfig, BitgetMessage, BitgetChannel};
use rust_decimal::prelude::*;
use crate::integrations::unified_orderbook_manager::{OrderBookSnapshot, PriceLevel};

/// Redis橋接服務
pub struct RedisBridge {
    redis_client: redis::Client,
    symbols: Vec<String>,
}

impl RedisBridge {
    pub async fn new() -> Result<Self> {
        let redis_client = redis::Client::open("redis://localhost:6379")?;
        
        // 測試Redis連接
        let mut conn = redis_client.get_async_connection().await?;
        let _: String = redis::cmd("PING").query_async(&mut conn).await?;
        info!("✅ Redis連接成功");

        Ok(Self {
            redis_client,
            symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string(), "ADAUSDT".to_string()],
        })
    }

    /// 發布訂單簿數據到Redis
    pub async fn publish_orderbook(&self, symbol: &str, orderbook: &OrderBookSnapshot) -> Result<()> {
        let mut conn = self.redis_client.get_async_connection().await?;
        
        let message = json!({
            "channel": "orderbook",
            "data": {
                "symbol": symbol,
                "bids": orderbook.bids.iter().take(15).map(|level| {
                    [level.price.to_string(), level.quantity.to_string(), level.order_count.unwrap_or(0).to_string()]
                }).collect::<Vec<_>>(),
                "asks": orderbook.asks.iter().take(15).map(|level| {
                    [level.price.to_string(), level.quantity.to_string(), level.order_count.unwrap_or(0).to_string()]
                }).collect::<Vec<_>>(),
                "timestamp": chrono::Utc::now().timestamp_millis(),
                "lastUpdateId": orderbook.sequence.unwrap_or(0),
                "spread": orderbook.asks.first().map(|ask| ask.price) 
                    .zip(orderbook.bids.first().map(|bid| bid.price))
                    .map(|(ask_price, bid_price)| ask_price - bid_price),
                "midPrice": orderbook.asks.first().map(|ask| ask.price)
                    .zip(orderbook.bids.first().map(|bid| bid.price))
                    .map(|(ask_price, bid_price)| (ask_price + bid_price) / 2.0)
            },
            "timestamp": chrono::Utc::now().timestamp_millis()
        });

        let _: () = conn.publish("market.orderbook", message.to_string()).await?;
        debug!("📊 發布{}訂單簿數據", symbol);
        Ok(())
    }

    /// 發布交易數據
    pub async fn publish_trade(&self, symbol: &str, price: f64, quantity: f64, side: &str) -> Result<()> {
        let mut conn = self.redis_client.get_async_connection().await?;
        
        let message = json!({
            "channel": "trade",
            "data": {
                "symbol": symbol,
                "price": price,
                "quantity": quantity,
                "side": side,
                "timestamp": chrono::Utc::now().timestamp_millis(),
                "isMaker": false
            },
            "timestamp": chrono::Utc::now().timestamp_millis()
        });

        let _: () = conn.publish("market.trade", message.to_string()).await?;
        debug!("💰 發布{}交易數據: {} @ {}", symbol, quantity, price);
        Ok(())
    }

    /// 發布Ticker數據
    pub async fn publish_ticker(&self, symbol: &str, ticker_data: TickerData) -> Result<()> {
        let mut conn = self.redis_client.get_async_connection().await?;
        
        let message = json!({
            "channel": "ticker",
            "data": {
                "symbol": symbol,
                "lastPrice": ticker_data.last_price,
                "priceChange": ticker_data.price_change,
                "priceChangePercent": ticker_data.price_change_percent,
                "high24h": ticker_data.high_24h,
                "low24h": ticker_data.low_24h,
                "volume": ticker_data.volume,
                "bestBid": ticker_data.best_bid,
                "bestAsk": ticker_data.best_ask,
                "timestamp": chrono::Utc::now().timestamp_millis()
            },
            "timestamp": chrono::Utc::now().timestamp_millis()
        });

        let _: () = conn.publish("market.ticker", message.to_string()).await?;
        debug!("📈 發布{}價格數據: {}", symbol, ticker_data.last_price);
        Ok(())
    }

    /// 發布系統指標
    pub async fn publish_metrics(&self, metrics: SystemMetrics) -> Result<()> {
        let mut conn = self.redis_client.get_async_connection().await?;
        
        let message = json!({
            "channel": "metrics",
            "data": {
                "latency": {
                    "p50": metrics.latency_p50,
                    "p90": metrics.latency_p90,
                    "p99": metrics.latency_p99,
                    "max": metrics.latency_max
                },
                "throughput": {
                    "ordersPerSecond": metrics.orders_per_second,
                    "messagesPerSecond": metrics.messages_per_second
                },
                "websocket": {
                    "connected": metrics.websocket_connected,
                    "lastHeartbeat": chrono::Utc::now().timestamp_millis(),
                    "reconnectCount": metrics.reconnect_count
                },
                "memory": {
                    "used": metrics.memory_used,
                    "available": metrics.memory_available,
                    "percentage": metrics.memory_percentage
                },
                "cpu": {
                    "usage": metrics.cpu_usage
                }
            },
            "timestamp": chrono::Utc::now().timestamp_millis()
        });

        let _: () = conn.publish("system.metrics", message.to_string()).await?;
        debug!("🔧 發布系統指標");
        Ok(())
    }

    /// 連接真實Bitget數據流並轉發到Redis
    pub async fn start_real_data_bridge(&self) -> Result<()> {
        info!("🚀 啟動真實市場數據橋接服務");
        
        // 創建Bitget WebSocket連接器
        let connector = UnifiedBitgetConnector::new(
            UnifiedBitgetConfig::default()
        );
        
        // 為每個交易對添加訂閱
        for symbol in &self.symbols {
            connector.subscribe(symbol, BitgetChannel::OrderBook).await?;
            connector.subscribe(symbol, BitgetChannel::Ticker).await?;
            connector.subscribe(symbol, BitgetChannel::Trades).await?;
        }
        
        info!("✅ Bitget連接器已創建，開始訂閱 {} 個交易對", self.symbols.len());
        
        // 啟動連接器並開始接收真實數據
        let mut receiver = connector.start().await?;
        
        while let Some(message) = receiver.recv().await {
            match message.channel {
                BitgetChannel::OrderBook => {
                    // 嘗試轉換為 OrderBookUpdate
                    if let Ok(Some(orderbook_update)) = message.to_orderbook_update() {
                        // 轉換為OrderBookSnapshot格式
                        let orderbook = OrderBookSnapshot {
                            symbol: message.symbol.clone(),
                            bids: orderbook_update.bids.into_iter().map(|level| PriceLevel {
                                price: level.price.0,
                                quantity: level.quantity.to_f64().unwrap_or(0.0),
                                order_count: None,
                            }).collect(),
                            asks: orderbook_update.asks.into_iter().map(|level| PriceLevel {
                                price: level.price.0,
                                quantity: level.quantity.to_f64().unwrap_or(0.0),
                                order_count: None,
                            }).collect(),
                            timestamp: orderbook_update.timestamp,
                            sequence: Some(orderbook_update.sequence_start),
                        };
                        
                        if let Err(e) = self.publish_orderbook(&message.symbol, &orderbook).await {
                            error!("轉發訂單簿數據失敗: {}", e);
                        }
                    }
                }
                BitgetChannel::Trades => {
                    // 解析交易數據，這裡簡化處理
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&message.data) {
                        if let (Some(price), Some(quantity), Some(side)) = (
                            json.get("price").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()),
                            json.get("size").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()),
                            json.get("side").and_then(|v| v.as_str())
                        ) {
                            if let Err(e) = self.publish_trade(
                                &message.symbol,
                                price,
                                quantity,
                                side
                            ).await {
                                error!("轉發交易數據失敗: {}", e);
                            }
                        }
                    }
                }
                BitgetChannel::Ticker => {
                    // 解析 Ticker 數據
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&message.data) {
                        let ticker_data = TickerData {
                            last_price: json.get("lastPr").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                            price_change: json.get("change").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                            price_change_percent: json.get("changeUtc").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                            high_24h: json.get("high24h").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                            low_24h: json.get("low24h").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                            volume: json.get("baseVolume").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                            best_bid: json.get("bidPr").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                            best_ask: json.get("askPr").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                        };
                        
                        if let Err(e) = self.publish_ticker(&message.symbol, ticker_data).await {
                            error!("轉發Ticker數據失敗: {}", e);
                        }
                    }
                }
                _ => {
                    debug!("接收到其他類型市場事件: {:?}", message.channel);
                }
            }
        }
        
        warn!("數據流接收結束");
        Ok(())
    }
}

/// Ticker數據結構
#[derive(Debug, Clone)]
pub struct TickerData {
    pub last_price: f64,
    pub price_change: f64,
    pub price_change_percent: f64,
    pub high_24h: f64,
    pub low_24h: f64,
    pub volume: f64,
    pub best_bid: f64,
    pub best_ask: f64,
}

/// 系統指標結構
#[derive(Debug, Clone)]
pub struct SystemMetrics {
    pub latency_p50: f64,
    pub latency_p90: f64,
    pub latency_p99: f64,
    pub latency_max: f64,
    pub orders_per_second: u32,
    pub messages_per_second: u32,
    pub websocket_connected: bool,
    pub reconnect_count: u32,
    pub memory_used: u64,
    pub memory_available: u64,
    pub memory_percentage: f64,
    pub cpu_usage: f64,
}