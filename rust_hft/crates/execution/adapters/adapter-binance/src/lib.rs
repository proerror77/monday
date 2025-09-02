//! Binance 執行 adapter（實作 `ports::ExecutionClient`）
//! - 最小可用的 Paper 模式：place_order 會發送 ACK 與模擬 Fill 回報

use async_trait::async_trait;
use futures::{stream, StreamExt};
use ports::{BoxStream, ExecutionClient, ExecutionEvent, OpenOrder};
use integration::{
    http::{HttpClient, HttpClientConfig},
    signing::{BinanceCredentials, BinanceSigner},
};
use std::collections::HashMap;
use hft_core::{HftResult, OrderId, Price, Quantity};
use tokio::sync::broadcast;
use tracing::{info, error};

pub struct BinanceExecutionClient {
    event_tx: Option<broadcast::Sender<ExecutionEvent>>, 
    connected: bool,
    http_client: Option<HttpClient>,
    signer: Option<BinanceSigner>,
    rest_base_url: String,
}

impl BinanceExecutionClient {
    pub fn new() -> Self { 
        // 從環境變數讀取 API 憑證（若不存在，僅能用於 Paper/模擬與無權限查詢）
        let api_key = std::env::var("BINANCE_API_KEY").ok();
        let secret = std::env::var("BINANCE_SECRET").ok();
        let signer = match (api_key, secret) {
            (Some(k), Some(s)) if !k.is_empty() && !s.is_empty() => {
                Some(BinanceSigner::new(BinanceCredentials::new(k, s)))
            }
            _ => None,
        };

        Self { 
            event_tx: None, 
            connected: false,
            http_client: None,
            signer,
            rest_base_url: std::env::var("BINANCE_REST_BASE").unwrap_or_else(|_| "https://api.binance.com".to_string()),
        }
    }

    fn ensure_http(&mut self) -> hft_core::HftResult<()> {
        if self.http_client.is_none() {
            let cfg = HttpClientConfig { 
                base_url: self.rest_base_url.clone(),
                timeout_ms: 5000,
                user_agent: "hft-binance-exec/1.0".to_string(),
            };
            self.http_client = Some(HttpClient::new(cfg).map_err(|e| hft_core::HftError::Network(e.to_string()))?);
        }
        Ok(())
    }
}

#[async_trait]
impl ExecutionClient for BinanceExecutionClient {
    async fn place_order(&mut self, intent: ports::OrderIntent) -> HftResult<OrderId> {
        // Paper: 立即回傳訂單ID並廣播 ACK/Fill
        let order_id = OrderId(format!("BINANCE_PAPER_{}", hft_core::now_micros()));
        info!("Binance 模擬下單: {} {} {} @ {:?}", 
              intent.symbol.0, 
              match intent.side { hft_core::Side::Buy => "buy", hft_core::Side::Sell => "sell" },
              intent.quantity.0,
              intent.price.map(|p| p.0));

        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderAck { order_id: order_id.clone(), timestamp: hft_core::now_micros() });
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
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderCanceled { order_id: order_id.clone(), timestamp: hft_core::now_micros() });
        }
        Ok(()) 
    }

    async fn modify_order(&mut self, order_id: &OrderId, new_quantity: Option<Quantity>, new_price: Option<Price>) -> HftResult<()> { 
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ExecutionEvent::OrderModified { order_id: order_id.clone(), new_quantity, new_price, timestamp: hft_core::now_micros() });
        }
        Ok(()) 
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> { 
        if let Some(ref tx) = self.event_tx {
            let rx = tx.subscribe();
            let stream = tokio_stream::wrappers::BroadcastStream::new(rx)
                .filter_map(|result| async move {
                    match result { Ok(event) => Some(Ok(event)), Err(e) => { error!("Binance 執行事件流錯誤: {}", e); None } }
                });
            Ok(Box::pin(stream))
        } else {
            Ok(Box::pin(stream::empty()))
        }
    }

    async fn connect(&mut self) -> HftResult<()> { 
        let (tx, _rx) = broadcast::channel(1000);
        self.event_tx = Some(tx);
        self.connected = true;
        // 惰性初始化 HTTP 客戶端
        let _ = self.ensure_http();
        Ok(()) 
    }

    async fn disconnect(&mut self) -> HftResult<()> { 
        self.event_tx = None; 
        self.connected = false; 
        Ok(()) 
    }

    async fn health(&self) -> ports::ConnectionHealth { 
        ports::ConnectionHealth { connected: self.connected, latency_ms: Some(1.0), last_heartbeat: hft_core::now_micros() } 
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        // 若無憑證，無法調用 Binance 簽名端點
        let signer = match &self.signer { Some(s) => s, None => {
            tracing::warn!("Binance list_open_orders: 未提供 API 憑證，返回空列表");
            return Ok(Vec::new());
        }};

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
            http_local = HttpClient::new(cfg).map_err(|e| hft_core::HftError::Network(e.to_string()))?;
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

        let resp = http.get(&path, Some(headers)).await
            .map_err(|e| hft_core::HftError::Network(e.to_string()))?;

        // 響應為陣列
        #[derive(Debug, serde::Deserialize)]
        struct BinanceOrder {
            symbol: String,
            orderId: u64,
            clientOrderId: String,
            price: String,
            origQty: String,
            executedQty: String,
            status: String,
            time: u64,
            updateTime: u64,
            side: String,
            r#type: String,
        }

        let items: Vec<BinanceOrder> = HttpClient::parse_json(resp).await
            .map_err(|e| hft_core::HftError::Serialization(e.to_string()))?;

        // 映射到統一 OpenOrder
        let mut out = Vec::new();
        for it in items {
            let side = match it.side.as_str() { "BUY" | "Buy" | "buy" => hft_core::Side::Buy, _ => hft_core::Side::Sell };
            let order_type = match it.r#type.as_str() { "MARKET" | "Market" | "market" => hft_core::OrderType::Market, _ => hft_core::OrderType::Limit };
            let qty = hft_core::Quantity::from_str(&it.origQty).unwrap_or(hft_core::Quantity::zero());
            let filled = hft_core::Quantity::from_str(&it.executedQty).unwrap_or(hft_core::Quantity::zero());
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
                order_id: hft_core::OrderId(it.orderId.to_string()),
                symbol: hft_core::Symbol(it.symbol),
                side,
                order_type,
                original_quantity: qty,
                remaining_quantity: remaining,
                filled_quantity: filled,
                price,
                status,
                created_at: it.time * 1000,      // ms -> μs
                updated_at: it.updateTime * 1000, // ms -> μs
            });
        }

        Ok(out)
    }
}
