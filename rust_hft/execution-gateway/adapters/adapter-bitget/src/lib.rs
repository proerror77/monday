//! Bitget 執行 adapter（實作 `ports::ExecutionClient`）
//! - REST 下單 + 私有 WS 回報 → 統一 ExecutionEvent
//!
//! 功能：
//! 1. REST API 下單、撤單、修改訂單
//! 2. 私有 WebSocket 接收成交回報、訂單狀態更新
//! 3. Live/Paper 模式切換
//! 4. 精度保護與錯誤處理
//! 5. 結構化可觀測性
//! 6. 韌性機制：重試、熔斷器、告警通知

#![allow(dead_code)]
use async_trait::async_trait;
use execution::{
    AlertCallback, CircuitBreakerConfig, CircuitState, ExecutionAlert, ExecutionAlertType,
    ExecutorStats, ResilientExecutor, RetryConfig,
};
// Re-export ExecutionMode for backwards compatibility
pub use execution::ExecutionMode;
use futures::{SinkExt, StreamExt};
use hft_core::{
    HftError, HftResult, OrderId, OrderType, Price, Quantity, Side, TimeInForce, Timestamp,
    UnifiedTimestamp,
};
use integration::{
    http::{HttpClient, HttpClientConfig},
    signing::{BitgetCredentials, BitgetSigner},
};
use ports::{
    AccountBalance, BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent, OpenOrder,
    OrderIntent, VenueSpec,
};
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, HftError> {
    let mut bytes = text.as_bytes().to_vec();
    simd_json::serde::from_slice(bytes.as_mut_slice())
        .map_err(|e| HftError::Serialization(e.to_string()))
}

fn parse_owned_value<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, HftError> {
    let owned: simd_json::OwnedValue = match value.try_into() {
        Ok(v) => v,
        Err(e) => return Err(HftError::Serialization(e.to_string())),
    };
    simd_json::serde::from_owned_value(owned).map_err(|e| HftError::Serialization(e.to_string()))
}

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
    side: String,       // "buy" or "sell"
    order_type: String, // "limit", "market"
    force: String,      // "gtc", "ioc", "fok", "post_only"
    price: String,
    size: String,
    client_order_id: String,
}

/// Bitget 撤單請求
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelOrderRequest {
    symbol: String,
    order_id: Option<String>,
    client_order_id: Option<String>,
}

/// Bitget 修改訂單請求
#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Deserialize)]
struct BitgetRestResponse<T> {
    code: String,
    msg: String,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct BitgetRestOpenOrder {
    #[serde(alias = "symbol", alias = "instId")]
    symbol: Option<String>,
    #[serde(alias = "orderId", alias = "ordId")]
    order_id: String,
    #[serde(alias = "clientOrderId", alias = "clOrdId")]
    client_order_id: Option<String>,
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

#[derive(Debug, Deserialize)]
struct BitgetSpotAssetInfo {
    #[serde(alias = "coin", alias = "coinName")]
    coin: String,
    available: String,
    #[serde(alias = "frozen", alias = "lock", alias = "locked")]
    frozen: String,
    #[serde(alias = "usdtValue")]
    usdt_value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BitgetMixAccountInfo {
    #[serde(alias = "marginCoin")]
    margin_coin: String,
    available: String,
    #[serde(alias = "frozen", alias = "locked")]
    frozen: String,
    equity: String,
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
    #[allow(dead_code)]
    fill_id_cache: std::collections::HashSet<String>,
    // 緩存最後清理時間 (用於定期清理舊的 fill_id)
    last_cache_cleanup: Timestamp,
    // WebSocket 延遲測量
    last_ping_sent: std::sync::Arc<std::sync::Mutex<Option<Timestamp>>>,
    measured_latency_ms: std::sync::Arc<std::sync::Mutex<Option<f64>>>,
    // 韌性執行器 (重試 + 熔斷器)
    resilient_executor: Option<Arc<ResilientExecutor>>,
    // 告警回調
    alert_callback: Option<AlertCallback>,
}

fn bitget_mode_label(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Paper => "Paper",
        ExecutionMode::Live => "Live",
        ExecutionMode::Testnet => "Testnet",
    }
}

fn parse_bitget_side(value: &str, source: &str) -> HftResult<Side> {
    match value {
        "buy" => Ok(Side::Buy),
        "sell" => Ok(Side::Sell),
        _ => Err(HftError::Parse(format!(
            "Bitget {} 未知 side: {}",
            source, value
        ))),
    }
}

fn parse_bitget_order_type(value: &str, source: &str) -> HftResult<OrderType> {
    match value {
        "market" => Ok(OrderType::Market),
        "limit" => Ok(OrderType::Limit),
        _ => Err(HftError::Parse(format!(
            "Bitget {} 未知 orderType: {}",
            source, value
        ))),
    }
}

fn parse_bitget_order_status(value: &str, source: &str) -> HftResult<ports::OrderStatus> {
    match value {
        "accepted" => Ok(ports::OrderStatus::Accepted),
        "new" => Ok(ports::OrderStatus::New),
        "partially_filled" => Ok(ports::OrderStatus::PartiallyFilled),
        "filled" => Ok(ports::OrderStatus::Filled),
        "cancelled" | "canceled" => Ok(ports::OrderStatus::Canceled),
        "rejected" => Ok(ports::OrderStatus::Rejected),
        _ => Err(HftError::Parse(format!(
            "Bitget {} 未知 status: {}",
            source, value
        ))),
    }
}

fn parse_bitget_quantity(
    field: &str,
    value: &str,
    source: &str,
    allow_zero: bool,
) -> HftResult<Quantity> {
    let decimal = value.parse::<Decimal>().map_err(|e| {
        HftError::Parse(format!(
            "Bitget {} {} 解析失敗: {} ({})",
            source, field, value, e
        ))
    })?;
    if decimal < Decimal::ZERO || (!allow_zero && decimal.is_zero()) {
        return Err(HftError::Parse(format!(
            "Bitget {} {} 非法數量: {}",
            source, field, value
        )));
    }
    Ok(Quantity(decimal))
}

fn parse_bitget_price(
    value: Option<&str>,
    order_type: OrderType,
    source: &str,
) -> HftResult<Option<Price>> {
    match value {
        Some("") | None if order_type == OrderType::Market => Ok(None),
        Some("") | None => Err(HftError::Parse(format!(
            "Bitget {} limit order 缺少 price",
            source
        ))),
        Some(raw) => Price::from_str(raw).map(Some).map_err(|e| {
            HftError::Parse(format!("Bitget {} price 解析失敗: {} ({})", source, raw, e))
        }),
    }
}

fn parse_bitget_timestamp(field: &str, value: Option<&str>, source: &str) -> HftResult<Timestamp> {
    let raw = value.ok_or_else(|| HftError::Parse(format!("Bitget {} 缺少 {}", source, field)))?;
    raw.parse::<u64>().map(|ts_ms| ts_ms * 1000).map_err(|e| {
        HftError::Parse(format!(
            "Bitget {} {} 解析失敗: {} ({})",
            source, field, raw, e
        ))
    })
}

fn parse_bitget_decimal(field: &str, value: &str, source: &str) -> HftResult<Decimal> {
    value.parse::<Decimal>().map_err(|e| {
        HftError::Parse(format!(
            "Bitget {} {} 解析失敗: {} ({})",
            source, field, value, e
        ))
    })
}

fn parse_bitget_optional_decimal(
    field: &str,
    value: Option<&str>,
    source: &str,
) -> HftResult<Option<Decimal>> {
    match value {
        Some(raw) => parse_bitget_decimal(field, raw, source).map(Some),
        None => Ok(None),
    }
}

fn bitget_numeric_cursor(order_id: &str, client_order_id: Option<&str>) -> Option<u64> {
    order_id
        .parse::<u64>()
        .ok()
        .or_else(|| client_order_id.and_then(|value| value.parse::<u64>().ok()))
}

fn parse_bitget_open_order(
    item: BitgetRestOpenOrder,
    source: &str,
) -> HftResult<(OpenOrder, Option<u64>)> {
    let symbol = item
        .symbol
        .ok_or_else(|| HftError::Parse(format!("Bitget {} 缺少 symbol", source)))?;
    let side = parse_bitget_side(
        item.side
            .as_deref()
            .ok_or_else(|| HftError::Parse(format!("Bitget {} 缺少 side", source)))?,
        source,
    )?;
    let order_type = parse_bitget_order_type(
        item.order_type
            .as_deref()
            .ok_or_else(|| HftError::Parse(format!("Bitget {} 缺少 orderType", source)))?,
        source,
    )?;
    let qty = parse_bitget_quantity(
        "quantity",
        item.quantity
            .as_deref()
            .ok_or_else(|| HftError::Parse(format!("Bitget {} 缺少 quantity", source)))?,
        source,
        false,
    )?;
    let filled = parse_bitget_quantity(
        "filledQuantity",
        item.filled_quantity
            .as_deref()
            .ok_or_else(|| HftError::Parse(format!("Bitget {} 缺少 filledQuantity", source)))?,
        source,
        true,
    )?;
    if filled.0 > qty.0 {
        return Err(HftError::Parse(format!(
            "Bitget {} filledQuantity 大於 quantity: {} > {}",
            source, filled, qty
        )));
    }
    let status = parse_bitget_order_status(
        item.status
            .as_deref()
            .ok_or_else(|| HftError::Parse(format!("Bitget {} 缺少 status", source)))?,
        source,
    )?;
    let price = parse_bitget_price(item.price.as_deref(), order_type, source)?;
    let created_at = parse_bitget_timestamp("createTime", item.c_time.as_deref(), source)?;
    let updated_at = parse_bitget_timestamp("updateTime", item.u_time.as_deref(), source)?;
    let cursor = bitget_numeric_cursor(&item.order_id, item.client_order_id.as_deref());

    Ok((
        OpenOrder {
            order_id: OrderId(item.order_id),
            client_order_id: item.client_order_id,
            symbol: hft_core::Symbol::from(symbol),
            side,
            order_type,
            original_quantity: qty,
            remaining_quantity: Quantity(qty.0 - filled.0),
            filled_quantity: filled,
            price,
            status,
            created_at,
            updated_at,
        },
        cursor,
    ))
}

fn parse_bitget_open_orders_page(
    response: BitgetRestResponse<Vec<BitgetRestOpenOrder>>,
    source: &str,
    page_limit: usize,
) -> HftResult<(Vec<OpenOrder>, Option<u64>)> {
    if response.code != "00000" {
        return Err(HftError::Exchange(format!(
            "Bitget {} 查詢失敗: {} - {}",
            source, response.code, response.msg
        )));
    }

    let items = response
        .data
        .ok_or_else(|| HftError::Parse(format!("Bitget {} 回應缺少 data", source)))?;
    let page_len = items.len();
    let mut min_cursor: Option<u64> = None;
    let mut orders = Vec::with_capacity(page_len);
    for item in items {
        let (order, cursor) = parse_bitget_open_order(item, source)?;
        if let Some(id) = cursor {
            min_cursor = Some(min_cursor.map(|current| current.min(id)).unwrap_or(id));
        }
        orders.push(order);
    }

    if page_len == page_limit && min_cursor.is_none() {
        return Err(HftError::Parse(format!(
            "Bitget {} 無法解析翻頁游標",
            source
        )));
    }

    Ok((orders, min_cursor))
}

fn parse_bitget_spot_balances(
    response: BitgetRestResponse<Vec<BitgetSpotAssetInfo>>,
) -> HftResult<Vec<AccountBalance>> {
    if response.code != "00000" {
        return Err(HftError::Exchange(format!(
            "Bitget spot 餘額查詢失敗: {} - {}",
            response.code, response.msg
        )));
    }

    let assets = response
        .data
        .ok_or_else(|| HftError::Parse("Bitget spot 餘額回應缺少 data".to_string()))?;

    assets
        .into_iter()
        .map(|asset| {
            let available = parse_bitget_decimal("available", &asset.available, "spot balance")?;
            let frozen = parse_bitget_decimal("frozen", &asset.frozen, "spot balance")?;
            let usd_value = parse_bitget_optional_decimal(
                "usdtValue",
                asset.usdt_value.as_deref(),
                "spot balance",
            )?;
            let total = available + frozen;

            Ok(AccountBalance {
                asset: asset.coin,
                available,
                frozen,
                total,
                usd_value,
            })
        })
        .collect()
}

fn parse_bitget_mix_balances(
    response: BitgetRestResponse<Vec<BitgetMixAccountInfo>>,
) -> HftResult<Vec<AccountBalance>> {
    if response.code != "00000" {
        return Err(HftError::Exchange(format!(
            "Bitget mix 餘額查詢失敗: {} - {}",
            response.code, response.msg
        )));
    }

    let accounts = response
        .data
        .ok_or_else(|| HftError::Parse("Bitget mix 餘額回應缺少 data".to_string()))?;

    accounts
        .into_iter()
        .map(|account| {
            let available = parse_bitget_decimal("available", &account.available, "mix balance")?;
            let frozen = parse_bitget_decimal("frozen", &account.frozen, "mix balance")?;
            let total = parse_bitget_decimal("equity", &account.equity, "mix balance")?;

            Ok(AccountBalance {
                asset: format!("MIX:{}", account.margin_coin),
                available,
                frozen,
                total,
                usd_value: Some(total),
            })
        })
        .collect()
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
            last_ping_sent: std::sync::Arc::new(std::sync::Mutex::new(None)),
            measured_latency_ms: std::sync::Arc::new(std::sync::Mutex::new(None)),
            resilient_executor: None,
            alert_callback: None,
        })
    }

    /// 設置告警回調
    ///
    /// 告警回調會在以下情況被調用：
    /// - 初始化失敗
    /// - 連續失敗達到閾值
    /// - 熔斷器開啟/恢復
    /// - 重試耗盡
    ///
    /// # 範例
    /// ```ignore
    /// client.with_alert_callback(|alert| {
    ///     // 發送到 Redis ops.alert
    ///     redis.publish("ops.alert", alert.to_ops_alert_json());
    /// });
    /// ```
    pub fn with_alert_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(ExecutionAlert) + Send + Sync + 'static,
    {
        self.alert_callback = Some(Arc::new(callback));
        self
    }

    /// 獲取韌性執行器統計信息
    pub fn resilience_stats(&self) -> Option<ExecutorStats> {
        self.resilient_executor.as_ref().map(|e| e.stats())
    }

    /// 獲取熔斷器狀態
    pub async fn circuit_state(&self) -> Option<CircuitState> {
        if let Some(ref executor) = self.resilient_executor {
            Some(executor.circuit_breaker.state().await)
        } else {
            None
        }
    }

    /// 強制重置熔斷器
    pub async fn reset_circuit_breaker(&self) {
        if let Some(ref executor) = self.resilient_executor {
            executor.circuit_breaker.reset().await;
            info!("[Bitget] 熔斷器已手動重置");
        }
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

    /// 執行帶韌性保護的操作
    ///
    /// 如果韌性執行器已初始化，使用熔斷器 + 重試機制
    /// 否則直接執行操作
    async fn execute_with_resilience<T, F, Fut>(&self, operation: F) -> HftResult<T>
    where
        F: FnMut() -> Fut + Clone,
        Fut: std::future::Future<Output = HftResult<T>>,
    {
        if let Some(ref executor) = self.resilient_executor {
            executor.execute(operation).await
        } else {
            // 未初始化時直接執行
            let mut op = operation;
            op().await
        }
    }

    /// 發送執行告警
    fn send_execution_alert(&self, alert: ExecutionAlert) {
        if let Some(ref callback) = self.alert_callback {
            callback(alert);
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

                if (price_decimal % tick_decimal) != Decimal::ZERO {
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

        self.http_client =
            Some(HttpClient::new(http_config).map_err(|e| HftError::Network(e.to_string()))?);

        Ok(())
    }

    /// 獲取 HTTP 客戶端引用（確保已初始化）
    #[inline]
    fn get_http_client(&self) -> HftResult<&HttpClient> {
        self.http_client
            .as_ref()
            .ok_or_else(|| HftError::Execution("HTTP client not initialized".to_string()))
    }

    /// 啟動私有 WebSocket 連接
    async fn start_private_websocket(
        &mut self,
        event_tx: broadcast::Sender<ExecutionEvent>,
    ) -> HftResult<()> {
        let ws_url = self.config.ws_private_url.clone();
        let credentials = self.config.credentials.clone();
        let latency_tracker = self.measured_latency_ms.clone();

        info!("啟動 Bitget 私有 WebSocket: {}", ws_url);

        let handle = tokio::spawn(async move {
            if let Err(e) =
                Self::private_websocket_loop(ws_url, credentials, event_tx.clone(), latency_tracker)
                    .await
            {
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
        latency_tracker: std::sync::Arc<std::sync::Mutex<Option<f64>>>,
    ) -> HftResult<()> {
        let (ws_stream, _) = connect_async(ws_url.as_str())
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        // 嘗試登入私有 WS（Bitget 需要登入後才能收到 orders/fill）
        // 參考 REST 簽名規則生成 WS 登入所需簽名：timestamp + method + requestPath + body
        // 這裡使用 GET /user/verify 作為 requestPath（常見做法），body 為空字串
        // 注意：不同產品線可能有差異，後續可根據實際回應調整
        let ts_ms = integration::signing::BitgetSigner::current_timestamp();
        let signer = integration::signing::BitgetSigner::new(credentials.clone());
        let headers = signer.generate_headers("GET", "/user/verify", "", Some(ts_ms));
        let ts_str = headers
            .get("ACCESS-TIMESTAMP")
            .cloned()
            .unwrap_or_else(|| ts_ms.to_string());
        let sign = headers.get("ACCESS-SIGN").cloned().unwrap_or_default();
        let login_msg = serde_json::json!({
            "op": "login",
            "args": [{
                "apiKey": credentials.api_key,
                "passphrase": credentials.passphrase,
                "timestamp": ts_str,
                "sign": sign,
            }]
        });
        write
            .send(Message::Text(login_msg.to_string().into()))
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;

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

        write
            .send(Message::Text(sub_msg.into()))
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;

        info!("已訂閱 Bitget 私有頻道");

        // 處理消息
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Err(e) =
                        Self::handle_private_message(&text, &event_tx, &latency_tracker).await
                    {
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
        latency_tracker: &std::sync::Arc<std::sync::Mutex<Option<f64>>>,
    ) -> HftResult<()> {
        let message: PrivateWSMessage = parse_json(text)?;
        let recv_time = Self::current_timestamp();

        debug!("收到私有消息: {:?}", message);

        // 根據頻道類型處理不同事件
        if let Some(arg) = &message.arg {
            match arg.channel.as_str() {
                "orders" => {
                    // 訂單狀態更新
                    if let Some(data) = &message.data {
                        Self::handle_order_update(data, event_tx, recv_time, latency_tracker)
                            .await?;
                    }
                }
                "fill" => {
                    // 成交回報
                    if let Some(data) = &message.data {
                        Self::handle_fill_update(data, event_tx, recv_time, latency_tracker)
                            .await?;
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
        recv_time: Timestamp,
        latency_tracker: &std::sync::Arc<std::sync::Mutex<Option<f64>>>,
    ) -> HftResult<()> {
        // 數據可能是數組格式
        let updates: Vec<BitgetOrderUpdate> = if data.is_array() {
            parse_owned_value(data.clone())?
        } else {
            vec![parse_owned_value(data.clone())?]
        };

        for update in updates {
            let order_id = OrderId(update.ord_id.clone());
            let timestamp = Self::parse_timestamp(&update.u_time)?;

            // 計算並更新延遲 (服務器時間戳到本地接收時間)
            if timestamp > 0 && recv_time > timestamp {
                let latency_us = recv_time - timestamp;
                let latency_ms = latency_us as f64 / 1000.0;
                if let Ok(mut tracker) = latency_tracker.lock() {
                    // 使用指數移動平均 (EMA) 平滑延遲測量
                    *tracker = Some(match *tracker {
                        Some(prev) => prev * 0.9 + latency_ms * 0.1,
                        None => latency_ms,
                    });
                }
            }

            // 根據訂單狀態發送相應事件
            match update.state.as_str() {
                "new" => {
                    // 訂單確認 (ACK)
                    info!(
                        "訂單確認: order_id={}, symbol={}",
                        update.ord_id, update.inst_id
                    );

                    if let Err(e) = event_tx.send(ExecutionEvent::OrderAck {
                        order_id: order_id.clone(),
                        timestamp,
                    }) {
                        warn!(
                            "訂單確認事件發送失敗: order_id={}, error={}",
                            update.ord_id, e
                        );
                    }
                }
                "cancelled" => {
                    // 訂單取消確認
                    info!(
                        "訂單取消: order_id={}, symbol={}",
                        update.ord_id, update.inst_id
                    );

                    if let Err(e) = event_tx.send(ExecutionEvent::OrderCanceled {
                        order_id: order_id.clone(),
                        timestamp,
                    }) {
                        warn!(
                            "訂單取消事件發送失敗: order_id={}, error={}",
                            update.ord_id, e
                        );
                    }
                }
                "rejected" => {
                    // 訂單拒絕
                    warn!(
                        "訂單拒絕: order_id={}, symbol={}",
                        update.ord_id, update.inst_id
                    );

                    if let Err(e) = event_tx.send(ExecutionEvent::OrderReject {
                        order_id: order_id.clone(),
                        reason: "Exchange rejected".to_string(),
                        timestamp,
                    }) {
                        warn!(
                            "訂單拒絕事件發送失敗: order_id={}, error={}",
                            update.ord_id, e
                        );
                    }
                }
                "filled" | "partially_filled" => {
                    // 全部成交或部分成交狀態更新 (僅記錄狀態變化，不發送 OrderCompleted)
                    // OrderCompleted 事件由 OMS 層基於累計 Fill 事件生成
                    debug!(
                        "訂單成交狀態更新: order_id={}, state={}, fill_size={}",
                        update.ord_id, update.state, update.fill_sz
                    );

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
        recv_time: Timestamp,
        latency_tracker: &std::sync::Arc<std::sync::Mutex<Option<f64>>>,
    ) -> HftResult<()> {
        // 數據可能是數組格式
        let fills: Vec<BitgetFillUpdate> = if data.is_array() {
            parse_owned_value(data.clone())?
        } else {
            vec![parse_owned_value(data.clone())?]
        };

        for fill in fills {
            let order_id = OrderId(fill.ord_id.clone());

            // 精確解析時間戳
            let timestamp = match Self::parse_timestamp(&fill.fill_time) {
                Ok(ts) => ts,
                Err(e) => {
                    error!(
                        "成交回報時間戳解析失敗: order_id={}, fill_time={}, error={}, payload={:?}",
                        fill.ord_id, fill.fill_time, e, fill
                    );
                    continue; // 跳過此條記錄，不發送事件
                }
            };

            // 計算並更新延遲
            if timestamp > 0 && recv_time > timestamp {
                let latency_us = recv_time - timestamp;
                let latency_ms = latency_us as f64 / 1000.0;
                if let Ok(mut tracker) = latency_tracker.lock() {
                    *tracker = Some(match *tracker {
                        Some(prev) => prev * 0.9 + latency_ms * 0.1,
                        None => latency_ms,
                    });
                }
            }

            // 精度保護：直接從字符串解析，避免二次轉換損失
            let fill_price = match Price::from_str(&fill.price) {
                Ok(price) => price,
                Err(e) => {
                    error!(
                        "成交價格解析失敗: order_id={}, price={}, error={}, payload={:?}",
                        fill.ord_id, fill.price, e, fill
                    );
                    continue; // 不發送事件
                }
            };

            let fill_quantity = match Quantity::from_str(&fill.size) {
                Ok(qty) => qty,
                Err(e) => {
                    error!(
                        "成交數量解析失敗: order_id={}, size={}, error={}, payload={:?}",
                        fill.ord_id, fill.size, e, fill
                    );
                    continue; // 不發送事件
                }
            };

            info!(
                "成交回報: order_id={}, trade_id={}, price={}, qty={}, symbol={}",
                fill.ord_id, fill.trade_id, fill_price.0, fill_quantity.0, fill.inst_id
            );

            // 發送成交事件，處理發送失敗
            if let Err(e) = event_tx.send(ExecutionEvent::Fill {
                order_id,
                price: fill_price,
                quantity: fill_quantity,
                timestamp,
                fill_id: fill.trade_id,
            }) {
                warn!("成交事件發送失敗: order_id={}, error={}", fill.ord_id, e);
            }
        }

        Ok(())
    }

    /// 解析時間戳字符串為統一時間戳
    fn parse_unified_timestamp(time_str: &str) -> HftResult<UnifiedTimestamp> {
        // Bitget 時間戳通常是毫秒格式
        let millis = time_str
            .parse::<u64>()
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
    #[allow(dead_code)]
    fn is_duplicate_fill(&mut self, fill_id: &str) -> bool {
        // 定期清理緩存 (每5分鐘)
        let now = Self::current_timestamp();
        if now - self.last_cache_cleanup > 5 * 60 * 1_000_000 {
            self.cleanup_fill_cache();
        }

        self.fill_id_cache.contains(fill_id)
    }

    /// 添加 Fill ID 到緩存
    #[allow(dead_code)]
    fn add_fill_to_cache(&mut self, fill_id: &str) {
        self.fill_id_cache.insert(fill_id.to_string());
    }

    /// 清理 Fill ID 緩存 (簡單的全量清理策略)
    /// 生產環境中可以改為基於時間戳的滑動窗口清理
    #[allow(dead_code)]
    fn cleanup_fill_cache(&mut self) {
        let cache_size_before = self.fill_id_cache.len();

        // 簡單策略：清理一半最舊的記錄
        // 更精確的方式是維護 fill_id -> timestamp 映射，只清理超時的
        if cache_size_before > 1000 {
            let mut fill_ids: Vec<String> = self.fill_id_cache.iter().cloned().collect();
            fill_ids.sort(); // 字典序排序，近似時間序

            let keep_count = cache_size_before / 2;
            let keep_set: std::collections::HashSet<String> = fill_ids
                .into_iter()
                .skip(cache_size_before - keep_count)
                .collect();

            self.fill_id_cache = keep_set;
        }

        self.last_cache_cleanup = Self::current_timestamp();

        let cache_size_after = self.fill_id_cache.len();
        info!(
            "Fill ID 緩存清理完成: {} -> {}",
            cache_size_before, cache_size_after
        );
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
        venue_spec: Option<&VenueSpec>,
    ) -> HftResult<OrderId> {
        // 先進行 VenueSpec 校驗
        self.validate_order_with_spec(&intent, venue_spec)?;

        // 然後執行下單逻輯
        if self.config.mode == ExecutionMode::Paper {
            // 模擬交易模式：立即返回虛擬訂單 ID
            let order_id = format!("PAPER_{}", Self::current_timestamp());
            info!(
                "模擬下單: {} {} {} @ {:?}",
                intent.symbol.as_str(),
                Self::convert_side(intent.side),
                intent.quantity.0,
                intent.price.map(|p| p.0)
            );

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

        let http_client = self.get_http_client()?;

        let request = PlaceOrderRequest {
            symbol: intent.symbol.as_str().to_string(),
            side: Self::convert_side(intent.side),
            order_type: Self::convert_order_type(intent.order_type),
            force: Self::convert_time_in_force(intent.time_in_force),
            price: intent.price.map(|p| p.0.to_string()).unwrap_or_default(),
            size: intent.quantity.0.to_string(),
            client_order_id: Self::generate_client_order_id(),
        };

        let body =
            serde_json::to_string(&request).map_err(|e| HftError::Serialization(e.to_string()))?;

        let headers =
            self.signer
                .generate_headers("POST", "/api/v2/spot/trade/place-order", &body, None);

        let response = http_client
            .post("/api/v2/spot/trade/place-order", Some(headers), &request)
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;

        let result: PlaceOrderResponse = HttpClient::parse_json(response)
            .await
            .map_err(|e| HftError::Serialization(e.to_string()))?;

        if result.code == "00000" {
            if let Some(data) = result.data {
                // 存儲客戶端訂單 ID 映射
                self.client_order_mapping
                    .insert(data.client_order_id.clone(), data.order_id.clone());

                // 存儲訂單記錄
                self.order_records.insert(
                    data.order_id.clone(),
                    OrderRecord {
                        symbol: intent.symbol.as_str().to_string(),
                        client_order_id: data.client_order_id.clone(),
                        side: intent.side,
                        quantity: intent.quantity,
                        price: intent.price,
                        timestamp: Self::current_timestamp(),
                    },
                );

                info!(
                    "下單成功: order_id={}, client_order_id={}",
                    data.order_id, data.client_order_id
                );
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

        // 提前提取所有需要的數據，避免在閉包中借用 self
        let symbol = self
            .order_records
            .get(&order_id.0)
            .map(|record| record.symbol.clone())
            .unwrap_or_else(|| "BTCUSDT".to_string());

        let request = CancelOrderRequest {
            symbol,
            order_id: Some(order_id.0.clone()),
            client_order_id: None,
        };

        let body =
            serde_json::to_string(&request).map_err(|e| HftError::Serialization(e.to_string()))?;

        let headers =
            self.signer
                .generate_headers("POST", "/api/v2/spot/trade/cancel-order", &body, None);

        let http_client = self.get_http_client()?.clone();
        let order_id_clone = order_id.clone();
        let event_tx = self.event_tx.clone();

        // 使用韌性執行器執行撤單操作
        let result = self
            .execute_with_resilience(|| {
                let http = http_client.clone();
                let req = request.clone();
                let hdrs = headers.clone();
                let oid = order_id_clone.clone();
                let tx = event_tx.clone();

                async move {
                    let response = http
                        .post("/api/v2/spot/trade/cancel-order", Some(hdrs), &req)
                        .await
                        .map_err(|e| HftError::Network(e.to_string()))?;

                    let result: BitgetApiResponse = HttpClient::parse_json(response)
                        .await
                        .map_err(|e| HftError::Serialization(e.to_string()))?;

                    if result.code == "00000" {
                        info!("撤單成功: order_id={}", oid.0);

                        // 發送撤單確認事件
                        if let Some(ref tx) = tx {
                            let _ = tx.send(ExecutionEvent::OrderCanceled {
                                order_id: oid,
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_micros() as u64,
                            });
                        }

                        Ok(())
                    } else {
                        Err(BitgetExecutionClient::classify_error(
                            &result.code,
                            &result.msg,
                        ))
                    }
                }
            })
            .await;

        // 如果執行失敗且熔斷器開啟，發送告警
        if let Err(ref e) = result {
            if let Some(ref executor) = self.resilient_executor {
                if executor.circuit_breaker.state().await == CircuitState::Open {
                    self.send_execution_alert(
                        ExecutionAlert::new(
                            ExecutionAlertType::RetriesExhausted,
                            "bitget",
                            "cancel_order",
                            format!("撤單失敗: {}", e),
                        )
                        .with_error(e.to_string()),
                    );
                }
            }
        }

        result
    }

    async fn modify_order(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> HftResult<()> {
        if self.config.mode == ExecutionMode::Paper {
            info!(
                "模擬修改訂單: {} - 數量: {:?}, 價格: {:?}",
                order_id.0, new_quantity, new_price
            );

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

        // 提前提取所有需要的數據
        let symbol = self
            .order_records
            .get(&order_id.0)
            .map(|record| record.symbol.clone())
            .unwrap_or_else(|| "BTCUSDT".to_string());

        let request = ModifyOrderRequest {
            symbol,
            order_id: order_id.0.clone(),
            client_order_id: None,
            new_size: new_quantity.map(|q| q.0.to_string()),
            price: new_price.map(|p| p.0.to_string()),
        };

        let body =
            serde_json::to_string(&request).map_err(|e| HftError::Serialization(e.to_string()))?;

        let headers =
            self.signer
                .generate_headers("POST", "/api/v2/spot/trade/modify-order", &body, None);

        let http_client = self.get_http_client()?.clone();
        let order_id_clone = order_id.clone();
        let event_tx = self.event_tx.clone();

        // 使用韌性執行器執行修改操作
        let result = self
            .execute_with_resilience(|| {
                let http = http_client.clone();
                let req = request.clone();
                let hdrs = headers.clone();
                let oid = order_id_clone.clone();
                let tx = event_tx.clone();
                let qty = new_quantity;
                let px = new_price;

                async move {
                    let response = http
                        .post("/api/v2/spot/trade/modify-order", Some(hdrs), &req)
                        .await
                        .map_err(|e| HftError::Network(e.to_string()))?;

                    let result: BitgetApiResponse = HttpClient::parse_json(response)
                        .await
                        .map_err(|e| HftError::Serialization(e.to_string()))?;

                    if result.code == "00000" {
                        info!(
                            "修改訂單成功: order_id={}, new_qty={:?}, new_price={:?}",
                            oid.0, qty, px
                        );

                        // 發送修改確認事件
                        if let Some(ref tx) = tx {
                            let _ = tx.send(ExecutionEvent::OrderModified {
                                order_id: oid,
                                new_quantity: qty,
                                new_price: px,
                                timestamp: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_micros() as u64,
                            });
                        }

                        Ok(())
                    } else {
                        Err(BitgetExecutionClient::classify_error(
                            &result.code,
                            &result.msg,
                        ))
                    }
                }
            })
            .await;

        // 如果執行失敗且熔斷器開啟，發送告警
        if let Err(ref e) = result {
            if let Some(ref executor) = self.resilient_executor {
                if executor.circuit_breaker.state().await == CircuitState::Open {
                    self.send_execution_alert(
                        ExecutionAlert::new(
                            ExecutionAlertType::RetriesExhausted,
                            "bitget",
                            "modify_order",
                            format!("修改訂單失敗: {}", e),
                        )
                        .with_error(e.to_string()),
                    );
                }
            }
        }

        result
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        if let Some(ref tx) = self.event_tx {
            let rx = tx.subscribe();
            let stream =
                tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|result| async move {
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

        // 初始化韌性執行器
        let retry_config = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 100,
            max_delay_ms: 5000,
            backoff_multiplier: 2.0,
            retry_on_init_error: true,
        };
        let cb_config = CircuitBreakerConfig {
            failure_threshold: 5,
            open_duration_secs: 30,
            half_open_max_requests: 3,
            half_open_success_threshold: 2,
        };

        let mut executor = ResilientExecutor::new("bitget", retry_config, cb_config);

        // 設置熔斷器告警回調
        if let Some(ref alert_cb) = self.alert_callback {
            let alert_cb = Arc::clone(alert_cb);
            executor = executor.with_alert_callback(move |cb_alert| {
                let alert_type = match cb_alert.state {
                    CircuitState::Open => ExecutionAlertType::CircuitOpen,
                    CircuitState::Closed => ExecutionAlertType::CircuitRecovered,
                    CircuitState::HalfOpen => return, // 半開狀態不發送告警
                };

                let alert =
                    ExecutionAlert::new(alert_type, "bitget", "execution", &cb_alert.message)
                        .with_failure_count(cb_alert.failure_count);

                alert_cb(alert);
            });
        }

        self.resilient_executor = Some(Arc::new(executor));

        // 創建事件廣播通道
        let (tx, _) = broadcast::channel(1000);
        self.event_tx = Some(tx.clone());

        // 啟動私有 WebSocket（僅在 Live 模式）
        if self.config.mode == ExecutionMode::Live {
            self.start_private_websocket(tx).await?;
        }

        self.connected = true;
        self.last_heartbeat = Self::current_timestamp();

        info!(
            "Bitget 執行客戶端連接成功 ({}模式)",
            if self.config.mode == ExecutionMode::Live {
                "真實"
            } else {
                "模擬"
            }
        );

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
        if self.config.mode != ExecutionMode::Live {
            return Err(HftError::Config(format!(
                "Bitget list_open_orders 不支援 {} 模式",
                bitget_mode_label(self.config.mode)
            )));
        }

        let http_cfg = HttpClientConfig {
            base_url: self.config.rest_base_url.clone(),
            timeout_ms: self.config.timeout_ms,
            user_agent: "hft-bitget-exec/1.0".to_string(),
        };
        let http = HttpClient::new(http_cfg).map_err(|e| HftError::Network(e.to_string()))?;
        const PAGE_LIMIT: usize = 100;

        let path = "/api/v2/spot/trade/unfilled-orders?limit=100";
        let headers = self.signer.generate_headers("GET", path, "", None);

        let resp = http
            .get(path, Some(headers))
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;

        let parsed: BitgetRestResponse<Vec<BitgetRestOpenOrder>> = HttpClient::parse_json(resp)
            .await
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        let (mut out, mut spot_min_id) =
            parse_bitget_open_orders_page(parsed, "spot open orders", PAGE_LIMIT)?;

        while out.len() % PAGE_LIMIT == 0 && spot_min_id.is_some() {
            if out.is_empty() {
                break;
            }
            let id = match spot_min_id {
                Some(v) => v,
                None => break,
            };
            let next_path = format!(
                "/api/v2/spot/trade/unfilled-orders?limit=100&idLessThan={}",
                id
            );
            let headers = self.signer.generate_headers("GET", &next_path, "", None);
            let resp = http
                .get(&next_path, Some(headers))
                .await
                .map_err(|e| HftError::Network(e.to_string()))?;
            let parsed: BitgetRestResponse<Vec<BitgetRestOpenOrder>> = HttpClient::parse_json(resp)
                .await
                .map_err(|e| HftError::Serialization(e.to_string()))?;
            let (batch, next_cursor) =
                parse_bitget_open_orders_page(parsed, "spot open orders page", PAGE_LIMIT)?;
            if batch.is_empty() {
                break;
            }
            spot_min_id = next_cursor;
            out.extend(batch);
        }

        let mix_path = "/api/v2/mix/order/orders-pending?limit=100";
        let mix_headers = self.signer.generate_headers("GET", mix_path, "", None);
        let mix_resp = http
            .get(mix_path, Some(mix_headers))
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;

        let mix_parsed: BitgetRestResponse<Vec<BitgetRestOpenOrder>> =
            HttpClient::parse_json(mix_resp)
                .await
                .map_err(|e| HftError::Serialization(e.to_string()))?;
        let (mix_orders, mut mix_min_id) =
            parse_bitget_open_orders_page(mix_parsed, "mix open orders", PAGE_LIMIT)?;
        let mut mix_count = mix_orders.len();
        out.extend(mix_orders);

        while mix_count % PAGE_LIMIT == 0 && mix_min_id.is_some() {
            if mix_count == 0 {
                break;
            }
            let id = match mix_min_id {
                Some(v) => v,
                None => break,
            };
            let mix_path = format!(
                "/api/v2/mix/order/orders-pending?limit=100&idLessThan={}",
                id
            );
            let mix_headers = self.signer.generate_headers("GET", &mix_path, "", None);
            let mix_resp = http
                .get(&mix_path, Some(mix_headers))
                .await
                .map_err(|e| HftError::Network(e.to_string()))?;
            let mix_parsed: BitgetRestResponse<Vec<BitgetRestOpenOrder>> =
                HttpClient::parse_json(mix_resp)
                    .await
                    .map_err(|e| HftError::Serialization(e.to_string()))?;
            let (batch, next_cursor) =
                parse_bitget_open_orders_page(mix_parsed, "mix open orders page", PAGE_LIMIT)?;
            if batch.is_empty() {
                break;
            }
            mix_count = batch.len();
            mix_min_id = next_cursor;
            out.extend(batch);
        }

        if let Ok(filter_sym) = std::env::var("HFT_OPEN_ORDERS_SYMBOL") {
            out.retain(|o| o.symbol.as_str() == filter_sym);
        }

        Ok(out)
    }

    async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
        if self.config.mode != ExecutionMode::Live {
            return Err(HftError::Config(format!(
                "Bitget get_balance 不支援 {} 模式的權威快照",
                bitget_mode_label(self.config.mode)
            )));
        }

        let http_cfg = HttpClientConfig {
            base_url: self.config.rest_base_url.clone(),
            timeout_ms: self.config.timeout_ms,
            user_agent: "hft-bitget-exec/1.0".to_string(),
        };
        let http = HttpClient::new(http_cfg).map_err(|e| HftError::Network(e.to_string()))?;

        let path = "/api/v2/spot/account/assets";
        let headers = self.signer.generate_headers("GET", path, "", None);

        let resp = http
            .get(path, Some(headers))
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;

        let parsed: BitgetRestResponse<Vec<BitgetSpotAssetInfo>> = HttpClient::parse_json(resp)
            .await
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        let mut balances = parse_bitget_spot_balances(parsed)?;
        let mix_path = "/api/v2/mix/account/accounts?productType=USDT-FUTURES";
        let mix_headers = self.signer.generate_headers("GET", mix_path, "", None);
        let mix_resp = http
            .get(mix_path, Some(mix_headers))
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;
        let mix_parsed: BitgetRestResponse<Vec<BitgetMixAccountInfo>> =
            HttpClient::parse_json(mix_resp)
                .await
                .map_err(|e| HftError::Serialization(e.to_string()))?;
        balances.extend(parse_bitget_mix_balances(mix_parsed)?);

        info!("Bitget 餘額同步完成: {} 個資產", balances.len());
        Ok(balances)
    }

    async fn health(&self) -> ConnectionHealth {
        let latency_ms = self
            .measured_latency_ms
            .lock()
            .ok()
            .and_then(|guard| *guard);

        ConnectionHealth {
            connected: self.connected,
            latency_ms,
            last_heartbeat: self.last_heartbeat,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(mode: ExecutionMode) -> BitgetExecutionConfig {
        BitgetExecutionConfig {
            credentials: BitgetCredentials::new("key".into(), "secret".into(), "pass".into()),
            rest_base_url: "https://api.bitget.com".into(),
            ws_private_url: "wss://ws.bitget.com/v2/ws/private".into(),
            mode,
            timeout_ms: 1000,
        }
    }

    #[tokio::test]
    async fn test_list_open_orders_paper_mode_fails() {
        let client = BitgetExecutionClient::new(make_config(ExecutionMode::Paper)).unwrap();
        let result = client.list_open_orders().await;

        assert!(
            matches!(result, Err(HftError::Config(message)) if message.contains("不支援 Paper"))
        );
    }

    #[tokio::test]
    async fn test_get_balance_paper_mode_fails() {
        let client = BitgetExecutionClient::new(make_config(ExecutionMode::Paper)).unwrap();
        let result = client.get_balance().await;

        assert!(matches!(result, Err(HftError::Config(message)) if message.contains("權威快照")));
    }

    #[test]
    fn test_parse_bitget_open_orders_missing_data_fails() {
        let response = BitgetRestResponse::<Vec<BitgetRestOpenOrder>> {
            code: "00000".to_string(),
            msg: "success".to_string(),
            data: None,
        };

        let result = parse_bitget_open_orders_page(response, "spot open orders", 100);
        assert!(matches!(result, Err(HftError::Parse(message)) if message.contains("缺少 data")));
    }

    #[test]
    fn test_parse_bitget_open_orders_unknown_side_fails() {
        let response = BitgetRestResponse {
            code: "00000".to_string(),
            msg: "success".to_string(),
            data: Some(vec![BitgetRestOpenOrder {
                symbol: Some("BTCUSDT".to_string()),
                order_id: "1".to_string(),
                client_order_id: Some("2".to_string()),
                side: Some("hold".to_string()),
                order_type: Some("limit".to_string()),
                price: Some("100".to_string()),
                quantity: Some("1".to_string()),
                filled_quantity: Some("0".to_string()),
                status: Some("new".to_string()),
                c_time: Some("1".to_string()),
                u_time: Some("2".to_string()),
            }]),
        };

        let result = parse_bitget_open_orders_page(response, "spot open orders", 100);
        assert!(matches!(result, Err(HftError::Parse(message)) if message.contains("未知 side")));
    }

    #[test]
    fn test_parse_bitget_spot_balances_malformed_available_fails() {
        let response = BitgetRestResponse {
            code: "00000".to_string(),
            msg: "success".to_string(),
            data: Some(vec![BitgetSpotAssetInfo {
                coin: "USDT".to_string(),
                available: "oops".to_string(),
                frozen: "1".to_string(),
                usdt_value: Some("1".to_string()),
            }]),
        };

        let result = parse_bitget_spot_balances(response);
        assert!(matches!(result, Err(HftError::Parse(message)) if message.contains("available")));
    }
}
