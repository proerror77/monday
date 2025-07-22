/*!
 * 簡化的 Bitget WebSocket 連接器
 * 
 * 基於測試成功的 debug_bitget_ws.rs 實現
 * 專注於穩定性和簡潔性，而非複雜的負載均衡
 */

use anyhow::{Result, Context};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn, error, debug};

/// 簡化的 Bitget 頻道
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum SimpleChannel {
    #[serde(rename = "books5")]
    OrderBook5,
    #[serde(rename = "books1")]
    OrderBook1,
    #[serde(rename = "books15")]
    OrderBook15,
    #[serde(rename = "trade")]
    Trade,
    #[serde(rename = "ticker")]
    Ticker,
}

/// 簡化的消息格式
#[derive(Debug, Clone)]
pub struct SimpleMessage {
    pub channel: SimpleChannel,
    pub symbol: String,
    pub data: serde_json::Value,
    pub timestamp: u64,
}

/// 簡化的連接器配置
#[derive(Debug, Clone)]
pub struct SimpleConfig {
    pub ws_url: String,
    pub max_reconnect_attempts: u32,
    pub reconnect_delay_ms: u64,
}

impl Default for SimpleConfig {
    fn default() -> Self {
        Self {
            ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            max_reconnect_attempts: 5,
            reconnect_delay_ms: 1000,
        }
    }
}

/// 訂閱項
#[derive(Debug, Clone)]
struct Subscription {
    symbol: String,
    channel: SimpleChannel,
}

/// 簡化的 Bitget 連接器
pub struct SimpleBitgetConnector {
    config: SimpleConfig,
    subscriptions: Arc<RwLock<Vec<Subscription>>>,
    is_running: Arc<RwLock<bool>>,
}

impl SimpleBitgetConnector {
    /// 創建新的連接器
    pub fn new(config: Option<SimpleConfig>) -> Self {
        Self {
            config: config.unwrap_or_default(),
            subscriptions: Arc::new(RwLock::new(Vec::new())),
            is_running: Arc::new(RwLock::new(false)),
        }
    }

    /// 添加訂閱
    pub async fn subscribe(&self, symbol: &str, channel: SimpleChannel) -> Result<()> {
        let mut subs = self.subscriptions.write().await;
        subs.push(Subscription {
            symbol: symbol.to_string(),
            channel,
        });
        info!("Added subscription: {} -> {:?}", symbol, channel);
        Ok(())
    }

    /// 開始連接並返回消息接收器
    pub async fn start(&self) -> Result<mpsc::UnboundedReceiver<SimpleMessage>> {
        *self.is_running.write().await = true;
        
        let (tx, rx) = mpsc::unbounded_channel();
        
        // 克隆所需的數據
        let config = self.config.clone();
        let subscriptions = self.subscriptions.clone();
        let is_running = self.is_running.clone();
        
        // 啟動連接任務
        tokio::spawn(async move {
            Self::connection_task(config, subscriptions, is_running, tx).await;
        });
        
        Ok(rx)
    }

    /// 停止連接器
    pub async fn stop(&self) -> Result<()> {
        *self.is_running.write().await = false;
        Ok(())
    }

    /// 連接任務的主要邏輯
    async fn connection_task(
        config: SimpleConfig,
        subscriptions: Arc<RwLock<Vec<Subscription>>>,
        is_running: Arc<RwLock<bool>>,
        tx: mpsc::UnboundedSender<SimpleMessage>,
    ) {
        let mut reconnect_attempts = 0;
        
        while *is_running.read().await && reconnect_attempts < config.max_reconnect_attempts {
            match Self::run_connection(&config, &subscriptions, &is_running, &tx).await {
                Ok(_) => {
                    info!("Connection terminated normally");
                    break;
                }
                Err(e) => {
                    error!("Connection error: {}", e);
                    reconnect_attempts += 1;
                    
                    if reconnect_attempts < config.max_reconnect_attempts {
                        let delay = config.reconnect_delay_ms * reconnect_attempts as u64;
                        warn!("Reconnecting in {}ms (attempt {}/{})", 
                              delay, reconnect_attempts, config.max_reconnect_attempts);
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
                    }
                }
            }
        }
        
        if reconnect_attempts >= config.max_reconnect_attempts {
            error!("Max reconnection attempts reached, giving up");
        }
    }

    /// 運行單個連接
    async fn run_connection(
        config: &SimpleConfig,
        subscriptions: &Arc<RwLock<Vec<Subscription>>>,
        is_running: &Arc<RwLock<bool>>,
        tx: &mpsc::UnboundedSender<SimpleMessage>,
    ) -> Result<()> {
        // 建立 WebSocket 連接
        info!("Connecting to {}", config.ws_url);
        let (ws_stream, response) = connect_async(&config.ws_url).await
            .context("Failed to connect to Bitget WebSocket")?;
        
        info!("Connected successfully, status: {:?}", response.status());
        
        let (mut write, mut read) = ws_stream.split();
        
        // 發送所有訂閱
        let subs = subscriptions.read().await;
        for sub in subs.iter() {
            let sub_msg = Self::build_subscription_message(&sub.symbol, &sub.channel)?;
            debug!("Sending subscription: {}", sub_msg);
            
            write.send(Message::Text(sub_msg)).await
                .context("Failed to send subscription message")?;
        }
        drop(subs);
        
        info!("All subscriptions sent, waiting for messages...");
        
        // 處理消息
        while *is_running.read().await {
            match read.next().await {
                Some(Ok(Message::Text(text))) => {
                    debug!("Received message: {}", &text[..std::cmp::min(100, text.len())]);
                    
                    match Self::parse_message(&text) {
                        Ok(message) => {
                            if tx.send(message).is_err() {
                                warn!("Message receiver dropped, stopping connection");
                                break;
                            }
                        }
                        Err(e) => {
                            debug!("Failed to parse message: {} (text: {})", e, 
                                   &text[..std::cmp::min(200, text.len())]);
                        }
                    }
                }
                Some(Ok(Message::Ping(data))) => {
                    debug!("Received ping, sending pong");
                    write.send(Message::Pong(data)).await?;
                }
                Some(Ok(Message::Pong(_))) => {
                    debug!("Received pong");
                }
                Some(Ok(Message::Close(_))) => {
                    info!("WebSocket closed by server");
                    break;
                }
                Some(Err(e)) => {
                    return Err(e.into());
                }
                None => {
                    info!("WebSocket stream ended");
                    break;
                }
                _ => {
                    debug!("Received other message type");
                }
            }
        }
        
        Ok(())
    }

    /// 構建訂閱消息
    fn build_subscription_message(symbol: &str, channel: &SimpleChannel) -> Result<String> {
        let channel_str = match channel {
            SimpleChannel::OrderBook5 => "books5",
            SimpleChannel::OrderBook1 => "books1",
            SimpleChannel::OrderBook15 => "books15",
            SimpleChannel::Trade => "trade",
            SimpleChannel::Ticker => "ticker",
        };
        
        let sub = serde_json::json!({
            "op": "subscribe",
            "args": [{
                "instType": "SPOT",
                "channel": channel_str,
                "instId": symbol
            }]
        });
        
        Ok(sub.to_string())
    }

    /// 解析消息
    fn parse_message(text: &str) -> Result<SimpleMessage> {
        let json: serde_json::Value = serde_json::from_str(text)?;
        
        // 處理訂閱確認消息
        if let Some(event) = json.get("event") {
            if event == "subscribe" {
                return Err(anyhow::anyhow!("Subscription confirmation"));
            }
        }
        
        // 解析實際數據消息
        let arg = json.get("arg")
            .ok_or_else(|| anyhow::anyhow!("Missing 'arg' field"))?;
            
        let channel_str = arg.get("channel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing channel in arg"))?;
            
        let symbol = arg.get("instId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing instId in arg"))?;
            
        let channel = match channel_str {
            "books5" => SimpleChannel::OrderBook5,
            "books1" => SimpleChannel::OrderBook1,
            "books15" => SimpleChannel::OrderBook15,
            "trade" => SimpleChannel::Trade,
            "ticker" => SimpleChannel::Ticker,
            _ => return Err(anyhow::anyhow!("Unknown channel: {}", channel_str)),
        };
        
        Ok(SimpleMessage {
            channel,
            symbol: symbol.to_string(),
            data: json.get("data").unwrap_or(&json).clone(),
            timestamp: chrono::Utc::now().timestamp_millis() as u64,
        })
    }
}

/// 便捷函數：創建默認連接器
pub fn create_simple_connector() -> SimpleBitgetConnector {
    SimpleBitgetConnector::new(None)
}

/// 便捷函數：創建自定義連接器
pub fn create_custom_connector(ws_url: String) -> SimpleBitgetConnector {
    let config = SimpleConfig {
        ws_url,
        max_reconnect_attempts: 5,
        reconnect_delay_ms: 1000,
    };
    SimpleBitgetConnector::new(Some(config))
}