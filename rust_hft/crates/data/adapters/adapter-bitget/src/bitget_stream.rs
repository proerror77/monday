//! Bitget WebSocket 行情流實現
use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{info, warn, error, debug};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use hft_core::*;
use ports::*;
use integration::ws::{WsClientConfig, ReconnectingWsClient, MessageHandler};

/// Bitget WebSocket 消息結構
#[derive(Debug, Clone, Deserialize)]
pub struct BitgetWsMessage {
    pub action: Option<String>,
    pub arg: Option<BitgetSubscriptionArg>,
    pub data: Option<serde_json::Value>,
    pub code: Option<u32>,
    pub msg: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BitgetSubscriptionArg {
    #[serde(rename = "instType")]
    pub inst_type: String,
    #[serde(rename = "channel")]
    pub channel: String,
    #[serde(rename = "instId")]
    pub inst_id: String,
}

/// Bitget L2 訂單簿數據結構
#[derive(Debug, Clone, Deserialize)]
pub struct BitgetOrderBookData {
    #[serde(rename = "asks")]
    pub asks: Vec<[String; 3]>, // [price, size, count]
    #[serde(rename = "bids")]
    pub bids: Vec<[String; 3]>, // [price, size, count]
    #[serde(rename = "checksum")]
    pub checksum: Option<i64>,
    #[serde(rename = "ts")]
    pub ts: String,
}

/// Bitget 成交數據結構
#[derive(Debug, Clone, Deserialize)]
pub struct BitgetTradeData {
    #[serde(rename = "instId")]
    pub inst_id: String,
    #[serde(rename = "tradeId")]
    pub trade_id: String,
    #[serde(rename = "px")]
    pub price: String,
    #[serde(rename = "sz")]
    pub size: String,
    #[serde(rename = "side")]
    pub side: String,
    #[serde(rename = "ts")]
    pub ts: String,
}

/// Bitget 訂閱請求
#[derive(Debug, Clone, Serialize)]
pub struct BitgetSubscriptionRequest {
    pub op: String,
    pub args: Vec<BitgetSubscriptionArg>,
}

pub struct BitgetMarketStream {
    ws_client: Option<ReconnectingWsClient>,
    event_sender: Option<mpsc::UnboundedSender<MarketEvent>>,
    subscribed_symbols: Vec<Symbol>,
}

impl BitgetMarketStream {
    pub fn new() -> Self {
        Self {
            ws_client: None,
            event_sender: None,
            subscribed_symbols: Vec::new(),
        }
    }

    fn create_ws_config() -> WsClientConfig {
        WsClientConfig {
            url: "wss://ws.bitget.com/spot/v1/stream".to_string(),
            heartbeat_interval: std::time::Duration::from_secs(30),
            reconnect_interval: std::time::Duration::from_secs(5),
            max_reconnect_attempts: 10,
        }
    }

    fn parse_orderbook_data(&self, data: &BitgetOrderBookData, symbol: &Symbol) -> Result<MarketSnapshot, HftError> {
        let timestamp = data.ts.parse::<u64>()
            .map_err(|_| HftError::Generic { message: "Invalid timestamp".to_string() })?;

        let mut bids = Vec::new();
        let mut asks = Vec::new();

        // 解析 bids (按價格降序)
        for bid in &data.bids {
            if bid.len() >= 2 {
                let price = Price::from_str(&bid[0])
                    .map_err(|_| HftError::Generic { message: format!("Invalid bid price: {}", bid[0]) })?;
                let quantity = Quantity::from_str(&bid[1])
                    .map_err(|_| HftError::Generic { message: format!("Invalid bid quantity: {}", bid[1]) })?;
                
                bids.push(BookLevel { price, quantity });
            }
        }

        // 解析 asks (按價格升序)
        for ask in &data.asks {
            if ask.len() >= 2 {
                let price = Price::from_str(&ask[0])
                    .map_err(|_| HftError::Generic { message: format!("Invalid ask price: {}", ask[0]) })?;
                let quantity = Quantity::from_str(&ask[1])
                    .map_err(|_| HftError::Generic { message: format!("Invalid ask quantity: {}", ask[1]) })?;
                
                asks.push(BookLevel { price, quantity });
            }
        }

        Ok(MarketSnapshot {
            symbol: symbol.clone(),
            timestamp,
            bids,
            asks,
            sequence: 0, // Bitget 不提供序列號，使用時間戳
        })
    }

    fn parse_trade_data(&self, data: &BitgetTradeData) -> Result<Trade, HftError> {
        let timestamp = data.ts.parse::<u64>()
            .map_err(|_| HftError::Generic { message: "Invalid timestamp".to_string() })?;

        let price = Price::from_str(&data.price)
            .map_err(|_| HftError::Generic { message: format!("Invalid trade price: {}", data.price) })?;
        
        let quantity = Quantity::from_str(&data.size)
            .map_err(|_| HftError::Generic { message: format!("Invalid trade size: {}", data.size) })?;

        let side = match data.side.as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => return Err(HftError::Generic { message: format!("Invalid trade side: {}", data.side) }),
        };

        Ok(Trade {
            symbol: Symbol(data.inst_id.clone()),
            timestamp,
            price,
            quantity,
            side,
            trade_id: data.trade_id.clone(),
        })
    }

    async fn send_subscription(&mut self, symbols: &[Symbol]) -> Result<(), HftError> {
        if let Some(ref mut client) = self.ws_client {
            // 訂閱 L2 深度數據
            let orderbook_args: Vec<BitgetSubscriptionArg> = symbols.iter().map(|s| {
                BitgetSubscriptionArg {
                    inst_type: "SP".to_string(),
                    channel: "depth".to_string(),
                    inst_id: s.0.clone(),
                }
            }).collect();

            let orderbook_request = BitgetSubscriptionRequest {
                op: "subscribe".to_string(),
                args: orderbook_args,
            };

            let orderbook_msg = serde_json::to_string(&orderbook_request)
                .map_err(|e| HftError::Generic { message: format!("Failed to serialize orderbook subscription: {}", e) })?;

            // 訂閱成交數據
            let trade_args: Vec<BitgetSubscriptionArg> = symbols.iter().map(|s| {
                BitgetSubscriptionArg {
                    inst_type: "SP".to_string(),
                    channel: "trades".to_string(),
                    inst_id: s.0.clone(),
                }
            }).collect();

            let trade_request = BitgetSubscriptionRequest {
                op: "subscribe".to_string(),
                args: trade_args,
            };

            let trade_msg = serde_json::to_string(&trade_request)
                .map_err(|e| HftError::Generic { message: format!("Failed to serialize trade subscription: {}", e) })?;

            // 發送訂閱請求
            client.client.send_message(&orderbook_msg).await
                .map_err(|e| HftError::Generic { message: format!("Failed to send orderbook subscription: {}", e) })?;

            client.client.send_message(&trade_msg).await
                .map_err(|e| HftError::Generic { message: format!("Failed to send trade subscription: {}", e) })?;

            info!("已訂閱 {} 個交易對的行情和成交數據", symbols.len());
            Ok(())
        } else {
            Err(HftError::Generic { message: "WebSocket client not initialized".to_string() })
        }
    }
}

#[async_trait]
impl MarketStream for BitgetMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        
        // 創建 WebSocket 客戶端
        let ws_config = Self::create_ws_config();
        let mut ws_client = ReconnectingWsClient::new(ws_config);
        
        // 創建消息處理器
        let handler = BitgetMessageHandler::new(tx.clone(), symbols.clone());

        // 在後台任務中運行 WebSocket 客戶端
        tokio::spawn(async move {
            if let Err(e) = ws_client.run_with_handler(handler).await {
                error!("Bitget WebSocket 客戶端錯誤: {}", e);
                let _ = tx.send(MarketEvent::Disconnect { 
                    reason: format!("WebSocket error: {}", e) 
                });
            }
        });

        // 延遲發送訂閱請求將在連接建立後由 MessageHandler 處理
        // 這裡我們先將訂閱信息存儲

        // 創建事件流
        let stream = async_stream::stream! {
            while let Some(event) = rx.recv().await {
                yield Ok(event);
            }
        };

        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.ws_client.as_ref()
                .map(|c| c.client.is_connected())
                .unwrap_or(false),
            latency_ms: None,
            last_heartbeat: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        let config = Self::create_ws_config();
        let mut client = ReconnectingWsClient::new(config);
        
        client.connect_with_retry().await
            .map_err(|e| HftError::Generic { message: format!("Connection failed: {}", e) })?;
        
        self.ws_client = Some(client);
        info!("Bitget MarketStream 連接成功");
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        if let Some(mut client) = self.ws_client.take() {
            client.client.disconnect().await
                .map_err(|e| HftError::Generic { message: format!("Disconnect failed: {}", e) })?;
        }
        info!("Bitget MarketStream 已斷開連接");
        Ok(())
    }
}

/// Bitget 消息處理器
struct BitgetMessageHandler {
    event_sender: mpsc::UnboundedSender<MarketEvent>,
    subscribed_symbols: Vec<Symbol>,
    subscription_sent: bool,
    pending_subscriptions: Option<(String, String)>,
}

impl BitgetMessageHandler {
    fn new(event_sender: mpsc::UnboundedSender<MarketEvent>, subscribed_symbols: Vec<Symbol>) -> Self {
        Self {
            event_sender,
            subscribed_symbols,
            subscription_sent: false,
            pending_subscriptions: None,
        }
    }

}

impl MessageHandler for BitgetMessageHandler {
    fn handle_message(&mut self, message: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        debug!("收到 Bitget 消息: {}", message);
        
        // 解析 WebSocket 消息
        let ws_msg: BitgetWsMessage = serde_json::from_str(&message)?;
        
        // 處理不同類型的消息
        if let Some(action) = &ws_msg.action {
            match action.as_str() {
                "snapshot" | "update" => {
                    if let Some(data) = &ws_msg.data {
                        self.handle_orderbook_data(data, &ws_msg.arg)?;
                    }
                }
                "insert" => {
                    if let Some(data) = &ws_msg.data {
                        self.handle_trade_data(data)?;
                    }
                }
                _ => {
                    debug!("未處理的消息類型: {}", action);
                }
            }
        } else if let Some(code) = &ws_msg.code {
            if *code == 0 {
                info!("訂閱成功");
            } else {
                warn!("收到錯誤代碼: {}, 消息: {:?}", code, ws_msg.msg);
            }
        }

        Ok(())
    }

    fn handle_disconnect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        warn!("Bitget WebSocket 連接斷開");
        self.subscription_sent = false; // 重置訂閱狀態，重連後需要重新訂閱
        let _ = self.event_sender.send(MarketEvent::Disconnect {
            reason: "Connection lost".to_string(),
        });
        Ok(())
    }
    
    fn handle_connected_async<'a>(&'a mut self, client: &'a mut integration::ws::WsClient) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send + 'a>> {
        Box::pin(async move {
        if self.subscription_sent || self.subscribed_symbols.is_empty() {
            return Ok(());
        }

        // 準備訂閱消息
        let orderbook_args: Vec<BitgetSubscriptionArg> = self.subscribed_symbols.iter().map(|s| {
            BitgetSubscriptionArg {
                inst_type: "SP".to_string(),
                channel: "depth".to_string(),
                inst_id: s.0.clone(),
            }
        }).collect();

        let orderbook_request = BitgetSubscriptionRequest {
            op: "subscribe".to_string(),
            args: orderbook_args,
        };

        let orderbook_msg = serde_json::to_string(&orderbook_request)?;

        let trade_args: Vec<BitgetSubscriptionArg> = self.subscribed_symbols.iter().map(|s| {
            BitgetSubscriptionArg {
                inst_type: "SP".to_string(),
                channel: "trades".to_string(),
                inst_id: s.0.clone(),
            }
        }).collect();

        let trade_request = BitgetSubscriptionRequest {
            op: "subscribe".to_string(),
            args: trade_args,
        };

        let trade_msg = serde_json::to_string(&trade_request)?;

        // 發送訂閱消息
        client.send_message(&orderbook_msg).await?;
        client.send_message(&trade_msg).await?;

        self.subscription_sent = true;
        info!("已成功訂閱 {} 個交易對的行情和成交數據", self.subscribed_symbols.len());
        
        Ok(())
        })
    }
}

impl BitgetMessageHandler {
    fn handle_orderbook_data(&mut self, data: &serde_json::Value, arg: &Option<BitgetSubscriptionArg>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(arg) = arg {
            if arg.channel == "depth" {
                let orderbook_array: Vec<BitgetOrderBookData> = serde_json::from_value(data.clone())?;
                
                for orderbook_data in orderbook_array {
                    let symbol = Symbol(arg.inst_id.clone());
                    
                    match self.parse_orderbook_data(&orderbook_data, &symbol) {
                        Ok(snapshot) => {
                            let _ = self.event_sender.send(MarketEvent::Snapshot(snapshot));
                        }
                        Err(e) => {
                            error!("解析訂單簿數據失敗: {}", e);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_trade_data(&mut self, data: &serde_json::Value) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let trade_array: Vec<BitgetTradeData> = serde_json::from_value(data.clone())?;
        
        for trade_data in trade_array {
            match self.parse_trade_data(&trade_data) {
                Ok(trade) => {
                    let _ = self.event_sender.send(MarketEvent::Trade(trade));
                }
                Err(e) => {
                    error!("解析成交數據失敗: {}", e);
                }
            }
        }
        Ok(())
    }

    fn parse_orderbook_data(&self, data: &BitgetOrderBookData, symbol: &Symbol) -> Result<MarketSnapshot, HftError> {
        let timestamp = data.ts.parse::<u64>()
            .map_err(|_| HftError::Generic { message: "Invalid timestamp".to_string() })?;

        let mut bids = Vec::new();
        let mut asks = Vec::new();

        // 解析 bids
        for bid in &data.bids {
            if bid.len() >= 2 {
                let price = Price::from_str(&bid[0])
                    .map_err(|_| HftError::Generic { message: format!("Invalid bid price: {}", bid[0]) })?;
                let quantity = Quantity::from_str(&bid[1])
                    .map_err(|_| HftError::Generic { message: format!("Invalid bid quantity: {}", bid[1]) })?;
                
                bids.push(BookLevel { price, quantity });
            }
        }

        // 解析 asks
        for ask in &data.asks {
            if ask.len() >= 2 {
                let price = Price::from_str(&ask[0])
                    .map_err(|_| HftError::Generic { message: format!("Invalid ask price: {}", ask[0]) })?;
                let quantity = Quantity::from_str(&ask[1])
                    .map_err(|_| HftError::Generic { message: format!("Invalid ask quantity: {}", ask[1]) })?;
                
                asks.push(BookLevel { price, quantity });
            }
        }

        Ok(MarketSnapshot {
            symbol: symbol.clone(),
            timestamp,
            bids,
            asks,
            sequence: 0,
        })
    }

    fn parse_trade_data(&self, data: &BitgetTradeData) -> Result<Trade, HftError> {
        let timestamp = data.ts.parse::<u64>()
            .map_err(|_| HftError::Generic { message: "Invalid timestamp".to_string() })?;

        let price = Price::from_str(&data.price)
            .map_err(|_| HftError::Generic { message: format!("Invalid trade price: {}", data.price) })?;
        
        let quantity = Quantity::from_str(&data.size)
            .map_err(|_| HftError::Generic { message: format!("Invalid trade size: {}", data.size) })?;

        let side = match data.side.as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => return Err(HftError::Generic { message: format!("Invalid trade side: {}", data.side) }),
        };

        Ok(Trade {
            symbol: Symbol(data.inst_id.clone()),
            timestamp,
            price,
            quantity,
            side,
            trade_id: data.trade_id.clone(),
        })
    }
}