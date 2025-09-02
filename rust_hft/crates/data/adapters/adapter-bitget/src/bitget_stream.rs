//! Bitget WebSocket 行情流實現
use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{info, warn, error, debug};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use std::collections::{BTreeMap, HashMap};

use hft_core::*;
use ports::*;
use integration::ws::{WsClientConfig, ReconnectingWsClient, MessageHandler};

/// Bitget WebSocket 消息結構
#[derive(Debug, Clone, Deserialize)]
pub struct BitgetWsMessage {
    pub action: Option<String>,
    pub arg: Option<BitgetSubscriptionArg>,
    pub data: Option<serde_json::Value>,
    /// v1 為數字，v2 文檔為字串，使用 Value 兼容
    pub code: Option<serde_json::Value>,
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
    pub asks: Vec<Vec<String>>, // [px, sz] 或 [px, sz, cnt]
    #[serde(rename = "bids")]
    pub bids: Vec<Vec<String>>, // [px, sz] 或 [px, sz, cnt]
    #[serde(rename = "checksum")]
    pub checksum: Option<i64>,
    #[serde(rename = "ts")]
    pub ts: String,
}

/// Bitget 成交數據結構
#[derive(Debug, Clone, Deserialize)]
pub struct BitgetTradeData {
    #[serde(rename = "instId")]
    pub inst_id: Option<String>,
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
    use_incremental_books: bool,
}

impl BitgetMarketStream {
    pub fn new() -> Self {
        Self {
            ws_client: None,
            event_sender: None,
            subscribed_symbols: Vec::new(),
            use_incremental_books: false,
        }
    }

    pub fn new_with_incremental(use_incremental_books: bool) -> Self {
        Self { use_incremental_books, ..Self::new() }
    }

    fn create_ws_config() -> WsClientConfig {
        // 放寬訊息/幀上限以容納多品種 LOB/Trade 聚合
        WsClientConfig {
            url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            heartbeat_interval: std::time::Duration::from_secs(30),
            reconnect_interval: std::time::Duration::from_secs(5),
            max_reconnect_attempts: 10,
            tcp_nodelay: true,           // HFT 必須啟用
            disable_compression: true,   // HFT 必須禁用壓縮
            max_message_size: 512 * 1024, // 512KB 訊息上限
            max_frame_size: 256 * 1024,   // 256KB 幀上限
        }
    }

    fn parse_orderbook_data(&self, data: &BitgetOrderBookData, symbol: &Symbol) -> Result<MarketSnapshot, HftError> {
        let ts_ms = data.ts.parse::<u64>()
            .map_err(|_| HftError::Generic { message: "Invalid timestamp".to_string() })?;
        let timestamp = ts_ms.saturating_mul(1000); // 轉為微秒

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
            source_venue: Some(VenueId::BITGET),
        })
    }

    fn parse_trade_data(&self, data: &BitgetTradeData, fallback_inst_id: &str) -> Result<Trade, HftError> {
        let ts_ms = data.ts.parse::<u64>()
            .map_err(|_| HftError::Generic { message: "Invalid timestamp".to_string() })?;
        let timestamp = ts_ms.saturating_mul(1000); // 轉為微秒

        let price = Price::from_str(&data.price)
            .map_err(|_| HftError::Generic { message: format!("Invalid trade price: {}", data.price) })?;
        
        let quantity = Quantity::from_str(&data.size)
            .map_err(|_| HftError::Generic { message: format!("Invalid trade size: {}", data.size) })?;

        let side = match data.side.as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => return Err(HftError::Generic { message: format!("Invalid trade side: {}", data.side) }),
        };

        let symbol = Symbol(data.inst_id.clone().unwrap_or_else(|| fallback_inst_id.to_string()));
        Ok(Trade {
            symbol,
            timestamp,
            price,
            quantity,
            side,
            trade_id: data.trade_id.clone(),
            source_venue: Some(VenueId::BITGET),
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
        let handler = BitgetMessageHandler::new(tx.clone(), symbols.clone(), self.use_incremental_books);

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
    use_incremental_books: bool,
    ob_state: HashMap<String, OrderBookState>,
}

impl BitgetMessageHandler {
    fn new(event_sender: mpsc::UnboundedSender<MarketEvent>, subscribed_symbols: Vec<Symbol>, use_incremental_books: bool) -> Self {
        Self {
            event_sender,
            subscribed_symbols,
            subscription_sent: false,
            pending_subscriptions: None,
            use_incremental_books,
            ob_state: HashMap::new(),
        }
    }

}

impl MessageHandler for BitgetMessageHandler {
    fn handle_message(&mut self, message: String) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        debug!("收到 Bitget 消息: {}", message);
        
        // 解析 WebSocket 消息
        let ws_msg: BitgetWsMessage = serde_json::from_str(&message)?;
        
        // 優先依據 arg.channel 分流處理
        if let Some(arg) = &ws_msg.arg {
            if let Some(data) = &ws_msg.data {
                match arg.channel.as_str() {
                    c if c.starts_with("books") || c == "depth" => {
                        self.handle_orderbook_data_with_action(data, &ws_msg.arg, ws_msg.action.as_deref())?;
                    }
                    "trade" | "trades" => {
                        self.handle_trade_data(data, &ws_msg.arg)?;
                    }
                    other => {
                        debug!("未處理的頻道: {}", other);
                    }
                }
            }
        } else if let Some(code) = &ws_msg.code {
            // 兼容數字/字串的錯誤碼格式
            let is_ok = match code {
                serde_json::Value::Number(n) => n.as_i64() == Some(0),
                serde_json::Value::String(s) => s == "0",
                _ => false,
            };
            if is_ok {
                info!("訂閱成功");
            } else {
                warn!("收到錯誤代碼: {:?}, 消息: {:?}", code, ws_msg.msg);
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

        // 採用明確且兼容的訂閱參數：
        // - 增量模式：books（先 snapshot 後 update）
        // - 否則：books15 快照
        let lob_channel = if self.use_incremental_books { "books" } else { "books15" };
        let lob_args: Vec<BitgetSubscriptionArg> = self.subscribed_symbols.iter().map(|s| BitgetSubscriptionArg {
            inst_type: "SPOT".to_string(),
            channel: lob_channel.to_string(),
            inst_id: s.0.clone(),
        }).collect();
        let lob_req = BitgetSubscriptionRequest { op: "subscribe".to_string(), args: lob_args };
        let lob_msg = serde_json::to_string(&lob_req)?;
        client.send_message(&lob_msg).await?;
        info!("已發送 LOB 訂閱: instType=SPOT, channel={}, symbols={}", lob_channel, self.subscribed_symbols.len());

        let trade_args: Vec<BitgetSubscriptionArg> = self.subscribed_symbols.iter().map(|s| BitgetSubscriptionArg {
            inst_type: "SPOT".to_string(),
            channel: "trade".to_string(),
            inst_id: s.0.clone(),
        }).collect();
        let trade_req = BitgetSubscriptionRequest { op: "subscribe".to_string(), args: trade_args };
        let trade_msg = serde_json::to_string(&trade_req)?;
        client.send_message(&trade_msg).await?;
        info!("已發送成交訂閱: instType=SPOT, channel=trade, symbols={}", self.subscribed_symbols.len());

        self.subscription_sent = true;
        info!("已發送訂閱參數，等待服務端回覆");
        
        Ok(())
        })
    }
}

impl BitgetMessageHandler {
    // LOB 狀態（僅增量模式）
    fn get_state_mut(&mut self, sym: &str) -> &mut OrderBookState {
        self.ob_state.entry(sym.to_string()).or_insert_with(OrderBookState::default)
    }

    fn handle_orderbook_data(&mut self, data: &serde_json::Value, arg: &Option<BitgetSubscriptionArg>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(arg) = arg {
            if arg.channel == "depth" || arg.channel.starts_with("books") {
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

    fn handle_orderbook_data_with_action(&mut self, data: &serde_json::Value, arg: &Option<BitgetSubscriptionArg>, action: Option<&str>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.use_incremental_books {
            return self.handle_orderbook_data(data, arg);
        }
        let Some(arg) = arg else { return Ok(()); };
        if !(arg.channel == "books" || arg.channel.starts_with("books")) { return Ok(()); }

        let entries: Vec<BitgetOrderBookData> = serde_json::from_value(data.clone())?;
        let sym = arg.inst_id.as_str();
        let state = self.get_state_mut(sym);

        let is_snapshot = matches!(action, Some("snapshot"));

        for ob in entries.iter() {
            if is_snapshot { state.apply_snapshot(ob)?; } else { state.apply_update(ob)?; }
        }

        // 生成快照事件（從狀態構建）
        let snapshot = state.to_snapshot(Symbol(sym.to_string()), entries.last().map(|e| &e.ts))?;
        let _ = self.event_sender.send(MarketEvent::Snapshot(snapshot));
        Ok(())
    }

    fn handle_trade_data(&mut self, data: &serde_json::Value, arg: &Option<BitgetSubscriptionArg>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let fallback_inst = arg.as_ref().map(|a| a.inst_id.as_str()).unwrap_or("");

        // 通用解析：兼容 px/price、sz/size 以及 ts 字符串/數字
        let items: Vec<serde_json::Value> = if let Some(arr) = data.as_array() {
            arr.clone()
        } else if data.is_object() {
            vec![data.clone()]
        } else {
            return Ok(());
        };

        for item in items {
            // 解析 symbol
            let symbol = item.get("instId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| fallback_inst.to_string());
            if symbol.is_empty() { continue; }

            // 解析時間戳（ms → μs）
            let ts_ms_opt = match item.get("ts") {
                Some(v) if v.is_string() => v.as_str().and_then(|s| s.parse::<u64>().ok()),
                Some(v) if v.is_u64() => v.as_u64(),
                Some(v) if v.is_number() => v.as_f64().map(|f| f as u64),
                _ => None,
            };
            let ts_us = ts_ms_opt.unwrap_or(0).saturating_mul(1000);
            if ts_us == 0 { continue; }

            // 解析價格
            let price_str = item.get("px").and_then(|v| v.as_str())
                .or_else(|| item.get("price").and_then(|v| v.as_str()));
            let price_num = item.get("px").and_then(|v| v.as_f64())
                .or_else(|| item.get("price").and_then(|v| v.as_f64()));
            let price = if let Some(s) = price_str {
                match Price::from_str(s) { Ok(p) => p, Err(_) => continue }
            } else if let Some(n) = price_num { match Price::from_f64(n) { Ok(p)=>p, Err(_)=>continue } }
            else { continue };

            // 解析數量
            let size_str = item.get("sz").and_then(|v| v.as_str())
                .or_else(|| item.get("size").and_then(|v| v.as_str()));
            let size_num = item.get("sz").and_then(|v| v.as_f64())
                .or_else(|| item.get("size").and_then(|v| v.as_f64()));
            let quantity = if let Some(s) = size_str {
                match Quantity::from_str(s) { Ok(q) => q, Err(_) => continue }
            } else if let Some(n) = size_num { match Quantity::from_f64(n) { Ok(q)=>q, Err(_)=>continue } }
            else { continue };

            // 解析方向
            let side = item.get("side").and_then(|v| v.as_str()).unwrap_or("buy");
            let side = match side { "buy"|"BUY" => Side::Buy, "sell"|"SELL" => Side::Sell, _ => Side::Buy };

            // 解析 tradeId（可選）
            let trade_id = item.get("tradeId").and_then(|v| v.as_str()).unwrap_or("").to_string();

            let trade = Trade {
                symbol: Symbol(symbol),
                timestamp: ts_us,
                price,
                quantity,
                side,
                trade_id,
                source_venue: Some(VenueId::BITGET),
            };
            let _ = self.event_sender.send(MarketEvent::Trade(trade));
        }

        Ok(())
    }

    fn parse_orderbook_data(&self, data: &BitgetOrderBookData, symbol: &Symbol) -> Result<MarketSnapshot, HftError> {
        let ts_ms = data.ts.parse::<u64>()
            .map_err(|_| HftError::Generic { message: "Invalid timestamp".to_string() })?;
        let timestamp = ts_ms.saturating_mul(1000); // 轉為微秒

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
            source_venue: Some(VenueId::BITGET),
        })
    }

    fn parse_trade_data(&self, data: &BitgetTradeData, fallback_inst_id: &str) -> Result<Trade, HftError> {
        let ts_ms = data.ts.parse::<u64>()
            .map_err(|_| HftError::Generic { message: "Invalid timestamp".to_string() })?;
        let timestamp = ts_ms.saturating_mul(1000); // 轉為微秒

        let price = Price::from_str(&data.price)
            .map_err(|_| HftError::Generic { message: format!("Invalid trade price: {}", data.price) })?;
        
        let quantity = Quantity::from_str(&data.size)
            .map_err(|_| HftError::Generic { message: format!("Invalid trade size: {}", data.size) })?;

        let side = match data.side.as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => return Err(HftError::Generic { message: format!("Invalid trade side: {}", data.side) }),
        };

        let symbol = Symbol(data.inst_id.clone().unwrap_or_else(|| fallback_inst_id.to_string()));
        Ok(Trade {
            symbol,
            timestamp,
            price,
            quantity,
            side,
            trade_id: data.trade_id.clone(),
            source_venue: Some(VenueId::BITGET),
        })
    }
}

#[derive(Default)]
struct OrderBookState {
    bids: BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>, // 價格→數量（bids 遍歷時取反向）
    asks: BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>, // 價格→數量（asks 遍歷時正向）
}

impl OrderBookState {
    fn apply_snapshot(&mut self, data: &BitgetOrderBookData) -> Result<(), HftError> {
        self.bids.clear();
        self.asks.clear();
        self.ingest_levels(&data.bids, true)?;
        self.ingest_levels(&data.asks, false)?;
        Ok(())
    }

    fn apply_update(&mut self, data: &BitgetOrderBookData) -> Result<(), HftError> {
        self.ingest_levels(&data.bids, true)?;
        self.ingest_levels(&data.asks, false)?;
        Ok(())
    }

    fn ingest_levels(&mut self, levels: &Vec<Vec<String>>, is_bid: bool) -> Result<(), HftError> {
        for lvl in levels {
            if lvl.len() < 2 { continue; }
            let px = rust_decimal::Decimal::from_str_exact(&lvl[0])
                .or_else(|_| lvl[0].parse::<f64>().map(|v| rust_decimal::Decimal::try_from(v).unwrap_or(rust_decimal::Decimal::ZERO)))
                .unwrap_or(rust_decimal::Decimal::ZERO);
            let sz = rust_decimal::Decimal::from_str_exact(&lvl[1])
                .or_else(|_| lvl[1].parse::<f64>().map(|v| rust_decimal::Decimal::try_from(v).unwrap_or(rust_decimal::Decimal::ZERO)))
                .unwrap_or(rust_decimal::Decimal::ZERO);
            if px.is_zero() { continue; }
            let book = if is_bid { &mut self.bids } else { &mut self.asks };
            if sz.is_zero() {
                book.remove(&px);
            } else {
                book.insert(px, sz);
            }
        }
        Ok(())
    }

    fn to_snapshot(&self, symbol: Symbol, ts_str: Option<&String>) -> Result<MarketSnapshot, HftError> {
        let ts_ms = if let Some(s) = ts_str { s.parse::<u64>().unwrap_or(0) } else { 0 };
        let timestamp = ts_ms.saturating_mul(1000);

        let mut bids = Vec::new();
        for (price, qty) in self.bids.iter().rev() { // 由高到低
            let p = Price(price.clone());
            let q = Quantity(qty.clone());
            bids.push(BookLevel { price: p, quantity: q });
        }
        let mut asks = Vec::new();
        for (price, qty) in self.asks.iter() { // 由低到高
            let p = Price(price.clone());
            let q = Quantity(qty.clone());
            asks.push(BookLevel { price: p, quantity: q });
        }
        Ok(MarketSnapshot { symbol, timestamp, bids, asks, sequence: 0, source_venue: Some(VenueId::BITGET) })
    }
}
