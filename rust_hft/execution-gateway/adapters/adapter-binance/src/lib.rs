//! Binance 執行 adapter（實作 `ports::ExecutionClient`）
//! - 支援 Paper 模式（模擬 ACK/Fill）與 Live 模式（REST 下單 + 私有 WS 回報）
//! - 韌性機制：重試、熔斷器、告警通知

use async_trait::async_trait;
use execution::{
    AlertCallback, CircuitBreakerConfig, CircuitState, ExecutionAlert, ExecutionAlertType,
    ExecutorStats, ResilientExecutor, RetryConfig,
};
// Re-export ExecutionMode for backwards compatibility
pub use execution::ExecutionMode;
use futures::{stream, StreamExt};
use hft_core::{HftResult, OrderId, Price, Quantity};
use integration::{
    http::{HttpClient, HttpClientConfig},
    signing::{BinanceCredentials, BinanceSigner},
};
use ports::{BoxStream, ExecutionClient, ExecutionEvent, OpenOrder};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

#[derive(Debug, Clone)]
pub struct BinanceExecutionConfig {
    pub credentials: BinanceCredentials,
    pub rest_base_url: String,
    pub ws_base_url: String,
    pub timeout_ms: u64,
    pub mode: ExecutionMode,
}

pub struct BinanceExecutionClient {
    event_tx: Option<broadcast::Sender<ExecutionEvent>>,
    connected: bool,
    http_client: Option<HttpClient>,
    signer: Option<BinanceSigner>,
    rest_base_url: String,
    ws_base_url: String,
    mode: ExecutionMode,
    // order_id -> symbol 快取，撤單時需要
    order_symbol: HashMap<String, String>,
    // listenKey 維護
    listen_key: Option<String>,
    // 韌性執行器 (重試 + 熔斷器)
    resilient_executor: Option<Arc<ResilientExecutor>>,
    // 告警回調
    alert_callback: Option<AlertCallback>,
}

impl BinanceExecutionClient {
    pub fn new(cfg: BinanceExecutionConfig) -> Self {
        let signer =
            if !cfg.credentials.api_key.is_empty() && !cfg.credentials.secret_key.is_empty() {
                Some(BinanceSigner::new(cfg.credentials.clone()))
            } else {
                None
            };

        Self {
            event_tx: None,
            connected: false,
            http_client: None,
            signer,
            rest_base_url: cfg.rest_base_url,
            ws_base_url: cfg.ws_base_url,
            mode: cfg.mode,
            order_symbol: HashMap::new(),
            listen_key: None,
            resilient_executor: None,
            alert_callback: None,
        }
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
            info!("[Binance] 熔斷器已手動重置");
        }
    }

    fn ensure_http(&mut self) -> hft_core::HftResult<()> {
        if self.http_client.is_none() {
            let cfg = HttpClientConfig {
                base_url: self.rest_base_url.clone(),
                timeout_ms: 5000,
                user_agent: "hft-binance-exec/1.0".to_string(),
            };
            self.http_client =
                Some(HttpClient::new(cfg).map_err(|e| hft_core::HftError::Network(e.to_string()))?);
        }
        Ok(())
    }

    /// 獲取 HTTP 客戶端引用
    #[inline]
    fn get_http(&self) -> HftResult<&HttpClient> {
        self.http_client
            .as_ref()
            .ok_or_else(|| hft_core::HftError::Execution("HTTP client not initialized".to_string()))
    }

    /// 獲取 Signer 引用
    #[inline]
    fn get_signer(&self) -> HftResult<&BinanceSigner> {
        self.signer
            .as_ref()
            .ok_or_else(|| hft_core::HftError::Execution("Signer not initialized - missing credentials".to_string()))
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
impl ExecutionClient for BinanceExecutionClient {
    async fn place_order(&mut self, intent: ports::OrderIntent) -> HftResult<OrderId> {
        // Live 模式（需要 signer）
        if self.mode == ExecutionMode::Live && self.signer.is_some() {
            if self.http_client.is_none() {
                self.ensure_http()?;
            }
            let signer = self.get_signer()?;
            let http = self.get_http()?;

            // 構建參數
            let mut params: HashMap<String, String> = HashMap::new();
            params.insert("symbol".to_string(), intent.symbol.as_str().to_string());
            params.insert(
                "side".to_string(),
                match intent.side {
                    hft_core::Side::Buy => "BUY",
                    _ => "SELL",
                }
                .to_string(),
            );
            let typ = match intent.order_type {
                hft_core::OrderType::Market => "MARKET",
                _ => "LIMIT",
            };
            params.insert("type".to_string(), typ.to_string());
            if let Some(p) = intent.price {
                params.insert("price".to_string(), p.0.to_string());
            }
            params.insert("quantity".to_string(), intent.quantity.0.to_string());
            // 僅限限價單需要 TIF
            if matches!(intent.order_type, hft_core::OrderType::Limit) {
                let tif = match intent.time_in_force {
                    hft_core::TimeInForce::IOC => "IOC",
                    hft_core::TimeInForce::FOK => "FOK",
                    _ => "GTC",
                };
                params.insert("timeInForce".to_string(), tif.to_string());
            }
            params.insert("recvWindow".to_string(), "5000".to_string());
            let signed_query = signer.sign_request(&mut params);
            let path = format!("/api/v3/order?{}", signed_query);
            let headers = signer.generate_headers();

            let resp = http
                .signed_request(reqwest::Method::POST, &path, Some(headers), None)
                .await
                .map_err(|e| hft_core::HftError::Network(e.to_string()))?;

            #[derive(Debug, serde::Deserialize)]
            #[allow(dead_code)]
            struct PlaceResp {
                symbol: String,
                #[serde(rename = "orderId")]
                order_id: u64,
                #[serde(rename = "clientOrderId")]
                client_order_id: String,
                #[serde(rename = "transactTime")]
                transact_time: u64,
            }
            let pr: PlaceResp = integration::http::HttpClient::parse_json(resp)
                .await
                .map_err(|e| hft_core::HftError::Serialization(e.to_string()))?;

            let oid = OrderId(pr.order_id.to_string());
            self.order_symbol.insert(oid.0.clone(), pr.symbol);
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(ExecutionEvent::OrderAck {
                    order_id: oid.clone(),
                    timestamp: hft_core::now_micros(),
                });
            }
            return Ok(oid);
        }

        // Paper: 立即回傳訂單ID並廣播 ACK/Fill
        let order_id = OrderId(format!("BINANCE_PAPER_{}", hft_core::now_micros()));
        info!(
            "Binance 模擬下單: {} {} {} @ {:?}",
            intent.symbol.as_str(),
            match intent.side {
                hft_core::Side::Buy => "buy",
                hft_core::Side::Sell => "sell",
            },
            intent.quantity.0,
            intent.price.map(|p| p.0)
        );

        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderAck {
                order_id: order_id.clone(),
                timestamp: hft_core::now_micros(),
            });
            let tx2 = tx.clone();
            let q = intent.quantity;
            let maybe_p = intent.price;
            let oid = order_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if let Some(p) = maybe_p {
                    let _ = tx2.send(ExecutionEvent::Fill {
                        order_id: oid,
                        price: p,
                        quantity: q,
                        timestamp: hft_core::now_micros(),
                        fill_id: format!("BN_FILL_{}", hft_core::now_micros()),
                    });
                } else {
                    tracing::warn!("Binance Paper 模式跳過 Fill：缺少價格 (請讓引擎補全頂檔價格)");
                }
            });
        }

        Ok(order_id)
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        if self.mode == ExecutionMode::Live && self.signer.is_some() {
            if self.http_client.is_none() {
                self.ensure_http()?;
            }
            let signer = self
                .signer
                .as_ref()
                .ok_or_else(|| hft_core::HftError::Authentication("缺少API憑證".to_string()))?;
            let http_client = self.get_http()?.clone();
            let symbol = self
                .order_symbol
                .get(&order_id.0)
                .cloned()
                .unwrap_or_else(|| "BTCUSDT".to_string());
            let order_id_str = order_id.0.clone();
            let signer_clone = signer.clone();

            let result = self
                .execute_with_resilience(|| {
                    let http = http_client.clone();
                    let sym = symbol.clone();
                    let oid = order_id_str.clone();
                    let sig = signer_clone.clone();
                    async move {
                        let mut params: HashMap<String, String> = HashMap::new();
                        params.insert("symbol".to_string(), sym);
                        params.insert("orderId".to_string(), oid);
                        params.insert("recvWindow".to_string(), "5000".to_string());
                        let signed_query = sig.sign_request(&mut params);
                        let path = format!("/api/v3/order?{}", signed_query);
                        let headers = sig.generate_headers();
                        http.signed_request(reqwest::Method::DELETE, &path, Some(headers), None)
                            .await
                            .map_err(|e| hft_core::HftError::Network(e.to_string()))?;
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
                                "binance",
                                "cancel_order",
                                &format!("撤單失敗且熔斷器已開啟 (order_id={}): {}", order_id.0, e),
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
        if self.mode == ExecutionMode::Live {
            // Binance 原生修改有限制，這裡採用 Cancel + New 的簡化策略
            if new_quantity.is_none() && new_price.is_none() {
                return Ok(());
            }
            let _symbol = self
                .order_symbol
                .get(&order_id.0)
                .cloned()
                .unwrap_or_else(|| "BTCUSDT".to_string());

            // 取消原單（已帶韌性機制）
            let cancel_result = self.cancel_order(order_id).await;

            // 簡化：按剩餘資料重下一張限價/市價單（需由上層提供完整 intent 更佳）
            warn!("Binance 修改訂單以撤單重下實現: order_id={}", order_id.0);

            // 處理撤單結果
            if let Err(ref e) = cancel_result {
                // 發送修改失敗告警
                self.send_execution_alert(
                    ExecutionAlert::new(
                        ExecutionAlertType::RetriesExhausted,
                        "binance",
                        "modify_order",
                        &format!("修改訂單時撤單失敗 (order_id={}): {}", order_id.0, e),
                    )
                    .with_error(e.to_string()),
                );
                return cancel_result;
            }

            // 撤單成功，發送修改事件
            // 無法重建完整意圖，僅回傳修改事件以避免阻塞（可後續改為攜帶原意圖）
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
            let stream =
                tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|result| async move {
                    match result {
                        Ok(event) => Some(Ok(event)),
                        Err(e) => {
                            error!("Binance 執行事件流錯誤: {}", e);
                            None
                        }
                    }
                });
            Ok(Box::pin(stream))
        } else {
            Ok(Box::pin(stream::empty()))
        }
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

        let mut executor = ResilientExecutor::new("binance", retry_config, cb_config);

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
                    "binance",
                    "execution",
                    &cb_alert.message,
                )
                .with_failure_count(cb_alert.failure_count);

                alert_cb(alert);
            });
        }

        self.resilient_executor = Some(Arc::new(executor));

        let (tx, _rx) = broadcast::channel(1000);
        self.event_tx = Some(tx.clone());
        self.connected = true;
        // 惰性初始化 HTTP 客戶端
        let _ = self.ensure_http();

        info!("[Binance] 執行客戶端連接成功");

        // 啟動私有 WS（Live）
        if self.mode == ExecutionMode::Live {
            // 1) 創建 listenKey
            let http = self.get_http()?;
            let headers = self.get_signer()?.generate_headers();
            let resp = http
                .signed_request(
                    reqwest::Method::POST,
                    "/api/v3/userDataStream",
                    Some(headers),
                    None,
                )
                .await
                .map_err(|e| hft_core::HftError::Network(e.to_string()))?;
            #[derive(serde::Deserialize)]
            struct Lk {
                #[serde(rename = "listenKey")]
                listen_key: String,
            }
            let lk: Lk = integration::http::HttpClient::parse_json(resp)
                .await
                .map_err(|e| hft_core::HftError::Serialization(e.to_string()))?;
            self.listen_key = Some(lk.listen_key.clone());

            // 2) 連線 WS
            let url = format!(
                "{}/{}",
                self.ws_base_url.trim_end_matches('/'),
                lk.listen_key
            );
            let ws_tx = tx.clone();
            tokio::spawn(async move {
                match tokio_tungstenite::connect_async(&url).await {
                    Ok((mut ws, _)) => {
                        info!("Binance 私有 WS 已連線");
                        while let Some(msg) = ws.next().await {
                            match msg {
                                Ok(tokio_tungstenite::tungstenite::Message::Text(txt)) => {
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                                        // 解析 executionReport
                                        if v.get("e").and_then(|e| e.as_str())
                                            == Some("executionReport")
                                        {
                                            let status =
                                                v.get("X").and_then(|x| x.as_str()).unwrap_or("");
                                            let order_id = v
                                                .get("i")
                                                .and_then(|x| x.as_i64())
                                                .unwrap_or(0)
                                                .to_string();
                                            let ts = v
                                                .get("E")
                                                .and_then(|x| x.as_u64())
                                                .unwrap_or(hft_core::now_micros());
                                            let ts_us = ts * 1000; // ms -> μs
                                            match status {
                                                "NEW" => {
                                                    let _ = ws_tx.send(ExecutionEvent::OrderAck {
                                                        order_id: OrderId(order_id.clone()),
                                                        timestamp: ts_us,
                                                    });
                                                }
                                                "CANCELED" => {
                                                    let _ =
                                                        ws_tx.send(ExecutionEvent::OrderCanceled {
                                                            order_id: OrderId(order_id.clone()),
                                                            timestamp: ts_us,
                                                        });
                                                }
                                                "REJECTED" => {
                                                    let _ =
                                                        ws_tx.send(ExecutionEvent::OrderReject {
                                                            order_id: OrderId(order_id.clone()),
                                                            reason: "Exchange rejected".to_string(),
                                                            timestamp: ts_us,
                                                        });
                                                }
                                                _ => {
                                                    // 成交
                                                    let last_qty = v
                                                        .get("l")
                                                        .and_then(|x| x.as_str())
                                                        .and_then(|s| {
                                                            hft_core::Quantity::from_str(s).ok()
                                                        });
                                                    let last_px = v
                                                        .get("L")
                                                        .and_then(|x| x.as_str())
                                                        .and_then(|s| {
                                                            hft_core::Price::from_str(s).ok()
                                                        });
                                                    if let (Some(q), Some(p)) = (last_qty, last_px)
                                                    {
                                                        let fill_id =
                                                            format!("BNFILL-{}-{}", order_id, ts);
                                                        let _ = ws_tx.send(ExecutionEvent::Fill {
                                                            order_id: OrderId(order_id.clone()),
                                                            price: p,
                                                            quantity: q,
                                                            timestamp: ts_us,
                                                            fill_id,
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                                    warn!("Binance 私有 WS 關閉");
                                    break;
                                }
                                Err(e) => {
                                    warn!("Binance 私有 WS 錯誤: {}", e);
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => warn!("Binance 私有 WS 連接失敗: {}", e),
                }
            });

            // 3) KeepAlive listenKey（每 30 分鐘）
            if let Some(lk) = self.listen_key.clone() {
                let base_url = self.rest_base_url.clone();
                let headers2 = self
                    .signer
                    .as_ref()
                    .map(|s| s.generate_headers())
                    .unwrap_or_default();
                tokio::spawn(async move {
                    let cfg = integration::http::HttpClientConfig {
                        base_url,
                        timeout_ms: 5000,
                        user_agent: "hft-binance-exec/1.0".to_string(),
                    };
                    if let Ok(http2) = integration::http::HttpClient::new(cfg) {
                        let mut interval =
                            tokio::time::interval(std::time::Duration::from_secs(30 * 60));
                        loop {
                            interval.tick().await;
                            let _ = http2
                                .signed_request(
                                    reqwest::Method::PUT,
                                    &format!("/api/v3/userDataStream?listenKey={}", lk),
                                    Some(headers2.clone()),
                                    None,
                                )
                                .await;
                        }
                    }
                });
            }
        } else {
            warn!("Binance Paper mode: 跳過私有 WS 建立");
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
        // 若無憑證，無法調用 Binance 簽名端點
        let signer = match &self.signer {
            Some(s) => s,
            None => {
                tracing::warn!("Binance list_open_orders: 未提供 API 憑證，返回空列表");
                return Ok(Vec::new());
            }
        };

        // 確保存在 HTTP 客戶端，然後以借用方式使用
        // 注意：此方法簽名為 &self，因此不要移動所有權
        let http_local;
        let http: &HttpClient = if let Some(http0) = &self.http_client {
            http0
        } else {
            let cfg = HttpClientConfig {
                base_url: self.rest_base_url.clone(),
                timeout_ms: 5000,
                user_agent: "hft-binance-exec/1.0".to_string(),
            };
            http_local =
                HttpClient::new(cfg).map_err(|e| hft_core::HftError::Network(e.to_string()))?;
            // 使用臨時本地客戶端的引用
            &http_local
        };

        // 構建簽名查詢
        let mut params: HashMap<String, String> = HashMap::new();
        // 可選：加上 recvWindow 以提升容錯
        params.insert("recvWindow".to_string(), "5000".to_string());
        let signed_query = signer.sign_request(&mut params); // 會自動加入 timestamp 並返回 "query&signature=..."
        let path = format!("/api/v3/openOrders?{}", signed_query);
        let headers = signer.generate_headers();

        let resp = http
            .get(&path, Some(headers))
            .await
            .map_err(|e| hft_core::HftError::Network(e.to_string()))?;

        // 響應為陣列
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct BinanceOrder {
            symbol: String,
            #[serde(rename = "orderId")]
            order_id: u64,
            #[serde(rename = "clientOrderId")]
            client_order_id: String,
            price: String,
            #[serde(rename = "origQty")]
            orig_qty: String,
            #[serde(rename = "executedQty")]
            executed_qty: String,
            status: String,
            time: u64,
            #[serde(rename = "updateTime")]
            update_time: u64,
            side: String,
            r#type: String,
        }

        let items: Vec<BinanceOrder> = HttpClient::parse_json(resp)
            .await
            .map_err(|e| hft_core::HftError::Serialization(e.to_string()))?;

        // 映射到統一 OpenOrder
        let mut out = Vec::new();
        for it in items {
            let side = match it.side.as_str() {
                "BUY" | "Buy" | "buy" => hft_core::Side::Buy,
                _ => hft_core::Side::Sell,
            };
            let order_type = match it.r#type.as_str() {
                "MARKET" | "Market" | "market" => hft_core::OrderType::Market,
                _ => hft_core::OrderType::Limit,
            };
            let qty =
                hft_core::Quantity::from_str(&it.orig_qty).unwrap_or(hft_core::Quantity::zero());
            let filled = hft_core::Quantity::from_str(&it.executed_qty)
                .unwrap_or(hft_core::Quantity::zero());
            let remaining = hft_core::Quantity(qty.0 - filled.0);
            let price = hft_core::Price::from_str(&it.price).ok();
            let status = match it.status.as_str() {
                "NEW" => ports::OrderStatus::New,
                "PARTIALLY_FILLED" => ports::OrderStatus::PartiallyFilled,
                "FILLED" => ports::OrderStatus::Filled,
                "CANCELED" => ports::OrderStatus::Canceled,
                "REJECTED" => ports::OrderStatus::Rejected,
                "EXPIRED" => ports::OrderStatus::Expired,
                _ => ports::OrderStatus::Accepted,
            };

            out.push(OpenOrder {
                order_id: hft_core::OrderId(it.order_id.to_string()),
                symbol: hft_core::Symbol::from(it.symbol),
                side,
                order_type,
                original_quantity: qty,
                remaining_quantity: remaining,
                filled_quantity: filled,
                price,
                status,
                created_at: it.time * 1000,        // ms -> μs
                updated_at: it.update_time * 1000, // ms -> μs
            });
        }

        Ok(out)
    }
}
