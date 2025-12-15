//! OKX 執行適配器（REST + 私有 WS）
//!
//! 功能：
//! - REST API 下單、撤單、修改訂單
//! - 私有 WebSocket 接收成交回報
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
    signing::{OkxCredentials, OkxSigner},
};
use ports::{BoxStream, ExecutionClient, ExecutionEvent, OpenOrder};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{info, warn};

fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, HftError> {
    let mut bytes = text.as_bytes().to_vec();
    simd_json::serde::from_slice(bytes.as_mut_slice())
        .map_err(|e| HftError::Serialization(e.to_string()))
}

#[derive(Debug, Clone)]
pub struct OkxExecutionConfig {
    pub credentials: OkxCredentials,
    pub rest_base_url: String,
    pub ws_private_url: String,
    pub timeout_ms: u64,
    pub mode: ExecutionMode,
}

pub struct OkxExecutionClient {
    cfg: OkxExecutionConfig,
    http: Option<HttpClient>,
    signer: OkxSigner,
    event_tx: Option<broadcast::Sender<ExecutionEvent>>,
    connected: bool,
    order_inst: std::collections::HashMap<String, String>,
    // 韌性執行器 (重試 + 熔斷器)
    resilient_executor: Option<Arc<ResilientExecutor>>,
    // 告警回調
    alert_callback: Option<AlertCallback>,
}

impl OkxExecutionClient {
    pub fn new(cfg: OkxExecutionConfig) -> Result<Self, HftError> {
        Ok(Self {
            http: None,
            signer: OkxSigner::new(cfg.credentials.clone()),
            cfg,
            event_tx: None,
            connected: false,
            order_inst: std::collections::HashMap::new(),
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
            info!("[OKX] 熔斷器已手動重置");
        }
    }

    fn ensure_http(&mut self) -> HftResult<()> {
        if self.http.is_none() {
            let hc = HttpClientConfig {
                base_url: self.cfg.rest_base_url.clone(),
                timeout_ms: self.cfg.timeout_ms,
                user_agent: "hft-okx-exec/1.0".to_string(),
            };
            self.http = Some(HttpClient::new(hc).map_err(|e| HftError::Network(e.to_string()))?);
        }
        Ok(())
    }

    /// 獲取 HTTP 客戶端引用
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

#[async_trait]
impl ExecutionClient for OkxExecutionClient {
    async fn place_order(&mut self, intent: ports::OrderIntent) -> HftResult<OrderId> {
        if self.cfg.mode == ExecutionMode::Live {
            self.ensure_http()?;
            let http = self.get_http()?;
            #[derive(serde::Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Req<'a> {
                inst_id: &'a str,
                td_mode: &'a str,
                side: &'a str,
                ord_type: &'a str,
                sz: String,
                px: Option<String>,
            }
            let req = Req {
                inst_id: intent.symbol.as_str(),
                td_mode: "cash",
                side: match intent.side {
                    hft_core::Side::Buy => "buy",
                    _ => "sell",
                },
                ord_type: match intent.order_type {
                    hft_core::OrderType::Market => "market",
                    _ => "limit",
                },
                sz: intent.quantity.0.to_string(),
                px: intent.price.map(|p| p.0.to_string()),
            };
            let body =
                serde_json::to_string(&req).map_err(|e| HftError::Serialization(e.to_string()))?;
            let headers = self
                .signer
                .generate_headers("POST", "/api/v5/trade/order", &body, None);
            let resp = http
                .signed_request(
                    reqwest::Method::POST,
                    "/api/v5/trade/order",
                    Some(headers),
                    Some(body),
                )
                .await
                .map_err(|e| HftError::Network(e.to_string()))?;
            #[derive(serde::Deserialize)]
            struct Resp {
                code: String,
                msg: String,
                #[serde(default)]
                data: Vec<serde_json::Value>,
            }
            let r: Resp = HttpClient::parse_json(resp)
                .await
                .map_err(|e| HftError::Serialization(e.to_string()))?;
            if r.code != "0" {
                return Err(HftError::Exchange(format!(
                    "OKX 下單失敗: {} {}",
                    r.code, r.msg
                )));
            }
            let ord_id = r
                .data
                .first()
                .and_then(|v| v.get("ordId"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(ExecutionEvent::OrderAck {
                    order_id: OrderId(ord_id.clone()),
                    timestamp: hft_core::now_micros(),
                });
            }
            self.order_inst
                .insert(ord_id.clone(), intent.symbol.as_str().to_string());
            return Ok(OrderId(ord_id));
        }
        // Paper
        let oid = OrderId(format!("OKX_PAPER_{}", hft_core::now_micros()));
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
                    fill_id: format!("OKXFILL-{}", hft_core::now_micros()),
                });
            }
        }
        Ok(oid)
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        if self.cfg.mode != ExecutionMode::Live {
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(ExecutionEvent::OrderCanceled {
                    order_id: order_id.clone(),
                    timestamp: hft_core::now_micros(),
                });
            }
            return Ok(());
        }

        self.ensure_http()?;

        // 提前提取所有需要的數據
        let inst = self
            .order_inst
            .get(&order_id.0)
            .cloned()
            .unwrap_or_else(|| "BTC-USDT".to_string());

        #[derive(Clone, serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Req {
            inst_id: String,
            ord_id: String,
        }

        let req = Req {
            inst_id: inst,
            ord_id: order_id.0.clone(),
        };

        let body = serde_json::to_string(&req).map_err(|e| HftError::Serialization(e.to_string()))?;
        let headers = self
            .signer
            .generate_headers("POST", "/api/v5/trade/cancel-order", &body, None);

        let http_client = self.get_http()?.clone();
        let order_id_clone = order_id.clone();
        let event_tx = self.event_tx.clone();

        // 使用韌性執行器執行撤單操作
        let result = self
            .execute_with_resilience(|| {
                let http = http_client.clone();
                let hdrs = headers.clone();
                let bd = body.clone();
                let oid = order_id_clone.clone();
                let tx = event_tx.clone();

                async move {
                    #[derive(serde::Deserialize)]
                    struct Resp {
                        code: String,
                        msg: String,
                    }

                    let resp = http
                        .signed_request(
                            reqwest::Method::POST,
                            "/api/v5/trade/cancel-order",
                            Some(hdrs),
                            Some(bd),
                        )
                        .await
                        .map_err(|e| HftError::Network(e.to_string()))?;

                    let r: Resp = HttpClient::parse_json(resp)
                        .await
                        .map_err(|e| HftError::Serialization(e.to_string()))?;

                    if r.code != "0" {
                        return Err(HftError::Exchange(format!("OKX 撤單失敗: {} {}", r.code, r.msg)));
                    }

                    if let Some(ref tx) = tx {
                        let _ = tx.send(ExecutionEvent::OrderCanceled {
                            order_id: oid,
                            timestamp: hft_core::now_micros(),
                        });
                    }

                    Ok(())
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
                            "okx",
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
        if self.cfg.mode != ExecutionMode::Live {
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(ExecutionEvent::OrderModified {
                    order_id: order_id.clone(),
                    new_quantity,
                    new_price,
                    timestamp: hft_core::now_micros(),
                });
            }
            return Ok(());
        }

        self.ensure_http()?;

        // 提前提取所有需要的數據
        let inst = self
            .order_inst
            .get(&order_id.0)
            .cloned()
            .unwrap_or_else(|| "BTC-USDT".to_string());

        #[derive(Clone, serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Req {
            inst_id: String,
            ord_id: String,
            new_sz: Option<String>,
            new_px: Option<String>,
        }

        let req = Req {
            inst_id: inst,
            ord_id: order_id.0.clone(),
            new_sz: new_quantity.map(|q| q.0.to_string()),
            new_px: new_price.map(|p| p.0.to_string()),
        };

        let body = serde_json::to_string(&req).map_err(|e| HftError::Serialization(e.to_string()))?;
        let headers = self
            .signer
            .generate_headers("POST", "/api/v5/trade/amend-order", &body, None);

        let http_client = self.get_http()?.clone();
        let order_id_clone = order_id.clone();
        let event_tx = self.event_tx.clone();

        // 使用韌性執行器執行修改操作
        let result = self
            .execute_with_resilience(|| {
                let http = http_client.clone();
                let hdrs = headers.clone();
                let bd = body.clone();
                let oid = order_id_clone.clone();
                let tx = event_tx.clone();
                let qty = new_quantity;
                let px = new_price;

                async move {
                    #[derive(serde::Deserialize)]
                    struct Resp {
                        code: String,
                        msg: String,
                    }

                    let resp = http
                        .signed_request(
                            reqwest::Method::POST,
                            "/api/v5/trade/amend-order",
                            Some(hdrs),
                            Some(bd),
                        )
                        .await
                        .map_err(|e| HftError::Network(e.to_string()))?;

                    let r: Resp = HttpClient::parse_json(resp)
                        .await
                        .map_err(|e| HftError::Serialization(e.to_string()))?;

                    if r.code != "0" {
                        return Err(HftError::Exchange(format!("OKX 改單失敗: {} {}", r.code, r.msg)));
                    }

                    if let Some(ref tx) = tx {
                        let _ = tx.send(ExecutionEvent::OrderModified {
                            order_id: oid,
                            new_quantity: qty,
                            new_price: px,
                            timestamp: hft_core::now_micros(),
                        });
                    }

                    Ok(())
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
                            "okx",
                            "modify_order",
                            format!("改單失敗: {}", e),
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

        let mut executor = ResilientExecutor::new("okx", retry_config, cb_config);

        // 設置熔斷器告警回調
        if let Some(ref alert_cb) = self.alert_callback {
            let alert_cb = Arc::clone(alert_cb);
            executor = executor.with_alert_callback(move |cb_alert| {
                let alert_type = match cb_alert.state {
                    CircuitState::Open => ExecutionAlertType::CircuitOpen,
                    CircuitState::Closed => ExecutionAlertType::CircuitRecovered,
                    CircuitState::HalfOpen => return,
                };

                let alert = ExecutionAlert::new(
                    alert_type,
                    "okx",
                    "execution",
                    &cb_alert.message,
                )
                .with_failure_count(cb_alert.failure_count);

                alert_cb(alert);
            });
        }

        self.resilient_executor = Some(Arc::new(executor));

        let (tx, _) = broadcast::channel(1000);
        self.event_tx = Some(tx.clone());
        self.connected = true;
        self.ensure_http()?;

        info!("[OKX] 執行客戶端連接成功");

        if self.cfg.mode == ExecutionMode::Live {
            let url = self.cfg.ws_private_url.clone();
            let api_key = self.cfg.credentials.api_key.clone();
            let passphrase = self.cfg.credentials.passphrase.clone();
            let signer = self.signer.clone();
            tokio::spawn(async move {
                match tokio_tungstenite::connect_async(&url).await {
                    Ok((mut ws, _)) => {
                        // login
                        let ts = integration::signing::OkxSigner::rfc3339_timestamp();
                        let sign = signer.ws_login_signature(&ts);
                        let login = serde_json::json!({
                            "op": "login",
                            "args": [{"apiKey": api_key, "passphrase": passphrase, "timestamp": ts, "sign": sign}]
                        });
                        let _ = ws
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                login.to_string(),
                            ))
                            .await;
                        // subscribe orders
                        let sub = serde_json::json!({"op":"subscribe","args":[{"channel":"orders","instType":"SPOT"}]});
                        let _ = ws
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                sub.to_string(),
                            ))
                            .await;
                        while let Some(msg) = ws.next().await {
                            if let Ok(tokio_tungstenite::tungstenite::Message::Text(txt)) = msg {
                                if let Ok(v) = parse_json::<Value>(&txt) {
                                    if v.get("arg")
                                        .and_then(|a| a.get("channel"))
                                        .and_then(|x| x.as_str())
                                        == Some("orders")
                                    {
                                        if let Some(data) = v
                                            .get("data")
                                            .and_then(|d| d.as_array())
                                            .and_then(|arr| arr.first())
                                        {
                                            let oid = data
                                                .get("ordId")
                                                .and_then(|x| x.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let state = data
                                                .get("state")
                                                .and_then(|x| x.as_str())
                                                .unwrap_or("");
                                            let ts = data
                                                .get("uTime")
                                                .and_then(|x| x.as_str())
                                                .and_then(|s| s.parse::<u64>().ok())
                                                .unwrap_or(hft_core::now_micros())
                                                * 1000;
                                            match state {
                                                "live" | "new" => {
                                                    let _ = tx.send(ExecutionEvent::OrderAck {
                                                        order_id: OrderId(oid.clone()),
                                                        timestamp: ts,
                                                    });
                                                }
                                                "canceled" => {
                                                    let _ =
                                                        tx.send(ExecutionEvent::OrderCanceled {
                                                            order_id: OrderId(oid.clone()),
                                                            timestamp: ts,
                                                        });
                                                }
                                                "rejected" => {
                                                    let _ = tx.send(ExecutionEvent::OrderReject {
                                                        order_id: OrderId(oid.clone()),
                                                        reason: "Exchange rejected".to_string(),
                                                        timestamp: ts,
                                                    });
                                                }
                                                _ => {
                                                    // fill fields: fillPx, fillSz
                                                    let px = data
                                                        .get("fillPx")
                                                        .and_then(|x| x.as_str())
                                                        .and_then(|s| Price::from_str(s).ok());
                                                    let sz = data
                                                        .get("fillSz")
                                                        .and_then(|x| x.as_str())
                                                        .and_then(|s| Quantity::from_str(s).ok());
                                                    if let (Some(p), Some(q)) = (px, sz) {
                                                        let _ = tx.send(ExecutionEvent::Fill {
                                                            order_id: OrderId(oid.clone()),
                                                            price: p,
                                                            quantity: q,
                                                            timestamp: ts,
                                                            fill_id: format!("OKX-{}", ts),
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if let Ok(tokio_tungstenite::tungstenite::Message::Close(_)) =
                                msg
                            {
                                break;
                            }
                        }
                    }
                    Err(e) => warn!("OKX 私有 WS 連線失敗: {}", e),
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
        if self.cfg.mode != ExecutionMode::Live {
            return Ok(Vec::new());
        }
        let http_local;
        let http: &HttpClient = if let Some(h) = &self.http {
            h
        } else {
            let hc = HttpClientConfig {
                base_url: self.cfg.rest_base_url.clone(),
                timeout_ms: self.cfg.timeout_ms,
                user_agent: "hft-okx-exec/1.0".to_string(),
            };
            http_local = HttpClient::new(hc).map_err(|e| HftError::Network(e.to_string()))?;
            &http_local
        };
        // GET /api/v5/trade/orders-pending?instType=SPOT
        let query = "instType=SPOT";
        let path = format!("/api/v5/trade/orders-pending?{}", query);
        let headers = self.signer.generate_headers("GET", &path, "", None);
        let resp = http
            .signed_request(reqwest::Method::GET, &path, Some(headers), None)
            .await
            .map_err(|e| HftError::Network(e.to_string()))?;

        #[derive(serde::Deserialize)]
        #[allow(dead_code)]
        struct Item {
            #[serde(rename = "ordId")]
            ord_id: String,
            #[serde(rename = "instId")]
            inst_id: String,
            side: String,
            #[serde(rename = "ordType")]
            ord_type: String,
            sz: String,
            #[serde(rename = "fillSz")]
            fill_sz: String,
            px: Option<String>,
            state: String,
            #[serde(rename = "cTime")]
            c_time: String,
            #[serde(rename = "uTime")]
            u_time: String,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            code: String,
            msg: String,
            data: Option<Vec<Item>>,
        }

        let r: Resp = integration::http::HttpClient::parse_json(resp)
            .await
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        if r.code != "0" {
            return Err(HftError::Exchange(format!(
                "OKX 查詢未結失敗: {} {}",
                r.code, r.msg
            )));
        }
        let mut out = Vec::new();
        if let Some(items) = r.data {
            for it in items {
                let side = match it.side.as_str() {
                    "buy" => hft_core::Side::Buy,
                    _ => hft_core::Side::Sell,
                };
                let order_type = match it.ord_type.as_str() {
                    "market" => hft_core::OrderType::Market,
                    _ => hft_core::OrderType::Limit,
                };
                let qty = Quantity::from_str(&it.sz).unwrap_or(Quantity::zero());
                let filled = Quantity::from_str(&it.fill_sz).unwrap_or(Quantity::zero());
                let remaining = Quantity(qty.0 - filled.0);
                let price = it.px.as_ref().and_then(|s| Price::from_str(s).ok());
                let status = match it.state.as_str() {
                    "live" | "new" => ports::OrderStatus::New,
                    "partially_filled" => ports::OrderStatus::PartiallyFilled,
                    "filled" => ports::OrderStatus::Filled,
                    "canceled" => ports::OrderStatus::Canceled,
                    "rejected" => ports::OrderStatus::Rejected,
                    _ => ports::OrderStatus::Accepted,
                };
                let created_at = it.c_time.parse::<u64>().unwrap_or(0) * 1000;
                let updated_at = it.u_time.parse::<u64>().unwrap_or(0) * 1000;
                out.push(OpenOrder {
                    order_id: OrderId(it.ord_id),
                    symbol: hft_core::Symbol::from(it.inst_id),
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
        Ok(out)
    }
}
