//! Binance交易所實現

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures::{StreamExt, SinkExt};
use reqwest::Client;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tracing::{info, warn, error, debug};
use uuid;
use hex;

use super::exchange_trait::*;
use super::message_types::*;
use crate::core::types::*;

// 需要添加缺少的導入
use urlencoding;

type HmacSha256 = Hmac<Sha256>;

/// Binance交易所配置
#[derive(Debug, Clone)]
pub struct BinanceConfig {
    pub api_key: String,
    pub secret_key: String,
    pub sandbox: bool,
    pub ws_public_url: String,
    pub ws_private_url: String,
    pub rest_base_url: String,
}

impl Default for BinanceConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            sandbox: false,
            ws_public_url: "wss://stream.binance.com:9443/ws/".to_string(),
            ws_private_url: "wss://stream.binance.com:9443/ws/".to_string(),
            rest_base_url: "https://api.binance.com".to_string(),
        }
    }
}

/// Binance交易所實現
pub struct BinanceExchange {
    config: BinanceConfig,
    http_client: Client,
    
    // 市場數據相關 - 修復：分離讀寫端，保留寫端引用
    public_ws_writer: Arc<Mutex<Option<futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>>>>,
    market_event_sender: Arc<Mutex<Option<mpsc::Sender<MarketEvent>>>>,
    subscribed_symbols: Arc<RwLock<HashMap<String, Vec<String>>>>,
    
    // 交易相關 - 修復：分離讀寫端，保留寫端引用
    private_ws_writer: Arc<Mutex<Option<futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>>>>,
    execution_report_sender: Arc<Mutex<Option<mpsc::Sender<ExecutionReport>>>>,
    
    // 狀態管理
    connection_status: Arc<RwLock<ConnectionStatus>>,
    market_data_status: Arc<RwLock<MarketDataStatus>>,
    trading_status: Arc<RwLock<TradingStatus>>,
    
    // 性能指標
    latency_ms: Arc<RwLock<Option<f64>>>,
    last_heartbeat: Arc<RwLock<u64>>,
    error_count: Arc<RwLock<u64>>,
}

impl BinanceExchange {
    pub fn new() -> Self {
        Self::with_config(BinanceConfig::default())
    }

    pub fn with_config(config: BinanceConfig) -> Self {
        let http_client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            config,
            http_client,
            public_ws_writer: Arc::new(Mutex::new(None)),
            private_ws_writer: Arc::new(Mutex::new(None)),
            market_event_sender: Arc::new(Mutex::new(None)),
            execution_report_sender: Arc::new(Mutex::new(None)),
            subscribed_symbols: Arc::new(RwLock::new(HashMap::new())),
            connection_status: Arc::new(RwLock::new(ConnectionStatus::Disconnected)),
            market_data_status: Arc::new(RwLock::new(MarketDataStatus::Inactive)),
            trading_status: Arc::new(RwLock::new(TradingStatus::Disabled)),
            latency_ms: Arc::new(RwLock::new(None)),
            last_heartbeat: Arc::new(RwLock::new(0)),
            error_count: Arc::new(RwLock::new(0)),
        }
    }

    /// 生成API簽名
    fn generate_signature(&self, query_string: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.config.secret_key.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(query_string.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// 解析訂單簿消息
    fn parse_orderbook_message(&self, data: &Value) -> Option<MarketEvent> {
        let symbol = data.get("s")?.as_str()?.to_string();
        
        let bids: Vec<OrderBookLevel> = data
            .get("b")?
            .as_array()?
            .iter()
            .filter_map(|bid| {
                let array = bid.as_array()?;
                Some(OrderBookLevel {
                    price: array.get(0)?.as_str()?.parse().ok()?,
                    quantity: array.get(1)?.as_str()?.parse().ok()?,
                    order_count: 1, // Binance不提供訂單數量
                })
            })
            .collect();

        let asks: Vec<OrderBookLevel> = data
            .get("a")?
            .as_array()?
            .iter()
            .filter_map(|ask| {
                let array = ask.as_array()?;
                Some(OrderBookLevel {
                    price: array.get(0)?.as_str()?.parse().ok()?,
                    quantity: array.get(1)?.as_str()?.parse().ok()?,
                    order_count: 1,
                })
            })
            .collect();

        let timestamp = chrono::Utc::now().timestamp_millis() as u64;
        let sequence = data.get("u")?.as_u64()?;

        Some(MarketEvent::OrderBookUpdate {
            symbol,
            exchange: "binance".to_string(),
            bids,
            asks,
            timestamp,
            sequence,
            is_snapshot: false, // Binance差分更新
        })
    }

    /// 解析成交消息
    fn parse_trade_message(&self, data: &Value) -> Option<MarketEvent> {
        let symbol = data.get("s")?.as_str()?.to_string();
        let trade_id = data.get("t")?.as_u64()?.to_string();
        let price = data.get("p")?.as_str()?.parse().ok()?;
        let quantity = data.get("q")?.as_str()?.parse().ok()?;
        let buyer_maker = data.get("m")?.as_bool()?;
        let timestamp = data.get("T")?.as_u64()?;
        
        // 根據buyer_maker判斷side
        let side = if buyer_maker {
            OrderSide::Sell // 如果買方是Maker，那麼這是賣單
        } else {
            OrderSide::Buy // 如果買方是Taker，那麼這是買單
        };

        Some(MarketEvent::Trade {
            symbol,
            exchange: "binance".to_string(),
            trade_id,
            price,
            quantity,
            side,
            timestamp,
            buyer_maker,
        })
    }

    /// 處理WebSocket消息
    async fn handle_ws_message(&self, message: Message) {
        if let Ok(text) = message.to_text() {
            if let Ok(data) = serde_json::from_str::<Value>(text) {
                // 檢查消息類型
                if let Some(stream) = data.get("stream").and_then(|s| s.as_str()) {
                    if stream.contains("@depth") {
                        if let Some(event) = self.parse_orderbook_message(data.get("data")?) {
                            if let Some(sender) = self.market_event_sender.lock().await.as_ref() {
                                // P0 修復：使用 try_send 實現背壓，通道滿時丟棄新消息
                                if let Err(e) = sender.try_send(event) {
                                    warn!("Market event channel full, dropping orderbook update: {:?}", e);
                                }
                            }
                        }
                    } else if stream.contains("@trade") {
                        if let Some(event) = self.parse_trade_message(data.get("data")?) {
                            if let Some(sender) = self.market_event_sender.lock().await.as_ref() {
                                // P0 修復：使用 try_send 實現背壓，通道滿時丟棄新消息
                                if let Err(e) = sender.try_send(event) {
                                    warn!("Market event channel full, dropping trade update: {:?}", e);
                                }
                            }
                        }
                    }
                } else if let Some(event) = data.get("e").and_then(|e| e.as_str()) {
                    match event {
                        "depthUpdate" => {
                            if let Some(event) = self.parse_orderbook_message(&data) {
                                if let Some(sender) = self.market_event_sender.lock().await.as_ref() {
                                    // P0 修復：使用 try_send 實現背壓，通道滿時丟棄新消息
                                    if let Err(e) = sender.try_send(event) {
                                        warn!("Market event channel full, dropping orderbook update: {:?}", e);
                                    }
                                }
                            }
                        }
                        "trade" => {
                            if let Some(event) = self.parse_trade_message(&data) {
                                if let Some(sender) = self.market_event_sender.lock().await.as_ref() {
                                    // P0 修復：使用 try_send 實現背壓，通道滿時丟棄新消息
                                    if let Err(e) = sender.try_send(event) {
                                        warn!("Market event channel full, dropping trade update: {:?}", e);
                                    }
                                }
                            }
                        }
                        _ => {
                            debug!("Unhandled Binance event: {}", event);
                        }
                    }
                }
            }
        }
    }

    /// 構建流名稱
    fn build_stream_name(&self, symbol: &str, channel: &str) -> String {
        let normalized_symbol = symbol.to_lowercase();
        match channel {
            "orderbook" => format!("{}@depth20@100ms", normalized_symbol),
            "trades" => format!("{}@trade", normalized_symbol),
            "ticker" => format!("{}@ticker", normalized_symbol),
            _ => format!("{}@{}", normalized_symbol, channel),
        }
    }
}

#[async_trait]
impl MarketDataClient for BinanceExchange {
    async fn connect_public(&mut self) -> Result<(), String> {
        *self.connection_status.write().await = ConnectionStatus::Connecting;
        
        match connect_async(&self.config.ws_public_url).await {
            Ok((ws_stream, _)) => {
                // 修復：分離讀寫端，避免 writer 丟失
                let (ws_writer, mut ws_reader) = ws_stream.split();
                *self.public_ws_writer.lock().await = Some(ws_writer);
                
                *self.connection_status.write().await = ConnectionStatus::Connected;
                *self.market_data_status.write().await = MarketDataStatus::Active;
                
                // 啟動消息處理循環 - 修復：直接使用 reader
                let exchange_clone = self.clone();
                
                tokio::spawn(async move {
                    // P0 修復：添加背壓策略，避免消息堆積
                    let (msg_tx, mut msg_rx) = mpsc::channel::<Message>(1000); // 限制緩衝區大小
                    let mut message_count = 0u64;
                    let mut last_log = std::time::Instant::now();
                    
                    // 消息接收任務（快速路徑）
                    let reader_handle = tokio::spawn(async move {
                        while let Some(msg) = ws_reader.next().await {
                            match msg {
                                Ok(message) => {
                                    // 背壓控制：如果通道滿了，丟棄舊消息
                                    if let Err(_) = msg_tx.try_send(message) {
                                        debug!("Message channel full, dropping WebSocket message");
                                    }
                                }
                                Err(e) => {
                                    error!("Binance WebSocket error: {}", e);
                                    break;
                                }
                            }
                        }
                    });
                    
                    // 消息處理任務（背壓控制）
                    while let Some(message) = msg_rx.recv().await {
                        exchange_clone.handle_ws_message(message).await;
                        message_count += 1;
                        
                        // 每秒記錄處理速度
                        if last_log.elapsed() >= std::time::Duration::from_secs(10) {
                            debug!("Processed {} messages in 10s", message_count);
                            message_count = 0;
                            last_log = std::time::Instant::now();
                        }
                    }
                    
                    reader_handle.abort();
                });
                
                info!("Connected to Binance public WebSocket");
                Ok(())
            }
            Err(e) => {
                *self.connection_status.write().await = ConnectionStatus::Error(format!("Connection failed: {}", e));
                Err(format!("Failed to connect to Binance WebSocket: {}", e))
            }
        }
    }

    async fn disconnect_public(&mut self) -> Result<(), String> {
        if let Some(mut ws_writer) = self.public_ws_writer.lock().await.take() {
            let _ = ws_writer.close().await;
        }
        
        *self.connection_status.write().await = ConnectionStatus::Disconnected;
        *self.market_data_status.write().await = MarketDataStatus::Inactive;
        
        info!("Disconnected from Binance public WebSocket");
        Ok(())
    }

    async fn subscribe_orderbook(&mut self, symbol: &str, depth: u32) -> Result<(), String> {
        let stream_name = match depth {
            5 => format!("{}@depth5@100ms", symbol.to_lowercase()),
            10 => format!("{}@depth10@100ms", symbol.to_lowercase()),
            20 => format!("{}@depth20@100ms", symbol.to_lowercase()),
            _ => format!("{}@depth@100ms", symbol.to_lowercase()),
        };
        
        let subscribe_msg = json!({
            "method": "SUBSCRIBE",
            "params": [stream_name],
            "id": 1
        });

        if let Some(ws_writer) = self.public_ws_writer.lock().await.as_mut() {
            ws_writer.send(Message::Text(subscribe_msg.to_string())).await
                .map_err(|e| format!("Failed to subscribe to orderbook: {}", e))?;
            
            // 記錄訂閱
            let mut subs = self.subscribed_symbols.write().await;
            subs.entry(symbol.to_string()).or_insert_with(Vec::new).push("orderbook".to_string());
            
            info!("Subscribed to Binance orderbook for {} (depth: {})", symbol, depth);
            Ok(())
        } else {
            Err("WebSocket not connected".to_string())
        }
    }

    async fn unsubscribe_orderbook(&mut self, symbol: &str) -> Result<(), String> {
        let stream_name = format!("{}@depth20@100ms", symbol.to_lowercase());
        
        let unsubscribe_msg = json!({
            "method": "UNSUBSCRIBE",
            "params": [stream_name],
            "id": 2
        });

        if let Some(ws_writer) = self.public_ws_writer.lock().await.as_mut() {
            ws_writer.send(Message::Text(unsubscribe_msg.to_string())).await
                .map_err(|e| format!("Failed to unsubscribe from orderbook: {}", e))?;
            
            // 移除訂閱記錄
            let mut subs = self.subscribed_symbols.write().await;
            subs.remove(symbol);
            
            info!("Unsubscribed from Binance orderbook for {}", symbol);
            Ok(())
        } else {
            Err("WebSocket not connected".to_string())
        }
    }

    async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), String> {
        let stream_name = format!("{}@trade", symbol.to_lowercase());
        
        let subscribe_msg = json!({
            "method": "SUBSCRIBE",
            "params": [stream_name],
            "id": 3
        });

        if let Some(ws_writer) = self.public_ws_writer.lock().await.as_mut() {
            ws_writer.send(Message::Text(subscribe_msg.to_string())).await
                .map_err(|e| format!("Failed to subscribe to trades: {}", e))?;
            
            // 記錄訂閱
            let mut subs = self.subscribed_symbols.write().await;
            subs.entry(symbol.to_string()).or_insert_with(Vec::new).push("trades".to_string());
            
            info!("Subscribed to Binance trades for {}", symbol);
            Ok(())
        } else {
            Err("WebSocket not connected".to_string())
        }
    }

    async fn unsubscribe_trades(&mut self, symbol: &str) -> Result<(), String> {
        let stream_name = format!("{}@trade", symbol.to_lowercase());
        
        let unsubscribe_msg = json!({
            "method": "UNSUBSCRIBE",
            "params": [stream_name],
            "id": 4
        });

        if let Some(ws_writer) = self.public_ws_writer.lock().await.as_mut() {
            ws_writer.send(Message::Text(unsubscribe_msg.to_string())).await
                .map_err(|e| format!("Failed to unsubscribe from trades: {}", e))?;
            
            info!("Unsubscribed from Binance trades for {}", symbol);
            Ok(())
        } else {
            Err("WebSocket not connected".to_string())
        }
    }

    async fn subscribe_ticker(&mut self, symbol: &str) -> Result<(), String> {
        let stream_name = format!("{}@ticker", symbol.to_lowercase());
        
        let subscribe_msg = json!({
            "method": "SUBSCRIBE",
            "params": [stream_name],
            "id": 5
        });

        if let Some(ws_writer) = self.public_ws_writer.lock().await.as_mut() {
            ws_writer.send(Message::Text(subscribe_msg.to_string())).await
                .map_err(|e| format!("Failed to subscribe to ticker: {}", e))?;
            
            info!("Subscribed to Binance ticker for {}", symbol);
            Ok(())
        } else {
            Err("WebSocket not connected".to_string())
        }
    }

    async fn get_market_events(&self) -> Result<mpsc::Receiver<MarketEvent>, String> {
        // P0 修復：使用有界通道防止內存泄漏，緩衝區大小 10000
        let (sender, receiver) = mpsc::channel(10000);
        *self.market_event_sender.lock().await = Some(sender);
        Ok(receiver)
    }

    async fn get_symbols(&self) -> Result<Vec<String>, String> {
        let url = format!("{}/api/v3/exchangeInfo", self.config.rest_base_url);
        
        let response = self.http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch symbols: {}", e))?;
            
        let data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse symbols response: {}", e))?;
        
        if let Some(symbols_data) = data.get("symbols").and_then(|d| d.as_array()) {
            let symbols: Vec<String> = symbols_data
                .iter()
                .filter_map(|s| {
                    // 只返回TRADING狀態的交易對
                    if s.get("status").and_then(|status| status.as_str()) == Some("TRADING") {
                        s.get("symbol").and_then(|sym| sym.as_str()).map(|sym| sym.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            Ok(symbols)
        } else {
            Err("Invalid symbols response format".to_string())
        }
    }

    async fn is_healthy(&self) -> bool {
        *self.connection_status.read().await == ConnectionStatus::Connected &&
        *self.market_data_status.read().await == MarketDataStatus::Active
    }
}

// TradingClient實現（基礎版本）
#[async_trait]
impl TradingClient for BinanceExchange {
    async fn connect_private(&mut self) -> Result<(), String> {
        *self.trading_status.write().await = TradingStatus::Enabled;
        Ok(())
    }

    async fn disconnect_private(&mut self) -> Result<(), String> {
        *self.trading_status.write().await = TradingStatus::Disabled;
        Ok(())
    }

    async fn place_order(&mut self, request: OrderRequest) -> Result<OrderResponse, String> {
        let timestamp = chrono::Utc::now().timestamp_millis();
        
        let mut query_params = vec![
            ("symbol", request.symbol.clone()),
            ("side", match request.side {
                OrderSide::Buy => "BUY".to_string(),
                OrderSide::Sell => "SELL".to_string(),
            }),
            ("type", match request.order_type {
                OrderType::Market => "MARKET".to_string(),
                OrderType::Limit => "LIMIT".to_string(),
                _ => return Err("Unsupported order type".to_string()),
            }),
            ("quantity", request.quantity.to_string()),
            ("timestamp", timestamp.to_string()),
        ];

        if let Some(price) = request.price {
            query_params.push(("price", price.to_string()));
        }

        if request.order_type == OrderType::Limit {
            query_params.push(("timeInForce", match request.time_in_force {
                TimeInForce::GTC => "GTC".to_string(),
                TimeInForce::IOC => "IOC".to_string(),
                TimeInForce::FOK => "FOK".to_string(),
                _ => "GTC".to_string(),
            }));
        }

        let query_string = query_params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");

        let signature = self.generate_signature(&query_string);
        let final_query = format!("{}&signature={}", query_string, signature);

        let url = format!("{}/api/v3/order", self.config.rest_base_url);

        let response = self.http_client
            .post(&url)
            .header("X-MBX-APIKEY", &self.config.api_key)
            .body(final_query)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        let response_data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        if let Some(order_id) = response_data.get("orderId") {
            Ok(OrderResponse {
                success: true,
                order_id: order_id.to_string(),
                client_order_id: request.client_order_id,
                status: OrderStatus::New,
                error: None,
                timestamp: chrono::Utc::now().timestamp_nanos() as u64,
            })
        } else if let Some(code) = response_data.get("code") {
            let error_msg = response_data.get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error")
                .to_string();

            Ok(OrderResponse {
                success: false,
                order_id: String::new(),
                client_order_id: request.client_order_id,
                status: OrderStatus::Rejected,
                error: Some(format!("Code {}: {}", code, error_msg)),
                timestamp: chrono::Utc::now().timestamp_nanos() as u64,
            })
        } else {
            Err("Invalid response format".to_string())
        }
    }

    async fn cancel_order(&mut self, request: CancelRequest) -> Result<OrderResponse, String> {
        let timestamp = chrono::Utc::now().timestamp_millis();
        
        let mut query_params = vec![
            ("symbol", request.symbol.clone()),
            ("timestamp", timestamp.to_string()),
        ];
        
        // 使用訂單ID或客戶端訂單ID
        if let Some(order_id) = request.order_id {
            query_params.push(("orderId", order_id));
        } else if let Some(client_order_id) = request.client_order_id {
            query_params.push(("origClientOrderId", client_order_id));
        } else {
            return Err("Either order_id or client_order_id must be provided".to_string());
        }
        
        let query_string = query_params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        
        let signature = self.generate_signature(&query_string);
        let final_query = format!("{}&signature={}", query_string, signature);
        
        let url = format!("{}/api/v3/order", self.config.rest_base_url);
        
        let response = self.http_client
            .delete(&url)
            .header("X-MBX-APIKEY", &self.config.api_key)
            .body(final_query)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| format!("Cancel request failed: {}", e))?;
        
        let response_data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse cancel response: {}", e))?;
        
        if let Some(order_id) = response_data.get("orderId") {
            Ok(OrderResponse {
                success: true,
                order_id: order_id.to_string(),
                client_order_id: response_data.get("clientOrderId")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string(),
                status: OrderStatus::Cancelled,
                error: None,
                timestamp: chrono::Utc::now().timestamp_nanos() as u64,
            })
        } else if let Some(code) = response_data.get("code") {
            let error_msg = response_data.get("msg")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown cancel error")
                .to_string();
            
            Ok(OrderResponse {
                success: false,
                order_id: String::new(),
                client_order_id: String::new(),
                status: OrderStatus::Rejected,
                error: Some(format!("Code {}: {}", code, error_msg)),
                timestamp: chrono::Utc::now().timestamp_nanos() as u64,
            })
        } else {
            Err("Invalid cancel response format".to_string())
        }
    }

    async fn amend_order(&mut self, request: AmendRequest) -> Result<OrderResponse, String> {
        // Binance不直接支持修改訂單，需要先取消後重新下單
        // 這裡實現基本的修改邏輯
        
        // 首先查詢原訂單信息
        let original_order = self.get_order(&request.order_id).await
            .map_err(|e| format!("Failed to get original order: {}", e))?;
        
        // 創建取消請求
        let cancel_request = CancelRequest {
            order_id: Some(request.order_id.clone()),
            client_order_id: None,
            symbol: original_order.symbol.clone(),
        };
        
        // 取消原訂單
        let cancel_response = self.cancel_order(cancel_request).await?;
        
        if !cancel_response.success {
            return Ok(OrderResponse {
                success: false,
                order_id: String::new(),
                client_order_id: String::new(),
                status: OrderStatus::Rejected,
                error: Some("Failed to cancel original order for amendment".to_string()),
                timestamp: chrono::Utc::now().timestamp_nanos() as u64,
            });
        }
        
        // 創建新的訂單請求
        let new_request = OrderRequest {
            client_order_id: uuid::Uuid::new_v4().to_string(),
            symbol: original_order.symbol,
            side: original_order.side,
            order_type: original_order.order_type,
            quantity: request.quantity.unwrap_or(original_order.original_quantity),
            price: request.price.or(Some(original_order.price)),
            stop_price: request.stop_price,
            time_in_force: TimeInForce::GTC,
            post_only: false,
            reduce_only: false,
            metadata: std::collections::HashMap::new(),
        };
        
        // 下新訂單
        self.place_order(new_request).await
    }

    async fn get_order(&mut self, order_id: &str) -> Result<ExecutionReport, String> {
        let timestamp = chrono::Utc::now().timestamp_millis();
        
        // 注意：這裡假設order_id是Binance的orderId，如果是symbol需要額外處理
        // 為了簡化，我們假設可以通過orderId查詢所有交易對的訂單
        let query_params = vec![
            ("orderId", order_id.to_string()),
            ("timestamp", timestamp.to_string()),
        ];
        
        let query_string = query_params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        
        let signature = self.generate_signature(&query_string);
        let final_query = format!("{}&signature={}", query_string, signature);
        
        let url = format!("{}/api/v3/order?{}", self.config.rest_base_url, final_query);
        
        let response = self.http_client
            .get(&url)
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send()
            .await
            .map_err(|e| format!("Get order request failed: {}", e))?;
        
        let response_data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse get order response: {}", e))?;
        
        // 解析訂單信息為ExecutionReport
        let symbol = response_data.get("symbol")
            .and_then(|s| s.as_str())
            .ok_or("Missing symbol in order response")?
            .to_string();
        
        let side = match response_data.get("side").and_then(|s| s.as_str()) {
            Some("BUY") => OrderSide::Buy,
            Some("SELL") => OrderSide::Sell,
            _ => return Err("Invalid order side".to_string()),
        };
        
        let order_type = match response_data.get("type").and_then(|t| t.as_str()) {
            Some("MARKET") => OrderType::Market,
            Some("LIMIT") => OrderType::Limit,
            Some("STOP_LOSS") => OrderType::StopLoss,
            Some("STOP_LOSS_LIMIT") => OrderType::StopLossLimit,
            _ => OrderType::Limit, // 默認
        };
        
        let status = match response_data.get("status").and_then(|s| s.as_str()) {
            Some("NEW") => OrderStatus::New,
            Some("PARTIALLY_FILLED") => OrderStatus::PartiallyFilled,
            Some("FILLED") => OrderStatus::Filled,
            Some("CANCELED") => OrderStatus::Cancelled,
            Some("REJECTED") => OrderStatus::Rejected,
            Some("EXPIRED") => OrderStatus::Expired,
            _ => OrderStatus::Rejected,
        };
        
        let original_quantity = response_data.get("origQty")
            .and_then(|q| q.as_str())
            .and_then(|q| q.parse().ok())
            .unwrap_or(0.0);
        
        let executed_quantity = response_data.get("executedQty")
            .and_then(|q| q.as_str())
            .and_then(|q| q.parse().ok())
            .unwrap_or(0.0);
        
        let price = response_data.get("price")
            .and_then(|p| p.as_str())
            .and_then(|p| p.parse().ok())
            .unwrap_or(0.0);
        
        let avg_price = response_data.get("avgPrice")
            .and_then(|p| p.as_str())
            .and_then(|p| p.parse().ok())
            .unwrap_or(0.0);
        
        let create_time = response_data.get("time")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        
        let update_time = response_data.get("updateTime")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        
        Ok(ExecutionReport {
            order_id: order_id.to_string(),
            client_order_id: response_data.get("clientOrderId")
                .and_then(|c| c.as_str())
                .map(|c| c.to_string()),
            exchange: "binance".to_string(),
            symbol,
            side,
            order_type,
            status,
            original_quantity,
            executed_quantity,
            remaining_quantity: original_quantity - executed_quantity,
            price,
            avg_price,
            last_executed_price: avg_price,
            last_executed_quantity: 0.0, // Binance API不直接提供
            commission: 0.0, // 需要額外查詢
            commission_asset: "BNB".to_string(),
            create_time: create_time * 1_000_000, // 轉換為納秒
            update_time: update_time * 1_000_000,
            transaction_time: update_time * 1_000_000,
            reject_reason: None,
        })
    }

    async fn get_open_orders(&mut self, symbol: Option<&str>) -> Result<Vec<ExecutionReport>, String> {
        let timestamp = chrono::Utc::now().timestamp_millis();
        
        let mut query_params = vec![
            ("timestamp", timestamp.to_string()),
        ];
        
        if let Some(sym) = symbol {
            query_params.push(("symbol", sym.to_string()));
        }
        
        let query_string = query_params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        
        let signature = self.generate_signature(&query_string);
        let final_query = format!("{}&signature={}", query_string, signature);
        
        let url = format!("{}/api/v3/openOrders?{}", self.config.rest_base_url, final_query);
        
        let response = self.http_client
            .get(&url)
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send()
            .await
            .map_err(|e| format!("Get open orders request failed: {}", e))?;
        
        let response_data: Vec<Value> = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse open orders response: {}", e))?;
        
        let mut execution_reports = Vec::new();
        
        for order_data in response_data {
            let order_id = order_data.get("orderId")
                .and_then(|id| id.as_u64())
                .ok_or("Missing order ID")?
                .to_string();
            
            // 重用get_order的解析邏輯
            if let Ok(report) = self.parse_order_to_execution_report(&order_data) {
                execution_reports.push(report);
            }
        }
        
        Ok(execution_reports)
    }

    async fn get_execution_reports(&self) -> Result<mpsc::UnboundedReceiver<ExecutionReport>, String> {
        let (sender, receiver) = mpsc::unbounded_channel();
        *self.execution_report_sender.lock().await = Some(sender);
        Ok(receiver)
    }

    async fn get_balance(&mut self) -> Result<HashMap<String, f64>, String> {
        let timestamp = chrono::Utc::now().timestamp_millis();
        
        let query_params = vec![
            ("timestamp", timestamp.to_string()),
        ];
        
        let query_string = query_params
            .iter()
            .map(|(k, v)| format!("{}={}", k, urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        
        let signature = self.generate_signature(&query_string);
        let final_query = format!("{}&signature={}", query_string, signature);
        
        let url = format!("{}/api/v3/account?{}", self.config.rest_base_url, final_query);
        
        let response = self.http_client
            .get(&url)
            .header("X-MBX-APIKEY", &self.config.api_key)
            .send()
            .await
            .map_err(|e| format!("Get balance request failed: {}", e))?;
        
        let response_data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse balance response: {}", e))?;
        
        let mut balances = HashMap::new();
        
        if let Some(balance_data) = response_data.get("balances").and_then(|b| b.as_array()) {
            for balance in balance_data {
                if let (Some(asset), Some(free_str)) = (
                    balance.get("asset").and_then(|a| a.as_str()),
                    balance.get("free").and_then(|f| f.as_str()),
                ) {
                    if let Ok(free_amount) = free_str.parse::<f64>() {
                        if free_amount > 0.0 {
                            balances.insert(asset.to_string(), free_amount);
                        }
                    }
                }
            }
        }
        
        Ok(balances)
    }

    async fn get_positions(&mut self) -> Result<Vec<Position>, String> {
        // Binance現貨交易不支持持倉概念，這裡返回基於餘額的"持倉"
        let balances = self.get_balance().await?;
        let mut positions = Vec::new();
        
        for (asset, balance) in balances {
            if balance > 0.0 {
                positions.push(Position {
                    symbol: asset.clone(),
                    side: PositionSide::Long, // 現貨只有多頭
                    size: balance,
                    entry_price: 0.0, // 現貨無入場價格概念
                    mark_price: 0.0,
                    unrealized_pnl: 0.0,
                    realized_pnl: 0.0,
                    margin_used: 0.0, // 現貨無保證金
                    timestamp: chrono::Utc::now().timestamp_nanos() as u64,
                });
            }
        }
        
        Ok(positions)
    }
}

#[async_trait]
impl Exchange for BinanceExchange {
    fn name(&self) -> &str {
        "binance"
    }

    async fn get_info(&self) -> ExchangeInfo {
        ExchangeInfo {
            name: "binance".to_string(),
            connection_status: self.connection_status.read().await.clone(),
            market_data_status: self.market_data_status.read().await.clone(),
            trading_status: self.trading_status.read().await.clone(),
            supported_symbols: self.get_symbols().await.unwrap_or_default(),
            latency_ms: *self.latency_ms.read().await,
            last_heartbeat: *self.last_heartbeat.read().await,
            error_count: *self.error_count.read().await,
        }
    }

    async fn initialize(&mut self) -> Result<(), String> {
        info!("Initializing Binance exchange");
        
        self.connect_public().await?;
        
        if !self.config.api_key.is_empty() {
            self.connect_private().await?;
        }
        
        info!("Binance exchange initialized successfully");
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), String> {
        info!("Shutting down Binance exchange");
        
        self.disconnect_public().await?;
        self.disconnect_private().await?;
        
        info!("Binance exchange shut down successfully");
        Ok(())
    }

    async fn is_ready_for_trading(&self) -> bool {
        *self.connection_status.read().await == ConnectionStatus::Connected &&
        *self.trading_status.read().await == TradingStatus::Enabled
    }

    async fn get_fees(&self, _symbol: &str) -> Result<(f64, f64), String> {
        // Binance標準費率
        Ok((0.001, 0.001)) // (maker, taker)
    }

    async fn get_trading_rules(&self, symbol: &str) -> Result<TradingRules, String> {
        Ok(TradingRules {
            symbol: symbol.to_string(),
            min_quantity: 0.00001,
            max_quantity: 9000000.0,
            quantity_precision: 8,
            price_precision: 8,
            min_notional: 10.0,
            tick_size: 0.01,
            step_size: 0.00001,
        })
    }

    fn normalize_symbol(&self, symbol: &str) -> String {
        symbol.to_uppercase()
    }

    fn denormalize_symbol(&self, symbol: &str) -> String {
        symbol.to_uppercase()
    }
}

impl BinanceExchange {
    /// 輔助方法：將訂單數據解析為ExecutionReport
    fn parse_order_to_execution_report(&self, order_data: &Value) -> Result<ExecutionReport, String> {
        let order_id = order_data.get("orderId")
            .and_then(|id| id.as_u64())
            .ok_or("Missing order ID")?
            .to_string();
        
        let symbol = order_data.get("symbol")
            .and_then(|s| s.as_str())
            .ok_or("Missing symbol")?
            .to_string();
        
        let side = match order_data.get("side").and_then(|s| s.as_str()) {
            Some("BUY") => OrderSide::Buy,
            Some("SELL") => OrderSide::Sell,
            _ => return Err("Invalid order side".to_string()),
        };
        
        let order_type = match order_data.get("type").and_then(|t| t.as_str()) {
            Some("MARKET") => OrderType::Market,
            Some("LIMIT") => OrderType::Limit,
            Some("STOP_LOSS") => OrderType::StopLoss,
            Some("STOP_LOSS_LIMIT") => OrderType::StopLossLimit,
            _ => OrderType::Limit,
        };
        
        let status = match order_data.get("status").and_then(|s| s.as_str()) {
            Some("NEW") => OrderStatus::New,
            Some("PARTIALLY_FILLED") => OrderStatus::PartiallyFilled,
            Some("FILLED") => OrderStatus::Filled,
            Some("CANCELED") => OrderStatus::Cancelled,
            Some("REJECTED") => OrderStatus::Rejected,
            Some("EXPIRED") => OrderStatus::Expired,
            _ => OrderStatus::Rejected,
        };
        
        let original_quantity = order_data.get("origQty")
            .and_then(|q| q.as_str())
            .and_then(|q| q.parse().ok())
            .unwrap_or(0.0);
        
        let executed_quantity = order_data.get("executedQty")
            .and_then(|q| q.as_str())
            .and_then(|q| q.parse().ok())
            .unwrap_or(0.0);
        
        let price = order_data.get("price")
            .and_then(|p| p.as_str())
            .and_then(|p| p.parse().ok())
            .unwrap_or(0.0);
        
        let create_time = order_data.get("time")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        
        let update_time = order_data.get("updateTime")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        
        Ok(ExecutionReport {
            order_id,
            client_order_id: order_data.get("clientOrderId")
                .and_then(|c| c.as_str())
                .map(|c| c.to_string()),
            exchange: "binance".to_string(),
            symbol,
            side,
            order_type,
            status,
            original_quantity,
            executed_quantity,
            remaining_quantity: original_quantity - executed_quantity,
            price,
            avg_price: 0.0, // 需要計算或從API獲取
            last_executed_price: 0.0,
            last_executed_quantity: 0.0,
            commission: 0.0,
            commission_asset: "BNB".to_string(),
            create_time: create_time * 1_000_000,
            update_time: update_time * 1_000_000,
            transaction_time: update_time * 1_000_000,
            reject_reason: None,
        })
    }
}

impl Clone for BinanceExchange {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            http_client: self.http_client.clone(),
            
            // 正確克隆所有共享狀態的Arc引用
            public_ws_writer: self.public_ws_writer.clone(),
            market_event_sender: self.market_event_sender.clone(),
            subscribed_symbols: self.subscribed_symbols.clone(),
            
            private_ws_writer: self.private_ws_writer.clone(),
            execution_report_sender: self.execution_report_sender.clone(),
            
            connection_status: self.connection_status.clone(),
            market_data_status: self.market_data_status.clone(),
            trading_status: self.trading_status.clone(),
            
            latency_ms: self.latency_ms.clone(),
            last_heartbeat: self.last_heartbeat.clone(),
            error_count: self.error_count.clone(),
        }
    }
}