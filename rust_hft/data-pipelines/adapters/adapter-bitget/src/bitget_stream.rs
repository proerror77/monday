//! Bitget WebSocket 行情流實現

#![allow(dead_code)]
use adapters_common::ws_helpers::constants;
use async_trait::async_trait;
use serde::{
    de::{DeserializeOwned, IgnoredAny, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::marker::PhantomData;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

#[cfg(feature = "metrics")]
use infra_metrics::MetricsRegistry;

use hft_core::*;
use integration::latency::WsFrameMetrics;
use integration::ws::{MessageHandler, ReconnectingWsClient, WsClientConfig};
use ports::*;

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

#[derive(Debug, Deserialize)]
pub struct BitgetWsEnvelope<'a> {
    #[serde(borrow)]
    pub action: Option<&'a str>,
    #[serde(borrow)]
    pub arg: Option<BitgetSubscriptionArgRef<'a>>,
    #[serde(borrow)]
    pub code: Option<BitgetCode<'a>>,
    #[serde(borrow)]
    pub msg: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum BitgetCode<'a> {
    String(&'a str),
    Number(i64),
}

impl BitgetCode<'_> {
    #[inline]
    fn is_ok(&self) -> bool {
        match self {
            Self::String(code) => *code == "0",
            Self::Number(code) => *code == 0,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct BitgetSubscriptionArgRef<'a> {
    #[serde(rename = "instType", borrow)]
    pub inst_type: &'a str,
    #[serde(rename = "channel", borrow)]
    pub channel: &'a str,
    #[serde(rename = "instId", borrow)]
    pub inst_id: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct BitgetOrderBookFrame<'a> {
    #[serde(borrow)]
    pub action: Option<&'a str>,
    #[serde(borrow)]
    pub arg: Option<BitgetSubscriptionArgRef<'a>>,
    #[serde(borrow)]
    pub data: Vec<BitgetOrderBookDataRef<'a>>,
}

#[derive(Debug, Deserialize)]
pub struct BitgetOrderBookDataRef<'a> {
    #[serde(rename = "asks", borrow)]
    pub asks: Vec<BitgetLevelRef<'a>>,
    #[serde(rename = "bids", borrow)]
    pub bids: Vec<BitgetLevelRef<'a>>,
    #[serde(rename = "checksum")]
    pub checksum: Option<i64>,
    #[serde(rename = "ts", borrow)]
    pub ts: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct BitgetLevelRef<'a> {
    pub price: &'a str,
    pub size: &'a str,
}

impl<'de: 'a, 'a> Deserialize<'de> for BitgetLevelRef<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LevelVisitor<'a>(PhantomData<&'a ()>);

        impl<'de: 'a, 'a> Visitor<'de> for LevelVisitor<'a> {
            type Value = BitgetLevelRef<'a>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Bitget price level [price, size, ...]")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let price = seq
                    .next_element::<&'a str>()?
                    .ok_or_else(|| serde::de::Error::invalid_length(0, &"price at index 0"))?;
                let size = seq
                    .next_element::<&'a str>()?
                    .ok_or_else(|| serde::de::Error::invalid_length(1, &"size at index 1"))?;
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(BitgetLevelRef { price, size })
            }
        }

        deserializer.deserialize_seq(LevelVisitor(PhantomData))
    }
}

#[derive(Debug, Deserialize)]
pub struct BitgetTradeFrame<'a> {
    #[serde(borrow)]
    pub arg: Option<BitgetSubscriptionArgRef<'a>>,
    #[serde(borrow)]
    pub data: Vec<BitgetTradeDataRef<'a>>,
}

#[derive(Debug, Deserialize)]
pub struct BitgetTradeDataRef<'a> {
    #[serde(rename = "instId", borrow)]
    pub inst_id: Option<&'a str>,
    #[serde(rename = "tradeId", borrow)]
    pub trade_id: Option<&'a str>,
    #[serde(rename = "px", borrow)]
    pub price: Option<&'a str>,
    #[serde(rename = "price", borrow)]
    pub price_alt: Option<&'a str>,
    #[serde(rename = "sz", borrow)]
    pub size: Option<&'a str>,
    #[serde(rename = "size", borrow)]
    pub size_alt: Option<&'a str>,
    #[serde(rename = "side", borrow)]
    pub side: Option<&'a str>,
    #[serde(rename = "ts", borrow)]
    pub ts: &'a str,
}

/// 使用共用的 JSON 解析函數
#[inline]
fn parse_json<T: DeserializeOwned>(text: &str) -> HftResult<T> {
    adapters_common::parse_json(text).map_err(Into::into)
}

#[inline]
pub fn parse_bitget_ws_envelope(text: &str) -> HftResult<BitgetWsEnvelope<'_>> {
    serde_json::from_str(text).map_err(|e| HftError::Parse(e.to_string()))
}

#[inline]
pub fn parse_bitget_orderbook_frame(text: &str) -> HftResult<BitgetOrderBookFrame<'_>> {
    serde_json::from_str(text).map_err(|e| HftError::Parse(e.to_string()))
}

#[inline]
pub fn parse_bitget_trade_frame(text: &str) -> HftResult<BitgetTradeFrame<'_>> {
    serde_json::from_str(text).map_err(|e| HftError::Parse(e.to_string()))
}

#[inline]
fn selected_depth_channel(use_incremental_books: bool, depth_channel: &str) -> &str {
    if use_incremental_books {
        "books"
    } else {
        depth_channel
    }
}

#[inline]
pub fn parse_bitget_orderbook_snapshot(text: &str) -> HftResult<Option<MarketSnapshot>> {
    let frame = parse_bitget_orderbook_frame(text)?;
    let Some(arg) = &frame.arg else {
        return Ok(None);
    };
    if !(arg.channel == "depth" || arg.channel.starts_with("books")) {
        return Ok(None);
    }
    let Some(data) = frame.data.first() else {
        return Ok(None);
    };
    orderbook_data_ref_to_snapshot(data, arg.inst_id).map(Some)
}

#[inline]
pub fn parse_bitget_trade_event(text: &str) -> HftResult<Option<Trade>> {
    let frame = parse_bitget_trade_frame(text)?;
    let fallback_inst = frame.arg.as_ref().map(|a| a.inst_id).unwrap_or("");
    let Some(data) = frame.data.first() else {
        return Ok(None);
    };
    trade_data_ref_to_trade(data, fallback_inst).map(Some)
}

#[inline]
fn orderbook_data_ref_to_snapshot(
    data: &BitgetOrderBookDataRef<'_>,
    symbol: &str,
) -> HftResult<MarketSnapshot> {
    let ts_ms = data.ts.parse::<u64>().map_err(|_| HftError::Generic {
        message: "Invalid timestamp".to_string(),
    })?;
    let timestamp = ts_ms.saturating_mul(1000);

    let mut bids = Vec::with_capacity(data.bids.len());
    let mut asks = Vec::with_capacity(data.asks.len());

    for bid in &data.bids {
        let price = Price::from_str(bid.price).map_err(|_| HftError::Generic {
            message: format!("Invalid bid price: {}", bid.price),
        })?;
        let quantity = Quantity::from_str(bid.size).map_err(|_| HftError::Generic {
            message: format!("Invalid bid quantity: {}", bid.size),
        })?;
        bids.push(BookLevel { price, quantity });
    }

    for ask in &data.asks {
        let price = Price::from_str(ask.price).map_err(|_| HftError::Generic {
            message: format!("Invalid ask price: {}", ask.price),
        })?;
        let quantity = Quantity::from_str(ask.size).map_err(|_| HftError::Generic {
            message: format!("Invalid ask quantity: {}", ask.size),
        })?;
        asks.push(BookLevel { price, quantity });
    }

    Ok(MarketSnapshot {
        symbol: Symbol::from(symbol),
        timestamp,
        bids,
        asks,
        sequence: 0,
        source_venue: Some(VenueId::BITGET),
    })
}

#[inline]
fn trade_data_ref_to_trade(data: &BitgetTradeDataRef<'_>, fallback_inst: &str) -> HftResult<Trade> {
    let symbol = data.inst_id.unwrap_or(fallback_inst);
    if symbol.is_empty() {
        return Err(HftError::Generic {
            message: "Missing Bitget trade symbol".to_string(),
        });
    }

    let ts_ms = data.ts.parse::<u64>().map_err(|_| HftError::Generic {
        message: "Invalid timestamp".to_string(),
    })?;
    let timestamp = ts_ms.saturating_mul(1000);

    let price_str = data
        .price
        .or(data.price_alt)
        .ok_or_else(|| HftError::Generic {
            message: "Missing Bitget trade price".to_string(),
        })?;
    let price = Price::from_str(price_str).map_err(|_| HftError::Generic {
        message: format!("Invalid trade price: {}", price_str),
    })?;

    let size_str = data
        .size
        .or(data.size_alt)
        .ok_or_else(|| HftError::Generic {
            message: "Missing Bitget trade size".to_string(),
        })?;
    let quantity = Quantity::from_str(size_str).map_err(|_| HftError::Generic {
        message: format!("Invalid trade size: {}", size_str),
    })?;

    let side = match data.side.unwrap_or("buy") {
        "buy" | "BUY" => Side::Buy,
        "sell" | "SELL" => Side::Sell,
        other => {
            return Err(HftError::Generic {
                message: format!("Invalid trade side: {}", other),
            })
        }
    };

    Ok(Trade {
        symbol: Symbol::from(symbol),
        timestamp,
        price,
        quantity,
        side,
        trade_id: data.trade_id.unwrap_or("").to_string(),
        source_venue: Some(VenueId::BITGET),
    })
}

/// 使用共用的 Value 解析函數
#[inline]
fn parse_value_owned<T: DeserializeOwned>(value: serde_json::Value) -> HftResult<T> {
    adapters_common::parse_owned_value(value).map_err(Into::into)
}

/// JSON 序列化函數
#[inline]
fn to_json_string<T: Serialize>(value: &T) -> HftResult<String> {
    #[cfg(feature = "json-simd")]
    {
        simd_json::serde::to_string(value).map_err(|e| HftError::Serialization(e.to_string()))
    }
    #[cfg(not(feature = "json-simd"))]
    {
        serde_json::to_string(value).map_err(|e| HftError::Serialization(e.to_string()))
    }
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
    ws_url: Option<String>,
    /// Bitget 產品型別（SPOT / USDT-FUTURES / COIN-FUTURES / USDC-FUTURES）
    inst_type: String,
    /// 深度頻道：books/books1/books5/books15
    depth_channel: String,
}

impl Default for BitgetMarketStream {
    fn default() -> Self {
        Self::new()
    }
}

impl BitgetMarketStream {
    pub fn new() -> Self {
        Self {
            ws_client: None,
            event_sender: None,
            subscribed_symbols: Vec::new(),
            use_incremental_books: false,
            ws_url: None,
            inst_type: "SPOT".to_string(),
            depth_channel: "books15".to_string(),
        }
    }

    pub fn new_with_incremental(use_incremental_books: bool) -> Self {
        Self {
            use_incremental_books,
            ..Self::new()
        }
    }

    /// 指定自訂 WS URL（預設讀取官方端點）
    pub fn with_ws_url(mut self, ws_url: impl Into<String>) -> Self {
        self.ws_url = Some(ws_url.into());
        self
    }

    /// 指定 Bitget 產品型別（SPOT / USDT-FUTURES / COIN-FUTURES / USDC-FUTURES）
    pub fn with_inst_type(mut self, inst_type: impl Into<String>) -> Self {
        self.inst_type = inst_type.into();
        self
    }

    /// 指定深度頻道（books/books1/books5/books15）
    pub fn with_depth_channel(mut self, ch: impl Into<String>) -> Self {
        self.depth_channel = ch.into();
        self
    }

    fn create_ws_config(&self) -> WsClientConfig {
        // 放寬訊息/幀上限以容納多品種 LOB/Trade 聚合
        // 支持從環境變量或 runtime 傳入 URL
        let url = self
            .ws_url
            .clone()
            .or_else(|| std::env::var("BITGET_WS_URL").ok())
            .unwrap_or_else(|| "wss://ws.bitget.com/v2/ws/public".to_string());

        WsClientConfig {
            url,
            heartbeat_interval: constants::heartbeat_interval(),
            reconnect_interval: constants::reconnect_interval(),
            max_reconnect_attempts: 10,
            tcp_nodelay: true,            // HFT 必須啟用
            disable_compression: true,    // HFT 必須禁用壓縮
            max_message_size: 512 * 1024, // 512KB 訊息上限
            max_frame_size: 256 * 1024,   // 256KB 幀上限
            heartbeat_text: Some("ping".to_string()),
        }
    }

    fn parse_orderbook_data(
        &self,
        data: &BitgetOrderBookData,
        symbol: &Symbol,
    ) -> Result<MarketSnapshot, HftError> {
        let ts_ms = data.ts.parse::<u64>().map_err(|_| HftError::Generic {
            message: "Invalid timestamp".to_string(),
        })?;
        let timestamp = ts_ms.saturating_mul(1000); // 轉為微秒

        let mut bids = Vec::new();
        let mut asks = Vec::new();

        // 解析 bids (按價格降序)
        for bid in &data.bids {
            if bid.len() >= 2 {
                let price = Price::from_str(&bid[0]).map_err(|_| HftError::Generic {
                    message: format!("Invalid bid price: {}", bid[0]),
                })?;
                let quantity = Quantity::from_str(&bid[1]).map_err(|_| HftError::Generic {
                    message: format!("Invalid bid quantity: {}", bid[1]),
                })?;

                bids.push(BookLevel { price, quantity });
            }
        }

        // 解析 asks (按價格升序)
        for ask in &data.asks {
            if ask.len() >= 2 {
                let price = Price::from_str(&ask[0]).map_err(|_| HftError::Generic {
                    message: format!("Invalid ask price: {}", ask[0]),
                })?;
                let quantity = Quantity::from_str(&ask[1]).map_err(|_| HftError::Generic {
                    message: format!("Invalid ask quantity: {}", ask[1]),
                })?;

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

    fn parse_trade_data(
        &self,
        data: &BitgetTradeData,
        fallback_inst_id: &str,
    ) -> Result<Trade, HftError> {
        let ts_ms = data.ts.parse::<u64>().map_err(|_| HftError::Generic {
            message: "Invalid timestamp".to_string(),
        })?;
        let timestamp = ts_ms.saturating_mul(1000); // 轉為微秒

        let price = Price::from_str(&data.price).map_err(|_| HftError::Generic {
            message: format!("Invalid trade price: {}", data.price),
        })?;

        let quantity = Quantity::from_str(&data.size).map_err(|_| HftError::Generic {
            message: format!("Invalid trade size: {}", data.size),
        })?;

        let side = match data.side.as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => {
                return Err(HftError::Generic {
                    message: format!("Invalid trade side: {}", data.side),
                })
            }
        };

        let symbol = Symbol::from(
            data.inst_id
                .clone()
                .unwrap_or_else(|| fallback_inst_id.to_string()),
        );
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
            let orderbook_channel =
                selected_depth_channel(self.use_incremental_books, &self.depth_channel);
            // 訂閱 L2 深度數據
            let orderbook_args: Vec<BitgetSubscriptionArg> = symbols
                .iter()
                .map(|s| BitgetSubscriptionArg {
                    inst_type: self.inst_type.clone(),
                    channel: orderbook_channel.to_string(),
                    inst_id: s.as_str().to_string(),
                })
                .collect();

            let orderbook_request = BitgetSubscriptionRequest {
                op: "subscribe".to_string(),
                args: orderbook_args,
            };

            let orderbook_msg =
                to_json_string(&orderbook_request).map_err(|e| HftError::Generic {
                    message: format!("Failed to serialize orderbook subscription: {}", e),
                })?;

            // 訂閱成交數據
            let trade_args: Vec<BitgetSubscriptionArg> = symbols
                .iter()
                .map(|s| BitgetSubscriptionArg {
                    inst_type: self.inst_type.clone(),
                    channel: "trade".to_string(),
                    inst_id: s.as_str().to_string(),
                })
                .collect();

            let trade_request = BitgetSubscriptionRequest {
                op: "subscribe".to_string(),
                args: trade_args,
            };

            let trade_msg = to_json_string(&trade_request).map_err(|e| HftError::Generic {
                message: format!("Failed to serialize trade subscription: {}", e),
            })?;

            // 發送訂閱請求
            client
                .client
                .send_message(&orderbook_msg)
                .await
                .map_err(|e| HftError::Generic {
                    message: format!("Failed to send orderbook subscription: {}", e),
                })?;

            client
                .client
                .send_message(&trade_msg)
                .await
                .map_err(|e| HftError::Generic {
                    message: format!("Failed to send trade subscription: {}", e),
                })?;

            info!("已訂閱 {} 個交易對的行情和成交數據", symbols.len());
            Ok(())
        } else {
            Err(HftError::Generic {
                message: "WebSocket client not initialized".to_string(),
            })
        }
    }
}

#[async_trait]
impl MarketStream for BitgetMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        let (tx, mut rx) = mpsc::unbounded_channel();

        // 創建 WebSocket 客戶端
        let ws_config = self.create_ws_config();
        let mut ws_client = ReconnectingWsClient::new(ws_config);

        // 創建消息處理器
        let handler = BitgetMessageHandler::new(
            tx.clone(),
            symbols.clone(),
            self.use_incremental_books,
            self.inst_type.clone(),
            self.depth_channel.clone(),
        );

        // 在後台任務中運行 WebSocket 客戶端
        tokio::spawn(async move {
            if let Err(e) = ws_client.run_with_handler(handler).await {
                error!("Bitget WebSocket 客戶端錯誤: {}", e);
                let _ = tx.send(MarketEvent::Disconnect {
                    reason: format!("WebSocket error: {}", e),
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
            connected: self
                .ws_client
                .as_ref()
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
        let config = self.create_ws_config();
        let mut client = ReconnectingWsClient::new(config);

        client
            .connect_with_retry()
            .await
            .map_err(|e| HftError::Generic {
                message: format!("Connection failed: {}", e),
            })?;

        self.ws_client = Some(client);
        info!("Bitget MarketStream 連接成功");
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        if let Some(mut client) = self.ws_client.take() {
            client
                .client
                .disconnect()
                .await
                .map_err(|e| HftError::Generic {
                    message: format!("Disconnect failed: {}", e),
                })?;
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
    /// Bitget 產品型別（SPOT/USDT-FUTURES/COIN-FUTURES/USDC-FUTURES）
    inst_type: String,
    /// 深度頻道：books/books1/books5/books15（僅非增量模式使用）
    depth_channel: String,
}

impl BitgetMessageHandler {
    fn new(
        event_sender: mpsc::UnboundedSender<MarketEvent>,
        subscribed_symbols: Vec<Symbol>,
        use_incremental_books: bool,
        inst_type: String,
        depth_channel: String,
    ) -> Self {
        Self {
            event_sender,
            subscribed_symbols,
            subscription_sent: false,
            pending_subscriptions: None,
            use_incremental_books,
            ob_state: HashMap::new(),
            inst_type,
            depth_channel,
        }
    }
}

impl MessageHandler for BitgetMessageHandler {
    fn handle_message(
        &mut self,
        message: String,
        mut metrics: WsFrameMetrics,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        debug!("收到 Bitget 消息: {}", message);

        let envelope = match parse_bitget_ws_envelope(&message) {
            Ok(msg) => msg,
            Err(e) => {
                metrics.mark_parsed();
                let parse_latency = metrics.parsed_at_us.saturating_sub(metrics.received_at_us);
                trace!("Bitget envelope 解析失敗，耗時 {}μs", parse_latency);
                return Err(Box::new(e));
            }
        };

        // 優先依據 arg.channel 分流處理
        if let Some(arg) = &envelope.arg {
            match arg.channel {
                c if c.starts_with("books") || c == "depth" => {
                    match parse_bitget_orderbook_frame(&message) {
                        Ok(frame) => {
                            metrics.mark_parsed();
                            self.handle_orderbook_frame(&frame)?;
                        }
                        Err(e) => {
                            metrics.mark_parsed();
                            trace!("Bitget typed orderbook 解析失敗，回退 Value path: {}", e);
                            self.handle_message_legacy(&message)?;
                        }
                    }
                }
                "trade" | "trades" => match parse_bitget_trade_frame(&message) {
                    Ok(frame) => {
                        metrics.mark_parsed();
                        self.handle_trade_frame(&frame)?;
                    }
                    Err(e) => {
                        metrics.mark_parsed();
                        trace!("Bitget typed trade 解析失敗，回退 Value path: {}", e);
                        self.handle_message_legacy(&message)?;
                    }
                },
                other => {
                    metrics.mark_parsed();
                    debug!("未處理的頻道: {}", other);
                }
            }
        } else if let Some(code) = &envelope.code {
            metrics.mark_parsed();
            if code.is_ok() {
                info!("訂閱成功");
            } else {
                warn!("收到錯誤代碼: {:?}, 消息: {:?}", code, envelope.msg);
            }
        } else {
            metrics.mark_parsed();
        }

        let parse_latency = metrics.parsed_at_us.saturating_sub(metrics.received_at_us);
        trace!("Bitget JSON 解析完成，耗時 {}μs", parse_latency);

        #[cfg(feature = "metrics")]
        MetricsRegistry::global().record_parsing_latency(parse_latency as f64);

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

    fn handle_connected_async<'a>(
        &'a mut self,
        client: &'a mut integration::ws::WsClient,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if self.subscription_sent || self.subscribed_symbols.is_empty() {
                return Ok(());
            }

            // 採用明確且兼容的訂閱參數：
            // - 增量模式：books（先 snapshot 後 update）
            // - 否則：books15 快照
            let lob_channel =
                selected_depth_channel(self.use_incremental_books, &self.depth_channel);
            let lob_args: Vec<BitgetSubscriptionArg> = self
                .subscribed_symbols
                .iter()
                .map(|s| BitgetSubscriptionArg {
                    inst_type: self.inst_type.clone(),
                    channel: lob_channel.to_string(),
                    inst_id: s.as_str().to_string(),
                })
                .collect();
            let lob_req = BitgetSubscriptionRequest {
                op: "subscribe".to_string(),
                args: lob_args,
            };
            let lob_msg = to_json_string(&lob_req)?;
            client.send_message(&lob_msg).await?;
            info!(
                "已發送 LOB 訂閱: instType={}, channel={}, symbols={}",
                self.inst_type,
                lob_channel,
                self.subscribed_symbols.len()
            );

            let trade_args: Vec<BitgetSubscriptionArg> = self
                .subscribed_symbols
                .iter()
                .map(|s| BitgetSubscriptionArg {
                    inst_type: self.inst_type.clone(),
                    channel: "trade".to_string(),
                    inst_id: s.as_str().to_string(),
                })
                .collect();
            let trade_req = BitgetSubscriptionRequest {
                op: "subscribe".to_string(),
                args: trade_args,
            };
            let trade_msg = to_json_string(&trade_req)?;
            client.send_message(&trade_msg).await?;
            info!(
                "已發送成交訂閱: instType={}, channel=trade, symbols={}",
                self.inst_type,
                self.subscribed_symbols.len()
            );

            self.subscription_sent = true;
            info!("已發送訂閱參數，等待服務端回覆");

            Ok(())
        })
    }
}

impl BitgetMessageHandler {
    // LOB 狀態（僅增量模式）
    fn get_state_mut(&mut self, sym: &str) -> &mut OrderBookState {
        self.ob_state.entry(sym.to_string()).or_default()
    }

    fn handle_message_legacy(
        &mut self,
        message: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ws_msg: BitgetWsMessage = parse_json(message)?;

        if let Some(arg) = &ws_msg.arg {
            if let Some(data) = &ws_msg.data {
                match arg.channel.as_str() {
                    c if c.starts_with("books") || c == "depth" => {
                        self.handle_orderbook_data_with_action(
                            data,
                            &ws_msg.arg,
                            ws_msg.action.as_deref(),
                        )?;
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

    fn handle_orderbook_frame(
        &mut self,
        frame: &BitgetOrderBookFrame<'_>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let Some(arg) = &frame.arg else {
            return Ok(());
        };
        if !(arg.channel == "depth" || arg.channel.starts_with("books")) {
            return Ok(());
        }

        if !self.use_incremental_books {
            for orderbook_data in &frame.data {
                if let Some(cs) = orderbook_data.checksum {
                    if let Some(calc) = Self::calc_checksum_ref(orderbook_data) {
                        if calc != cs {
                            warn!("Bitget checksum mismatch: expected={}, got={}", cs, calc);
                        }
                    }
                }

                let symbol = Symbol::from(arg.inst_id);
                match self.parse_orderbook_data_ref(orderbook_data, &symbol) {
                    Ok(snapshot) => {
                        let _ = self.event_sender.send(MarketEvent::Snapshot(snapshot));
                    }
                    Err(e) => {
                        error!("解析訂單簿數據失敗: {}", e);
                    }
                }
            }
            return Ok(());
        }

        let sym = arg.inst_id;
        let is_snapshot = matches!(frame.action, Some("snapshot"));

        {
            let state = self.get_state_mut(sym);
            for ob in &frame.data {
                if is_snapshot {
                    state.apply_snapshot_ref(ob)?;
                } else {
                    state.apply_update_ref(ob)?;
                }
                if let Some(cs) = ob.checksum {
                    if let Some(calc) = Self::calc_checksum_ref(ob) {
                        if calc != cs {
                            warn!(
                                "Bitget incremental checksum mismatch: expected={}, got={}",
                                cs, calc
                            );
                        }
                    }
                }
            }
        }

        let snapshot = self
            .get_state_mut(sym)
            .to_snapshot(Symbol::new(sym), frame.data.last().map(|e| e.ts))?;
        let _ = self.event_sender.send(MarketEvent::Snapshot(snapshot));
        Ok(())
    }

    fn handle_trade_frame(
        &mut self,
        frame: &BitgetTradeFrame<'_>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let fallback_inst = frame.arg.as_ref().map(|a| a.inst_id).unwrap_or("");

        for item in &frame.data {
            match trade_data_ref_to_trade(item, fallback_inst) {
                Ok(trade) => {
                    let _ = self.event_sender.send(MarketEvent::Trade(trade));
                }
                Err(e) => {
                    trace!("忽略無效 Bitget trade frame: {}", e);
                }
            }
        }

        Ok(())
    }

    fn handle_orderbook_data(
        &mut self,
        data: &serde_json::Value,
        arg: &Option<BitgetSubscriptionArg>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(arg) = arg {
            if arg.channel == "depth" || arg.channel.starts_with("books") {
                let orderbook_array: Vec<BitgetOrderBookData> = parse_value_owned(data.clone())?;

                for orderbook_data in orderbook_array {
                    // 可選：校驗 checksum（僅快照頻道更可靠）
                    if let Some(cs) = orderbook_data.checksum {
                        if let Some(calc) = Self::calc_checksum(&orderbook_data) {
                            if calc != cs {
                                warn!("Bitget checksum mismatch: expected={}, got={}", cs, calc);
                            }
                        }
                    }
                    let symbol = Symbol::from(arg.inst_id.clone());

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

    fn handle_orderbook_data_with_action(
        &mut self,
        data: &serde_json::Value,
        arg: &Option<BitgetSubscriptionArg>,
        action: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !self.use_incremental_books {
            return self.handle_orderbook_data(data, arg);
        }
        let Some(arg) = arg else {
            return Ok(());
        };
        if !(arg.channel == "books" || arg.channel.starts_with("books")) {
            return Ok(());
        }

        let entries: Vec<BitgetOrderBookData> = parse_value_owned(data.clone())?;
        let sym = arg.inst_id.as_str();
        let state = self.get_state_mut(sym);

        let is_snapshot = matches!(action, Some("snapshot"));

        for ob in entries.iter() {
            if is_snapshot {
                state.apply_snapshot(ob)?;
            } else {
                state.apply_update(ob)?;
            }
            // 增量模式下，若提供 checksum，可嘗試以本地狀態前 N 檔計算檢驗（注意格式化差異）
            if let Some(cs) = ob.checksum {
                if let Some(calc) = Self::calc_checksum(ob) {
                    if calc != cs {
                        warn!(
                            "Bitget incremental checksum mismatch: expected={}, got={}",
                            cs, calc
                        );
                    }
                }
            }
        }

        // 生成快照事件（從狀態構建）
        let snapshot =
            state.to_snapshot(Symbol::new(sym), entries.last().map(|e| e.ts.as_str()))?;
        let _ = self.event_sender.send(MarketEvent::Snapshot(snapshot));
        Ok(())
    }

    /// 根據資料中的 bids/asks 原始字串計算 CRC32（signed 32-bit）
    fn calc_checksum(data: &BitgetOrderBookData) -> Option<i64> {
        use crc32fast::Hasher;
        let bids = &data.bids;
        let asks = &data.asks;
        let max_n = std::cmp::min(25, std::cmp::max(bids.len(), asks.len()));
        if max_n == 0 {
            return Some(0);
        }
        let mut parts: Vec<String> = Vec::with_capacity(max_n * 2);
        for i in 0..max_n {
            if let Some(b) = bids.get(i) {
                if b.len() >= 2 {
                    // 使用原始字串，避免去掉末尾 0
                    parts.push(format!("{}:{}", b[0], b[1]));
                }
            }
            if let Some(a) = asks.get(i) {
                if a.len() >= 2 {
                    parts.push(format!("{}:{}", a[0], a[1]));
                }
            }
        }
        let s = parts.join(":");
        let mut hasher = Hasher::new();
        hasher.update(s.as_bytes());
        let crc = hasher.finalize() as i64;
        Some(crc)
    }

    /// 根據借用資料計算 CRC32，避免 hot path 先建立 owned JSON/字串層級。
    fn calc_checksum_ref(data: &BitgetOrderBookDataRef<'_>) -> Option<i64> {
        use crc32fast::Hasher;
        let max_n = std::cmp::min(25, std::cmp::max(data.bids.len(), data.asks.len()));
        if max_n == 0 {
            return Some(0);
        }

        let mut hasher = Hasher::new();
        let mut first = true;
        for i in 0..max_n {
            if let Some(b) = data.bids.get(i) {
                Self::checksum_push_level(&mut hasher, &mut first, b.price, b.size);
            }
            if let Some(a) = data.asks.get(i) {
                Self::checksum_push_level(&mut hasher, &mut first, a.price, a.size);
            }
        }
        Some(hasher.finalize() as i64)
    }

    #[inline]
    fn checksum_push_level(hasher: &mut crc32fast::Hasher, first: &mut bool, px: &str, sz: &str) {
        if *first {
            *first = false;
        } else {
            hasher.update(b":");
        }
        hasher.update(px.as_bytes());
        hasher.update(b":");
        hasher.update(sz.as_bytes());
    }

    fn handle_trade_data(
        &mut self,
        data: &serde_json::Value,
        arg: &Option<BitgetSubscriptionArg>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
            let symbol = item
                .get("instId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| fallback_inst.to_string());
            if symbol.is_empty() {
                continue;
            }

            // 解析時間戳（ms → μs）
            let ts_ms_opt = match item.get("ts") {
                Some(v) if v.is_string() => v.as_str().and_then(|s| s.parse::<u64>().ok()),
                Some(v) if v.is_u64() => v.as_u64(),
                Some(v) if v.is_number() => v.as_f64().map(|f| f as u64),
                _ => None,
            };
            let ts_us = ts_ms_opt.unwrap_or(0).saturating_mul(1000);
            if ts_us == 0 {
                continue;
            }

            // 解析價格
            let price_str = item
                .get("px")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("price").and_then(|v| v.as_str()));
            let price_num = item
                .get("px")
                .and_then(|v| v.as_f64())
                .or_else(|| item.get("price").and_then(|v| v.as_f64()));
            let price = if let Some(s) = price_str {
                match Price::from_str(s) {
                    Ok(p) => p,
                    Err(_) => continue,
                }
            } else if let Some(n) = price_num {
                match Price::from_f64(n) {
                    Ok(p) => p,
                    Err(_) => continue,
                }
            } else {
                continue;
            };

            // 解析數量
            let size_str = item
                .get("sz")
                .and_then(|v| v.as_str())
                .or_else(|| item.get("size").and_then(|v| v.as_str()));
            let size_num = item
                .get("sz")
                .and_then(|v| v.as_f64())
                .or_else(|| item.get("size").and_then(|v| v.as_f64()));
            let quantity = if let Some(s) = size_str {
                match Quantity::from_str(s) {
                    Ok(q) => q,
                    Err(_) => continue,
                }
            } else if let Some(n) = size_num {
                match Quantity::from_f64(n) {
                    Ok(q) => q,
                    Err(_) => continue,
                }
            } else {
                continue;
            };

            // 解析方向
            let side = item.get("side").and_then(|v| v.as_str()).unwrap_or("buy");
            let side = match side {
                "buy" | "BUY" => Side::Buy,
                "sell" | "SELL" => Side::Sell,
                _ => Side::Buy,
            };

            // 解析 tradeId（可選）
            let trade_id = item
                .get("tradeId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let trade = Trade {
                symbol: Symbol::from(symbol),
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

    fn parse_orderbook_data(
        &self,
        data: &BitgetOrderBookData,
        symbol: &Symbol,
    ) -> Result<MarketSnapshot, HftError> {
        let ts_ms = data.ts.parse::<u64>().map_err(|_| HftError::Generic {
            message: "Invalid timestamp".to_string(),
        })?;
        let timestamp = ts_ms.saturating_mul(1000); // 轉為微秒

        let mut bids = Vec::new();
        let mut asks = Vec::new();

        // 解析 bids
        for bid in &data.bids {
            if bid.len() >= 2 {
                let price = Price::from_str(&bid[0]).map_err(|_| HftError::Generic {
                    message: format!("Invalid bid price: {}", bid[0]),
                })?;
                let quantity = Quantity::from_str(&bid[1]).map_err(|_| HftError::Generic {
                    message: format!("Invalid bid quantity: {}", bid[1]),
                })?;

                bids.push(BookLevel { price, quantity });
            }
        }

        // 解析 asks
        for ask in &data.asks {
            if ask.len() >= 2 {
                let price = Price::from_str(&ask[0]).map_err(|_| HftError::Generic {
                    message: format!("Invalid ask price: {}", ask[0]),
                })?;
                let quantity = Quantity::from_str(&ask[1]).map_err(|_| HftError::Generic {
                    message: format!("Invalid ask quantity: {}", ask[1]),
                })?;

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

    fn parse_orderbook_data_ref(
        &self,
        data: &BitgetOrderBookDataRef<'_>,
        symbol: &Symbol,
    ) -> Result<MarketSnapshot, HftError> {
        orderbook_data_ref_to_snapshot(data, symbol.as_str())
    }

    fn parse_trade_data(
        &self,
        data: &BitgetTradeData,
        fallback_inst_id: &str,
    ) -> Result<Trade, HftError> {
        let ts_ms = data.ts.parse::<u64>().map_err(|_| HftError::Generic {
            message: "Invalid timestamp".to_string(),
        })?;
        let timestamp = ts_ms.saturating_mul(1000); // 轉為微秒

        let price = Price::from_str(&data.price).map_err(|_| HftError::Generic {
            message: format!("Invalid trade price: {}", data.price),
        })?;

        let quantity = Quantity::from_str(&data.size).map_err(|_| HftError::Generic {
            message: format!("Invalid trade size: {}", data.size),
        })?;

        let side = match data.side.as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => {
                return Err(HftError::Generic {
                    message: format!("Invalid trade side: {}", data.side),
                })
            }
        };

        let symbol = Symbol::from(
            data.inst_id
                .clone()
                .unwrap_or_else(|| fallback_inst_id.to_string()),
        );
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

    fn apply_snapshot_ref(&mut self, data: &BitgetOrderBookDataRef<'_>) -> Result<(), HftError> {
        self.bids.clear();
        self.asks.clear();
        self.ingest_levels_ref(&data.bids, true)?;
        self.ingest_levels_ref(&data.asks, false)?;
        Ok(())
    }

    fn apply_update(&mut self, data: &BitgetOrderBookData) -> Result<(), HftError> {
        self.ingest_levels(&data.bids, true)?;
        self.ingest_levels(&data.asks, false)?;
        Ok(())
    }

    fn apply_update_ref(&mut self, data: &BitgetOrderBookDataRef<'_>) -> Result<(), HftError> {
        self.ingest_levels_ref(&data.bids, true)?;
        self.ingest_levels_ref(&data.asks, false)?;
        Ok(())
    }

    fn ingest_levels(&mut self, levels: &Vec<Vec<String>>, is_bid: bool) -> Result<(), HftError> {
        for lvl in levels {
            if lvl.len() < 2 {
                continue;
            }
            let px = rust_decimal::Decimal::from_str_exact(&lvl[0])
                .or_else(|_| {
                    lvl[0].parse::<f64>().map(|v| {
                        rust_decimal::Decimal::try_from(v).unwrap_or(rust_decimal::Decimal::ZERO)
                    })
                })
                .unwrap_or(rust_decimal::Decimal::ZERO);
            let sz = rust_decimal::Decimal::from_str_exact(&lvl[1])
                .or_else(|_| {
                    lvl[1].parse::<f64>().map(|v| {
                        rust_decimal::Decimal::try_from(v).unwrap_or(rust_decimal::Decimal::ZERO)
                    })
                })
                .unwrap_or(rust_decimal::Decimal::ZERO);
            if px.is_zero() {
                continue;
            }
            let book = if is_bid {
                &mut self.bids
            } else {
                &mut self.asks
            };
            if sz.is_zero() {
                book.remove(&px);
            } else {
                book.insert(px, sz);
            }
        }
        Ok(())
    }

    fn ingest_levels_ref(
        &mut self,
        levels: &[BitgetLevelRef<'_>],
        is_bid: bool,
    ) -> Result<(), HftError> {
        for lvl in levels {
            let px = rust_decimal::Decimal::from_str_exact(lvl.price)
                .or_else(|_| {
                    lvl.price.parse::<f64>().map(|v| {
                        rust_decimal::Decimal::try_from(v).unwrap_or(rust_decimal::Decimal::ZERO)
                    })
                })
                .unwrap_or(rust_decimal::Decimal::ZERO);
            let sz = rust_decimal::Decimal::from_str_exact(lvl.size)
                .or_else(|_| {
                    lvl.size.parse::<f64>().map(|v| {
                        rust_decimal::Decimal::try_from(v).unwrap_or(rust_decimal::Decimal::ZERO)
                    })
                })
                .unwrap_or(rust_decimal::Decimal::ZERO);
            if px.is_zero() {
                continue;
            }
            let book = if is_bid {
                &mut self.bids
            } else {
                &mut self.asks
            };
            if sz.is_zero() {
                book.remove(&px);
            } else {
                book.insert(px, sz);
            }
        }
        Ok(())
    }

    fn to_snapshot(
        &self,
        symbol: Symbol,
        ts_str: Option<&str>,
    ) -> Result<MarketSnapshot, HftError> {
        let ts_ms = if let Some(s) = ts_str {
            s.parse::<u64>().unwrap_or(0)
        } else {
            0
        };
        let timestamp = ts_ms.saturating_mul(1000);

        let mut bids = Vec::new();
        for (price, qty) in self.bids.iter().rev() {
            // 由高到低
            let p = Price(*price);
            let q = Quantity(*qty);
            bids.push(BookLevel {
                price: p,
                quantity: q,
            });
        }
        let mut asks = Vec::new();
        for (price, qty) in self.asks.iter() {
            // 由低到高
            let p = Price(*price);
            let q = Quantity(*qty);
            asks.push(BookLevel {
                price: p,
                quantity: q,
            });
        }
        Ok(MarketSnapshot {
            symbol,
            timestamp,
            bids,
            asks,
            sequence: 0,
            source_venue: Some(VenueId::BITGET),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORDERBOOK_FRAME: &str = r#"{
        "action":"snapshot",
        "arg":{"instType":"SPOT","channel":"books15","instId":"BTCUSDT"},
        "data":[{
            "asks":[["67189.5","0.25","1"],["67190.0","0.30"]],
            "bids":[["67188.1","0.12","2"],["67187.5","0.15"]],
            "ts":"1777353243014"
        }]
    }"#;

    const TRADE_FRAME: &str = r#"{
        "arg":{"instType":"SPOT","channel":"trade","instId":"BTCUSDT"},
        "data":[{
            "px":"67188.1",
            "sz":"0.12",
            "side":"buy",
            "ts":"1777353243014",
            "tradeId":"t-1"
        }]
    }"#;

    #[test]
    fn borrowed_orderbook_parser_ignores_level_count_without_allocating_strings() {
        let frame = parse_bitget_orderbook_frame(ORDERBOOK_FRAME).expect("valid orderbook");

        let arg = frame.arg.expect("arg");
        assert_eq!(arg.channel, "books15");
        assert_eq!(arg.inst_id, "BTCUSDT");
        assert_eq!(frame.data.len(), 1);
        assert_eq!(frame.data[0].asks[0].price, "67189.5");
        assert_eq!(frame.data[0].asks[0].size, "0.25");
        assert_eq!(frame.data[0].bids[0].price, "67188.1");
        assert_eq!(frame.data[0].ts, "1777353243014");
    }

    #[test]
    fn borrowed_trade_parser_accepts_official_trade_channel() {
        let frame = parse_bitget_trade_frame(TRADE_FRAME).expect("valid trade");

        let arg = frame.arg.expect("arg");
        assert_eq!(arg.channel, "trade");
        assert_eq!(frame.data[0].price, Some("67188.1"));
        assert_eq!(frame.data[0].size, Some("0.12"));
        assert_eq!(frame.data[0].trade_id, Some("t-1"));
    }

    #[test]
    fn selected_depth_channel_never_uses_legacy_depth_subscription() {
        assert_eq!(selected_depth_channel(true, "books15"), "books");
        assert_eq!(selected_depth_channel(false, "books1"), "books1");
        assert_eq!(selected_depth_channel(false, "books15"), "books15");
    }

    #[test]
    fn parses_orderbook_snapshot_event_with_decimal_levels() {
        let snapshot = parse_bitget_orderbook_snapshot(ORDERBOOK_FRAME)
            .expect("valid orderbook")
            .expect("snapshot");

        assert_eq!(snapshot.symbol.as_str(), "BTCUSDT");
        assert_eq!(snapshot.timestamp, 1_777_353_243_014_000);
        assert_eq!(snapshot.bids.len(), 2);
        assert_eq!(snapshot.asks.len(), 2);
        assert_eq!(snapshot.bids[0].price.to_string(), "67188.1");
        assert_eq!(snapshot.bids[0].quantity.to_string(), "0.12");
    }

    #[test]
    fn parses_trade_event_with_decimal_price_quantity() {
        let trade = parse_bitget_trade_event(TRADE_FRAME)
            .expect("valid trade")
            .expect("trade");

        assert_eq!(trade.symbol.as_str(), "BTCUSDT");
        assert_eq!(trade.timestamp, 1_777_353_243_014_000);
        assert_eq!(trade.price.to_string(), "67188.1");
        assert_eq!(trade.quantity.to_string(), "0.12");
        assert_eq!(trade.side, Side::Buy);
        assert_eq!(trade.trade_id, "t-1");
    }
}
