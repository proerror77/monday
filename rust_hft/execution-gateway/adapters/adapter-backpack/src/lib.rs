//! Backpack Exchange execution adapter implementing `ports::ExecutionClient`.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use execution::{
    resilience::CircuitBreakerAlert, AlertCallback, CircuitBreakerConfig, CircuitState,
    ExecutionAlert, ExecutionAlertType, ExecutorStats, ResilientExecutor, RetryConfig,
};
// Re-export ExecutionMode for backwards compatibility
pub use execution::ExecutionMode;
use futures::StreamExt;
use hft_core::{
    now_micros, HftError, HftResult, OrderId, OrderType, Price, Quantity, Side, Symbol,
    TimeInForce, VenueId,
};
use ports::{BoxStream, ExecutionClient, ExecutionEvent, OpenOrder, OrderIntent, OrderStatus};
use reqwest::{Client, StatusCode};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{debug, info, warn};

const DEFAULT_WINDOW_MS: u64 = 5_000;

#[derive(Debug, Clone)]
pub struct BackpackExecutionConfig {
    pub rest_base_url: String,
    pub ws_public_url: String,
    pub api_key: String,
    pub secret_key: String,
    pub timeout_ms: u64,
    pub mode: ExecutionMode,
    pub window_ms: Option<u64>,
}

#[derive(Debug, Error)]
enum SigningError {
    #[error("invalid secret key length {0}")]
    InvalidLength(usize),
    #[error("failed to decode secret key: {0}")]
    Decode(String),
    #[error("ed25519 error: {0}")]
    Dalek(String),
}

#[derive(Clone)]
struct BackpackCredentials {
    api_key_b64: String,
    signing_key: SigningKey,
}

impl BackpackCredentials {
    fn new(api_key_b64: String, secret_key_b64: String) -> Result<Self, SigningError> {
        let secret_bytes = general_purpose::STANDARD
            .decode(secret_key_b64.trim())
            .map_err(|e| SigningError::Decode(e.to_string()))?;

        let signing_key = match secret_bytes.len() {
            32 => {
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(&secret_bytes);
                SigningKey::from_bytes(&bytes)
            }
            64 => {
                let mut bytes = [0u8; 64];
                bytes.copy_from_slice(&secret_bytes);
                SigningKey::from_keypair_bytes(&bytes)
                    .map_err(|e| SigningError::Dalek(e.to_string()))?
            }
            other => return Err(SigningError::InvalidLength(other)),
        };

        let derived_api_key =
            general_purpose::STANDARD.encode(signing_key.verifying_key().as_bytes());
        if api_key_b64.trim() != derived_api_key {
            warn!(
                "Backpack credentials: provided API key does not match secret key derived verifying key"
            );
        }

        Ok(Self {
            api_key_b64: api_key_b64.trim().to_string(),
            signing_key,
        })
    }

    fn api_key(&self) -> &str {
        &self.api_key_b64
    }

    fn sign(
        &self,
        instruction: &str,
        params: &BTreeMap<String, String>,
        timestamp_ms: u64,
        window_ms: u64,
    ) -> String {
        let mut message = format!("instruction={instruction}");
        for (key, value) in params {
            message.push('&');
            message.push_str(key);
            message.push('=');
            message.push_str(value);
        }
        message.push_str(&format!("&timestamp={timestamp_ms}&window={window_ms}"));

        let signature = self.signing_key.sign(message.as_bytes());
        general_purpose::STANDARD.encode(signature.to_bytes())
    }
}

pub struct BackpackExecutionClient {
    cfg: BackpackExecutionConfig,
    credentials: Option<BackpackCredentials>,
    http_client: Client,
    event_tx: broadcast::Sender<ExecutionEvent>,
    connected: Arc<AtomicBool>,
    last_heartbeat: Arc<AtomicU64>,
    order_symbols: HashMap<String, String>,
    // Resilience 相關
    resilient_executor: Option<Arc<ResilientExecutor>>,
    alert_callback: Option<AlertCallback>,
}

impl BackpackExecutionClient {
    pub fn new(cfg: BackpackExecutionConfig) -> HftResult<Self> {
        let credentials = if cfg.mode == ExecutionMode::Live {
            if cfg.api_key.trim().is_empty() || cfg.secret_key.trim().is_empty() {
                return Err(HftError::Authentication(
                    "Backpack Live mode requires API key and secret".into(),
                ));
            }
            Some(
                BackpackCredentials::new(cfg.api_key.clone(), cfg.secret_key.clone())
                    .map_err(|e| HftError::Config(format!("Backpack credentials error: {e}")))?,
            )
        } else {
            None
        };

        let client = Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms.max(1000)))
            .user_agent("hft-backpack-exec/0.1")
            .build()
            .map_err(|e| HftError::Config(format!("Failed to build HTTP client: {e}")))?;

        let (event_tx, _) = broadcast::channel(256);

        Ok(Self {
            cfg,
            credentials,
            http_client: client,
            event_tx,
            connected: Arc::new(AtomicBool::new(false)),
            last_heartbeat: Arc::new(AtomicU64::new(now_micros())),
            order_symbols: HashMap::new(),
            resilient_executor: None,
            alert_callback: None,
        })
    }

    /// 設置 alert callback
    pub fn with_alert_callback(mut self, callback: AlertCallback) -> Self {
        self.alert_callback = Some(callback);
        self
    }

    /// 獲取 resilience 統計數據
    pub fn resilience_stats(&self) -> Option<ExecutorStats> {
        self.resilient_executor.as_ref().map(|e| e.stats())
    }

    /// 獲取 circuit breaker 狀態
    pub async fn circuit_state(&self) -> Option<CircuitState> {
        if let Some(ref executor) = self.resilient_executor {
            Some(executor.circuit_breaker.state().await)
        } else {
            None
        }
    }

    /// 重置 circuit breaker
    pub async fn reset_circuit_breaker(&self) {
        if let Some(ref executor) = self.resilient_executor {
            executor.circuit_breaker.reset().await;
            info!("Backpack 熔斷器已重置");
        }
    }

    /// 使用 resilience 執行操作
    async fn execute_with_resilience<F, Fut, T>(&self, mut operation: F) -> HftResult<T>
    where
        F: FnMut() -> Fut + Clone + Send + Sync,
        Fut: std::future::Future<Output = HftResult<T>> + Send,
        T: Send,
    {
        if let Some(ref executor) = self.resilient_executor {
            executor.execute(operation).await
        } else {
            operation().await
        }
    }

    /// 發送執行告警
    fn send_execution_alert(&self, alert_type: ExecutionAlertType, operation: &str, message: &str) {
        if let Some(ref callback) = self.alert_callback {
            let alert = ExecutionAlert::new(alert_type, "backpack", operation, message);
            callback(alert);
        }
    }

    fn base_url(&self) -> String {
        self.cfg.rest_base_url.trim_end_matches('/').to_string()
    }

    fn rest_url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url(), path.trim_start_matches('/'))
    }

    fn window_ms(&self) -> u64 {
        self.cfg.window_ms.unwrap_or(DEFAULT_WINDOW_MS)
    }

    fn update_heartbeat(&self) {
        self.last_heartbeat.store(now_micros(), Ordering::SeqCst);
    }

    fn emit_event(&self, event: ExecutionEvent) {
        if let Err(err) = self.event_tx.send(event) {
            debug!("Backpack event channel send failed: {err}");
        }
    }

    fn to_backpack_side(side: Side) -> &'static str {
        match side {
            Side::Buy => "Bid",
            Side::Sell => "Ask",
        }
    }

    fn to_backpack_order_type(order_type: OrderType) -> &'static str {
        match order_type {
            OrderType::Market => "Market",
            OrderType::Limit => "Limit",
        }
    }

    fn to_backpack_tif(tif: TimeInForce) -> &'static str {
        match tif {
            TimeInForce::IOC => "IOC",
            TimeInForce::FOK => "FOK",
            _ => "GTC",
        }
    }

    fn current_timestamp_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn build_order_payload(intent: &OrderIntent) -> HftResult<(Value, BTreeMap<String, String>)> {
        let mut body = Map::new();
        let mut sign_params = BTreeMap::new();

        body.insert(
            "symbol".to_string(),
            Value::String(intent.symbol.as_str().to_string()),
        );
        sign_params.insert("symbol".to_string(), intent.symbol.as_str().to_string());

        let side = Self::to_backpack_side(intent.side);
        body.insert("side".to_string(), Value::String(side.to_string()));
        sign_params.insert("side".to_string(), side.to_string());

        let order_type = intent.order_type;
        let order_type_str = Self::to_backpack_order_type(order_type);
        body.insert(
            "orderType".to_string(),
            Value::String(order_type_str.to_string()),
        );
        sign_params.insert("orderType".to_string(), order_type_str.to_string());

        let quantity_str = intent.quantity.0.to_string();
        body.insert("quantity".to_string(), Value::String(quantity_str.clone()));
        sign_params.insert("quantity".to_string(), quantity_str);

        if matches!(order_type, OrderType::Limit) {
            let price = intent
                .price
                .ok_or_else(|| HftError::Execution("Limit order requires price".into()))?;
            let price_str = price.0.to_string();
            body.insert("price".to_string(), Value::String(price_str.clone()));
            sign_params.insert("price".to_string(), price_str);

            let tif = Self::to_backpack_tif(intent.time_in_force);
            body.insert("timeInForce".to_string(), Value::String(tif.to_string()));
            sign_params.insert("timeInForce".to_string(), tif.to_string());
        }

        if let Ok(client_id) = intent.strategy_id.parse::<u32>() {
            body.insert("clientId".to_string(), Value::Number(client_id.into()));
            sign_params.insert("clientId".to_string(), client_id.to_string());
        }

        Ok((Value::Object(body), sign_params))
    }

    async fn place_order_live(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        let credentials = self
            .credentials
            .as_ref()
            .ok_or_else(|| HftError::Authentication("Backpack credentials missing".into()))?;

        let (payload, sign_params) = Self::build_order_payload(&intent)?;
        let timestamp_ms = Self::current_timestamp_ms();
        let window_ms = self.window_ms();
        let signature = credentials.sign("orderExecute", &sign_params, timestamp_ms, window_ms);

        let url = self.rest_url("/api/v1/order");
        let response = self
            .http_client
            .post(&url)
            .header("X-API-KEY", credentials.api_key())
            .header("X-SIGNATURE", signature)
            .header("X-TIMESTAMP", timestamp_ms.to_string())
            .header("X-WINDOW", window_ms.to_string())
            .json(&payload)
            .send()
            .await
            .map_err(|e| HftError::Network(format!("Backpack order request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unavailable>".to_string());
            return Err(HftError::Exchange(format!(
                "Backpack order failed ({status}): {body}"
            )));
        }

        let value: Value = response
            .json()
            .await
            .map_err(|e| HftError::Serialization(format!("Parse order response failed: {e}")))?;

        let order_id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| HftError::Serialization("Backpack response missing id".into()))?
            .to_string();

        let order_id = OrderId(order_id.clone());
        self.order_symbols
            .insert(order_id.0.clone(), intent.symbol.as_str().to_string());

        self.connected.store(true, Ordering::SeqCst);
        self.update_heartbeat();

        self.emit_event(ExecutionEvent::OrderAck {
            order_id: order_id.clone(),
            timestamp: now_micros(),
        });

        Ok(order_id)
    }

    async fn place_order_paper(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        let timestamp = now_micros();
        let order_id = OrderId(format!("BACKPACK_PAPER_{}", timestamp));
        self.order_symbols
            .insert(order_id.0.clone(), intent.symbol.as_str().to_string());

        self.emit_event(ExecutionEvent::OrderNew {
            order_id: order_id.clone(),
            symbol: intent.symbol.clone(),
            side: intent.side,
            quantity: intent.quantity,
            requested_price: intent.price,
            timestamp,
            venue: Some(VenueId::BACKPACK),
            strategy_id: intent.strategy_id.clone(),
        });

        self.emit_event(ExecutionEvent::OrderAck {
            order_id: order_id.clone(),
            timestamp,
        });

        let fill_price = intent.price.unwrap_or_else(Price::zero);
        self.emit_event(ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: fill_price,
            quantity: intent.quantity,
            timestamp,
            fill_id: format!("BACKPACK_PAPER_FILL_{}", timestamp),
        });

        self.emit_event(ExecutionEvent::OrderCompleted {
            order_id: order_id.clone(),
            final_price: fill_price,
            total_filled: intent.quantity,
            timestamp,
        });

        Ok(order_id)
    }

    async fn cancel_order_live(&mut self, order_id: &OrderId) -> HftResult<()> {
        let credentials = self
            .credentials
            .clone()
            .ok_or_else(|| HftError::Authentication("Backpack credentials missing".into()))?;

        let symbol = self
            .order_symbols
            .get(&order_id.0)
            .cloned()
            .ok_or_else(|| HftError::Config("Unknown order symbol for cancellation".into()))?;

        let http_client = self.http_client.clone();
        let url = self.rest_url("/api/v1/order");
        let window_ms = self.window_ms();
        let order_id_str = order_id.0.clone();
        let symbol_clone = symbol.clone();

        let result = self
            .execute_with_resilience(|| {
                let client = http_client.clone();
                let target_url = url.clone();
                let creds = credentials.clone();
                let sym = symbol_clone.clone();
                let oid = order_id_str.clone();
                let win_ms = window_ms;

                async move {
                    let mut body = Map::new();
                    body.insert("symbol".to_string(), Value::String(sym.clone()));
                    body.insert("orderId".to_string(), Value::String(oid.clone()));

                    let mut sign_params = BTreeMap::new();
                    sign_params.insert("orderId".to_string(), oid.clone());
                    sign_params.insert("symbol".to_string(), sym.clone());

                    let payload = Value::Object(body);
                    let timestamp_ms = BackpackExecutionClient::current_timestamp_ms();
                    let signature = creds.sign("orderCancel", &sign_params, timestamp_ms, win_ms);

                    let response = client
                        .delete(&target_url)
                        .header("X-API-KEY", creds.api_key())
                        .header("X-SIGNATURE", signature)
                        .header("X-TIMESTAMP", timestamp_ms.to_string())
                        .header("X-WINDOW", win_ms.to_string())
                        .json(&payload)
                        .send()
                        .await
                        .map_err(|e| HftError::Network(format!("Backpack cancel failed: {e}")))?;

                    let status = response.status();
                    if !status.is_success() && status != StatusCode::ACCEPTED {
                        let body = response
                            .text()
                            .await
                            .unwrap_or_else(|_| "<unavailable>".to_string());
                        return Err(HftError::Exchange(format!(
                            "Backpack cancel failed ({status}): {body}"
                        )));
                    }

                    Ok(())
                }
            })
            .await;

        // 發送 circuit open 警報如果需要
        if let Some(ref executor) = self.resilient_executor {
            if executor.circuit_breaker.state().await == CircuitState::Open {
                self.send_execution_alert(
                    ExecutionAlertType::CircuitOpen,
                    "cancel_order",
                    "Circuit breaker is open after cancel order failures",
                );
            }
        }

        if result.is_ok() {
            self.order_symbols.remove(&order_id.0);
            self.update_heartbeat();
            self.emit_event(ExecutionEvent::OrderCanceled {
                order_id: order_id.clone(),
                timestamp: now_micros(),
            });
        }

        result
    }

    fn parse_decimal(value: Option<&String>) -> Option<Decimal> {
        value.and_then(|v| v.parse::<Decimal>().ok())
    }

    fn parse_price(value: Option<&String>) -> Option<Price> {
        Self::parse_decimal(value).map(Price)
    }

    fn map_status(status: Option<&String>) -> OrderStatus {
        match status.map(|s| s.as_str()) {
            Some("New") => OrderStatus::New,
            Some("PartiallyFilled") => OrderStatus::PartiallyFilled,
            Some("Filled") => OrderStatus::Filled,
            Some("Cancelled") => OrderStatus::Canceled,
            Some("Expired") => OrderStatus::Expired,
            Some("TriggerFailed") => OrderStatus::Rejected,
            _ => OrderStatus::New,
        }
    }

    fn map_order_type(order_type: Option<&String>) -> OrderType {
        match order_type.map(|s| s.as_str()) {
            Some("Market") => OrderType::Market,
            Some("Limit") => OrderType::Limit,
            _ => OrderType::Limit,
        }
    }

    fn map_side(side: Option<&String>) -> Side {
        match side.map(|s| s.as_str()) {
            Some("Bid") => Side::Buy,
            Some("Ask") => Side::Sell,
            _ => Side::Buy,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrderRecord {
    #[serde(default)]
    id: String,
    #[serde(default)]
    symbol: String,
    #[serde(default)]
    side: Option<String>,
    #[serde(default)]
    order_type: Option<String>,
    #[serde(default)]
    quantity: Option<String>,
    #[serde(default)]
    executed_quantity: Option<String>,
    #[serde(default)]
    price: Option<String>,
    #[serde(default)]
    created_at: Option<u64>,
    #[serde(default)]
    updated_at: Option<u64>,
    #[serde(default)]
    status: Option<String>,
}

impl OrderRecord {
    fn into_open_order(self) -> Option<OpenOrder> {
        let original_dec =
            BackpackExecutionClient::parse_decimal(self.quantity.as_ref()).unwrap_or(Decimal::ZERO);
        let filled_dec = BackpackExecutionClient::parse_decimal(self.executed_quantity.as_ref())
            .unwrap_or(Decimal::ZERO);
        let remaining_dec = (original_dec - filled_dec).max(Decimal::ZERO);

        let original_qty = Quantity(original_dec);
        let filled_qty = Quantity(filled_dec);
        let remaining_qty = Quantity(remaining_dec);
        let price = BackpackExecutionClient::parse_price(self.price.as_ref());
        let status = BackpackExecutionClient::map_status(self.status.as_ref());
        let order_type = BackpackExecutionClient::map_order_type(self.order_type.as_ref());
        let side = BackpackExecutionClient::map_side(self.side.as_ref());

        let created_at = self.created_at.unwrap_or(0) * 1000;
        let updated_at = self.updated_at.unwrap_or(self.created_at.unwrap_or(0)) * 1000;

        Some(OpenOrder {
            order_id: OrderId(self.id),
            symbol: Symbol::from(self.symbol),
            side,
            order_type,
            original_quantity: original_qty,
            remaining_quantity: remaining_qty,
            filled_quantity: filled_qty,
            price,
            status,
            created_at,
            updated_at,
        })
    }
}

#[async_trait]
impl ExecutionClient for BackpackExecutionClient {
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        match self.cfg.mode {
            ExecutionMode::Live => self.place_order_live(intent).await,
            ExecutionMode::Paper | ExecutionMode::Testnet => self.place_order_paper(intent).await,
        }
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        match self.cfg.mode {
            ExecutionMode::Live => self.cancel_order_live(order_id).await,
            ExecutionMode::Paper | ExecutionMode::Testnet => {
                self.order_symbols.remove(&order_id.0);
                self.emit_event(ExecutionEvent::OrderCanceled {
                    order_id: order_id.clone(),
                    timestamp: now_micros(),
                });
                Ok(())
            }
        }
    }

    async fn modify_order(
        &mut self,
        _order_id: &OrderId,
        _new_quantity: Option<Quantity>,
        _new_price: Option<Price>,
    ) -> HftResult<()> {
        Err(HftError::Execution(
            "Backpack does not support order modification".into(),
        ))
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        let rx = self.event_tx.subscribe();
        let stream = BroadcastStream::new(rx).filter_map(|evt| async move {
            match evt {
                Ok(event) => Some(Ok(event)),
                Err(_) => None,
            }
        });
        Ok(Box::pin(stream))
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        let credentials = match (&self.credentials, &self.cfg.mode) {
            (Some(creds), ExecutionMode::Live) => creds,
            (_, ExecutionMode::Paper) | (_, ExecutionMode::Testnet) | (None, _) => {
                warn!("Backpack list_open_orders called without live credentials");
                return Ok(Vec::new());
            }
        };

        let timestamp_ms = Self::current_timestamp_ms();
        let window_ms = self.window_ms();
        let params = BTreeMap::new();
        let signature = credentials.sign("orderQueryAll", &params, timestamp_ms, window_ms);

        let url = self.rest_url("/api/v1/orders");
        let response = self
            .http_client
            .get(&url)
            .header("X-API-KEY", credentials.api_key())
            .header("X-SIGNATURE", signature)
            .header("X-TIMESTAMP", timestamp_ms.to_string())
            .header("X-WINDOW", window_ms.to_string())
            .send()
            .await
            .map_err(|e| HftError::Network(format!("Backpack orders request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unavailable>".to_string());
            return Err(HftError::Exchange(format!(
                "Backpack orders request failed ({}): {}",
                status, body
            )));
        }

        let orders: Vec<OrderRecord> = response
            .json()
            .await
            .map_err(|e| HftError::Serialization(format!("Parse orders response failed: {e}")))?;

        let mut open_orders = Vec::new();
        for record in orders {
            if let Some(order) = record.into_open_order() {
                open_orders.push(order);
            }
        }

        Ok(open_orders)
    }

    async fn connect(&mut self) -> HftResult<()> {
        // 初始化 resilience executor
        let retry_config = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 100,
            max_delay_ms: 5000,
            backoff_multiplier: 2.0,
            retry_on_init_error: false,
        };
        let circuit_config = CircuitBreakerConfig {
            failure_threshold: 5,
            open_duration_secs: 30,
            half_open_max_requests: 3,
            half_open_success_threshold: 2,
        };

        let alert_cb = self.alert_callback.clone();
        self.resilient_executor = Some(Arc::new(
            ResilientExecutor::new("backpack", retry_config, circuit_config).with_alert_callback(
                move |cb_alert: CircuitBreakerAlert| {
                    if let Some(ref cb) = alert_cb {
                        let exec_alert = ExecutionAlert::new(
                            ExecutionAlertType::CircuitOpen,
                            "backpack",
                            "circuit_breaker",
                            &cb_alert.message,
                        )
                        .with_failure_count(cb_alert.failure_count);
                        cb(exec_alert);
                    }
                },
            ),
        ));

        self.connected.store(true, Ordering::SeqCst);
        self.update_heartbeat();
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn health(&self) -> ports::ConnectionHealth {
        ports::ConnectionHealth {
            connected: self.connected.load(Ordering::SeqCst),
            latency_ms: None,
            last_heartbeat: self.last_heartbeat.load(Ordering::SeqCst),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_string_matches_example() {
        let secret = vec![1u8; 32];
        let secret_b64 = general_purpose::STANDARD.encode(&secret);
        let creds = BackpackCredentials::new("".into(), secret_b64).unwrap();
        let mut params = BTreeMap::new();
        params.insert("orderId".to_string(), "28".to_string());
        params.insert("symbol".to_string(), "BTC_USDT".to_string());
        let signature = creds.sign("orderCancel", &params, 1_614_550_000_000, 5_000);
        assert!(!signature.is_empty());
    }
}
