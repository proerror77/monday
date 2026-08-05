//! Bybit 執行適配器（v5 REST + 私有 WS）
//! - 支援 Live/Testnet/Paper 三種模式
//! - 韌性機制：重試、熔斷器、告警通知

mod ws_order;

use async_trait::async_trait;
use execution::{
    AlertCallback, CircuitBreakerConfig, CircuitState, ExecutionAlert, ExecutionAlertType,
    ExecutorStats, ResilientExecutor, RetryConfig,
};
// Re-export ExecutionMode for backwards compatibility
pub use execution::ExecutionMode;
use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult, OrderId, Price, ProductType, Quantity};
use integration::{
    http::{HttpClient, HttpClientConfig},
    signing::{BybitCredentials, BybitSigner},
};
use ports::{
    AccountBalance, AssetInventoryCapability, BoxStream, ExecutionClient, ExecutionEvent,
    OpenOrder, OrderIntentEnvelope,
};
use rust_decimal::Decimal;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use tracing::{info, warn};
use ws_order::BybitWsOrderClient;

#[derive(Debug, Clone)]
pub struct BybitExecutionConfig {
    pub credentials: BybitCredentials,
    pub mode: ExecutionMode,
    pub rest_base_url: String,
    pub ws_private_url: String,
    pub timeout_ms: u64,
}

pub struct BybitExecutionClient {
    config: BybitExecutionConfig,
    http: Option<HttpClient>,
    signer: BybitSigner,
    event_tx: Option<broadcast::Sender<ExecutionEvent>>,
    connected: bool,
    // 韌性執行器 (重試 + 熔斷器)
    resilient_executor: Option<Arc<ResilientExecutor>>,
    // 告警回調
    alert_callback: Option<AlertCallback>,
    next_client_order_id: Option<String>,
    ws_order: Option<BybitWsOrderClient>,
    shutdown_tx: Option<watch::Sender<bool>>,
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
struct BybitOpenOrdersItem {
    orderId: String,
    #[serde(default)]
    orderLinkId: String,
    symbol: String,
    side: String,
    orderType: String,
    qty: String,
    cumExecQty: String,
    price: String,
    orderStatus: String,
    createdTime: String,
    updatedTime: String,
}

#[derive(serde::Deserialize)]
struct BybitOpenOrdersData {
    list: Vec<BybitOpenOrdersItem>,
    #[serde(rename = "nextPageCursor")]
    next_page_cursor: Option<String>,
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
struct BybitOpenOrdersResponse {
    retCode: i64,
    retMsg: String,
    result: Option<BybitOpenOrdersData>,
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
struct BybitOrderMutationResult {
    orderId: String,
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
struct BybitOrderMutationResponse {
    retCode: i64,
    retMsg: String,
    result: Option<BybitOrderMutationResult>,
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
struct BybitWalletCoin {
    coin: String,
    walletBalance: String,
    locked: String,
    usdValue: String,
}

#[derive(serde::Deserialize)]
struct BybitWalletAccount {
    coin: Vec<BybitWalletCoin>,
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
struct BybitWalletResponse {
    retCode: i64,
    retMsg: String,
    #[serde(default)]
    result: serde_json::Value,
}

fn parse_bybit_wallet_response(response: BybitWalletResponse) -> HftResult<Vec<AccountBalance>> {
    if response.retCode != 0 {
        return Err(classify_bybit_response_error(
            "wallet balance",
            response.retCode,
            &response.retMsg,
        ));
    }
    let accounts: Vec<BybitWalletAccount> =
        serde_json::from_value(
            response.result.get("list").cloned().ok_or_else(|| {
                HftError::Parse("Bybit wallet response missing result.list".into())
            })?,
        )
        .map_err(|error| HftError::Parse(format!("Bybit wallet response: {error}")))?;
    let mut balances = Vec::new();
    for coin in accounts.into_iter().flat_map(|account| account.coin) {
        let total = coin.walletBalance.parse::<Decimal>().map_err(|error| {
            HftError::Parse(format!("Bybit {} walletBalance: {error}", coin.coin))
        })?;
        let frozen = coin
            .locked
            .parse::<Decimal>()
            .map_err(|error| HftError::Parse(format!("Bybit {} locked: {error}", coin.coin)))?;
        let usd_value = coin
            .usdValue
            .parse::<Decimal>()
            .map_err(|error| HftError::Parse(format!("Bybit {} usdValue: {error}", coin.coin)))?;
        balances.push(AccountBalance {
            asset: coin.coin,
            available: total - frozen,
            frozen,
            total,
            usd_value: Some(usd_value),
        });
    }
    Ok(balances)
}

fn bybit_ambiguous_mutation_error(
    operation: &str,
    expected_order_id: &str,
    detail: &str,
) -> HftError {
    HftError::Execution(format!(
        "Bybit {operation} outcome is ambiguous for {expected_order_id}; reconciliation required before retry: {detail}"
    ))
}

fn classify_bybit_response_error(operation: &str, code: i64, message: &str) -> HftError {
    let detail = format!("Bybit {operation} failed: {code} {message}");
    match code {
        10000 | 10016 | 170007 | 170146 => HftError::Network(format!(
            "ambiguous submission outcome; reconcile by orderLinkId: {detail}"
        )),
        10006 | 10429 | 20003 => HftError::RateLimit(detail),
        10003 | 10004 | 10005 | 10007 => HftError::Authentication(detail),
        _ => HftError::Exchange(detail),
    }
}

fn classify_bybit_http_error(status: reqwest::StatusCode, body: &str) -> HftError {
    let detail = format!("Bybit HTTP {status}: {body}");
    let normalized = body.to_ascii_lowercase();
    match status.as_u16() {
        429 => HftError::RateLimit(detail),
        403 if normalized.contains("too frequent") || normalized.contains("rate") => {
            HftError::RateLimit(detail)
        }
        401 | 403 => HftError::Authentication(detail),
        400..=499 => HftError::Exchange(detail),
        _ => HftError::Network(detail),
    }
}

fn bybit_ws_order_url(private_url: &str) -> String {
    private_url.replace("/v5/private", "/v5/trade")
}

fn build_bybit_ws_order_request(
    intent: &ports::OrderIntent,
    client_order_id: &str,
) -> serde_json::Value {
    let mut order = serde_json::json!({
        "category": "spot",
        "symbol": intent.symbol.as_str(),
        "side": match intent.side { hft_core::Side::Buy => "Buy", hft_core::Side::Sell => "Sell" },
        "orderType": match intent.order_type { hft_core::OrderType::Market => "Market", hft_core::OrderType::Limit => "Limit" },
        "qty": intent.quantity.0.to_string(),
        "orderLinkId": client_order_id,
    });
    if matches!(intent.order_type, hft_core::OrderType::Limit) {
        if let Some(price) = intent.price {
            order["price"] = serde_json::Value::String(price.0.to_string());
        }
        order["timeInForce"] = serde_json::Value::String(
            match intent.time_in_force {
                hft_core::TimeInForce::IOC => "IOC",
                hft_core::TimeInForce::FOK => "FOK",
                _ => "GTC",
            }
            .to_string(),
        );
    }
    serde_json::json!({
        "reqId": client_order_id,
        "header": {
            "X-BAPI-TIMESTAMP": BybitSigner::current_timestamp().to_string(),
            "X-BAPI-RECV-WINDOW": "5000",
        },
        "op": "order.create",
        "args": [order],
    })
}

fn parse_bybit_ws_order_response(
    response: serde_json::Value,
    client_order_id: &str,
) -> HftResult<OrderId> {
    let code = response
        .get("retCode")
        .and_then(|value| value.as_i64())
        .unwrap_or(-1);
    if code != 0 {
        let message = response
            .get("retMsg")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown WS order error");
        return Err(classify_bybit_response_error(
            "WS order create",
            code,
            message,
        ));
    }
    let returned = response
        .pointer("/data/orderLinkId")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if !returned.is_empty() && returned != client_order_id {
        return Err(HftError::Execution(format!(
            "Bybit WS order returned mismatched orderLinkId: {returned}"
        )));
    }
    Ok(OrderId(client_order_id.to_string()))
}

impl BybitExecutionClient {
    pub fn new(config: BybitExecutionConfig) -> Result<Self, HftError> {
        Ok(Self {
            http: None,
            signer: BybitSigner::new(config.credentials.clone()),
            config,
            event_tx: None,
            connected: false,
            resilient_executor: None,
            alert_callback: None,
            next_client_order_id: None,
            ws_order: None,
            shutdown_tx: None,
        })
    }

    /// 設置告警回調
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
            info!("[Bybit] 熔斷器已手動重置");
        }
    }

    fn ensure_http(&mut self) -> HftResult<()> {
        if self.http.is_none() {
            let cfg = HttpClientConfig {
                base_url: self.config.rest_base_url.clone(),
                timeout_ms: self.config.timeout_ms,
                user_agent: "hft-bybit-exec/1.0".to_string(),
            };
            self.http = Some(HttpClient::new(cfg).map_err(|e| HftError::Network(e.to_string()))?);
        }
        Ok(())
    }

    /// 獲取 HTTP 客戶端引用（呼叫前需先 ensure_http）
    #[inline]
    fn get_http(&self) -> HftResult<&HttpClient> {
        self.http
            .as_ref()
            .ok_or_else(|| HftError::Execution("HTTP client not initialized".to_string()))
    }

    fn require_private_access(&self, operation: &str) -> HftResult<()> {
        if !matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            return Err(HftError::Config(format!(
                "Bybit {operation} is not available in {} mode",
                bybit_mode_label(self.config.mode)
            )));
        }
        if !has_private_credentials(&self.config.credentials) {
            return Err(HftError::Authentication(format!(
                "Bybit {operation} requires API credentials"
            )));
        }
        Ok(())
    }

    fn snapshot_http(&self) -> HftResult<HttpClient> {
        self.http.clone().map_or_else(
            || {
                HttpClient::new(HttpClientConfig {
                    base_url: self.config.rest_base_url.clone(),
                    timeout_ms: self.config.timeout_ms,
                    user_agent: "hft-bybit-exec/1.0".to_string(),
                })
                .map_err(|error| HftError::Network(error.to_string()))
            },
            Ok,
        )
    }

    async fn submit_order_mutation(
        &mut self,
        path: &str,
        operation: &str,
        body: String,
        expected_order_id: &str,
    ) -> HftResult<()> {
        self.ensure_http()?;
        let http_client = self.get_http()?.clone();
        let signer = self.signer.clone();
        let headers = signer.generate_headers("POST", path, &body, None);
        let response = http_client
            .signed_request(reqwest::Method::POST, path, Some(headers), Some(body))
            .await
            .map_err(|error| {
                let detail = error.to_string();
                bybit_ambiguous_mutation_error(operation, expected_order_id, &detail)
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(
                if status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT {
                    bybit_ambiguous_mutation_error(
                        operation,
                        expected_order_id,
                        &format!("HTTP {status}: {body}"),
                    )
                } else {
                    classify_bybit_http_error(status, &body)
                },
            );
        }
        let response: BybitOrderMutationResponse = HttpClient::parse_json(response)
            .await
            .map_err(|error| HftError::Serialization(error.to_string()))?;
        if response.retCode != 0 {
            return Err(
                if matches!(response.retCode, 10000 | 10016 | 170007 | 170146) {
                    bybit_ambiguous_mutation_error(
                        operation,
                        expected_order_id,
                        &format!("{} {}", response.retCode, response.retMsg),
                    )
                } else {
                    classify_bybit_response_error(operation, response.retCode, &response.retMsg)
                },
            );
        }
        let returned = response
            .result
            .ok_or_else(|| HftError::Parse(format!("Bybit {operation} response missing result")))?;
        if returned.orderId != expected_order_id {
            return Err(HftError::Execution(format!(
                "Bybit {operation} response returned mismatched orderId: {}",
                returned.orderId
            )));
        }
        Ok(())
    }

    async fn confirm_order_absent(&self, order: &OpenOrder) -> HftResult<()> {
        if self
            .list_open_orders()
            .await?
            .iter()
            .any(|candidate| candidate.order_id == order.order_id)
        {
            return Err(HftError::Execution(format!(
                "Bybit cancel receipt remains open for {}",
                order.order_id.0
            )));
        }
        Ok(())
    }

    fn mutation_event_receiver(&self) -> HftResult<broadcast::Receiver<ExecutionEvent>> {
        self.event_tx
            .as_ref()
            .map(|sender| sender.subscribe())
            .ok_or_else(|| {
                HftError::Config(
                    "Bybit private execution stream is not connected for mutation confirmation"
                        .to_string(),
                )
            })
    }

    async fn wait_for_cancellation(
        &self,
        events: &mut broadcast::Receiver<ExecutionEvent>,
        order: &OpenOrder,
    ) -> HftResult<()> {
        tokio::time::timeout(
            std::time::Duration::from_millis(self.config.timeout_ms),
            async {
                loop {
                    let event = events.recv().await.map_err(|error| {
                        HftError::Execution(format!(
                            "Bybit private mutation confirmation stream failed: {error}"
                        ))
                    })?;
                    let matches_order = |order_id: &OrderId| {
                        order_id == &order.order_id
                            || order.client_order_id.as_deref() == Some(order_id.0.as_str())
                    };
                    match event {
                        ExecutionEvent::OrderCanceled { order_id, .. }
                            if matches_order(&order_id) =>
                        {
                            return Ok(());
                        }
                        ExecutionEvent::OrderReject {
                            order_id, reason, ..
                        } if matches_order(&order_id) => {
                            return Err(HftError::Execution(format!(
                                "Bybit cancellation was rejected for {}: {reason}",
                                order.order_id.0
                            )));
                        }
                        ExecutionEvent::ConnectionStatus {
                            connected: false, ..
                        } => {
                            return Err(HftError::Execution(
                                "Bybit private mutation confirmation stream disconnected"
                                    .to_string(),
                            ));
                        }
                        _ => {}
                    }
                }
            },
        )
        .await
        .map_err(|_| {
            HftError::Timeout(format!(
                "Bybit cancellation confirmation timed out after {}ms",
                self.config.timeout_ms
            ))
        })?
    }

    /// 發送執行告警
    fn send_execution_alert(&self, alert: ExecutionAlert) {
        if let Some(ref callback) = self.alert_callback {
            callback(alert);
        }
    }
}

fn bybit_mode_label(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Paper => "Paper",
        ExecutionMode::Live => "Live",
        ExecutionMode::Testnet => "Testnet",
    }
}

fn require_spot_intent(intent: &ports::OrderIntent) -> HftResult<()> {
    if intent.product_type != ProductType::Spot {
        return Err(HftError::InvalidOrder(
            "Bybit execution adapter is Spot only".to_string(),
        ));
    }
    Ok(())
}

fn parse_bybit_side(value: &str) -> HftResult<hft_core::Side> {
    match value {
        "Buy" | "BUY" | "buy" => Ok(hft_core::Side::Buy),
        "Sell" | "SELL" | "sell" => Ok(hft_core::Side::Sell),
        _ => Err(HftError::Parse(format!("Bybit 未知 side: {}", value))),
    }
}

fn parse_bybit_order_type(value: &str) -> HftResult<hft_core::OrderType> {
    match value {
        "Market" | "MARKET" | "market" => Ok(hft_core::OrderType::Market),
        "Limit" | "LIMIT" | "limit" => Ok(hft_core::OrderType::Limit),
        _ => Err(HftError::Parse(format!("Bybit 未知 orderType: {}", value))),
    }
}

fn parse_bybit_status(value: &str) -> HftResult<ports::OrderStatus> {
    match value {
        "New" | "Created" => Ok(ports::OrderStatus::New),
        "PartiallyFilled" | "PartiallyFilledCanceled" => Ok(ports::OrderStatus::PartiallyFilled),
        "Filled" => Ok(ports::OrderStatus::Filled),
        "Cancelled" => Ok(ports::OrderStatus::Canceled),
        "Rejected" => Ok(ports::OrderStatus::Rejected),
        _ => Err(HftError::Parse(format!(
            "Bybit 未知 orderStatus: {}",
            value
        ))),
    }
}

fn parse_bybit_quantity(field: &str, value: &str, allow_zero: bool) -> HftResult<Quantity> {
    let decimal = value
        .parse::<rust_decimal::Decimal>()
        .map_err(|e| HftError::Parse(format!("Bybit {} 解析失敗: {} ({})", field, value, e)))?;
    if decimal < rust_decimal::Decimal::ZERO || (!allow_zero && decimal.is_zero()) {
        return Err(HftError::Parse(format!(
            "Bybit {} 非法數量: {}",
            field, value
        )));
    }
    Ok(Quantity(decimal))
}

fn parse_bybit_price(value: &str, order_type: hft_core::OrderType) -> HftResult<Option<Price>> {
    if value.is_empty() {
        return if order_type == hft_core::OrderType::Market {
            Ok(None)
        } else {
            Err(HftError::Parse("Bybit limit order 缺少 price".to_string()))
        };
    }
    Price::from_str(value)
        .map(Some)
        .map_err(|e| HftError::Parse(format!("Bybit price 解析失敗: {} ({})", value, e)))
}

fn parse_bybit_timestamp(field: &str, value: &str) -> HftResult<u64> {
    value
        .parse::<u64>()
        .map(|ts_ms| ts_ms * 1000)
        .map_err(|e| HftError::Parse(format!("Bybit {} 解析失敗: {} ({})", field, value, e)))
}

fn parse_bybit_open_orders_response(
    response: BybitOpenOrdersResponse,
) -> HftResult<Vec<OpenOrder>> {
    if response.retCode != 0 {
        return Err(classify_bybit_response_error(
            "open-orders query",
            response.retCode,
            &response.retMsg,
        ));
    }

    let data = response
        .result
        .ok_or_else(|| HftError::Parse("Bybit 未結訂單回應缺少 result".to_string()))?;

    data.list
        .into_iter()
        .map(|it| {
            let side = parse_bybit_side(&it.side)?;
            let order_type = parse_bybit_order_type(&it.orderType)?;
            let qty = parse_bybit_quantity("qty", &it.qty, false)?;
            let filled = parse_bybit_quantity("cumExecQty", &it.cumExecQty, true)?;
            if filled.0 > qty.0 {
                return Err(HftError::Parse(format!(
                    "Bybit cumExecQty 大於 qty: {} > {}",
                    filled, qty
                )));
            }
            let price = parse_bybit_price(&it.price, order_type)?;
            let status = parse_bybit_status(&it.orderStatus)?;
            let created_at = parse_bybit_timestamp("createdTime", &it.createdTime)?;
            let updated_at = parse_bybit_timestamp("updatedTime", &it.updatedTime)?;

            Ok(OpenOrder {
                order_id: OrderId(it.orderId),
                client_order_id: (!it.orderLinkId.is_empty()).then_some(it.orderLinkId),
                symbol: hft_core::Symbol::from(it.symbol),
                side,
                order_type,
                original_quantity: qty,
                remaining_quantity: Quantity(qty.0 - filled.0),
                filled_quantity: filled,
                price,
                status,
                created_at,
                updated_at,
            })
        })
        .collect()
}

fn bybit_order_matches(order: &OpenOrder, requested: &OrderId) -> bool {
    order.order_id == *requested
        || order.client_order_id.as_deref() == Some(requested.0.as_str())
}

fn has_private_credentials(credentials: &BybitCredentials) -> bool {
    !credentials.api_key.trim().is_empty()
        && !credentials.secret_key.trim().is_empty()
        && !credentials.api_key.contains("${")
        && !credentials.secret_key.contains("${")
}

fn bybit_private_ws_auth_payload(
    credentials: &BybitCredentials,
    expires: u64,
) -> serde_json::Value {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(credentials.secret_key.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(format!("GET/realtime{expires}").as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    serde_json::json!({
        "op": "auth",
        "args": [credentials.api_key, expires, signature],
    })
}

async fn await_private_ws_control<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    expected_op: &str,
) -> HftResult<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let message = ws
                .next()
                .await
                .ok_or_else(|| HftError::Network("Bybit private WS closed".to_string()))?
                .map_err(|error| HftError::Network(error.to_string()))?;

            match message {
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    let value: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|error| HftError::Serialization(error.to_string()))?;
                    if value.get("op").and_then(|value| value.as_str()) == Some(expected_op) {
                        return Ok(value);
                    }
                }
                tokio_tungstenite::tungstenite::Message::Ping(payload) => {
                    ws.send(tokio_tungstenite::tungstenite::Message::Pong(payload))
                        .await
                        .map_err(|error| HftError::Network(error.to_string()))?;
                }
                tokio_tungstenite::tungstenite::Message::Close(_) => {
                    return Err(HftError::Network("Bybit private WS closed".to_string()));
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| HftError::Network(format!("Bybit private WS {expected_op} timed out")))??;

    if response.get("success").and_then(|value| value.as_bool()) == Some(true) {
        Ok(())
    } else {
        Err(HftError::Authentication(format!(
            "Bybit private WS {expected_op} rejected: {}",
            response
                .get("ret_msg")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown response")
        )))
    }
}

type BybitPrivateSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_bybit_private_ws(
    url: &str,
    credentials: &BybitCredentials,
) -> HftResult<BybitPrivateSocket> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|error| HftError::Network(error.to_string()))?;
    integration::ws::set_ws_tcp_nodelay(ws.get_ref(), true)
        .map_err(|error| HftError::Network(error.to_string()))?;
    let expires = BybitSigner::current_timestamp().saturating_add(1_000);
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        bybit_private_ws_auth_payload(credentials, expires)
            .to_string()
            .into(),
    ))
    .await
    .map_err(|error| HftError::Network(error.to_string()))?;
    await_private_ws_control(&mut ws, "auth").await?;
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::json!({"op": "subscribe", "args": ["order.spot", "execution.spot"]})
            .to_string()
            .into(),
    ))
    .await
    .map_err(|error| HftError::Network(error.to_string()))?;
    await_private_ws_control(&mut ws, "subscribe").await?;
    Ok(ws)
}

async fn run_bybit_private_ws(
    url: String,
    credentials: BybitCredentials,
    event_tx: broadcast::Sender<ExecutionEvent>,
    mut shutdown: watch::Receiver<bool>,
    initial: BybitPrivateSocket,
) {
    let mut ws = initial;
    let mut backoff = std::time::Duration::from_millis(100);
    loop {
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
        heartbeat.tick().await;
        let disconnected = loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
                _ = heartbeat.tick() => {
                    if ws.send(tokio_tungstenite::tungstenite::Message::Text(
                        serde_json::json!({"op": "ping"}).to_string().into(),
                    )).await.is_err() {
                        break true;
                    }
                }
                message = ws.next() => match message {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                            publish_private_ws_event(&event_tx, &value);
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(payload))) => {
                        if ws.send(tokio_tungstenite::tungstenite::Message::Pong(payload)).await.is_err() {
                            break true;
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break true,
                    Some(Err(error)) => {
                        warn!(%error, "Bybit private WS read failed");
                        break true;
                    }
                    _ => {}
                }
            }
        };
        if disconnected {
            let _ = event_tx.send(ExecutionEvent::ConnectionStatus {
                connected: false,
                timestamp: hft_core::now_micros(),
            });
        }
        loop {
            if *shutdown.borrow() {
                return;
            }
            match connect_bybit_private_ws(&url, &credentials).await {
                Ok(connected) => {
                    ws = connected;
                    backoff = std::time::Duration::from_millis(100);
                    let _ = event_tx.send(ExecutionEvent::ConnectionStatus {
                        connected: true,
                        timestamp: hft_core::now_micros(),
                    });
                    break;
                }
                Err(error) => {
                    warn!(%error, "Bybit private WS reconnect failed");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
                }
            }
        }
    }
}

fn publish_private_ws_event(tx: &broadcast::Sender<ExecutionEvent>, value: &serde_json::Value) {
    let topic = value
        .get("topic")
        .and_then(|entry| entry.as_str())
        .unwrap_or("");
    let Some(entries) = value.get("data").and_then(|entry| entry.as_array()) else {
        return;
    };
    if !matches!(topic, "order.spot" | "execution.spot") {
        return;
    }

    for entry in entries {
        if entry.get("category").and_then(|value| value.as_str()) != Some("spot") {
            continue;
        }
        let order_id = entry
            .get("orderLinkId")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(|value| OrderId(value.to_string()))
            .or_else(|| {
                entry
                    .get("orderId")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .map(|value| OrderId(value.to_string()))
            });
        let Some(order_id) = order_id else {
            continue;
        };

        if topic.starts_with("order") {
            let timestamp = entry
                .get("updatedTime")
                .and_then(|value| value.as_str())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_else(|| hft_core::now_micros() / 1000)
                * 1000;
            match entry
                .get("orderStatus")
                .and_then(|value| value.as_str())
                .unwrap_or("")
            {
                "New" | "Created" => {
                    let _ = tx.send(ExecutionEvent::OrderAck {
                        order_id,
                        timestamp,
                    });
                }
                "Cancelled" | "PartiallyFilledCanceled" | "Deactivated" => {
                    let _ = tx.send(ExecutionEvent::OrderCanceled {
                        order_id,
                        timestamp,
                    });
                }
                "Rejected" => {
                    let reason = entry
                        .get("rejectReason")
                        .and_then(|value| value.as_str())
                        .filter(|value| !value.is_empty())
                        .unwrap_or("Exchange rejected")
                        .to_string();
                    let _ = tx.send(ExecutionEvent::OrderReject {
                        order_id,
                        reason,
                        timestamp,
                    });
                }
                _ => {}
            }
        } else if topic.starts_with("execution") {
            let price = entry
                .get("execPrice")
                .and_then(|value| value.as_str())
                .and_then(|value| Price::from_str(value).ok());
            let quantity = entry
                .get("execQty")
                .and_then(|value| value.as_str())
                .and_then(|value| Quantity::from_str(value).ok());
            let timestamp = entry
                .get("execTime")
                .and_then(|value| value.as_str())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_else(|| hft_core::now_micros() / 1000)
                * 1000;
            if let (Some(price), Some(quantity)) = (price, quantity) {
                let fill_id = entry
                    .get("execId")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| format!("BYBIT-{timestamp}"));
                let _ = tx.send(ExecutionEvent::Fill {
                    order_id,
                    price,
                    quantity,
                    timestamp,
                    fill_id,
                });
            }
        }
    }
}

#[async_trait]
impl ExecutionClient for BybitExecutionClient {
    async fn place_order(&mut self, intent: ports::OrderIntent) -> HftResult<OrderId> {
        require_spot_intent(&intent)?;
        if matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            self.require_private_access("place_order")?;
        }
        let client_order_id = self.next_client_order_id.take().unwrap_or_else(|| {
            if matches!(self.config.mode, ExecutionMode::Paper) {
                format!("BYBIT_PAPER_{:x}", hft_core::now_micros())
            } else {
                format!("BYBIT_{:x}", hft_core::now_micros())
            }
        });
        if client_order_id.is_empty()
            || client_order_id.len() > 36
            || !client_order_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(HftError::InvalidOrder(
                "Bybit orderLinkId must be 1-36 ASCII characters [A-Za-z0-9-_]".to_string(),
            ));
        }
        if matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            let payload = build_bybit_ws_order_request(&intent, &client_order_id);
            let ws_order = self.ws_order.as_ref().ok_or_else(|| {
                HftError::Network("Bybit WS order channel is not connected".to_string())
            })?;
            let response = ws_order.submit(client_order_id.clone(), payload).await?;
            return parse_bybit_ws_order_response(response, &client_order_id);
        }
        // Paper
        let oid = OrderId(client_order_id);
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderAck {
                order_id: oid.clone(),
                timestamp: hft_core::now_micros(),
            });
            if let Some(p) = intent.price {
                let _ = tx.send(ExecutionEvent::Fill {
                    order_id: oid.clone(),
                    price: p,
                    quantity: intent.quantity,
                    timestamp: hft_core::now_micros(),
                    fill_id: format!("BBFILL-{}", hft_core::now_micros()),
                });
            }
        }
        Ok(oid)
    }

    async fn place_order_envelope(&mut self, envelope: &OrderIntentEnvelope) -> HftResult<OrderId> {
        self.next_client_order_id = Some(envelope.client_order_id.clone());
        self.place_order(envelope.intent.clone()).await
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        if matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            self.require_private_access("cancel_order")?;
            let remote_order = self
                .list_open_orders()
                .await?
                .into_iter()
                .find(|order| bybit_order_matches(order, order_id))
                .ok_or_else(|| {
                    HftError::OrderNotFound(format!(
                        "Bybit venue snapshot has no open order for {}",
                        order_id.0
                    ))
                })?;
            let mut mutation_events = self.mutation_event_receiver()?;
            let body = serde_json::json!({
                "category": "spot",
                "symbol": remote_order.symbol.as_str(),
                "orderId": remote_order.order_id.0.clone(),
            })
            .to_string();
            let result = match self
                .submit_order_mutation(
                    "/v5/order/cancel",
                    "order cancel",
                    body,
                    &remote_order.order_id.0,
                )
                .await
            {
                Ok(()) => {
                    self.wait_for_cancellation(&mut mutation_events, &remote_order)
                        .await?;
                    self.confirm_order_absent(&remote_order).await
                }
                Err(error) => Err(error),
            };

            // 如果熔斷器開啟，發送告警
            if let Err(ref e) = result {
                if let Some(ref executor) = self.resilient_executor {
                    if executor.circuit_breaker.state().await == CircuitState::Open {
                        self.send_execution_alert(
                            ExecutionAlert::new(
                                ExecutionAlertType::CircuitOpen,
                                "bybit",
                                "cancel_order",
                                format!("撤單失敗且熔斷器已開啟 (order_id={}): {}", order_id.0, e),
                            )
                            .with_error(e.to_string()),
                        );
                    }
                }
            }

            return result;
        }
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderCanceled {
                order_id: order_id.clone(),
                timestamp: hft_core::now_micros(),
            });
        }
        Ok(())
    }

    async fn modify_order(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> HftResult<()> {
        if matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            if new_quantity.is_none() && new_price.is_none() {
                return Ok(());
            }
            return Err(HftError::Config(format!(
                "Bybit live modify is disabled for order {}; asynchronous amend confirmation is not implemented",
                order_id.0
            )));
        }
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderModified {
                order_id: order_id.clone(),
                new_quantity,
                new_price,
                timestamp: hft_core::now_micros(),
            });
        }
        Ok(())
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        if let Some(ref tx) = self.event_tx {
            let rx = tx.subscribe();
            let s = tokio_stream::wrappers::BroadcastStream::new(rx).map(|result| match result {
                Ok(event) => Ok(event),
                Err(error) => Ok(ExecutionEvent::ReconciliationRequired {
                    reason: format!(
                        "Bybit private execution stream lagged and has no in-process watermark recovery; restart is required: {error}"
                    ),
                    timestamp: hft_core::now_micros(),
                }),
            });
            return Ok(Box::pin(s));
        }
        Ok(Box::pin(futures::stream::empty()))
    }

    fn asset_inventory_capability(&self) -> AssetInventoryCapability {
        AssetInventoryCapability::AuthoritativeAssetInventory {
            product_type: ProductType::Spot,
        }
    }

    async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
        if !matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            return Err(HftError::Config(
                "Bybit authoritative balances require live/testnet mode".to_string(),
            ));
        }
        let http = self
            .http
            .as_ref()
            .ok_or_else(|| HftError::Config("Bybit HTTP client is not connected".to_string()))?;
        let query = "accountType=UNIFIED";
        let path = format!("/v5/account/wallet-balance?{query}");
        let headers =
            self.signer
                .generate_headers("GET", "/v5/account/wallet-balance", query, None);
        let response = http
            .get(&path, Some(headers))
            .await
            .map_err(|error| HftError::Network(error.to_string()))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(classify_bybit_http_error(status, &body));
        }
        let response: BybitWalletResponse = HttpClient::parse_json(response)
            .await
            .map_err(|error| HftError::Parse(format!("Bybit wallet response: {error}")))?;
        parse_bybit_wallet_response(response)
    }

    async fn connect(&mut self) -> HftResult<()> {
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

        let mut executor = ResilientExecutor::new("bybit", retry_config, cb_config);

        // 設置熔斷器告警回調
        if let Some(ref alert_cb) = self.alert_callback {
            let alert_cb = Arc::clone(alert_cb);
            executor = executor.with_alert_callback(move |cb_alert| {
                let alert_type = match cb_alert.state {
                    CircuitState::Open => ExecutionAlertType::CircuitOpen,
                    CircuitState::Closed => ExecutionAlertType::CircuitRecovered,
                    CircuitState::HalfOpen => return,
                };

                let alert =
                    ExecutionAlert::new(alert_type, "bybit", "execution", &cb_alert.message)
                        .with_failure_count(cb_alert.failure_count);

                alert_cb(alert);
            });
        }

        self.resilient_executor = Some(Arc::new(executor));

        self.ensure_http()?;

        let (tx, _) = broadcast::channel(1000);

        if matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            if !has_private_credentials(&self.config.credentials) {
                return Err(HftError::Authentication(
                    "Bybit live/testnet connection requires API credentials".to_string(),
                ));
            }

            self.ws_order = Some(
                BybitWsOrderClient::connect(
                    bybit_ws_order_url(&self.config.ws_private_url),
                    self.config.credentials.clone(),
                    std::time::Duration::from_millis(self.config.timeout_ms),
                )
                .await?,
            );

            let ws =
                connect_bybit_private_ws(&self.config.ws_private_url, &self.config.credentials)
                    .await?;
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            self.shutdown_tx = Some(shutdown_tx);
            tokio::spawn(run_bybit_private_ws(
                self.config.ws_private_url.clone(),
                self.config.credentials.clone(),
                tx.clone(),
                shutdown_rx,
                ws,
            ));
        }

        self.event_tx = Some(tx);
        self.connected = true;
        info!("[Bybit] 執行客戶端連接成功");
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        if let Some(shutdown) = self.shutdown_tx.take() {
            let _ = shutdown.send(true);
        }
        self.ws_order = None;
        self.event_tx = None;
        self.connected = false;
        Ok(())
    }
    async fn health(&self) -> ports::ConnectionHealth {
        ports::ConnectionHealth {
            connected: self.connected,
            latency_ms: Some(1.0),
            last_heartbeat: hft_core::now_micros(),
        }
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        self.require_private_access("list_open_orders")?;
        let http = self.snapshot_http()?;

        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        let mut seen_order_ids = HashSet::new();
        let mut orders = Vec::new();
        for _ in 0..100 {
            let query = {
                let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                serializer
                    .append_pair("category", "spot")
                    .append_pair("limit", "50");
                if let Some(cursor) = &cursor {
                    serializer.append_pair("cursor", cursor);
                }
                serializer.finish()
            };
            let path = format!("/v5/order/realtime?{query}");
            let headers = self
                .signer
                .generate_headers("GET", "/v5/order/realtime", &query, None);
            let response = http
                .get(&path, Some(headers))
                .await
                .map_err(|error| HftError::Network(error.to_string()))?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(classify_bybit_http_error(status, &body));
            }
            let response: BybitOpenOrdersResponse = HttpClient::parse_json(response)
                .await
                .map_err(|error| HftError::Parse(format!("Bybit open orders: {error}")))?;
            let next_cursor = response
                .result
                .as_ref()
                .ok_or_else(|| HftError::Parse("Bybit open-orders response missing result".to_string()))?
                .next_page_cursor
                .clone()
                .ok_or_else(|| {
                    HftError::Parse("Bybit open-orders response missing nextPageCursor".to_string())
                })?;
            for order in parse_bybit_open_orders_response(response)? {
                if seen_order_ids.insert(order.order_id.0.clone()) {
                    orders.push(order);
                }
            }
            if next_cursor.is_empty() {
                return Ok(orders);
            }
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(HftError::Exchange(
                    "Bybit open-order pagination cursor is cyclic".to_string(),
                ));
            }
            cursor = Some(next_cursor);
        }
        Err(HftError::Exchange(
            "Bybit open-order pagination exceeded 100 pages".to_string(),
        ))
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderType, ProductType, Side, Symbol, TimeInForce};
    use integration::signing::BybitCredentials;
    use ports::{ExecutionClient, OrderIntent, OrderIntentLifecycle};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn make_test_config(mode: ExecutionMode) -> BybitExecutionConfig {
        BybitExecutionConfig {
            credentials: BybitCredentials {
                api_key: "test_key".to_string(),
                secret_key: "test_secret".to_string(),
            },
            mode,
            rest_base_url: "https://api.bybit.com".to_string(),
            ws_private_url: "wss://stream.bybit.com/v5/private".to_string(),
            timeout_ms: 5000,
        }
    }

    async fn bybit_rest_server(
        responses: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for body in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 16 * 1024];
                let size = socket.read(&mut request).await.unwrap();
                requests.push(String::from_utf8_lossy(&request[..size]).to_string());
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (format!("http://{address}"), task)
    }

    async fn bybit_rest_server_with_request_signal(
        responses: Vec<String>,
    ) -> (
        String,
        tokio::sync::mpsc::UnboundedReceiver<usize>,
        tokio::task::JoinHandle<Vec<String>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (signal_tx, signal_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (index, body) in responses.into_iter().enumerate() {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 16 * 1024];
                let size = socket.read(&mut request).await.unwrap();
                requests.push(String::from_utf8_lossy(&request[..size]).to_string());
                let _ = signal_tx.send(index);
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (format!("http://{address}"), signal_rx, task)
    }

    fn attach_mutation_events(
        client: &mut BybitExecutionClient,
    ) -> broadcast::Sender<ExecutionEvent> {
        let (tx, _) = broadcast::channel(4);
        client.event_tx = Some(tx.clone());
        tx
    }

    fn spot_open_orders_response(order_id: &str, client_order_id: &str) -> String {
        spot_open_orders_response_with_values(order_id, client_order_id, "1", "100")
    }

    fn spot_open_orders_response_with_values(
        order_id: &str,
        client_order_id: &str,
        quantity: &str,
        price: &str,
    ) -> String {
        format!(
            r#"{{"retCode":0,"retMsg":"OK","result":{{"list":[{{"orderId":"{order_id}","orderLinkId":"{client_order_id}","symbol":"BTCUSDT","side":"Buy","orderType":"Limit","qty":"{quantity}","cumExecQty":"0","price":"{price}","orderStatus":"New","createdTime":"1","updatedTime":"2"}}],"nextPageCursor":""}}}}"#
        )
    }

    #[test]
    fn exchange_rate_limits_are_classified_without_retrying_order_submission() {
        assert!(matches!(
            classify_bybit_response_error("order create", 10006, "Too many visits"),
            HftError::RateLimit(_)
        ));
        assert!(matches!(
            classify_bybit_http_error(reqwest::StatusCode::FORBIDDEN, "access too frequent"),
            HftError::RateLimit(_)
        ));
        assert!(matches!(
            classify_bybit_response_error("order create", 110007, "insufficient balance"),
            HftError::Exchange(_)
        ));
    }

    #[test]
    fn test_config_creation() {
        let config = make_test_config(ExecutionMode::Paper);
        assert_eq!(config.rest_base_url, "https://api.bybit.com");
        assert_eq!(config.ws_private_url, "wss://stream.bybit.com/v5/private");
        assert_eq!(config.timeout_ms, 5000);
        assert_eq!(config.mode, ExecutionMode::Paper);
    }

    #[test]
    fn test_config_clone() {
        let config = make_test_config(ExecutionMode::Live);
        let cloned = config.clone();
        assert_eq!(cloned.rest_base_url, config.rest_base_url);
        assert_eq!(cloned.mode, config.mode);
    }

    #[test]
    fn test_config_debug() {
        let config = make_test_config(ExecutionMode::Paper);
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("BybitExecutionConfig"));
    }

    #[test]
    fn private_ws_auth_uses_bybit_v5_get_realtime_signature() {
        let credentials = BybitCredentials {
            api_key: "test_key".to_string(),
            secret_key: "test_secret".to_string(),
        };

        let payload = bybit_private_ws_auth_payload(&credentials, 123_456);

        assert_eq!(payload["op"], "auth");
        assert_eq!(payload["args"][0], "test_key");
        assert_eq!(payload["args"][1], 123_456);
        assert_eq!(
            payload["args"][2],
            "c78e068710f8dfc40c7c66173ff9bbce29560787724e255b5cdd76b81f05ede9"
        );
    }

    #[test]
    fn test_client_creation() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BybitExecutionClient::new(config).unwrap();
        assert!(!client.connected);
        assert!(client.event_tx.is_none());
        assert!(client.http.is_none());
    }

    #[test]
    fn test_execution_mode_reexport() {
        // Verify ExecutionMode is properly re-exported
        let paper = ExecutionMode::Paper;
        let live = ExecutionMode::Live;
        let testnet = ExecutionMode::Testnet;
        assert_eq!(paper, ExecutionMode::Paper);
        assert_eq!(live, ExecutionMode::Live);
        assert_eq!(testnet, ExecutionMode::Testnet);
    }

    #[test]
    fn test_resilience_stats_none_before_connect() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BybitExecutionClient::new(config).unwrap();
        assert!(client.resilience_stats().is_none());
    }

    #[test]
    fn test_with_alert_callback() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let config = make_test_config(ExecutionMode::Paper);
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = Arc::clone(&called);

        let client = BybitExecutionClient::new(config)
            .unwrap()
            .with_alert_callback(move |_alert| {
                called_clone.store(true, Ordering::SeqCst);
            });

        // Alert callback should be set
        assert!(client.alert_callback.is_some());
    }

    #[tokio::test]
    async fn test_health_check_initial() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BybitExecutionClient::new(config).unwrap();
        let health = client.health().await;

        assert!(!health.connected);
        assert!(health.latency_ms.is_some());
        assert!(health.last_heartbeat > 0);
    }

    #[tokio::test]
    async fn test_connect_paper_mode() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BybitExecutionClient::new(config).unwrap();

        let result = client.connect().await;
        assert!(result.is_ok());
        assert!(client.connected);
        assert!(client.event_tx.is_some());
        assert!(client.resilient_executor.is_some());
    }

    #[tokio::test]
    async fn live_connect_without_credentials_is_rejected_before_network_io() {
        let mut config = make_test_config(ExecutionMode::Testnet);
        config.credentials = BybitCredentials {
            api_key: String::new(),
            secret_key: String::new(),
        };
        let mut client = BybitExecutionClient::new(config).unwrap();

        let error = client.connect().await.unwrap_err();

        assert!(error.to_string().contains("requires API credentials"));
        assert!(!client.connected);
    }

    #[tokio::test]
    async fn test_disconnect() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BybitExecutionClient::new(config).unwrap();

        // Connect first
        client.connect().await.unwrap();
        assert!(client.connected);

        // Disconnect
        let result = client.disconnect().await;
        assert!(result.is_ok());
        assert!(!client.connected);
        assert!(client.event_tx.is_none());
    }

    #[tokio::test]
    async fn test_health_check_after_connect() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BybitExecutionClient::new(config).unwrap();

        client.connect().await.unwrap();
        let health = client.health().await;

        assert!(health.connected);
    }

    #[tokio::test]
    async fn test_paper_mode_place_order() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BybitExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();

        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            asset_class: hft_core::AssetClass::Crypto,
            product_type: hft_core::ProductType::Spot,
            compliance_context: hft_core::ComplianceContext::default(),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::from_f64(0.001).unwrap(),
            price: Some(Price::from_f64(50000.0).unwrap()),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: None,
        };

        let result = client.place_order(intent).await;
        assert!(result.is_ok());

        let order_id = result.unwrap();
        assert!(order_id.0.starts_with("BYBIT_PAPER_"));
    }

    #[tokio::test]
    async fn non_spot_intent_is_rejected_before_private_transport() {
        let mut client = BybitExecutionClient::new(make_test_config(ExecutionMode::Testnet)).unwrap();
        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            asset_class: hft_core::AssetClass::Crypto,
            product_type: ProductType::Perp,
            compliance_context: hft_core::ComplianceContext::default(),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::from_f64(0.001).unwrap(),
            price: Some(Price::from_f64(50_000.0).unwrap()),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: None,
        };

        let error = client.place_order(intent).await.unwrap_err();

        assert!(matches!(error, HftError::InvalidOrder(message) if message.contains("Spot only")));
    }

    #[tokio::test]
    async fn order_envelope_preserves_the_venue_link_id() {
        let mut client =
            BybitExecutionClient::new(make_test_config(ExecutionMode::Paper)).unwrap();
        let envelope = OrderIntentEnvelope::new(
            OrderIntent::crypto_spot(
                Symbol::new("BTCUSDT"),
                Side::Buy,
                Quantity::from_f64(0.001).unwrap(),
                OrderType::Limit,
                Some(Price::from_f64(50_000.0).unwrap()),
                TimeInForce::GTC,
                "test_strategy".to_string(),
                None,
            ),
            OrderIntentLifecycle::default(),
        )
        .with_client_order_id("bybit-link-42");

        assert_eq!(
            client.place_order_envelope(&envelope).await.unwrap(),
            OrderId("bybit-link-42".to_string())
        );
    }

    #[tokio::test]
    async fn test_paper_mode_cancel_order() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BybitExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();

        let order_id = OrderId("test_order_123".to_string());
        let result = client.cancel_order(&order_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_paper_mode_modify_order() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BybitExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();

        let order_id = OrderId("test_order_123".to_string());
        let result = client
            .modify_order(
                &order_id,
                Some(Quantity::from_f64(0.002).unwrap()),
                Some(Price::from_f64(51000.0).unwrap()),
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn live_modify_fails_before_network_io() {
        let config = make_test_config(ExecutionMode::Live);
        let mut client = BybitExecutionClient::new(config).unwrap();

        let error = client
            .modify_order(
                &OrderId("exchange-order".to_string()),
                Some(Quantity::from_f64(0.002).unwrap()),
                None,
            )
            .await
            .unwrap_err();

        assert!(matches!(error, HftError::Config(message) if message.contains("disabled")));
    }

    #[tokio::test]
    async fn test_execution_stream_before_connect() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BybitExecutionClient::new(config).unwrap();

        let result = client.execution_stream().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execution_stream_after_connect() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BybitExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();

        let result = client.execution_stream().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_circuit_state_before_connect() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BybitExecutionClient::new(config).unwrap();
        let state = client.circuit_state().await;
        assert!(state.is_none());
    }

    #[tokio::test]
    async fn test_circuit_state_after_connect() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BybitExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();

        let state = client.circuit_state().await;
        assert!(state.is_some());
        assert_eq!(state.unwrap(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_reset_circuit_breaker() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BybitExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();

        // Should not panic
        client.reset_circuit_breaker().await;

        // State should still be closed
        let state = client.circuit_state().await;
        assert_eq!(state.unwrap(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_resilience_stats_after_connect() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BybitExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();

        let stats = client.resilience_stats();
        assert!(stats.is_some());
        let stats = stats.unwrap();
        assert_eq!(stats.total_calls, 0);
        assert_eq!(stats.failed_calls, 0);
    }

    #[tokio::test]
    async fn test_list_open_orders_paper_mode() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BybitExecutionClient::new(config).unwrap();

        let result = client.list_open_orders().await;
        assert!(matches!(result, Err(HftError::Config(_))));
    }

    #[tokio::test]
    async fn open_orders_page_through_deduplicates_and_keeps_spot_category() {
        let (base_url, server) = bybit_rest_server(vec![
            spot_open_orders_response_with_values("venue-1", "client-1", "1", "100")
                .replace("\"nextPageCursor\":\"\"", "\"nextPageCursor\":\"cursor-2\""),
            r#"{"retCode":0,"retMsg":"OK","result":{"list":[{"orderId":"venue-1","orderLinkId":"client-1","symbol":"BTCUSDT","side":"Buy","orderType":"Limit","qty":"1","cumExecQty":"0","price":"100","orderStatus":"New","createdTime":"1","updatedTime":"2"},{"orderId":"venue-2","orderLinkId":"client-2","symbol":"ETHUSDT","side":"Sell","orderType":"Limit","qty":"2","cumExecQty":"0","price":"200","orderStatus":"New","createdTime":"3","updatedTime":"4"}],"nextPageCursor":""}}"#.to_string(),
        ])
        .await;
        let mut config = make_test_config(ExecutionMode::Testnet);
        config.rest_base_url = base_url;
        let client = BybitExecutionClient::new(config).unwrap();

        let orders = client.list_open_orders().await.unwrap();
        let requests = server.await.unwrap();

        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].order_id, OrderId("venue-1".to_string()));
        assert_eq!(orders[1].order_id, OrderId("venue-2".to_string()));
        assert!(requests[0].starts_with("GET /v5/order/realtime?category=spot&limit=50"));
        assert!(requests[1].contains("category=spot&limit=50&cursor=cursor-2"));
    }

    #[tokio::test]
    async fn open_orders_reject_missing_or_cyclic_cursors() {
        let (missing_base_url, missing_server) = bybit_rest_server(vec![
            r#"{"retCode":0,"retMsg":"OK","result":{"list":[]}}"#.to_string(),
        ])
        .await;
        let mut missing_config = make_test_config(ExecutionMode::Testnet);
        missing_config.rest_base_url = missing_base_url;
        let missing_client = BybitExecutionClient::new(missing_config).unwrap();
        assert!(matches!(
            missing_client.list_open_orders().await,
            Err(HftError::Parse(message)) if message.contains("nextPageCursor")
        ));
        let _ = missing_server.await.unwrap();

        let (cyclic_base_url, cyclic_server) = bybit_rest_server(vec![
            r#"{"retCode":0,"retMsg":"OK","result":{"list":[],"nextPageCursor":"cursor-a"}}"#.to_string(),
            r#"{"retCode":0,"retMsg":"OK","result":{"list":[],"nextPageCursor":"cursor-a"}}"#.to_string(),
        ])
        .await;
        let mut cyclic_config = make_test_config(ExecutionMode::Testnet);
        cyclic_config.rest_base_url = cyclic_base_url;
        let cyclic_client = BybitExecutionClient::new(cyclic_config).unwrap();
        assert!(matches!(
            cyclic_client.list_open_orders().await,
            Err(HftError::Exchange(message)) if message.contains("cursor")
        ));
        let _ = cyclic_server.await.unwrap();
    }

    #[tokio::test]
    async fn ambiguous_mutation_response_is_not_retried() {
        let (base_url, server) = bybit_rest_server(vec![
            r#"{"retCode":10000,"retMsg":"timeout","result":null}"#.to_string(),
        ])
        .await;
        let mut config = make_test_config(ExecutionMode::Testnet);
        config.rest_base_url = base_url;
        let mut client = BybitExecutionClient::new(config).unwrap();
        client.resilient_executor = Some(Arc::new(ResilientExecutor::new(
            "bybit-test",
            RetryConfig {
                max_retries: 2,
                initial_delay_ms: 1,
                max_delay_ms: 1,
                backoff_multiplier: 1.0,
                retry_on_init_error: true,
            },
            CircuitBreakerConfig::default(),
        )));

        let error = client
            .submit_order_mutation(
                "/v5/order/cancel",
                "order cancel",
                r#"{"category":"spot","symbol":"BTCUSDT","orderId":"venue-1"}"#.to_string(),
                "venue-1",
            )
            .await
            .unwrap_err();
        let requests = server.await.unwrap();

        assert!(matches!(error, HftError::Execution(message) if message.contains("reconciliation required")));
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn restart_discovered_order_cancels_by_venue_order_id_and_requires_terminal_readback() {
        let (base_url, mut request_signal, server) = bybit_rest_server_with_request_signal(vec![
            spot_open_orders_response("venue-1", "client-1"),
            r#"{"retCode":0,"retMsg":"OK","result":{"orderId":"venue-1","orderLinkId":"client-1"}}"#.to_string(),
            r#"{"retCode":0,"retMsg":"OK","result":{"list":[],"nextPageCursor":""}}"#.to_string(),
        ])
        .await;
        let mut config = make_test_config(ExecutionMode::Testnet);
        config.rest_base_url = base_url;
        let mut client = BybitExecutionClient::new(config).unwrap();
        let events = attach_mutation_events(&mut client);
        let event = tokio::spawn(async move {
            loop {
                match request_signal.recv().await {
                    Some(1) => break,
                    Some(_) => {}
                    None => panic!("cancel request was not observed"),
                }
            }
            publish_private_ws_event(
                &events,
                &serde_json::json!({
                    "topic": "order.spot",
                    "data": [{
                        "category": "spot",
                        "orderId": "venue-1",
                        "orderLinkId": "client-1",
                        "updatedTime": "2",
                        "orderStatus": "Cancelled"
                    }]
                }),
            );
        });

        client
            .cancel_order(&OrderId("client-1".to_string()))
            .await
            .unwrap();
        event.await.unwrap();
        let requests = server.await.unwrap();

        assert!(requests[0].starts_with("GET /v5/order/realtime?category=spot&limit=50"));
        assert!(requests[1].starts_with("POST /v5/order/cancel"));
        assert!(requests[1].contains(r#""category":"spot""#));
        assert!(requests[1].contains(r#""orderId":"venue-1""#));
        assert!(requests[2].starts_with("GET /v5/order/realtime?category=spot&limit=50"));
    }

    #[tokio::test]
    async fn cancel_receipt_is_incomplete_when_the_venue_still_reports_the_order_open() {
        let (base_url, mut request_signal, server) = bybit_rest_server_with_request_signal(vec![
            spot_open_orders_response("venue-1", "client-1"),
            r#"{"retCode":0,"retMsg":"OK","result":{"orderId":"venue-1","orderLinkId":"client-1"}}"#.to_string(),
            spot_open_orders_response("venue-1", "client-1"),
        ])
        .await;
        let mut config = make_test_config(ExecutionMode::Testnet);
        config.rest_base_url = base_url;
        let mut client = BybitExecutionClient::new(config).unwrap();
        let events = attach_mutation_events(&mut client);
        let event = tokio::spawn(async move {
            loop {
                match request_signal.recv().await {
                    Some(1) => break,
                    Some(_) => {}
                    None => panic!("cancel request was not observed"),
                }
            }
            publish_private_ws_event(
                &events,
                &serde_json::json!({
                    "topic": "order.spot",
                    "data": [{
                        "category": "spot",
                        "orderId": "venue-1",
                        "orderLinkId": "client-1",
                        "updatedTime": "2",
                        "orderStatus": "Cancelled"
                    }]
                }),
            );
        });

        let error = client
            .cancel_order(&OrderId("client-1".to_string()))
            .await
            .unwrap_err();
        event.await.unwrap();
        let _ = server.await.unwrap();

        assert!(matches!(error, HftError::Execution(message) if message.contains("remains open")));
    }

    #[test]
    fn test_parse_bybit_open_orders_missing_data_fails() {
        let response = BybitOpenOrdersResponse {
            retCode: 0,
            retMsg: "OK".to_string(),
            result: None,
        };

        let result = parse_bybit_open_orders_response(response);
        assert!(matches!(result, Err(HftError::Parse(message)) if message.contains("缺少 result")));
    }

    #[test]
    fn open_orders_parser_accepts_v5_result_envelope() {
        let response: BybitOpenOrdersResponse =
            serde_json::from_str(r#"{"retCode":0,"retMsg":"OK","result":{"list":[]}}"#).unwrap();

        assert!(parse_bybit_open_orders_response(response)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_parse_bybit_open_orders_unknown_side_fails() {
        let response = BybitOpenOrdersResponse {
            retCode: 0,
            retMsg: "OK".to_string(),
            result: Some(BybitOpenOrdersData {
                list: vec![BybitOpenOrdersItem {
                    orderId: "1".to_string(),
                    orderLinkId: "client-1".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    side: "Hold".to_string(),
                    orderType: "Limit".to_string(),
                    qty: "1".to_string(),
                    cumExecQty: "0".to_string(),
                    price: "100".to_string(),
                    orderStatus: "New".to_string(),
                    createdTime: "1".to_string(),
                    updatedTime: "2".to_string(),
                }],
                next_page_cursor: Some(String::new()),
            }),
        };

        let result = parse_bybit_open_orders_response(response);
        assert!(matches!(result, Err(HftError::Parse(message)) if message.contains("未知 side")));
    }

    #[test]
    fn private_order_rejection_is_not_reported_as_cancellation() {
        let (tx, mut rx) = broadcast::channel(4);
        publish_private_ws_event(
            &tx,
            &serde_json::json!({
                "topic": "order.spot",
                "data": [{
                    "category": "spot",
                    "orderId": "rejected-order",
                    "orderLinkId": "client-rejected-order",
                    "updatedTime": "123",
                    "orderStatus": "Rejected",
                    "rejectReason": "EC_OrderCheckFailed"
                }]
            }),
        );

        match rx.try_recv().expect("rejection event") {
            ExecutionEvent::OrderReject {
                order_id, reason, ..
            } => {
                assert_eq!(order_id, OrderId("client-rejected-order".to_string()));
                assert!(reason.contains("EC_OrderCheckFailed"));
            }
            event => panic!("expected rejection, got {event:?}"),
        }
    }

    #[test]
    fn private_stream_ignores_non_spot_frames() {
        let (tx, mut rx) = broadcast::channel(1);
        publish_private_ws_event(
            &tx,
            &serde_json::json!({
                "topic": "order.linear",
                "data": [{
                    "category": "linear",
                    "orderId": "linear-order",
                    "updatedTime": "123",
                    "orderStatus": "Cancelled"
                }]
            }),
        );

        assert!(matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
    }

    #[test]
    fn private_spot_terminal_cancel_statuses_close_the_order() {
        for status in ["PartiallyFilledCanceled", "Deactivated"] {
            let (tx, mut rx) = broadcast::channel(2);
            publish_private_ws_event(
                &tx,
                &serde_json::json!({
                    "topic": "order.spot",
                    "data": [{
                        "category": "spot",
                        "orderId": "terminal-order",
                        "updatedTime": "123",
                        "orderStatus": status
                    }]
                }),
            );

            assert!(matches!(
                rx.try_recv().expect("terminal cancellation event"),
                ExecutionEvent::OrderCanceled { order_id, .. }
                    if order_id == OrderId("terminal-order".to_string())
            ));
        }
    }

    #[test]
    fn websocket_order_request_uses_trade_protocol_and_stable_link_id() {
        let mut intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            asset_class: hft_core::AssetClass::Crypto,
            product_type: hft_core::ProductType::Spot,
            compliance_context: hft_core::ComplianceContext::default(),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::from_f64(0.01).unwrap(),
            price: Some(Price::from_f64(100.0).unwrap()),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test".to_string(),
            target_venue: None,
        };
        let request = build_bybit_ws_order_request(&intent, "client-42");

        assert_eq!(request["op"], "order.create");
        assert_eq!(request["reqId"], "client-42");
        assert_eq!(request["args"][0]["orderLinkId"], "client-42");
        assert_eq!(request["args"][0]["category"], "spot");
        assert_eq!(request["args"][0]["price"], "100");
        assert_eq!(request["args"][0]["timeInForce"], "GTC");

        intent.order_type = OrderType::Market;
        intent.price = None;
        let market_request = build_bybit_ws_order_request(&intent, "client-market");
        assert!(market_request["args"][0].get("price").is_none());
        assert!(market_request["args"][0].get("timeInForce").is_none());
        assert_eq!(
            parse_bybit_ws_order_response(
                serde_json::json!({
                    "retCode": 0,
                    "retMsg": "OK",
                    "data": {"orderId": "7", "orderLinkId": "client-42"}
                }),
                "client-42"
            )
            .unwrap(),
            OrderId("client-42".to_string())
        );
    }
}
#[test]
fn order_creation_timeouts_are_ambiguous_not_exchange_rejections() {
    for code in [10000, 10016, 170007, 170146] {
        assert!(matches!(
            classify_bybit_response_error("order create", code, "timeout"),
            HftError::Network(_)
        ));
    }
}
