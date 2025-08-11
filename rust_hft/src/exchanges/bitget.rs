//! Bitget交易所完整實現

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
use base64;
use tracing::{info, warn, error, debug};

use super::exchange_trait::*;
use super::message_types::*;
use crate::core::types::*;

type HmacSha256 = Hmac<Sha256>;

/// Bitget交易所配置
#[derive(Debug, Clone)]
pub struct BitgetConfig {
    pub api_key: String,
    pub secret_key: String,
    pub passphrase: String,
    pub sandbox: bool,
    pub ws_public_url: String,
    pub ws_private_url: String,
    pub rest_base_url: String,
}

impl Default for BitgetConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            passphrase: String::new(),
            sandbox: false,
            ws_public_url: "wss://ws.bitget.com/spot/v1/stream".to_string(),
            ws_private_url: "wss://ws.bitget.com/spot/v1/stream".to_string(),
            rest_base_url: "https://api.bitget.com".to_string(),
        }
    }
}

/// Bitget交易所實現
pub struct BitgetExchange {
    config: BitgetConfig,
    http_client: Client,
    
    // 市場數據相關 - 修復：分離讀寫端，保留寫端引用
    public_ws_writer: Arc<Mutex<Option<futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>>>>,
    market_event_sender: Arc<Mutex<Option<mpsc::Sender<MarketEvent>>>>,
    subscribed_symbols: Arc<RwLock<HashMap<String, Vec<String>>>>, // symbol -> channels
    
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

impl BitgetExchange {
    pub fn new() -> Self {
        Self::with_config(BitgetConfig::default())
    }

    pub fn with_config(config: BitgetConfig) -> Self {
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
    fn generate_signature(&self, timestamp: u64, method: &str, request_path: &str, body: &str) -> String {
        let message = format!("{}{}{}{}", timestamp, method.to_uppercase(), request_path, body);
        let mut mac = HmacSha256::new_from_slice(self.config.secret_key.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(message.as_bytes());
        base64::encode(mac.finalize().into_bytes())
    }

    /// 構建請求頭
    fn build_headers(&self, timestamp: u64, method: &str, request_path: &str, body: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        
        let signature = self.generate_signature(timestamp, method, request_path, body);
        
        headers.insert("ACCESS-KEY", self.config.api_key.parse().unwrap());
        headers.insert("ACCESS-SIGN", signature.parse().unwrap());
        headers.insert("ACCESS-TIMESTAMP", timestamp.to_string().parse().unwrap());
        headers.insert("ACCESS-PASSPHRASE", self.config.passphrase.parse().unwrap());
        headers.insert("Content-Type", "application/json".parse().unwrap());
        
        headers
    }

    /// 解析訂單簿消息
    fn parse_orderbook_message(&self, data: &Value) -> Option<MarketEvent> {
        let symbol = data.get("arg")?.get("instId")?.as_str()?.to_string();
        let orderbook_data = data.get("data")?.as_array()?.first()?;
        
        let bids: Vec<OrderBookLevel> = orderbook_data
            .get("bids")?
            .as_array()?
            .iter()
            .filter_map(|bid| {
                let array = bid.as_array()?;
                Some(OrderBookLevel {
                    price: array.get(0)?.as_str()?.parse().ok()?,
                    quantity: array.get(1)?.as_str()?.parse().ok()?,
                    order_count: array.get(2)?.as_str()?.parse().ok()?,
                })
            })
            .collect();

        let asks: Vec<OrderBookLevel> = orderbook_data
            .get("asks")?
            .as_array()?
            .iter()
            .filter_map(|ask| {
                let array = ask.as_array()?;
                Some(OrderBookLevel {
                    price: array.get(0)?.as_str()?.parse().ok()?,
                    quantity: array.get(1)?.as_str()?.parse().ok()?,
                    order_count: array.get(2)?.as_str()?.parse().ok()?,
                })
            })
            .collect();

        let timestamp = orderbook_data.get("ts")?.as_str()?.parse().ok()?;
        let sequence = timestamp; // Bitget使用時間戳作為序列號
        let is_snapshot = data.get("action")?.as_str() == Some("snapshot");

        Some(MarketEvent::OrderBookUpdate {
            symbol,
            exchange: "bitget".to_string(),
            bids,
            asks,
            timestamp,
            sequence,
            is_snapshot,
        })
    }

    /// 解析成交消息
    fn parse_trade_message(&self, data: &Value) -> Option<MarketEvent> {
        let symbol = data.get("arg")?.get("instId")?.as_str()?.to_string();
        let trade_data = data.get("data")?.as_array()?.first()?;
        
        let trade_id = trade_data.get("tradeId")?.as_str()?.to_string();
        let price = trade_data.get("px")?.as_str()?.parse().ok()?;
        let quantity = trade_data.get("sz")?.as_str()?.parse().ok()?;
        let side = match trade_data.get("side")?.as_str()? {
            "buy" => OrderSide::Buy,
            "sell" => OrderSide::Sell,
            _ => return None,
        };
        let timestamp = trade_data.get("ts")?.as_str()?.parse().ok()?;

        Some(MarketEvent::Trade {
            symbol,
            exchange: "bitget".to_string(),
            trade_id,
            price,
            quantity,
            side,
            timestamp,
            buyer_maker: false, // Bitget不提供此字段，默認為false
        })
    }

    /// 處理WebSocket消息
    async fn handle_ws_message(&self, message: Message) {
        if let Ok(text) = message.to_text() {
            if let Ok(data) = serde_json::from_str::<Value>(text) {
                // 檢查是否是訂單簿更新
                if let Some(event) = self.parse_orderbook_message(&data) {
                    if let Some(sender) = self.market_event_sender.lock().await.as_ref() {
                        // P0 修復：使用 try_send 實現背壓，通道滿時丟棄新消息
                        if let Err(e) = sender.try_send(event) {
                            warn!("Market event channel full, dropping orderbook update: {:?}", e);
                        }
                    }
                }
                // 檢查是否是成交數據
                else if let Some(event) = self.parse_trade_message(&data) {
                    if let Some(sender) = self.market_event_sender.lock().await.as_ref() {
                        // P0 修復：使用 try_send 實現背壓，通道滿時丟棄新消息
                        if let Err(e) = sender.try_send(event) {
                            warn!("Market event channel full, dropping trade update: {:?}", e);
                        }
                    }
                }
                // 處理心跳和其他消息
                else if data.get("event").is_some() {
                    debug!("Received WebSocket event: {}", text);
                }
            }
        }
    }

    /// 啟動WebSocket連接維護任務 - P1 修復：添加停止條件
    async fn start_ws_maintenance(&self) {
        let public_ws_writer = self.public_ws_writer.clone();
        let connection_status = self.connection_status.clone();
        let last_heartbeat = self.last_heartbeat.clone();
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            
            loop {
                interval.tick().await;
                
                // P1 修復：檢查連接狀態，如果斷開則停止心跳循環
                {
                    let status = connection_status.read().await;
                    match *status {
                        ConnectionStatus::Disconnected | ConnectionStatus::Error(_) => {
                            info!("Connection lost, stopping ping maintenance task");
                            break;
                        },
                        _ => {}
                    }
                }
                
                // 發送心跳 - 修復：使用分離的 writer
                if let Some(ws_writer) = public_ws_writer.lock().await.as_mut() {
                    let ping_msg = json!({"op": "ping"});
                    if let Err(e) = ws_writer.send(Message::Text(ping_msg.to_string())).await {
                        error!("Failed to send ping: {}", e);
                        *connection_status.write().await = ConnectionStatus::Error(format!("Ping failed: {}", e));
                        break; // 心跳失敗時停止循環
                    } else {
                        *last_heartbeat.write().await = chrono::Utc::now().timestamp_nanos() as u64;
                    }
                } else {
                    warn!("WebSocket writer not available, stopping ping maintenance");
                    break; // writer 不可用時停止循環
                }
            }
            debug!("Bitget ping maintenance task stopped");
        });
    }
}

#[async_trait]
impl MarketDataClient for BitgetExchange {
    async fn connect_public(&mut self) -> Result<(), String> {
        *self.connection_status.write().await = ConnectionStatus::Connecting;
        
        match connect_async(&self.config.ws_public_url).await {
            Ok((ws_stream, _)) => {
                // 修復：分離讀寫端，避免 writer 丟失
                let (ws_writer, mut ws_reader) = ws_stream.split();
                *self.public_ws_writer.lock().await = Some(ws_writer);
                
                *self.connection_status.write().await = ConnectionStatus::Connected;
                *self.market_data_status.write().await = MarketDataStatus::Active;
                
                // 啟動消息處理循環 - 修復：直接使用 reader，無需 take()
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
                                    error!("WebSocket error: {}", e);
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
                
                // 啟動維護任務
                self.start_ws_maintenance().await;
                
                info!("Connected to Bitget public WebSocket");
                Ok(())
            }
            Err(e) => {
                *self.connection_status.write().await = ConnectionStatus::Error(format!("Connection failed: {}", e));
                Err(format!("Failed to connect to Bitget WebSocket: {}", e))
            }
        }
    }

    async fn disconnect_public(&mut self) -> Result<(), String> {
        if let Some(mut ws_writer) = self.public_ws_writer.lock().await.take() {
            let _ = ws_writer.close().await;
        }
        
        *self.connection_status.write().await = ConnectionStatus::Disconnected;
        *self.market_data_status.write().await = MarketDataStatus::Inactive;
        
        info!("Disconnected from Bitget public WebSocket");
        Ok(())
    }

    async fn subscribe_orderbook(&mut self, symbol: &str, depth: u32) -> Result<(), String> {
        let channel = match depth {
            5 => "books5",
            15 => "books15",
            _ => "books", // 默認完整深度
        };
        
        let subscribe_msg = json!({
            "op": "subscribe",
            "args": [{
                "instType": "spot",
                "channel": channel,
                "instId": symbol
            }]
        });

        if let Some(ws_writer) = self.public_ws_writer.lock().await.as_mut() {
            ws_writer.send(Message::Text(subscribe_msg.to_string())).await
                .map_err(|e| format!("Failed to subscribe to orderbook: {}", e))?;
            
            // 記錄訂閱
            let mut subs = self.subscribed_symbols.write().await;
            subs.entry(symbol.to_string()).or_insert_with(Vec::new).push(channel.to_string());
            
            info!("Subscribed to {} orderbook for {}", channel, symbol);
            Ok(())
        } else {
            Err("WebSocket not connected".to_string())
        }
    }

    async fn unsubscribe_orderbook(&mut self, symbol: &str) -> Result<(), String> {
        let channels: Vec<String> = {
            let mut subs = self.subscribed_symbols.write().await;
            subs.remove(symbol).unwrap_or_default()
        };

        for channel in channels {
            let unsubscribe_msg = json!({
                "op": "unsubscribe",
                "args": [{
                    "instType": "spot",
                    "channel": channel,
                    "instId": symbol
                }]
            });

            if let Some(ws_writer) = self.public_ws_writer.lock().await.as_mut() {
                ws_writer.send(Message::Text(unsubscribe_msg.to_string())).await
                    .map_err(|e| format!("Failed to unsubscribe from orderbook: {}", e))?;
            }
        }

        info!("Unsubscribed from orderbook for {}", symbol);
        Ok(())
    }

    async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), String> {
        let subscribe_msg = json!({
            "op": "subscribe",
            "args": [{
                "instType": "spot",
                "channel": "trade",
                "instId": symbol
            }]
        });

        if let Some(ws_writer) = self.public_ws_writer.lock().await.as_mut() {
            ws_writer.send(Message::Text(subscribe_msg.to_string())).await
                .map_err(|e| format!("Failed to subscribe to trades: {}", e))?;
            
            // 記錄訂閱
            let mut subs = self.subscribed_symbols.write().await;
            subs.entry(symbol.to_string()).or_insert_with(Vec::new).push("trade".to_string());
            
            info!("Subscribed to trades for {}", symbol);
            Ok(())
        } else {
            Err("WebSocket not connected".to_string())
        }
    }

    async fn unsubscribe_trades(&mut self, symbol: &str) -> Result<(), String> {
        let unsubscribe_msg = json!({
            "op": "unsubscribe",
            "args": [{
                "instType": "spot",
                "channel": "trade",
                "instId": symbol
            }]
        });

        if let Some(ws_writer) = self.public_ws_writer.lock().await.as_mut() {
            ws_writer.send(Message::Text(unsubscribe_msg.to_string())).await
                .map_err(|e| format!("Failed to unsubscribe from trades: {}", e))?;
            
            info!("Unsubscribed from trades for {}", symbol);
            Ok(())
        } else {
            Err("WebSocket not connected".to_string())
        }
    }

    async fn subscribe_ticker(&mut self, symbol: &str) -> Result<(), String> {
        let subscribe_msg = json!({
            "op": "subscribe",
            "args": [{
                "instType": "spot",
                "channel": "ticker",
                "instId": symbol
            }]
        });

        if let Some(ws_writer) = self.public_ws_writer.lock().await.as_mut() {
            ws_writer.send(Message::Text(subscribe_msg.to_string())).await
                .map_err(|e| format!("Failed to subscribe to ticker: {}", e))?;
            
            info!("Subscribed to ticker for {}", symbol);
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
        let url = format!("{}/api/v2/spot/public/symbols", self.config.rest_base_url);
        
        let response = self.http_client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch symbols: {}", e))?;
            
        let data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse symbols response: {}", e))?;
        
        if let Some(symbols_data) = data.get("data").and_then(|d| d.as_array()) {
            let symbols: Vec<String> = symbols_data
                .iter()
                .filter_map(|s| s.get("symbol").and_then(|sym| sym.as_str()).map(|sym| sym.to_string()))
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

// 實現TradingClient的基礎方法，其他方法將在後續添加
#[async_trait]
impl TradingClient for BitgetExchange {
    async fn connect_private(&mut self) -> Result<(), String> {
        // TODO: 實現私有WebSocket連接
        *self.trading_status.write().await = TradingStatus::Enabled;
        Ok(())
    }

    async fn disconnect_private(&mut self) -> Result<(), String> {
        *self.trading_status.write().await = TradingStatus::Disabled;
        Ok(())
    }

    async fn place_order(&mut self, request: OrderRequest) -> Result<OrderResponse, String> {
        let timestamp = chrono::Utc::now().timestamp_millis() as u64;
        let request_path = "/api/v2/spot/trade/place-order";
        
        let body = json!({
            "symbol": request.symbol,
            "side": match request.side {
                OrderSide::Buy => "buy",
                OrderSide::Sell => "sell",
            },
            "orderType": match request.order_type {
                OrderType::Market => "market",
                OrderType::Limit => "limit",
                _ => return Err("Unsupported order type".to_string()),
            },
            "quantity": request.quantity.to_string(),
            "price": request.price.map(|p| p.to_string()),
            "clientOid": request.client_order_id,
            "timeInForce": match request.time_in_force {
                TimeInForce::GTC => "GTC",
                TimeInForce::IOC => "IOC",
                TimeInForce::FOK => "FOK",
                _ => "GTC",
            }
        });

        let body_str = body.to_string();
        let headers = self.build_headers(timestamp, "POST", request_path, &body_str);
        let url = format!("{}{}", self.config.rest_base_url, request_path);

        let response = self.http_client
            .post(&url)
            .headers(headers)
            .body(body_str)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        let response_data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        if let Some(code) = response_data.get("code").and_then(|c| c.as_str()) {
            if code == "00000" {
                if let Some(data) = response_data.get("data") {
                    let order_id = data.get("orderId")
                        .and_then(|id| id.as_str())
                        .unwrap_or("")
                        .to_string();
                    
                    return Ok(OrderResponse {
                        success: true,
                        order_id,
                        client_order_id: request.client_order_id,
                        status: OrderStatus::New,
                        error: None,
                        timestamp: chrono::Utc::now().timestamp_nanos() as u64,
                    });
                }
            }
        }

        let error_msg = response_data.get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error")
            .to_string();

        Ok(OrderResponse {
            success: false,
            order_id: String::new(),
            client_order_id: request.client_order_id,
            status: OrderStatus::Rejected,
            error: Some(error_msg),
            timestamp: chrono::Utc::now().timestamp_nanos() as u64,
        })
    }

    async fn cancel_order(&mut self, request: CancelRequest) -> Result<OrderResponse, String> {
        let timestamp = chrono::Utc::now().timestamp_millis() as u64;
        let request_path = "/api/v2/spot/trade/cancel-order";
        
        let body = if let Some(ref order_id) = request.order_id {
            json!({
                "symbol": request.symbol,
                "orderId": order_id,
            })
        } else if let Some(ref client_order_id) = request.client_order_id {
            json!({
                "symbol": request.symbol,
                "clientOid": client_order_id,
            })
        } else {
            return Err("Either order_id or client_order_id must be provided".to_string());
        };
        
        let body_str = body.to_string();
        let headers = self.build_headers(timestamp, "POST", request_path, &body_str);
        let url = format!("{}{}", self.config.rest_base_url, request_path);
        
        let response = self.http_client
            .post(&url)
            .headers(headers)
            .body(body_str)
            .send()
            .await
            .map_err(|e| format!("Cancel request failed: {}", e))?;
        
        let response_data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse cancel response: {}", e))?;
        
        if let Some(code) = response_data.get("code").and_then(|c| c.as_str()) {
            if code == "00000" {
                return Ok(OrderResponse {
                    success: true,
                    order_id: request.order_id.unwrap_or_default(),
                    client_order_id: request.client_order_id.unwrap_or_default(),
                    status: OrderStatus::Cancelled,
                    error: None,
                    timestamp: chrono::Utc::now().timestamp_nanos() as u64,
                });
            }
        }
        
        let error_msg = response_data.get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown cancel error")
            .to_string();
        
        Ok(OrderResponse {
            success: false,
            order_id: String::new(),
            client_order_id: String::new(),
            status: OrderStatus::Rejected,
            error: Some(error_msg),
            timestamp: chrono::Utc::now().timestamp_nanos() as u64,
        })
    }

    async fn amend_order(&mut self, request: AmendRequest) -> Result<OrderResponse, String> {
        let timestamp = chrono::Utc::now().timestamp_millis() as u64;
        let request_path = "/api/v2/spot/trade/modify-order";
        
        let mut body_map = serde_json::Map::new();
        body_map.insert("orderId".to_string(), json!(request.order_id));
        
        if let Some(quantity) = request.quantity {
            body_map.insert("quantity".to_string(), json!(quantity.to_string()));
        }
        
        if let Some(price) = request.price {
            body_map.insert("price".to_string(), json!(price.to_string()));
        }
        
        let body = Value::Object(body_map);
        let body_str = body.to_string();
        let headers = self.build_headers(timestamp, "POST", request_path, &body_str);
        let url = format!("{}{}", self.config.rest_base_url, request_path);
        
        let response = self.http_client
            .post(&url)
            .headers(headers)
            .body(body_str)
            .send()
            .await
            .map_err(|e| format!("Amend request failed: {}", e))?;
        
        let response_data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse amend response: {}", e))?;
        
        if let Some(code) = response_data.get("code").and_then(|c| c.as_str()) {
            if code == "00000" {
                return Ok(OrderResponse {
                    success: true,
                    order_id: request.order_id.clone(),
                    client_order_id: String::new(),
                    status: OrderStatus::New, // 修改後的訂單狀態
                    error: None,
                    timestamp: chrono::Utc::now().timestamp_nanos() as u64,
                });
            }
        }
        
        let error_msg = response_data.get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown amend error")
            .to_string();
        
        Ok(OrderResponse {
            success: false,
            order_id: request.order_id,
            client_order_id: String::new(),
            status: OrderStatus::Rejected,
            error: Some(error_msg),
            timestamp: chrono::Utc::now().timestamp_nanos() as u64,
        })
    }

    async fn get_order(&mut self, order_id: &str) -> Result<ExecutionReport, String> {
        let timestamp = chrono::Utc::now().timestamp_millis() as u64;
        let request_path = "/api/v2/spot/trade/orderInfo";
        
        let query_params = format!("orderId={}", order_id);
        let headers = self.build_headers(timestamp, "GET", request_path, &query_params);
        let url = format!("{}{}/{}", self.config.rest_base_url, request_path, order_id);
        
        let response = self.http_client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("Get order request failed: {}", e))?;
        
        let response_data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse get order response: {}", e))?;
        
        if let Some(code) = response_data.get("code").and_then(|c| c.as_str()) {
            if code == "00000" {
                if let Some(data) = response_data.get("data").and_then(|d| d.as_array()) {
                    if let Some(order_data) = data.first() {
                        return self.parse_order_to_execution_report(order_data);
                    }
                }
            }
        }
        
        Err("Failed to get order information".to_string())
    }

    async fn get_open_orders(&mut self, symbol: Option<&str>) -> Result<Vec<ExecutionReport>, String> {
        let timestamp = chrono::Utc::now().timestamp_millis() as u64;
        let request_path = "/api/v2/spot/trade/unfilled-orders";
        
        let query_params = if let Some(sym) = symbol {
            format!("symbol={}", sym)
        } else {
            String::new()
        };
        
        let headers = self.build_headers(timestamp, "GET", request_path, &query_params);
        let url = if query_params.is_empty() {
            format!("{}{}", self.config.rest_base_url, request_path)
        } else {
            format!("{}{}?{}", self.config.rest_base_url, request_path, query_params)
        };
        
        let response = self.http_client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("Get open orders request failed: {}", e))?;
        
        let response_data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse open orders response: {}", e))?;
        
        let mut execution_reports = Vec::new();
        
        if let Some(code) = response_data.get("code").and_then(|c| c.as_str()) {
            if code == "00000" {
                if let Some(data) = response_data.get("data").and_then(|d| d.as_array()) {
                    for order_data in data {
                        if let Ok(report) = self.parse_order_to_execution_report(order_data) {
                            execution_reports.push(report);
                        }
                    }
                }
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
        let timestamp = chrono::Utc::now().timestamp_millis() as u64;
        let request_path = "/api/v2/spot/account/assets";
        
        let headers = self.build_headers(timestamp, "GET", request_path, "");
        let url = format!("{}{}", self.config.rest_base_url, request_path);
        
        let response = self.http_client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| format!("Get balance request failed: {}", e))?;
        
        let response_data: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse balance response: {}", e))?;
        
        let mut balances = HashMap::new();
        
        if let Some(code) = response_data.get("code").and_then(|c| c.as_str()) {
            if code == "00000" {
                if let Some(data) = response_data.get("data").and_then(|d| d.as_array()) {
                    for balance in data {
                        if let (Some(coin), Some(available_str)) = (
                            balance.get("coin").and_then(|c| c.as_str()),
                            balance.get("available").and_then(|a| a.as_str()),
                        ) {
                            if let Ok(available_amount) = available_str.parse::<f64>() {
                                if available_amount > 0.0 {
                                    balances.insert(coin.to_string(), available_amount);
                                }
                            }
                        }
                    }
                }
            }
        }
        
        Ok(balances)
    }

    async fn get_positions(&mut self) -> Result<Vec<Position>, String> {
        // Bitget現貨交易不支持持倉概念，這裡返回基於餘額的"持倉"
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
impl Exchange for BitgetExchange {
    fn name(&self) -> &str {
        "bitget"
    }

    async fn get_info(&self) -> ExchangeInfo {
        ExchangeInfo {
            name: "bitget".to_string(),
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
        info!("Initializing Bitget exchange");
        
        // 連接公共數據流
        self.connect_public().await?;
        
        // 如果有API密鑰，則連接私有數據流
        if !self.config.api_key.is_empty() {
            self.connect_private().await?;
        }
        
        info!("Bitget exchange initialized successfully");
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), String> {
        info!("Shutting down Bitget exchange");
        
        self.disconnect_public().await?;
        self.disconnect_private().await?;
        
        info!("Bitget exchange shut down successfully");
        Ok(())
    }

    async fn is_ready_for_trading(&self) -> bool {
        *self.connection_status.read().await == ConnectionStatus::Connected &&
        *self.trading_status.read().await == TradingStatus::Enabled
    }

    async fn get_fees(&self, _symbol: &str) -> Result<(f64, f64), String> {
        // Bitget標準費率
        Ok((0.001, 0.001)) // (maker, taker)
    }

    async fn get_trading_rules(&self, symbol: &str) -> Result<TradingRules, String> {
        // TODO: 從API獲取實際的交易規則
        Ok(TradingRules {
            symbol: symbol.to_string(),
            min_quantity: 0.0001,
            max_quantity: 1000000.0,
            quantity_precision: 8,
            price_precision: 8,
            min_notional: 5.0,
            tick_size: 0.01,
            step_size: 0.0001,
        })
    }

    fn normalize_symbol(&self, symbol: &str) -> String {
        // Bitget使用標準格式，無需轉換
        symbol.to_uppercase()
    }

    fn denormalize_symbol(&self, symbol: &str) -> String {
        // Bitget使用標準格式，無需轉換
        symbol.to_uppercase()
    }
}

impl BitgetExchange {
    /// 輔助方法：將訂單數據解析為ExecutionReport
    fn parse_order_to_execution_report(&self, order_data: &Value) -> Result<ExecutionReport, String> {
        let order_id = order_data.get("orderId")
            .and_then(|id| id.as_str())
            .ok_or("Missing order ID")?
            .to_string();
        
        let symbol = order_data.get("symbol")
            .and_then(|s| s.as_str())
            .ok_or("Missing symbol")?
            .to_string();
        
        let side = match order_data.get("side").and_then(|s| s.as_str()) {
            Some("buy") => OrderSide::Buy,
            Some("sell") => OrderSide::Sell,
            _ => return Err("Invalid order side".to_string()),
        };
        
        let order_type = match order_data.get("orderType").and_then(|t| t.as_str()) {
            Some("market") => OrderType::Market,
            Some("limit") => OrderType::Limit,
            _ => OrderType::Limit,
        };
        
        let status = match order_data.get("status").and_then(|s| s.as_str()) {
            Some("new") => OrderStatus::New,
            Some("partial_fill") => OrderStatus::PartiallyFilled,
            Some("full_fill") => OrderStatus::Filled,
            Some("cancelled") => OrderStatus::Cancelled,
            Some("rejected") => OrderStatus::Rejected,
            _ => OrderStatus::Rejected,
        };
        
        let original_quantity = order_data.get("quantity")
            .and_then(|q| q.as_str())
            .and_then(|q| q.parse().ok())
            .unwrap_or(0.0);
        
        let executed_quantity = order_data.get("fillQuantity")
            .and_then(|q| q.as_str())
            .and_then(|q| q.parse().ok())
            .unwrap_or(0.0);
        
        let price = order_data.get("price")
            .and_then(|p| p.as_str())
            .and_then(|p| p.parse().ok())
            .unwrap_or(0.0);
        
        let avg_price = order_data.get("averageFillPrice")
            .and_then(|p| p.as_str())
            .and_then(|p| p.parse().ok())
            .unwrap_or(0.0);
        
        let create_time = order_data.get("cTime")
            .and_then(|t| t.as_str())
            .and_then(|t| t.parse().ok())
            .unwrap_or(0u64);
        
        let update_time = order_data.get("uTime")
            .and_then(|t| t.as_str())
            .and_then(|t| t.parse().ok())
            .unwrap_or(0u64);
        
        Ok(ExecutionReport {
            order_id,
            client_order_id: order_data.get("clientOid")
                .and_then(|c| c.as_str())
                .map(|c| c.to_string()),
            exchange: "bitget".to_string(),
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
            last_executed_quantity: 0.0, // Bitget API不直接提供
            commission: 0.0, // 需要額外查詢
            commission_asset: "BGB".to_string(),
            create_time: create_time * 1_000_000, // 轉換為納秒
            update_time: update_time * 1_000_000,
            transaction_time: update_time * 1_000_000,
            reject_reason: None,
        })
    }
}

impl Clone for BitgetExchange {
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