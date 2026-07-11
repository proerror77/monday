//! Bybit 執行適配器（v5 REST + 私有 WS）
//! - 支援 Live/Testnet/Paper 三種模式
//! - 韌性機制：重試、熔斷器、告警通知

use async_trait::async_trait;
use execution::{
    AlertCallback, CircuitBreakerConfig, CircuitState, ExecutionAlert, ExecutionAlertType,
    ExecutorStats, ResilientExecutor, RetryConfig,
};
// Re-export ExecutionMode for backwards compatibility
pub use execution::ExecutionMode;
use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult, OrderId, Price, Quantity};
use integration::{
    http::{HttpClient, HttpClientConfig},
    signing::{BybitCredentials, BybitSigner},
};
use ports::{BoxStream, ExecutionClient, ExecutionEvent, OpenOrder};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{info, warn};

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
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
struct BybitOpenOrdersItem {
    orderId: String,
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
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
struct BybitOpenOrdersResponse {
    retCode: i64,
    retMsg: String,
    data: Option<BybitOpenOrdersData>,
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

    /// 執行帶韌性保護的操作
    async fn execute_with_resilience<T, F, Fut>(&self, operation: F) -> HftResult<T>
    where
        F: FnMut() -> Fut + Clone,
        Fut: std::future::Future<Output = HftResult<T>>,
    {
        if let Some(ref executor) = self.resilient_executor {
            executor.execute(operation).await
        } else {
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
}

fn bybit_mode_label(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Paper => "Paper",
        ExecutionMode::Live => "Live",
        ExecutionMode::Testnet => "Testnet",
    }
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
        return Err(HftError::Exchange(format!(
            "Bybit 查詢未結失敗: {} {}",
            response.retCode, response.retMsg
        )));
    }

    let data = response
        .data
        .ok_or_else(|| HftError::Parse("Bybit 未結訂單回應缺少 data".to_string()))?;

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

#[async_trait]
impl ExecutionClient for BybitExecutionClient {
    async fn place_order(&mut self, intent: ports::OrderIntent) -> HftResult<OrderId> {
        if matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            self.ensure_http()?;
            let http = self.get_http()?;
            #[derive(serde::Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Req<'a> {
                category: &'a str,
                symbol: &'a str,
                side: &'a str,
                order_type: &'a str,
                qty: String,
                price: Option<String>,
                time_in_force: &'a str,
            }
            let side = match intent.side {
                hft_core::Side::Buy => "Buy",
                _ => "Sell",
            };
            let typ = match intent.order_type {
                hft_core::OrderType::Market => "Market",
                _ => "Limit",
            };
            let tif = match intent.time_in_force {
                hft_core::TimeInForce::IOC => "IOC",
                hft_core::TimeInForce::FOK => "FOK",
                _ => "GTC",
            };
            let req = Req {
                category: "spot",
                symbol: intent.symbol.as_str(),
                side,
                order_type: typ,
                qty: intent.quantity.0.to_string(),
                price: intent.price.map(|p| p.0.to_string()),
                time_in_force: tif,
            };
            let body =
                serde_json::to_string(&req).map_err(|e| HftError::Serialization(e.to_string()))?;
            let headers = self
                .signer
                .generate_headers("POST", "/v5/order/create", &body, None);
            let resp = http
                .signed_request(
                    reqwest::Method::POST,
                    "/v5/order/create",
                    Some(headers),
                    Some(body),
                )
                .await
                .map_err(|e| HftError::Network(e.to_string()))?;
            #[derive(serde::Deserialize)]
            #[allow(non_snake_case)]
            struct Resp {
                retCode: i64,
                retMsg: String,
                #[serde(default)]
                data: serde_json::Value,
            }
            let r: Resp = HttpClient::parse_json(resp)
                .await
                .map_err(|e| HftError::Serialization(e.to_string()))?;
            if r.retCode != 0 {
                return Err(HftError::Exchange(format!(
                    "Bybit 下單失敗: {} {}",
                    r.retCode, r.retMsg
                )));
            }
            let ord_id = r
                .data
                .get("orderId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(ExecutionEvent::OrderAck {
                    order_id: OrderId(ord_id.clone()),
                    timestamp: hft_core::now_micros(),
                });
            }
            return Ok(OrderId(ord_id));
        }
        // Paper
        let oid = OrderId(format!("BYBIT_PAPER_{}", hft_core::now_micros()));
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

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        if matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            self.ensure_http()?;
            let http_client = self.get_http()?.clone();
            let order_id_str = order_id.0.clone();
            let signer_clone = self.signer.clone();

            let result = self
                .execute_with_resilience(|| {
                    let http = http_client.clone();
                    let oid = order_id_str.clone();
                    let sig = signer_clone.clone();
                    async move {
                        #[derive(serde::Serialize)]
                        #[serde(rename_all = "camelCase")]
                        struct Req {
                            category: String,
                            order_id: String,
                        }
                        let req = Req {
                            category: "spot".to_string(),
                            order_id: oid,
                        };
                        let body = serde_json::to_string(&req)
                            .map_err(|e| HftError::Serialization(e.to_string()))?;
                        let headers = sig.generate_headers("POST", "/v5/order/cancel", &body, None);
                        let resp = http
                            .signed_request(
                                reqwest::Method::POST,
                                "/v5/order/cancel",
                                Some(headers),
                                Some(body),
                            )
                            .await
                            .map_err(|e| HftError::Network(e.to_string()))?;
                        #[derive(serde::Deserialize)]
                        #[allow(non_snake_case)]
                        struct Resp {
                            retCode: i64,
                            retMsg: String,
                        }
                        let r: Resp = HttpClient::parse_json(resp)
                            .await
                            .map_err(|e| HftError::Serialization(e.to_string()))?;
                        if r.retCode != 0 {
                            return Err(HftError::Exchange(format!(
                                "Bybit 撤單失敗: {} {}",
                                r.retCode, r.retMsg
                            )));
                        }
                        Ok(())
                    }
                })
                .await;

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

            if result.is_ok() {
                if let Some(ref tx) = self.event_tx {
                    let _ = tx.send(ExecutionEvent::OrderCanceled {
                        order_id: order_id.clone(),
                        timestamp: hft_core::now_micros(),
                    });
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
            self.ensure_http()?;
            let http_client = self.get_http()?.clone();
            let order_id_str = order_id.0.clone();
            let signer_clone = self.signer.clone();
            let qty_str = new_quantity.map(|q| q.0.to_string());
            let price_str = new_price.map(|p| p.0.to_string());

            let result = self
                .execute_with_resilience(|| {
                    let http = http_client.clone();
                    let oid = order_id_str.clone();
                    let sig = signer_clone.clone();
                    let qty = qty_str.clone();
                    let px = price_str.clone();
                    async move {
                        #[derive(serde::Serialize)]
                        #[serde(rename_all = "camelCase")]
                        struct Req {
                            category: String,
                            order_id: String,
                            qty: Option<String>,
                            price: Option<String>,
                        }
                        let req = Req {
                            category: "spot".to_string(),
                            order_id: oid,
                            qty,
                            price: px,
                        };
                        let body = serde_json::to_string(&req)
                            .map_err(|e| HftError::Serialization(e.to_string()))?;
                        let headers = sig.generate_headers("POST", "/v5/order/amend", &body, None);
                        let resp = http
                            .signed_request(
                                reqwest::Method::POST,
                                "/v5/order/amend",
                                Some(headers),
                                Some(body),
                            )
                            .await
                            .map_err(|e| HftError::Network(e.to_string()))?;
                        #[derive(serde::Deserialize)]
                        #[allow(non_snake_case)]
                        struct Resp {
                            retCode: i64,
                            retMsg: String,
                        }
                        let r: Resp = HttpClient::parse_json(resp)
                            .await
                            .map_err(|e| HftError::Serialization(e.to_string()))?;
                        if r.retCode != 0 {
                            return Err(HftError::Exchange(format!(
                                "Bybit 改單失敗: {} {}",
                                r.retCode, r.retMsg
                            )));
                        }
                        Ok(())
                    }
                })
                .await;

            // 如果熔斷器開啟，發送告警
            if let Err(ref e) = result {
                if let Some(ref executor) = self.resilient_executor {
                    if executor.circuit_breaker.state().await == CircuitState::Open {
                        self.send_execution_alert(
                            ExecutionAlert::new(
                                ExecutionAlertType::CircuitOpen,
                                "bybit",
                                "modify_order",
                                format!("改單失敗且熔斷器已開啟 (order_id={}): {}", order_id.0, e),
                            )
                            .with_error(e.to_string()),
                        );
                    }
                }
            }

            if result.is_ok() {
                if let Some(ref tx) = self.event_tx {
                    let _ = tx.send(ExecutionEvent::OrderModified {
                        order_id: order_id.clone(),
                        new_quantity,
                        new_price,
                        timestamp: hft_core::now_micros(),
                    });
                }
            }
            return result;
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
            let s = tokio_stream::wrappers::BroadcastStream::new(rx)
                .filter_map(|e| async move { e.ok().map(Ok) });
            return Ok(Box::pin(s));
        }
        Ok(Box::pin(futures::stream::empty()))
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

        let (tx, _) = broadcast::channel(1000);
        self.event_tx = Some(tx.clone());
        self.connected = true;
        self.ensure_http()?;

        info!("[Bybit] 執行客戶端連接成功");

        if matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            // 私有 WS：簡化處理，僅嘗試 auth + 訂閱 order/execution
            let ws_url = self.config.ws_private_url.clone();
            let api_key = self.config.credentials.api_key.clone();
            let secret = self.config.credentials.secret_key.clone();
            tokio::spawn(async move {
                if let Ok((mut ws, _)) = tokio_tungstenite::connect_async(&ws_url).await {
                    // WS 認證：Bybit v5: op=auth
                    // 簽名: hex(HMAC_SHA256(secret, timestamp + apiKey + recvWindow))
                    let ts = integration::signing::BybitSigner::current_timestamp().to_string();
                    let recv_window = "5000";
                    let msg = format!("{}{}{}", ts, api_key, recv_window);
                    let sign = {
                        use hmac::{Hmac, Mac};
                        use sha2::Sha256;
                        type HmacSha256 = Hmac<Sha256>;
                        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
                            .expect("HMAC accepts any key length");
                        mac.update(msg.as_bytes());
                        hex::encode(mac.finalize().into_bytes())
                    };
                    let auth = serde_json::json!({
                        "op": "auth",
                        "args": [api_key, ts, recv_window, sign]
                    });
                    let _ = ws
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            auth.to_string().into(),
                        ))
                        .await;
                    // 訂閱 order/execution
                    let sub = serde_json::json!({"op":"subscribe","args":["order.spot","execution.spot"]});
                    let _ = ws
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            sub.to_string().into(),
                        ))
                        .await;
                    while let Some(msg) = ws.next().await {
                        if let Ok(tokio_tungstenite::tungstenite::Message::Text(txt)) = msg {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                                let topic = v.get("topic").and_then(|x| x.as_str()).unwrap_or("");
                                if topic.starts_with("order") {
                                    if let Some(d) = v
                                        .get("data")
                                        .and_then(|d| d.as_array())
                                        .and_then(|arr| arr.first())
                                    {
                                        let status = d
                                            .get("orderStatus")
                                            .and_then(|x| x.as_str())
                                            .unwrap_or("");
                                        let oid = d
                                            .get("orderId")
                                            .and_then(|x| x.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let ts = d
                                            .get("updatedTime")
                                            .and_then(|x| x.as_str())
                                            .and_then(|s| s.parse::<u64>().ok())
                                            .unwrap_or(hft_core::now_micros())
                                            * 1000;
                                        match status {
                                            "New" | "Created" => {
                                                let _ = tx.send(ExecutionEvent::OrderAck {
                                                    order_id: OrderId(oid.clone()),
                                                    timestamp: ts,
                                                });
                                            }
                                            "Cancelled" | "Rejected" => {
                                                let _ = tx.send(ExecutionEvent::OrderCanceled {
                                                    order_id: OrderId(oid.clone()),
                                                    timestamp: ts,
                                                });
                                            }
                                            _ => {}
                                        }
                                    }
                                } else if topic.starts_with("execution") {
                                    if let Some(d) = v
                                        .get("data")
                                        .and_then(|d| d.as_array())
                                        .and_then(|arr| arr.first())
                                    {
                                        let oid = d
                                            .get("orderId")
                                            .and_then(|x| x.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let px = d
                                            .get("execPrice")
                                            .and_then(|x| x.as_str())
                                            .and_then(|s| Price::from_str(s).ok());
                                        let qty = d
                                            .get("execQty")
                                            .and_then(|x| x.as_str())
                                            .and_then(|s| Quantity::from_str(s).ok());
                                        let ts = d
                                            .get("execTime")
                                            .and_then(|x| x.as_str())
                                            .and_then(|s| s.parse::<u64>().ok())
                                            .unwrap_or(hft_core::now_micros())
                                            * 1000;
                                        if let (Some(p), Some(q)) = (px, qty) {
                                            let _ = tx.send(ExecutionEvent::Fill {
                                                order_id: OrderId(oid),
                                                price: p,
                                                quantity: q,
                                                timestamp: ts,
                                                fill_id: format!("BYBIT-{}", ts),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    warn!("Bybit 私有 WS 連線失敗");
                }
            });
        }
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
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
        if !matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            return Err(HftError::Config(format!(
                "Bybit list_open_orders 不支援 {} 模式",
                bybit_mode_label(self.config.mode)
            )));
        }
        let http_local;
        let http: &HttpClient = if let Some(h) = &self.http {
            h
        } else {
            let cfg = HttpClientConfig {
                base_url: self.config.rest_base_url.clone(),
                timeout_ms: self.config.timeout_ms,
                user_agent: "hft-bybit-exec/1.0".to_string(),
            };
            http_local = HttpClient::new(cfg).map_err(|e| HftError::Network(e.to_string()))?;
            &http_local
        };

        // GET /v5/order/realtime?category=spot
        let path = "/v5/order/realtime?category=spot";
        let headers =
            self.signer
                .generate_headers("GET", "/v5/order/realtime", "category=spot", None);
        let resp = http
            .signed_request(reqwest::Method::GET, path, Some(headers), None)
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;

        let r: BybitOpenOrdersResponse = integration::http::HttpClient::parse_json(resp)
            .await
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        parse_bybit_open_orders_response(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderType, Side, Symbol, TimeInForce};
    use integration::signing::BybitCredentials;
    use ports::{ExecutionClient, OrderIntent};

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

    #[test]
    fn test_parse_bybit_open_orders_missing_data_fails() {
        let response = BybitOpenOrdersResponse {
            retCode: 0,
            retMsg: "OK".to_string(),
            data: None,
        };

        let result = parse_bybit_open_orders_response(response);
        assert!(matches!(result, Err(HftError::Parse(message)) if message.contains("缺少 data")));
    }

    #[test]
    fn test_parse_bybit_open_orders_unknown_side_fails() {
        let response = BybitOpenOrdersResponse {
            retCode: 0,
            retMsg: "OK".to_string(),
            data: Some(BybitOpenOrdersData {
                list: vec![BybitOpenOrdersItem {
                    orderId: "1".to_string(),
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
            }),
        };

        let result = parse_bybit_open_orders_response(response);
        assert!(matches!(result, Err(HftError::Parse(message)) if message.contains("未知 side")));
    }
}
