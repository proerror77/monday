//! Bybit 執行適配器（v5 REST + 私有 WS）

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult, OrderId, Price, Quantity};
use integration::{
    http::{HttpClient, HttpClientConfig},
    signing::{BybitCredentials, BybitSigner},
};
use ports::{BoxStream, ExecutionClient, ExecutionEvent, OpenOrder};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionMode {
    Live,
    Paper,
    Testnet,
}

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
}

impl BybitExecutionClient {
    pub fn new(config: BybitExecutionConfig) -> Result<Self, HftError> {
        Ok(Self {
            http: None,
            signer: BybitSigner::new(config.credentials.clone()),
            config,
            event_tx: None,
            connected: false,
        })
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
}

#[async_trait]
impl ExecutionClient for BybitExecutionClient {
    async fn place_order(&mut self, intent: ports::OrderIntent) -> HftResult<OrderId> {
        if matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            self.ensure_http()?;
            let http = self.http.as_ref().unwrap();
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
                symbol: &intent.symbol.0,
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
            let http = self.http.as_ref().unwrap();
            #[derive(serde::Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Req<'a> {
                category: &'a str,
                order_id: &'a str,
            }
            let req = Req {
                category: "spot",
                order_id: &order_id.0,
            };
            let body =
                serde_json::to_string(&req).map_err(|e| HftError::Serialization(e.to_string()))?;
            let headers = self
                .signer
                .generate_headers("POST", "/v5/order/cancel", &body, None);
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
        if matches!(
            self.config.mode,
            ExecutionMode::Live | ExecutionMode::Testnet
        ) {
            self.ensure_http()?;
            let http = self.http.as_ref().unwrap();
            #[derive(serde::Serialize)]
            #[serde(rename_all = "camelCase")]
            struct Req<'a> {
                category: &'a str,
                order_id: &'a str,
                qty: Option<String>,
                price: Option<String>,
            }
            let req = Req {
                category: "spot",
                order_id: &order_id.0,
                qty: new_quantity.map(|q| q.0.to_string()),
                price: new_price.map(|p| p.0.to_string()),
            };
            let body =
                serde_json::to_string(&req).map_err(|e| HftError::Serialization(e.to_string()))?;
            let headers = self
                .signer
                .generate_headers("POST", "/v5/order/amend", &body, None);
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
            let s = tokio_stream::wrappers::BroadcastStream::new(rx)
                .filter_map(|e| async move { e.ok().map(Ok) });
            return Ok(Box::pin(s));
        }
        Ok(Box::pin(futures::stream::empty()))
    }

    async fn connect(&mut self) -> HftResult<()> {
        let (tx, _) = broadcast::channel(1000);
        self.event_tx = Some(tx.clone());
        self.connected = true;
        self.ensure_http()?;
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
                        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
                        mac.update(msg.as_bytes());
                        hex::encode(mac.finalize().into_bytes())
                    };
                    let auth = serde_json::json!({
                        "op": "auth",
                        "args": [api_key, ts, recv_window, sign]
                    });
                    let _ = ws
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            auth.to_string(),
                        ))
                        .await;
                    // 訂閱 order/execution
                    let sub = serde_json::json!({"op":"subscribe","args":["order.spot","execution.spot"]});
                    let _ = ws
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            sub.to_string(),
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
                                        .and_then(|arr| arr.get(0))
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
                                        .and_then(|arr| arr.get(0))
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
            return Ok(Vec::new());
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

        #[derive(serde::Deserialize)]
        struct RespDataItem {
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
        struct RespData {
            list: Vec<RespDataItem>,
        }
        #[derive(serde::Deserialize)]
        struct Resp {
            retCode: i64,
            retMsg: String,
            data: Option<RespData>,
        }

        let r: Resp = integration::http::HttpClient::parse_json(resp)
            .await
            .map_err(|e| HftError::Serialization(e.to_string()))?;
        if r.retCode != 0 {
            return Err(HftError::Exchange(format!(
                "Bybit 查詢未結失敗: {} {}",
                r.retCode, r.retMsg
            )));
        }

        let mut out = Vec::new();
        if let Some(d) = r.data {
            for it in d.list {
                let side = match it.side.as_str() {
                    "Buy" | "BUY" | "buy" => hft_core::Side::Buy,
                    _ => hft_core::Side::Sell,
                };
                let order_type = match it.orderType.as_str() {
                    "Market" | "MARKET" | "market" => hft_core::OrderType::Market,
                    _ => hft_core::OrderType::Limit,
                };
                let qty = Quantity::from_str(&it.qty).unwrap_or(Quantity::zero());
                let filled = Quantity::from_str(&it.cumExecQty).unwrap_or(Quantity::zero());
                let remaining = Quantity(qty.0 - filled.0);
                let price = Price::from_str(&it.price).ok();
                let status = match it.orderStatus.as_str() {
                    "New" | "Created" => ports::OrderStatus::New,
                    "PartiallyFilled" | "PartiallyFilledCanceled" => {
                        ports::OrderStatus::PartiallyFilled
                    }
                    "Filled" => ports::OrderStatus::Filled,
                    "Cancelled" | "Rejected" => ports::OrderStatus::Canceled,
                    _ => ports::OrderStatus::Accepted,
                };
                let created_at = it.createdTime.parse::<u64>().unwrap_or(0) * 1000;
                let updated_at = it.updatedTime.parse::<u64>().unwrap_or(0) * 1000;
                out.push(OpenOrder {
                    order_id: OrderId(it.orderId),
                    symbol: hft_core::Symbol(it.symbol),
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
