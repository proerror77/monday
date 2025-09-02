//! Bitget 執行 adapter（實作 `ports::ExecutionClient`）
//! - REST 下單 + 私有 WS 回報 → 統一 ExecutionEvent
//! 
//! 功能：
//! 1. REST API 下單、撤單、修改訂單
//! 2. 私有 WebSocket 接收成交回報、訂單狀態更新
//! 3. Live/Paper 模式切換
//! 4. 精度保護與錯誤處理
//! 5. 完整的可觀測性與指標

#[cfg(feature = "metrics")]
pub mod metrics;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use integration::{
    http::{HttpClient, HttpClientConfig},
    signing::{BitgetCredentials, BitgetSigner},
};
use ports::{BoxStream, ExecutionClient, ExecutionEvent, OrderIntent, ConnectionHealth, VenueSpec, OpenOrder};
use hft_core::{HftResult, HftError, OrderId, Price, Quantity, Side, OrderType, TimeInForce, Timestamp, UnifiedTimestamp};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn, error, debug};
use rust_decimal::Decimal;
use url::Url;

/// 訂單記錄 (內部使用)
#[derive(Debug, Clone)]
struct OrderRecord {
    symbol: String,
    client_order_id: String,
    side: Side,
    quantity: Quantity,
    price: Option<Price>,
    timestamp: Timestamp,
}

/// Bitget 執行客戶端配置
#[derive(Debug, Clone)]
pub struct BitgetExecutionConfig {
    pub credentials: BitgetCredentials,
    pub rest_base_url: String,
    pub ws_private_url: String,
    pub mode: ExecutionMode,
    pub timeout_ms: u64,
}

/// 執行模式
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionMode {
    Live,   // 真實交易
    Paper,  // 模擬交易
}

impl Default for BitgetExecutionConfig {
    fn default() -> Self {
        Self {
            credentials: BitgetCredentials::new(
                std::env::var("BITGET_API_KEY").unwrap_or_default(),
                std::env::var("BITGET_SECRET_KEY").unwrap_or_default(),
                std::env::var("BITGET_PASSPHRASE").unwrap_or_default(),
            ),
            rest_base_url: "https://api.bitget.com".to_string(),
            ws_private_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            mode: ExecutionMode::Paper, // 默認模擬交易
            timeout_ms: 5000,
        }
    }
}

/// Bitget 下單請求
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaceOrderRequest {
    symbol: String,
    side: String,         // "buy" or "sell"
    order_type: String,   // "limit", "market"
    force: String,        // "gtc", "ioc", "fok", "post_only"
    price: String,
    size: String,
    client_order_id: String,
}

/// Bitget 撤單請求
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelOrderRequest {
    symbol: String,
    order_id: Option<String>,
    client_order_id: Option<String>,
}

/// Bitget 修改訂單請求
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModifyOrderRequest {
    symbol: String,
    order_id: String,
    client_order_id: Option<String>,
    new_size: Option<String>,
    price: Option<String>,
}

/// Bitget 下單響應
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaceOrderResponse {
    code: String,
    msg: String,
    data: Option<PlaceOrderData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaceOrderData {
    order_id: String,
    client_order_id: String,
}

/// Bitget 通用 API 響應
#[derive(Debug, Deserialize)]
struct BitgetApiResponse {
    code: String,
    msg: String,
    data: Option<serde_json::Value>,
}

/// 私有 WebSocket 訂閱消息
#[derive(Debug, Serialize)]
struct PrivateSubscription {
    op: String,
    args: Vec<SubscriptionArg>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SubscriptionArg {
    #[serde(rename = "instType")]
    inst_type: String,
    channel: String,
    #[serde(rename = "instId")]
    inst_id: String,
}

/// 私有 WebSocket 消息
#[derive(Debug, Deserialize)]
struct PrivateWSMessage {
    arg: Option<SubscriptionArg>,
    data: Option<serde_json::Value>,
    event: Option<String>,
}

/// Bitget 訂單狀態更新數據
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BitgetOrderUpdate {
    #[serde(rename = "instId")]
    inst_id: String,
    #[serde(rename = "ordId")]
    ord_id: String,
    #[serde(rename = "clOrdId")]
    cl_ord_id: String,
    #[serde(rename = "px")]
    price: String,
    #[serde(rename = "sz")]
    size: String,
    #[serde(rename = "side")]
    side: String,
    #[serde(rename = "ordType")]
    ord_type: String,
    #[serde(rename = "state")]
    state: String, // "new", "partially_filled", "filled", "cancelled", "rejected"
    #[serde(rename = "fillSz")]
    fill_sz: String,
    #[serde(rename = "avgPx")]
    avg_px: Option<String>,
    #[serde(rename = "uTime")]
    u_time: String, // 更新時間
}

/// Bitget 成交回報數據
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BitgetFillUpdate {
    #[serde(rename = "instId")]
    inst_id: String,
    #[serde(rename = "tradeId")]
    trade_id: String,
    #[serde(rename = "ordId")]
    ord_id: String,
    #[serde(rename = "clOrdId")]
    cl_ord_id: String,
    #[serde(rename = "px")]
    price: String,
    #[serde(rename = "sz")]
    size: String,
    #[serde(rename = "side")]
    side: String,
    #[serde(rename = "fillTime")]
    fill_time: String,
    #[serde(rename = "fee")]
    fee: Option<String>,
    #[serde(rename = "feeCcy")]
    fee_ccy: Option<String>,
}

/// Bitget 執行客戶端
pub struct BitgetExecutionClient {
    config: BitgetExecutionConfig,
    http_client: Option<HttpClient>,
    signer: BitgetSigner,
    event_tx: Option<broadcast::Sender<ExecutionEvent>>,
    ws_handle: Option<tokio::task::JoinHandle<()>>,
    connected: bool,
    last_heartbeat: Timestamp,
    // 客戶端訂單 ID 映射表 (用於幂等性與撤單)
    client_order_mapping: std::collections::HashMap<String, String>, // client_order_id -> order_id
    // 訂單記錄 (用於撤單時獲取 symbol)
    order_records: std::collections::HashMap<String, OrderRecord>, // order_id -> record
    // Fill ID 去重緩存 (滑動窗口，防止重複 Fill 事件)
    fill_id_cache: std::collections::HashSet<String>,
    // 緩存最後清理時間 (用於定期清理舊的 fill_id)
    last_cache_cleanup: Timestamp,
}

impl BitgetExecutionClient {
    pub fn new(config: BitgetExecutionConfig) -> HftResult<Self> {
        let signer = BitgetSigner::new(config.credentials.clone());
        
        Ok(Self {
            config,
            http_client: None,
            signer,
            event_tx: None,
            ws_handle: None,
            connected: false,
            last_heartbeat: 0,
            client_order_mapping: std::collections::HashMap::new(),
            order_records: std::collections::HashMap::new(),
            fill_id_cache: std::collections::HashSet::new(),
            last_cache_cleanup: Self::current_timestamp(),
        })
    }

    /// 生成客戶端訂單 ID (用於幂等性)
    fn generate_client_order_id() -> String {
        format!("hft_{}", Self::current_timestamp())
    }

    /// 解析 Bitget 錯誤類型
    fn classify_error(code: &str, msg: &str) -> HftError {
        match code {
            "40001" | "40002" | "40003" => HftError::Authentication(msg.to_string()),
            "40004" | "40005" => HftError::RateLimit(msg.to_string()),
            "40006" | "40007" => HftError::InsufficientBalance(msg.to_string()),
            "40008" | "40009" => HftError::OrderNotFound(msg.to_string()),
            "40010" | "40011" => HftError::InvalidOrder(msg.to_string()),
            "50001" | "50002" => HftError::Network(msg.to_string()),
            _ => HftError::Exchange(format!("Code: {}, Msg: {}", code, msg)),
        }
    }

    /// 實現退避重試策略
    async fn retry_with_backoff<T, F, Fut>(
        &self,
        mut operation: F,
        max_retries: usize,
    ) -> HftResult<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = HftResult<T>>,
    {
        let mut attempts = 0;
        let mut delay_ms = 100; // 起始延遲 100ms
        
        loop {
            match operation().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    attempts += 1;
                    if attempts >= max_retries {
                        return Err(e);
                    }
                    
                    // 根據錯誤類型決定是否重試
                    match &e {
                        HftError::RateLimit(_) | HftError::Network(_) => {
                            warn!("操作失敗，{}ms 後重試 ({}/{})：{}", delay_ms, attempts, max_retries, e);
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                            delay_ms = (delay_ms * 2).min(5000); // 指數退避，最大 5s
                        }
                        _ => {
                            // 不可重試的錯誤
                            return Err(e);
                        }
                    }
                }
            }
        }
    }

    /// 校驗訂單參數 (使用 VenueSpec)
    fn validate_order_with_spec(
        &self,
        intent: &OrderIntent,
        venue_spec: Option<&VenueSpec>,
    ) -> HftResult<()> {
        if let Some(spec) = venue_spec {
            // 檢查數量
            if intent.quantity < spec.min_qty {
                return Err(HftError::InvalidOrder(format!(
                    "數量過小: {} < 最小數量 {}", 
                    intent.quantity.0, spec.min_qty.0
                )));
            }
            
            if let Some(max_qty) = spec.max_quantity {
                if intent.quantity > max_qty {
                    return Err(HftError::InvalidOrder(format!(
                        "數量過大: {} > 最大數量 {}", 
                        intent.quantity.0, max_qty.0
                    )));
                }
            }
            
            // 檢查價格 (如果有)
            if let Some(price) = intent.price {
                let price_decimal = price.0;
                let tick_decimal = spec.tick_size.0;
                
                if (price_decimal % tick_decimal) != rust_decimal::Decimal::ZERO {
                    return Err(HftError::InvalidOrder(format!(
                        "價格不符合步進: {} 不是 {} 的倍數", 
                        price_decimal, tick_decimal
                    )));
                }
                
                // 檢查最小名義值
                let notional = price_decimal * intent.quantity.0;
                if notional < spec.min_notional {
                    return Err(HftError::InvalidOrder(format!(
                        "名義值過小: {} < 最小名義值 {}", 
                        notional, spec.min_notional
                    )));
                }
            }
        }
        
        Ok(())
    }

    /// 初始化 HTTP 客戶端
    fn init_http_client(&mut self) -> HftResult<()> {
        let http_config = HttpClientConfig {
            base_url: self.config.rest_base_url.clone(),
            timeout_ms: self.config.timeout_ms,
            user_agent: "hft-bitget-exec/1.0".to_string(),
        };
        
        self.http_client = Some(HttpClient::new(http_config)
            .map_err(|e| HftError::Network(e.to_string()))?);
        
        Ok(())
    }

    /// 啟動私有 WebSocket 連接
    async fn start_private_websocket(&mut self, event_tx: broadcast::Sender<ExecutionEvent>) -> HftResult<()> {
        let ws_url = self.config.ws_private_url.clone();
        let credentials = self.config.credentials.clone();
        
        info!("啟動 Bitget 私有 WebSocket: {}", ws_url);
        
        let handle = tokio::spawn(async move {
            if let Err(e) = Self::private_websocket_loop(ws_url, credentials, event_tx.clone()).await {
                error!("私有 WebSocket 錯誤: {}", e);
                let _ = event_tx.send(ExecutionEvent::ConnectionStatus {
                    connected: false,
                    timestamp: Self::current_timestamp(),
                });
            }
        });
        
        self.ws_handle = Some(handle);
        Ok(())
    }

    /// 私有 WebSocket 循環
    async fn private_websocket_loop(
        ws_url: String,
        credentials: BitgetCredentials,
        event_tx: broadcast::Sender<ExecutionEvent>,
    ) -> HftResult<()> {
        let url = Url::parse(&ws_url).map_err(|e| HftError::Network(e.to_string()))?;
        let (ws_stream, _) = connect_async(url).await
            .map_err(|e| HftError::Network(e.to_string()))?;
        
        let (mut write, mut read) = ws_stream.split();
        
        // 發送連接狀態
        let _ = event_tx.send(ExecutionEvent::ConnectionStatus {
            connected: true,
            timestamp: Self::current_timestamp(),
        });

        // 訂閱私有頻道
        let subscription = PrivateSubscription {
            op: "subscribe".to_string(),
            args: vec![
                SubscriptionArg {
                    inst_type: "SPOT".to_string(),
                    channel: "orders".to_string(),
                    inst_id: "default".to_string(),
                },
                SubscriptionArg {
                    inst_type: "SPOT".to_string(), 
                    channel: "fill".to_string(),
                    inst_id: "default".to_string(),
                },
            ],
        };
        
        let sub_msg = serde_json::to_string(&subscription)
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        
        write.send(Message::Text(sub_msg)).await
            .map_err(|e| HftError::Network(e.to_string()))?;
        
        info!("已訂閱 Bitget 私有頻道");

        // 處理消息
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Err(e) = Self::handle_private_message(&text, &event_tx).await {
                        warn!("處理私有消息失敗: {} - {}", e, text);
                    }
                }
                Ok(Message::Ping(payload)) => {
                    let _ = write.send(Message::Pong(payload)).await;
                    debug!("收到 ping，已回應 pong");
                }
                Ok(Message::Close(_)) => {
                    info!("WebSocket 連接關閉");
                    break;
                }
                Err(e) => {
                    error!("WebSocket 錯誤: {}", e);
                    break;
                }
                _ => {}
            }
        }
        
        let _ = event_tx.send(ExecutionEvent::ConnectionStatus {
            connected: false,
            timestamp: Self::current_timestamp(),
        });
        
        Ok(())
    }

    /// 處理私有 WebSocket 消息
    async fn handle_private_message(
        text: &str,
        event_tx: &broadcast::Sender<ExecutionEvent>,
    ) -> HftResult<()> {
        let message: PrivateWSMessage = serde_json::from_str(text)
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        
        debug!("收到私有消息: {:?}", message);
        
        // 根據頻道類型處理不同事件
        if let Some(arg) = &message.arg {
            match arg.channel.as_str() {
                "orders" => {
                    // 訂單狀態更新
                    if let Some(data) = &message.data {
                        Self::handle_order_update(data, event_tx).await?;
                    }
                }
                "fill" => {
                    // 成交回報
                    if let Some(data) = &message.data {
                        Self::handle_fill_update(data, event_tx).await?;
                    }
                }
                _ => {
                    debug!("未知頻道: {}", arg.channel);
                }
            }
        }
        
        Ok(())
    }
    
    /// 處理訂單狀態更新
    async fn handle_order_update(
        data: &serde_json::Value,
        event_tx: &broadcast::Sender<ExecutionEvent>,
    ) -> HftResult<()> {
        // 數據可能是數組格式
        let updates: Vec<BitgetOrderUpdate> = if data.is_array() {
            serde_json::from_value(data.clone())
                .map_err(|e| HftError::Serialization(e.to_string()))?
        } else {
            vec![serde_json::from_value(data.clone())
                .map_err(|e| HftError::Serialization(e.to_string()))?]
        };
        
        for update in updates {
            let order_id = OrderId(update.ord_id.clone());
            let timestamp = Self::parse_timestamp(&update.u_time)?;
            
            // 根據訂單狀態發送相應事件
            match update.state.as_str() {
                "new" => {
                    // 訂單確認 (ACK)
                    info!("訂單確認: order_id={}, symbol={}", 
                          update.ord_id, update.inst_id);
                    
                    if let Err(e) = event_tx.send(ExecutionEvent::OrderAck {
                        order_id: order_id.clone(),
                        timestamp,
                    }) {
                        warn!("訂單確認事件發送失敗: order_id={}, error={}", update.ord_id, e);
                        #[cfg(feature = "metrics")]
                        {
                            crate::metrics::EVENT_SEND_ERRORS
                                .with_label_values(&["order_ack_send_failed"])
                                .inc();
                        }
                    }
                }
                "cancelled" => {
                    // 訂單取消確認
                    info!("訂單取消: order_id={}, symbol={}", 
                          update.ord_id, update.inst_id);
                    
                    if let Err(e) = event_tx.send(ExecutionEvent::OrderCanceled {
                        order_id: order_id.clone(),
                        timestamp,
                    }) {
                        warn!("訂單取消事件發送失敗: order_id={}, error={}", update.ord_id, e);
                        #[cfg(feature = "metrics")]
                        {
                            crate::metrics::EVENT_SEND_ERRORS
                                .with_label_values(&["order_canceled_send_failed"])
                                .inc();
                        }
                    }
                }
                "rejected" => {
                    // 訂單拒絕
                    warn!("訂單拒絕: order_id={}, symbol={}", 
                          update.ord_id, update.inst_id);
                    
                    if let Err(e) = event_tx.send(ExecutionEvent::OrderRejected {
                        order_id: order_id.clone(),
                        reason: "Exchange rejected".to_string(),
                        timestamp,
                    }) {
                        warn!("訂單拒絕事件發送失敗: order_id={}, error={}", update.ord_id, e);
                        #[cfg(feature = "metrics")]
                        {
                            crate::metrics::EVENT_SEND_ERRORS
                                .with_label_values(&["order_rejected_send_failed"])
                                .inc();
                        }
                    }
                }
                "filled" | "partially_filled" => {
                    // 全部成交或部分成交狀態更新 (僅記錄狀態變化，不發送 OrderCompleted)
                    // OrderCompleted 事件由 OMS 層基於累計 Fill 事件生成
                    debug!("訂單成交狀態更新: order_id={}, state={}, fill_size={}", 
                           update.ord_id, update.state, update.fill_sz);
                    
                    // 注意：這裡不再直接發送 OrderCompleted 事件
                    // 語義修復：適配器只發送 Fill 事件，OrderCompleted 由 OMS 基於累計成交量判斷並派生
                }
                _ => {
                    debug!("未處理的訂單狀態: {}", update.state);
                }
            }
        }
        
        Ok(())
    }
    
    /// 處理成交回報
    ///
    /// 注意：此方法在獨立的 WebSocket 任務中被呼叫，無法取得 &mut self。
    /// 為了簡化並確保可編譯，這裡採用無狀態處理（暫不做去重）。
    async fn handle_fill_update(
        data: &serde_json::Value,
        event_tx: &broadcast::Sender<ExecutionEvent>,
    ) -> HftResult<()> {
        // 數據可能是數組格式
        let fills: Vec<BitgetFillUpdate> = if data.is_array() {
            serde_json::from_value(data.clone())
                .map_err(|e| HftError::Serialization(e.to_string()))?
        } else {
            vec![serde_json::from_value(data.clone())
                .map_err(|e| HftError::Serialization(e.to_string()))?]
        };
        
        for fill in fills {
            let order_id = OrderId(fill.ord_id.clone());
            
            // 精確解析時間戳
            let timestamp = match Self::parse_timestamp(&fill.fill_time) {
                Ok(ts) => ts,
                Err(e) => {
                    error!("成交回報時間戳解析失敗: order_id={}, fill_time={}, error={}, payload={:?}", 
                           fill.ord_id, fill.fill_time, e, fill);
                    continue; // 跳過此條記錄，不發送事件
                }
            };
            
            // 精度保護：直接從字符串解析，避免二次轉換損失
            let fill_price = match Price::from_str(&fill.price) {
                Ok(price) => price,
                Err(e) => {
                    error!("成交價格解析失敗: order_id={}, price={}, error={}, payload={:?}", 
                           fill.ord_id, fill.price, e, fill);
                    // 記錄指標
                    #[cfg(feature = "metrics")]
                    {
                        crate::metrics::FILL_PARSE_ERRORS
                            .with_label_values(&["price_parse_error"])
                            .inc();
                    }
                    continue; // 不發送事件
                }
            };
            
            let fill_quantity = match Quantity::from_str(&fill.size) {
                Ok(qty) => qty,
                Err(e) => {
                    error!("成交數量解析失敗: order_id={}, size={}, error={}, payload={:?}", 
                           fill.ord_id, fill.size, e, fill);
                    // 記錄指標
                    #[cfg(feature = "metrics")]
                    {
                        crate::metrics::FILL_PARSE_ERRORS
                            .with_label_values(&["quantity_parse_error"])
                            .inc();
                    }
                    continue; // 不發送事件
                }
            };
            
            info!("成交回報: order_id={}, trade_id={}, price={}, qty={}, symbol={}", 
                  fill.ord_id, fill.trade_id, fill_price.0, fill_quantity.0, fill.inst_id);
            
            // 發送成交事件，處理發送失敗
            if let Err(e) = event_tx.send(ExecutionEvent::Fill {
                order_id,
                price: fill_price,
                quantity: fill_quantity,
                timestamp,
                fill_id: fill.trade_id,
            }) {
                warn!("成交事件發送失敗: order_id={}, error={}", fill.ord_id, e);
                #[cfg(feature = "metrics")]
                {
                    crate::metrics::EVENT_SEND_ERRORS
                        .with_label_values(&["fill_event_send_failed"])
                        .inc();
                }
            }
        }
        
        Ok(())
    }
    
    /// 解析時間戳字符串為統一時間戳
    fn parse_unified_timestamp(time_str: &str) -> HftResult<UnifiedTimestamp> {
        let start_time = std::time::Instant::now();
        
        // Bitget 時間戳通常是毫秒格式
        let millis = time_str.parse::<u64>()
            .map_err(|e| HftError::Serialization(format!("無效的時間戳: {} - {}", time_str, e)))?;
        
        let exchange_ts = millis * 1000; // 轉換為微秒
        let unified = UnifiedTimestamp::auto(exchange_ts);
        
        // 驗證時間戳合理性
        if !unified.validate() {
            return Err(HftError::Serialization(format!(
                "時間戳驗證失敗: exchange_ts={}, local_ts={}", 
                unified.exchange_ts, unified.local_ts
            )));
        }
        
        // 記錄轉換延遲指標
        #[cfg(feature = "metrics")]
        {
            let conversion_time = start_time.elapsed().as_micros() as f64;
            crate::metrics::TIMESTAMP_CONVERSION_LATENCY
                .with_label_values(&["unified_ts"])
                .observe(conversion_time);
        }
        
        Ok(unified)
    }
    
    /// 解析時間戳字符串為微秒 (向後兼容)
    fn parse_timestamp(time_str: &str) -> HftResult<Timestamp> {
        let unified = Self::parse_unified_timestamp(time_str)?;
        Ok(unified.primary_ts())
    }

    /// 獲取當前時間戳
    fn current_timestamp() -> Timestamp {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }
    
    /// 檢查是否為重複 Fill 事件
    fn is_duplicate_fill(&mut self, fill_id: &str) -> bool {
        // 定期清理緩存 (每5分鐘)
        let now = Self::current_timestamp();
        if now - self.last_cache_cleanup > 5 * 60 * 1_000_000 {
            self.cleanup_fill_cache();
        }
        
        self.fill_id_cache.contains(fill_id)
    }
    
    /// 添加 Fill ID 到緩存
    fn add_fill_to_cache(&mut self, fill_id: &str) {
        self.fill_id_cache.insert(fill_id.to_string());
        
        #[cfg(feature = "metrics")]
        {
            crate::metrics::FILL_DEDUPLICATION
                .with_label_values(&["cache_add"])
                .inc();
        }
    }
    
    /// 清理 Fill ID 緩存 (簡單的全量清理策略)
    /// 生產環境中可以改為基於時間戳的滑動窗口清理
    fn cleanup_fill_cache(&mut self) {
        let cache_size_before = self.fill_id_cache.len();
        
        // 簡單策略：清理一半最舊的記錄
        // 更精確的方式是維護 fill_id -> timestamp 映射，只清理超時的
        if cache_size_before > 1000 {
            let mut fill_ids: Vec<String> = self.fill_id_cache.iter().cloned().collect();
            fill_ids.sort(); // 字典序排序，近似時間序
            
            let keep_count = cache_size_before / 2;
            let keep_set: std::collections::HashSet<String> = 
                fill_ids.into_iter().skip(cache_size_before - keep_count).collect();
            
            self.fill_id_cache = keep_set;
        }
        
        self.last_cache_cleanup = Self::current_timestamp();
        
        let cache_size_after = self.fill_id_cache.len();
        info!("Fill ID 緩存清理完成: {} -> {}", cache_size_before, cache_size_after);
        
        #[cfg(feature = "metrics")]
        {
            crate::metrics::FILL_DEDUPLICATION
                .with_label_values(&["cache_cleanup"])
                .inc();
        }
    }

    /// 轉換 Side 枚舉
    fn convert_side(side: Side) -> String {
        match side {
            Side::Buy => "buy".to_string(),
            Side::Sell => "sell".to_string(),
        }
    }

    /// 轉換 OrderType 枚舉
    fn convert_order_type(order_type: OrderType) -> String {
        match order_type {
            OrderType::Market => "market".to_string(),
            OrderType::Limit => "limit".to_string(),
        }
    }

    /// 轉換 TimeInForce 枚舉
    fn convert_time_in_force(tif: TimeInForce) -> String {
        match tif {
            TimeInForce::GTC => "gtc".to_string(),
            TimeInForce::IOC => "ioc".to_string(),
            TimeInForce::FOK => "fok".to_string(),
        }
    }
}

#[async_trait]
impl ExecutionClient for BitgetExecutionClient {
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        // 調用帶校驗的版本，但不传入 VenueSpec
        self.place_order_with_spec(intent, None).await
    }
    
    async fn place_order_with_spec(
        &mut self, 
        intent: OrderIntent, 
        venue_spec: Option<&VenueSpec>
    ) -> HftResult<OrderId> {
        // 先進行 VenueSpec 校驗
        self.validate_order_with_spec(&intent, venue_spec)?;
        
        // 然後執行下單逻輯
        if self.config.mode == ExecutionMode::Paper {
            // 模擬交易模式：立即返回虛擬訂單 ID
            let order_id = format!("PAPER_{}", Self::current_timestamp());
            info!("模擬下單: {} {} {} @ {:?}", 
                  intent.symbol.0, 
                  Self::convert_side(intent.side),
                  intent.quantity.0,
                  intent.price.map(|p| p.0));
            
            // 模擬發送確認事件
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(ExecutionEvent::OrderAck {
                    order_id: OrderId(order_id.clone()),
                    timestamp: Self::current_timestamp(),
                });
                // 延遲發送模擬成交事件（全額成交）。
                // 價格應由引擎層在 Market 單時補全為頂檔價格。
                let tx2 = tx.clone();
                let q = intent.quantity;
                let maybe_p = intent.price;
                let oid = order_id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    if let Some(p) = maybe_p {
                        info!("模擬成交: order_id={}, qty={}, price={}", oid, q.0, p.0);
                        let _ = tx2.send(ExecutionEvent::Fill {
                            order_id: OrderId(oid),
                            price: p,
                            quantity: q,
                            timestamp: Self::current_timestamp(),
                            fill_id: format!("FILL_{}", Self::current_timestamp()),
                        });
                    } else {
                        warn!("模擬成交跳過：缺少價格（請確認引擎已為 Market 單補全頂檔價格） order_id={}", oid);
                    }
                });
            }
            
            return Ok(OrderId(order_id));
        }

        // 真實交易模式
        if self.http_client.is_none() {
            self.init_http_client()?;
        }
        
        let http_client = self.http_client.as_ref().unwrap();
        
        let request = PlaceOrderRequest {
            symbol: intent.symbol.0.clone(),
            side: Self::convert_side(intent.side),
            order_type: Self::convert_order_type(intent.order_type),
            force: Self::convert_time_in_force(intent.time_in_force),
            price: intent.price.map(|p| p.0.to_string()).unwrap_or_default(),
            size: intent.quantity.0.to_string(),
            client_order_id: Self::generate_client_order_id(),
        };
        
        let body = serde_json::to_string(&request)
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        
        let headers = self.signer.generate_headers(
            "POST",
            "/api/v2/spot/trade/place-order",
            &body,
            None,
        );
        
        let response = http_client.post("/api/v2/spot/trade/place-order", Some(headers), &request).await
            .map_err(|e| HftError::Network(e.to_string()))?;
        
        let result: PlaceOrderResponse = HttpClient::parse_json(response).await
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        
        if result.code == "00000" {
            if let Some(data) = result.data {
                // 存儲客戶端訂單 ID 映射
                self.client_order_mapping.insert(
                    data.client_order_id.clone(),
                    data.order_id.clone(),
                );
                
                // 存儲訂單記錄
                self.order_records.insert(
                    data.order_id.clone(),
                    OrderRecord {
                        symbol: intent.symbol.0.clone(),
                        client_order_id: data.client_order_id.clone(),
                        side: intent.side,
                        quantity: intent.quantity,
                        price: intent.price,
                        timestamp: Self::current_timestamp(),
                    },
                );
                
                info!("下單成功: order_id={}, client_order_id={}", 
                      data.order_id, data.client_order_id);
                Ok(OrderId(data.order_id))
            } else {
                Err(HftError::Exchange("下單響應缺少數據".to_string()))
            }
        } else {
            Err(Self::classify_error(&result.code, &result.msg))
        }
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        if self.config.mode == ExecutionMode::Paper {
            info!("模擬撤單: {}", order_id.0);
            
            // 模擬發送撤單確認事件
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(ExecutionEvent::OrderCanceled {
                    order_id: order_id.clone(),
                    timestamp: Self::current_timestamp(),
                });
            }
            
            return Ok(());
        }
        
        if self.http_client.is_none() {
            self.init_http_client()?;
        }
        
        let http_client = self.http_client.as_ref().unwrap();
        
        // 撤單操作帶重試
        let result = self.retry_with_backoff(|| async {
            // 假設我們有 symbol 資訊，這裡需要從某處獲取
            // 實際實現中可能需要維護 order_id -> symbol 的映射
            // 從訂單記錄中獲取 symbol
            let symbol = self.order_records
                .get(&order_id.0)
                .map(|record| record.symbol.clone())
                .unwrap_or_else(|| "BTCUSDT".to_string()); // 備用默認值
                
            let request = CancelOrderRequest {
                symbol,
                order_id: Some(order_id.0.clone()),
                client_order_id: None,
            };
            
            let body = serde_json::to_string(&request)
                .map_err(|e| HftError::Serialization(e.to_string()))?;
            
            let headers = self.signer.generate_headers(
                "POST",
                "/api/v2/spot/trade/cancel-order",
                &body,
                None,
            );
            
            let response = http_client.post(
                "/api/v2/spot/trade/cancel-order", 
                Some(headers), 
                &request
            ).await
                .map_err(|e| HftError::Network(e.to_string()))?;
            
            let result: BitgetApiResponse = HttpClient::parse_json(response).await
                .map_err(|e| HftError::Serialization(e.to_string()))?;
            
            if result.code == "00000" {
                info!("撤單成功: order_id={}", order_id.0);
                
                // 發送撤單確認事件
                if let Some(ref tx) = self.event_tx {
                    let _ = tx.send(ExecutionEvent::OrderCanceled {
                        order_id: order_id.clone(),
                        timestamp: Self::current_timestamp(),
                    });
                }
                
                Ok(())
            } else {
                Err(Self::classify_error(&result.code, &result.msg))
            }
        }, 3).await; // 最多重試 3 次
        
        result
    }

    async fn modify_order(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> HftResult<()> {
        if self.config.mode == ExecutionMode::Paper {
            info!("模擬修改訂單: {} - 數量: {:?}, 價格: {:?}", 
                  order_id.0, new_quantity, new_price);
            
            // 模擬發送修改確認事件
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(ExecutionEvent::OrderModified {
                    order_id: order_id.clone(),
                    new_quantity,
                    new_price,
                    timestamp: Self::current_timestamp(),
                });
            }
            
            return Ok(());
        }
        
        if self.http_client.is_none() {
            self.init_http_client()?;
        }
        
        let http_client = self.http_client.as_ref().unwrap();
        
        // 修改訂單操作帶重試
        let result = self.retry_with_backoff(|| async {
            // 從訂單記錄中獲取 symbol
            let symbol = self.order_records
                .get(&order_id.0)
                .map(|record| record.symbol.clone())
                .unwrap_or_else(|| "BTCUSDT".to_string()); // 備用默認值
                
            let request = ModifyOrderRequest {
                symbol,
                order_id: order_id.0.clone(),
                client_order_id: None,
                new_size: new_quantity.map(|q| q.0.to_string()),
                price: new_price.map(|p| p.0.to_string()),
            };
            
            let body = serde_json::to_string(&request)
                .map_err(|e| HftError::Serialization(e.to_string()))?;
            
            let headers = self.signer.generate_headers(
                "POST",
                "/api/v2/spot/trade/modify-order",
                &body,
                None,
            );
            
            let response = http_client.post(
                "/api/v2/spot/trade/modify-order", 
                Some(headers), 
                &request
            ).await
                .map_err(|e| HftError::Network(e.to_string()))?;
            
            let result: BitgetApiResponse = HttpClient::parse_json(response).await
                .map_err(|e| HftError::Serialization(e.to_string()))?;
            
            if result.code == "00000" {
                info!("修改訂單成功: order_id={}, new_qty={:?}, new_price={:?}", 
                      order_id.0, new_quantity, new_price);
                
                // 發送修改確認事件
                if let Some(ref tx) = self.event_tx {
                    let _ = tx.send(ExecutionEvent::OrderModified {
                        order_id: order_id.clone(),
                        new_quantity,
                        new_price,
                        timestamp: Self::current_timestamp(),
                    });
                }
                
                Ok(())
            } else {
                Err(Self::classify_error(&result.code, &result.msg))
            }
        }, 3).await; // 最多重試 3 次
        
        result
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        if let Some(ref tx) = self.event_tx {
            let rx = tx.subscribe();
            let stream = tokio_stream::wrappers::BroadcastStream::new(rx)
                .filter_map(|result| async move {
                    match result {
                        Ok(event) => Some(Ok(event)),
                        Err(e) => {
                            error!("執行事件流錯誤: {}", e);
                            None
                        }
                    }
                });
            
            Ok(Box::pin(stream))
        } else {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        info!("連接 Bitget 執行客戶端");
        
        self.init_http_client()?;
        
        // 創建事件廣播通道
        let (tx, _) = broadcast::channel(1000);
        self.event_tx = Some(tx.clone());
        
        // 啟動私有 WebSocket（僅在 Live 模式）
        if self.config.mode == ExecutionMode::Live {
            self.start_private_websocket(tx).await?;
        }
        
        self.connected = true;
        self.last_heartbeat = Self::current_timestamp();
        
        info!("Bitget 執行客戶端連接成功 ({}模式)", 
              if self.config.mode == ExecutionMode::Live { "真實" } else { "模擬" });
        
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        info!("斷開 Bitget 執行客戶端");
        
        if let Some(handle) = self.ws_handle.take() {
            handle.abort();
        }
        
        self.connected = false;
        self.event_tx = None;
        
        Ok(())
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        // 模擬交易模式下返回空列表（沒有真實掛單）
        if self.config.mode == ExecutionMode::Paper {
            return Ok(Vec::new());
        }
        // 真實交易模式：調用 Bitget V2 REST API 獲取未結訂單（SPOT）
        // 端點: GET /api/v2/spot/trade/unfilled-orders
        // 規則: v2 統一 symbol 命名，查詢參數支持 idLessThan + limit 翻頁；此處先抓取前 100 條
        // 直接臨時構建 HTTP 客戶端（list 查詢不需要持久連線）
        let http_cfg = HttpClientConfig {
            base_url: self.config.rest_base_url.clone(),
            timeout_ms: self.config.timeout_ms,
            user_agent: "hft-bitget-exec/1.0".to_string(),
        };
        let http = HttpClient::new(http_cfg).map_err(|e| HftError::Network(e.to_string()))?;

        // 構建帶查詢的請求路徑（v2 建議用 idLessThan+limit 翻頁；此處只取前 100 條）
        let path = "/api/v2/spot/trade/unfilled-orders?limit=100";
        let headers = self.signer.generate_headers("GET", path, "", None);

        let resp = http.get(path, Some(headers)).await
            .map_err(|e| HftError::Network(e.to_string()))?;

        #[derive(Debug, Deserialize)]
        struct ApiResp<T> { code: String, msg: String, data: Option<T> }

        #[derive(Debug, Deserialize)]
        struct RestOpenOrder {
            // v2 統一字段（含常見別名方便兼容）
            #[serde(alias = "symbol", alias = "instId")]
            symbol: Option<String>,
            #[serde(alias = "orderId", alias = "ordId")]
            order_id: String,
            #[serde(alias = "clientOrderId", alias = "clOrdId")]
            client_order_id: Option<String>,
            #[serde(alias = "side")]
            side: Option<String>,
            #[serde(alias = "orderType", alias = "ordType")]
            order_type: Option<String>,
            #[serde(alias = "price", alias = "px")]
            price: Option<String>,
            #[serde(alias = "quantity", alias = "sz")]
            quantity: Option<String>,
            #[serde(alias = "filledQuantity", alias = "fillSz")]
            filled_quantity: Option<String>,
            #[serde(alias = "status", alias = "state")]
            status: Option<String>,
            #[serde(alias = "createTime", alias = "cTime")]
            c_time: Option<String>,
            #[serde(alias = "updateTime", alias = "uTime")]
            u_time: Option<String>,
        }

        let parsed: ApiResp<Vec<RestOpenOrder>> = HttpClient::parse_json(resp).await
            .map_err(|e| HftError::Serialization(e.to_string()))?;

        if parsed.code != "00000" {
            return Err(Self::classify_error(&parsed.code, &parsed.msg));
        }

        let mut out = Vec::new();
        let mut spot_min_id: Option<u64> = None;
        if let Some(items) = parsed.data {
            for it in items {
                let symbol = it.symbol.unwrap_or_else(|| "BTCUSDT".to_string());

                // 轉換 side / type
                let side = match it.side.as_deref() {
                    Some("buy") => Side::Buy,
                    Some("sell") => Side::Sell,
                    _ => Side::Buy,
                };
                let order_type = match it.order_type.as_deref() {
                    Some("market") => OrderType::Market,
                    _ => OrderType::Limit,
                };

                // 轉數量/價格
                let qty = Quantity::from_str(it.quantity.as_deref().unwrap_or("0")).unwrap_or(Quantity::zero());
                let filled = Quantity::from_str(it.filled_quantity.as_deref().unwrap_or("0")).unwrap_or(Quantity::zero());
                let remaining = Quantity(qty.0 - filled.0);
                let price = match &it.price { Some(s) => Price::from_str(s).ok(), None => None };

                // 狀態映射
                let status = match it.status.as_deref() {
                    Some("partially_filled") => ports::OrderStatus::PartiallyFilled,
                    Some("filled") => ports::OrderStatus::Filled,
                    Some("cancelled") | Some("canceled") => ports::OrderStatus::Canceled,
                    Some("rejected") => ports::OrderStatus::Rejected,
                    Some("accepted") | Some("new") => ports::OrderStatus::Accepted,
                    _ => ports::OrderStatus::New,
                };

                // 時間戳（毫秒/微秒兼容，這裡一律按毫秒解析再轉微秒）
                let parse_ts = |s: &Option<String>| -> Timestamp {
                    s.as_ref()
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|ms| ms * 1000)
                        .unwrap_or_else(Self::current_timestamp)
                };
                let created_at = parse_ts(&it.c_time);
                let updated_at = parse_ts(&it.u_time);

                // 計算最小游標 id（嘗試從 order_id 或 client_order_id 解析數值）
                if let Ok(idv) = it.order_id.parse::<u64>() {
                    spot_min_id = Some(spot_min_id.map(|m| m.min(idv)).unwrap_or(idv));
                } else if let Some(cid) = &it.client_order_id {
                    if let Ok(idv) = cid.parse::<u64>() {
                        spot_min_id = Some(spot_min_id.map(|m| m.min(idv)).unwrap_or(idv));
                    }
                }

                out.push(OpenOrder {
                    order_id: OrderId(it.order_id),
                    symbol: hft_core::Symbol(symbol),
                    side,
                    order_type,
                    original_quantity: qty,
                    remaining_quantity: remaining,
                    filled_quantity: filled,
                    price,
                    status,
                    created_at: created_at,
                    updated_at: updated_at,
                });
            }
        }

        // 追加 Spot 游標翻頁（最多 9 次）
        for _ in 0..9 {
            let id = match spot_min_id { Some(v) => v, None => break };
            let next_path = format!("/api/v2/spot/trade/unfilled-orders?limit=100&idLessThan={}", id);
            let headers = self.signer.generate_headers("GET", &next_path, "", None);
            let resp = http.get(&next_path, Some(headers)).await
                .map_err(|e| HftError::Network(e.to_string()))?;
            let parsed: ApiResp<Vec<RestOpenOrder>> = HttpClient::parse_json(resp).await
                .map_err(|e| HftError::Serialization(e.to_string()))?;
            if parsed.code != "00000" { break; }
            let batch = parsed.data.unwrap_or_default();
            if batch.is_empty() { break; }
            for it in batch {
                let symbol = it.symbol.clone().unwrap_or_else(|| "BTCUSDT".to_string());
                let side = match it.side.as_deref() { Some("buy") => Side::Buy, Some("sell") => Side::Sell, _ => Side::Buy };
                let order_type = match it.order_type.as_deref() { Some("market") => OrderType::Market, _ => OrderType::Limit };
                let qty = Quantity::from_str(it.quantity.as_deref().unwrap_or("0")).unwrap_or(Quantity::zero());
                let filled = Quantity::from_str(it.filled_quantity.as_deref().unwrap_or("0")).unwrap_or(Quantity::zero());
                let remaining = Quantity(qty.0 - filled.0);
                let price = match &it.price { Some(s) => Price::from_str(s).ok(), None => None };
                let status = match it.status.as_deref() {
                    Some("partially_filled") => ports::OrderStatus::PartiallyFilled,
                    Some("filled") => ports::OrderStatus::Filled,
                    Some("cancelled") | Some("canceled") => ports::OrderStatus::Canceled,
                    Some("rejected") => ports::OrderStatus::Rejected,
                    Some("accepted") | Some("new") => ports::OrderStatus::Accepted,
                    _ => ports::OrderStatus::New,
                };
                let parse_ts = |s: &Option<String>| -> Timestamp { s.as_ref().and_then(|v| v.parse::<u64>().ok()).map(|ms| ms * 1000).unwrap_or_else(Self::current_timestamp) };
                let created_at = parse_ts(&it.c_time);
                let updated_at = parse_ts(&it.u_time);

                if let Ok(idv) = it.order_id.parse::<u64>() { spot_min_id = Some(spot_min_id.map(|m| m.min(idv)).unwrap_or(idv)); }
                else if let Some(cid) = &it.client_order_id { if let Ok(idv) = cid.parse::<u64>() { spot_min_id = Some(spot_min_id.map(|m| m.min(idv)).unwrap_or(idv)); } }

                out.push(OpenOrder {
                    order_id: OrderId(it.order_id),
                    symbol: hft_core::Symbol(symbol),
                    side,
                    order_type,
                    original_quantity: qty,
                    remaining_quantity: remaining,
                    filled_quantity: filled,
                    price,
                    status,
                    created_at,
                    updated_at,
                });
            }
        }

        // 追加 Mix（合約）未結訂單（v2）
        // 端點: GET /api/v2/mix/order/orders-pending
        // 注意：若後端仍需 productType，則需根據具體合約類型傳遞；此處先按 v2 說明僅用 limit
        let mix_path = "/api/v2/mix/order/orders-pending?limit=100";
        let mix_headers = self.signer.generate_headers("GET", mix_path, "", None);
        let mix_resp = http.get(mix_path, Some(mix_headers)).await
            .map_err(|e| HftError::Network(e.to_string()))?;

        #[derive(Debug, Deserialize)]
        struct MixApiResp<T> { code: String, msg: String, data: Option<T> }

        // 與現貨結構相似，沿用同一字段映射
        #[derive(Debug, Deserialize)]
        struct RestMixOpenOrder {
            #[serde(alias = "symbol", alias = "instId")]
            symbol: Option<String>,
            #[serde(alias = "orderId", alias = "ordId")]
            order_id: String,
            #[serde(alias = "clientOrderId", alias = "clOrdId")]
            client_order_id: Option<String>,
            #[serde(alias = "side")]
            side: Option<String>,
            #[serde(alias = "orderType", alias = "ordType")]
            order_type: Option<String>,
            #[serde(alias = "price", alias = "px")]
            price: Option<String>,
            #[serde(alias = "quantity", alias = "sz")]
            quantity: Option<String>,
            #[serde(alias = "filledQuantity", alias = "fillSz")]
            filled_quantity: Option<String>,
            #[serde(alias = "status", alias = "state")]
            status: Option<String>,
            #[serde(alias = "createTime", alias = "cTime")]
            c_time: Option<String>,
            #[serde(alias = "updateTime", alias = "uTime")]
            u_time: Option<String>,
        }

        let mix_parsed: MixApiResp<Vec<RestMixOpenOrder>> = HttpClient::parse_json(mix_resp).await
            .map_err(|e| HftError::Serialization(e.to_string()))?;

        if mix_parsed.code == "00000" {
            let mut mix_min_id: Option<u64> = None;
            if let Some(items) = mix_parsed.data {
                for it in items {
                    let symbol = it.symbol.unwrap_or_else(|| "BTCUSDT".to_string());
                    let side = match it.side.as_deref() {
                        Some("buy") => Side::Buy,
                        Some("sell") => Side::Sell,
                        _ => Side::Buy,
                    };
                    let order_type = match it.order_type.as_deref() {
                        Some("market") => OrderType::Market,
                        _ => OrderType::Limit,
                    };
                    let qty = Quantity::from_str(it.quantity.as_deref().unwrap_or("0")).unwrap_or(Quantity::zero());
                    let filled = Quantity::from_str(it.filled_quantity.as_deref().unwrap_or("0")).unwrap_or(Quantity::zero());
                    let remaining = Quantity(qty.0 - filled.0);
                    let price = match &it.price { Some(s) => Price::from_str(s).ok(), None => None };
                    let status = match it.status.as_deref() {
                        Some("partially_filled") => ports::OrderStatus::PartiallyFilled,
                        Some("filled") => ports::OrderStatus::Filled,
                        Some("cancelled") | Some("canceled") => ports::OrderStatus::Canceled,
                        Some("rejected") => ports::OrderStatus::Rejected,
                        Some("accepted") | Some("new") => ports::OrderStatus::Accepted,
                        _ => ports::OrderStatus::New,
                    };
                    let parse_ts = |s: &Option<String>| -> Timestamp {
                        s.as_ref()
                            .and_then(|v| v.parse::<u64>().ok())
                            .map(|ms| ms * 1000)
                            .unwrap_or_else(Self::current_timestamp)
                    };
                    let created_at = parse_ts(&it.c_time);
                    let updated_at = parse_ts(&it.u_time);

                    if let Ok(idv) = it.order_id.parse::<u64>() { mix_min_id = Some(mix_min_id.map(|m| m.min(idv)).unwrap_or(idv)); }
                    else if let Some(cid) = &it.client_order_id { if let Ok(idv) = cid.parse::<u64>() { mix_min_id = Some(mix_min_id.map(|m| m.min(idv)).unwrap_or(idv)); } }

                    out.push(OpenOrder {
                        order_id: OrderId(it.order_id),
                        symbol: hft_core::Symbol(symbol),
                        side,
                        order_type,
                        original_quantity: qty,
                        remaining_quantity: remaining,
                        filled_quantity: filled,
                        price,
                        status,
                        created_at,
                        updated_at,
                    });
                }
            }
            // 游標翻頁（最多 9 次）
            for _ in 0..9 {
                let id = match mix_min_id { Some(v) => v, None => break };
                let mix_path = format!("/api/v2/mix/order/orders-pending?limit=100&idLessThan={}", id);
                let mix_headers = self.signer.generate_headers("GET", &mix_path, "", None);
                let mix_resp = http.get(&mix_path, Some(mix_headers)).await
                    .map_err(|e| HftError::Network(e.to_string()))?;
                let mix_parsed: MixApiResp<Vec<RestMixOpenOrder>> = HttpClient::parse_json(mix_resp).await
                    .map_err(|e| HftError::Serialization(e.to_string()))?;
                if mix_parsed.code != "00000" { break; }
                let batch = mix_parsed.data.unwrap_or_default();
                if batch.is_empty() { break; }
                for it in batch {
                    let symbol = it.symbol.clone().unwrap_or_else(|| "BTCUSDT".to_string());
                    let side = match it.side.as_deref() { Some("buy") => Side::Buy, Some("sell") => Side::Sell, _ => Side::Buy };
                    let order_type = match it.order_type.as_deref() { Some("market") => OrderType::Market, _ => OrderType::Limit };
                    let qty = Quantity::from_str(it.quantity.as_deref().unwrap_or("0")).unwrap_or(Quantity::zero());
                    let filled = Quantity::from_str(it.filled_quantity.as_deref().unwrap_or("0")).unwrap_or(Quantity::zero());
                    let remaining = Quantity(qty.0 - filled.0);
                    let price = match &it.price { Some(s) => Price::from_str(s).ok(), None => None };
                    let status = match it.status.as_deref() {
                        Some("partially_filled") => ports::OrderStatus::PartiallyFilled,
                        Some("filled") => ports::OrderStatus::Filled,
                        Some("cancelled") | Some("canceled") => ports::OrderStatus::Canceled,
                        Some("rejected") => ports::OrderStatus::Rejected,
                        Some("accepted") | Some("new") => ports::OrderStatus::Accepted,
                        _ => ports::OrderStatus::New,
                    };
                    let parse_ts = |s: &Option<String>| -> Timestamp { s.as_ref().and_then(|v| v.parse::<u64>().ok()).map(|ms| ms * 1000).unwrap_or_else(Self::current_timestamp) };
                    let created_at = parse_ts(&it.c_time);
                    let updated_at = parse_ts(&it.u_time);

                    if let Ok(idv) = it.order_id.parse::<u64>() { mix_min_id = Some(mix_min_id.map(|m| m.min(idv)).unwrap_or(idv)); }
                    else if let Some(cid) = &it.client_order_id { if let Ok(idv) = cid.parse::<u64>() { mix_min_id = Some(mix_min_id.map(|m| m.min(idv)).unwrap_or(idv)); } }

                    out.push(OpenOrder {
                        order_id: OrderId(it.order_id),
                        symbol: hft_core::Symbol(symbol),
                        side,
                        order_type,
                        original_quantity: qty,
                        remaining_quantity: remaining,
                        filled_quantity: filled,
                        price,
                        status,
                        created_at,
                        updated_at,
                    });
                }
            }
        } else {
            warn!("Bitget mix 未結訂單查詢失敗: {} - {}", mix_parsed.code, mix_parsed.msg);
        }

        // 可選：基於環境變數過濾 symbol，便於測試
        if let Ok(filter_sym) = std::env::var("HFT_OPEN_ORDERS_SYMBOL") {
            out.retain(|o| o.symbol.0 == filter_sym);
        }

        Ok(out)
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.connected,
            latency_ms: Some(1.0), // TODO: 實際測量延遲
            last_heartbeat: self.last_heartbeat,
        }
    }
}
