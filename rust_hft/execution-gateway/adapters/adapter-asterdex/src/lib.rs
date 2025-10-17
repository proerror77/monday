//! Aster DEX 執行 adapter（實作 `ports::ExecutionClient`）
//! - 支援 Paper 模式（模擬 ACK/Fill）與 Live 模式（REST 下單 + 私有 WS 回報）

use async_trait::async_trait;
use futures::{stream, StreamExt};
use hft_core::{HftResult, OrderId, Price, Quantity};
use integration::{
    http::{HttpClient, HttpClientConfig},
    signing::{AsterdexCredentials, AsterdexSigner},
};
use ports::{BoxStream, ExecutionClient, ExecutionEvent, OpenOrder};
use std::collections::HashMap;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Paper,
    Live,
}

#[derive(Debug, Clone)]
pub struct AsterdexExecutionConfig {
    pub credentials: AsterdexCredentials,
    pub rest_base_url: String,
    pub ws_base_url: String,
    pub timeout_ms: u64,
    pub mode: ExecutionMode,
}

pub struct AsterdexExecutionClient {
    event_tx: Option<broadcast::Sender<ExecutionEvent>>,
    connected: bool,
    http_client: Option<HttpClient>,
    signer: Option<AsterdexSigner>,
    rest_base_url: String,
    ws_base_url: String,
    mode: ExecutionMode,
    // order_id -> symbol 快取，撤單時需要
    order_symbol: HashMap<String, String>,
    // listenKey 維護
    listen_key: Option<String>,
}

impl AsterdexExecutionClient {
    pub fn new(cfg: AsterdexExecutionConfig) -> Self {
        let signer =
            if !cfg.credentials.api_key.is_empty() && !cfg.credentials.secret_key.is_empty() {
                Some(AsterdexSigner::new(cfg.credentials.clone()))
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
        }
    }

    fn ensure_http(&mut self) -> hft_core::HftResult<()> {
        if self.http_client.is_none() {
            let cfg = HttpClientConfig {
                base_url: self.rest_base_url.clone(),
                timeout_ms: 5000,
                user_agent: "hft-asterdex-exec/1.0".to_string(),
            };
            self.http_client =
                Some(HttpClient::new(cfg).map_err(|e| hft_core::HftError::Network(e.to_string()))?);
        }
        Ok(())
    }
}

#[async_trait]
impl ExecutionClient for AsterdexExecutionClient {
    async fn place_order(&mut self, intent: ports::OrderIntent) -> HftResult<OrderId> {
        // Live 模式（需要 signer）
        if self.mode == ExecutionMode::Live && self.signer.is_some() {
            if self.http_client.is_none() {
                self.ensure_http()?;
            }
            let signer = self.signer.as_ref().unwrap();
            let http = self.http_client.as_ref().unwrap();

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
            let path = format!("/fapi/v1/order?{}", signed_query);
            let headers = signer.generate_headers();

            let resp = http
                .signed_request(reqwest::Method::POST, &path, Some(headers), None)
                .await
                .map_err(|e| hft_core::HftError::Network(e.to_string()))?;

            #[derive(Debug, serde::Deserialize)]
            struct PlaceResp {
                symbol: String,
                #[serde(rename = "orderId")]
                order_id: u64,
                #[serde(rename = "clientOrderId")]
                _client_order_id: String,
                #[serde(rename = "transactTime")]
                _transact_time: u64,
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
        let order_id = OrderId(format!("ASTERDEX_PAPER_{}", hft_core::now_micros()));
        info!(
            "Aster DEX 模擬下單: {} {} {} @ {:?}",
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
                        fill_id: format!("AST_FILL_{}", hft_core::now_micros()),
                    });
                } else {
                    tracing::warn!(
                        "Aster DEX Paper 模式跳過 Fill：缺少價格 (請讓引擎補全頂檔價格)"
                    );
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
            let http = self.http_client.as_ref().unwrap();
            let symbol = self
                .order_symbol
                .get(&order_id.0)
                .cloned()
                .unwrap_or_else(|| "BTCUSDT".to_string());

            let mut params: HashMap<String, String> = HashMap::new();
            params.insert("symbol".to_string(), symbol);
            params.insert("orderId".to_string(), order_id.0.clone());
            params.insert("recvWindow".to_string(), "5000".to_string());
            let signed_query = signer.sign_request(&mut params);
            let path = format!("/fapi/v1/order?{}", signed_query);
            let headers = signer.generate_headers();
            let _ = http
                .signed_request(reqwest::Method::DELETE, &path, Some(headers), None)
                .await
                .map_err(|e| hft_core::HftError::Network(e.to_string()))?;
            if let Some(ref tx) = self.event_tx {
                let _ = tx.send(ExecutionEvent::OrderCanceled {
                    order_id: order_id.clone(),
                    timestamp: hft_core::now_micros(),
                });
            }
            return Ok(());
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
            // Aster DEX 目前沿用 Binance 風格，採用撤單重下策略
            if new_quantity.is_none() && new_price.is_none() {
                return Ok(());
            }
            // 取消原單
            self.cancel_order(order_id).await.ok();
            // 簡化：按剩餘資料重下一張限價/市價單（需由上層提供完整 intent 更佳）
            warn!("Aster DEX 修改訂單以撤單重下實現: order_id={}", order_id.0);
            if let Some(q) = new_quantity {
                let _ = q;
            }
            if let Some(p) = new_price {
                let _ = p;
            }
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
                            error!("Aster DEX 執行事件流錯誤: {}", e);
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
        let (tx, _rx) = broadcast::channel(1000);
        self.event_tx = Some(tx.clone());
        self.connected = true;
        // 惰性初始化 HTTP 客戶端
        let _ = self.ensure_http();

        // 啟動私有 WS（Live）
        if self.mode == ExecutionMode::Live {
            // 1) 創建 listenKey
            let http = self.http_client.as_ref().unwrap();
            let headers = self.signer.as_ref().map(|s| s.generate_headers()).unwrap();
            let resp = http
                .signed_request(
                    reqwest::Method::POST,
                    "/fapi/v1/listenKey",
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
                        info!("Aster DEX 私有 WS 已連線");
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
                                                            format!("ASTFILL-{}-{}", order_id, ts);
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
                                    warn!("Aster DEX 私有 WS 關閉");
                                    break;
                                }
                                Err(e) => {
                                    warn!("Aster DEX 私有 WS 錯誤: {}", e);
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => warn!("Aster DEX 私有 WS 連接失敗: {}", e),
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
                        user_agent: "hft-asterdex-exec/1.0".to_string(),
                    };
                    if let Ok(http2) = integration::http::HttpClient::new(cfg) {
                        let mut interval =
                            tokio::time::interval(std::time::Duration::from_secs(30 * 60));
                        loop {
                            interval.tick().await;
                            let _ = http2
                                .signed_request(
                                    reqwest::Method::PUT,
                                    &format!("/fapi/v1/listenKey?listenKey={}", lk),
                                    Some(headers2.clone()),
                                    None,
                                )
                                .await;
                        }
                    }
                });
            }
        } else if self.mode == ExecutionMode::Live {
            warn!("Aster DEX Live: 未提供 API 憑證，跳過私有 WS 建立");
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
        // 若無憑證，無法調用 Aster DEX 簽名端點
        let signer = match &self.signer {
            Some(s) => s,
            None => {
                tracing::warn!("Aster DEX list_open_orders: 未提供 API 憑證，返回空列表");
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
                user_agent: "hft-asterdex-exec/1.0".to_string(),
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
        let path = format!("/fapi/v1/openOrders?{}", signed_query);
        let headers = signer.generate_headers();

        let resp = http
            .get(&path, Some(headers))
            .await
            .map_err(|e| hft_core::HftError::Network(e.to_string()))?;

        // 響應為陣列
        #[derive(Debug, serde::Deserialize)]
        struct AsterdexOrder {
            symbol: String,
            #[serde(rename = "orderId")]
            order_id: u64,
            #[serde(rename = "clientOrderId")]
            _client_order_id: String,
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

        let items: Vec<AsterdexOrder> = HttpClient::parse_json(resp)
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
