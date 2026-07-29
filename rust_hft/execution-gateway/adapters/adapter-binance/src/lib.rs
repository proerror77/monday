//! Binance 執行 adapter（實作 `ports::ExecutionClient`）
//! - 支援 Paper 模式（模擬 ACK/Fill）與 Live 模式（REST 下單 + 私有 WS 回報）
//! - 韌性機制：重試、熔斷器、告警通知

mod ws_order;

use async_trait::async_trait;
use execution::{
    AlertCallback, CircuitBreakerConfig, CircuitState, ExecutionAlert, ExecutionAlertType,
    ExecutorStats, ResilientExecutor, RetryConfig,
};
// Re-export ExecutionMode for backwards compatibility
pub use execution::ExecutionMode;
use futures::{stream, SinkExt, StreamExt};
use hft_core::{AccountCapability, AssetClass, HftResult, OrderId, Price, ProductType, Quantity};
use integration::{
    http::{HttpClient, HttpClientConfig},
    signing::{BinanceCredentials, BinanceSigner},
};
use ports::{
    AccountBalance, BoxStream, ExecutionClient, ExecutionEvent, ExecutionSubmissionAttempt,
    OpenOrder, OrderIntentEnvelope, PrivateOrderEventKind,
};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use tracing::{info, warn};
use ws_order::BinanceWsOrderClient;

#[derive(Debug, Clone)]
pub struct BinanceExecutionConfig {
    pub credentials: BinanceCredentials,
    pub rest_base_url: String,
    pub ws_base_url: String,
    pub timeout_ms: u64,
    pub mode: ExecutionMode,
    pub account_capability: AccountCapability,
}

pub struct BinanceExecutionClient {
    event_tx: Option<broadcast::Sender<ExecutionEvent>>,
    connected: bool,
    http_client: Option<HttpClient>,
    signer: Option<BinanceSigner>,
    rest_base_url: String,
    ws_base_url: String,
    mode: ExecutionMode,
    account_capability: AccountCapability,
    timeout_ms: u64,
    // order_id -> symbol 快取，撤單時需要
    order_symbol: HashMap<String, String>,
    // listenKey 維護
    listen_key: Option<String>,
    // 韌性執行器 (重試 + 熔斷器)
    resilient_executor: Option<Arc<ResilientExecutor>>,
    // 告警回調
    alert_callback: Option<AlertCallback>,
    next_client_order_id: Option<String>,
    ws_order: Option<BinanceWsOrderClient>,
    shutdown_tx: Option<watch::Sender<bool>>,
    last_submission_timing: Option<(Option<u64>, Option<u64>, Option<u64>)>,
}

fn uses_exchange_api(mode: ExecutionMode) -> bool {
    matches!(mode, ExecutionMode::Live | ExecutionMode::Testnet)
}

fn binance_ws_order_url(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Testnet => "wss://ws-api.testnet.binance.vision:443/ws-api/v3",
        _ => "wss://ws-api.binance.com:443/ws-api/v3",
    }
}

fn build_binance_ws_order_request(
    signer: &BinanceSigner,
    intent: &ports::OrderIntent,
    client_order_id: &str,
) -> serde_json::Value {
    let mut params = HashMap::from([
        ("symbol".to_string(), intent.symbol.as_str().to_string()),
        (
            "side".to_string(),
            match intent.side {
                hft_core::Side::Buy => "BUY",
                hft_core::Side::Sell => "SELL",
            }
            .to_string(),
        ),
        (
            "type".to_string(),
            match intent.order_type {
                hft_core::OrderType::Market => "MARKET",
                hft_core::OrderType::Limit => "LIMIT",
            }
            .to_string(),
        ),
        ("quantity".to_string(), intent.quantity.0.to_string()),
        ("newClientOrderId".to_string(), client_order_id.to_string()),
        ("recvWindow".to_string(), "5000".to_string()),
        ("newOrderRespType".to_string(), "ACK".to_string()),
    ]);
    if let Some(price) = intent.price {
        params.insert("price".to_string(), price.0.to_string());
    }
    if matches!(intent.order_type, hft_core::OrderType::Limit) {
        params.insert(
            "timeInForce".to_string(),
            match intent.time_in_force {
                hft_core::TimeInForce::IOC => "IOC",
                hft_core::TimeInForce::FOK => "FOK",
                _ => "GTC",
            }
            .to_string(),
        );
    }
    signer.sign_ws_api_params(&mut params);
    serde_json::json!({
        "id": client_order_id,
        "method": "order.place",
        "params": params,
    })
}

fn parse_binance_ws_order_response(
    response: serde_json::Value,
    client_order_id: &str,
) -> HftResult<OrderId> {
    let status = response
        .get("status")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    if status != 200 {
        let detail = response.to_string();
        return Err(match status {
            418 | 429 => hft_core::HftError::RateLimit(detail),
            401 | 403 => hft_core::HftError::Authentication(detail),
            400..=499 => hft_core::HftError::Exchange(detail),
            _ => hft_core::HftError::Network(format!("Binance WS order outcome unknown: {detail}")),
        });
    }
    let returned_client_id = response
        .pointer("/result/clientOrderId")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if returned_client_id != client_order_id {
        return Err(hft_core::HftError::Execution(format!(
            "Binance WS order returned mismatched client id: {returned_client_id}"
        )));
    }
    Ok(OrderId(client_order_id.to_string()))
}

type BinancePrivateSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_binance_private_ws(
    http: &HttpClient,
    signer: &BinanceSigner,
    ws_base_url: &str,
) -> HftResult<(BinancePrivateSocket, String)> {
    let response = http
        .signed_request(
            reqwest::Method::POST,
            "/api/v3/userDataStream",
            Some(signer.generate_headers()),
            None,
        )
        .await
        .map_err(|error| hft_core::HftError::Network(error.to_string()))?;
    if !response.status().is_success() {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.text().await.unwrap_or_default();
        return Err(classify_binance_http_error(
            status,
            retry_after.as_deref(),
            &body,
        ));
    }
    #[derive(serde::Deserialize)]
    struct ListenKeyResponse {
        #[serde(rename = "listenKey")]
        listen_key: String,
    }
    let listen_key: ListenKeyResponse = HttpClient::parse_json(response)
        .await
        .map_err(|error| hft_core::HftError::Serialization(error.to_string()))?;
    let url = format!(
        "{}/{}",
        ws_base_url.trim_end_matches('/'),
        listen_key.listen_key
    );
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|error| hft_core::HftError::Network(error.to_string()))?;
    integration::ws::set_ws_tcp_nodelay(ws.get_ref(), true)
        .map_err(|error| hft_core::HftError::Network(error.to_string()))?;
    Ok((ws, listen_key.listen_key))
}

async fn run_binance_private_ws(
    http: HttpClient,
    signer: BinanceSigner,
    ws_base_url: String,
    event_tx: broadcast::Sender<ExecutionEvent>,
    mut shutdown: watch::Receiver<bool>,
    initial: BinancePrivateSocket,
    initial_listen_key: String,
) {
    let mut ws = initial;
    let mut listen_key = initial_listen_key;
    let mut backoff = std::time::Duration::from_millis(100);
    loop {
        let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(30 * 60));
        keepalive.tick().await;
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
                _ = keepalive.tick() => {
                    let path = format!("/api/v3/userDataStream?listenKey={listen_key}");
                    let result = http.signed_request(
                        reqwest::Method::PUT,
                        &path,
                        Some(signer.generate_headers()),
                        None,
                    ).await;
                    if !matches!(result, Ok(response) if response.status().is_success()) {
                        warn!("Binance listenKey keepalive failed; reconnecting private stream");
                        break;
                    }
                }
                message = ws.next() => match message {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        let received_mono_us = hft_core::monotonic_micros();
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                            publish_binance_execution_report(&event_tx, &value, received_mono_us);
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(payload))) => {
                        if ws.send(tokio_tungstenite::tungstenite::Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
                    Some(Err(error)) => {
                        warn!(%error, "Binance private WS read failed");
                        break;
                    }
                    _ => {}
                }
            }
        }
        let _ = event_tx.send(ExecutionEvent::ConnectionStatus {
            connected: false,
            timestamp: hft_core::now_micros(),
        });
        loop {
            if *shutdown.borrow() {
                return;
            }
            match connect_binance_private_ws(&http, &signer, &ws_base_url).await {
                Ok((connected, new_listen_key)) => {
                    ws = connected;
                    listen_key = new_listen_key;
                    backoff = std::time::Duration::from_millis(100);
                    let _ = event_tx.send(ExecutionEvent::ConnectionStatus {
                        connected: true,
                        timestamp: hft_core::now_micros(),
                    });
                    break;
                }
                Err(error) => {
                    warn!(%error, "Binance private WS reconnect failed");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
                }
            }
        }
    }
}

fn classify_binance_http_error(
    status: reqwest::StatusCode,
    retry_after: Option<&str>,
    body: &str,
) -> hft_core::HftError {
    let retry = retry_after
        .filter(|value| !value.is_empty())
        .map(|value| format!("; retry-after={value}s"))
        .unwrap_or_default();
    let detail = format!("Binance HTTP {status}{retry}: {body}");
    match status.as_u16() {
        418 | 429 => hft_core::HftError::RateLimit(detail),
        401 | 403 => hft_core::HftError::Authentication(detail),
        400..=499 => hft_core::HftError::Exchange(detail),
        _ => hft_core::HftError::Network(detail),
    }
}

fn publish_binance_execution_report(
    tx: &broadcast::Sender<ExecutionEvent>,
    value: &serde_json::Value,
    received_mono_us: u64,
) {
    if value.get("e").and_then(|entry| entry.as_str()) != Some("executionReport") {
        return;
    }
    let order_id = value
        .get("c")
        .and_then(|entry| entry.as_str())
        .filter(|client_order_id| !client_order_id.is_empty())
        .map(|client_order_id| OrderId(client_order_id.to_string()))
        .or_else(|| {
            value
                .get("i")
                .and_then(|entry| entry.as_i64())
                .filter(|order_id| *order_id > 0)
                .map(|order_id| OrderId(order_id.to_string()))
        });
    let Some(order_id) = order_id else {
        return;
    };
    let timestamp = value
        .get("E")
        .and_then(|entry| entry.as_u64())
        .unwrap_or_else(|| hft_core::now_micros() / 1000)
        .saturating_mul(1000);
    let status = value
        .get("X")
        .and_then(|entry| entry.as_str())
        .unwrap_or("");
    let execution_type = value
        .get("x")
        .and_then(|entry| entry.as_str())
        .unwrap_or("");
    let publish_timing = |kind| {
        let _ = tx.send(ExecutionEvent::PrivateOrderTiming {
            order_id: order_id.clone(),
            kind,
            received_mono_us,
        });
    };

    match (execution_type, status) {
        ("NEW", "NEW") => {
            publish_timing(PrivateOrderEventKind::Ack);
            let _ = tx.send(ExecutionEvent::OrderAck {
                order_id: order_id.clone(),
                timestamp,
            });
        }
        ("CANCELED", "CANCELED")
        | ("EXPIRED", "EXPIRED")
        | ("TRADE_PREVENTION", "EXPIRED_IN_MATCH") => {
            publish_timing(PrivateOrderEventKind::Report);
            let _ = tx.send(ExecutionEvent::OrderCanceled {
                order_id: order_id.clone(),
                timestamp,
            });
        }
        ("REJECTED", "REJECTED") => {
            let reason = value
                .get("r")
                .and_then(|entry| entry.as_str())
                .filter(|reason| !reason.is_empty() && *reason != "NONE")
                .unwrap_or("Exchange rejected")
                .to_string();
            publish_timing(PrivateOrderEventKind::Report);
            let _ = tx.send(ExecutionEvent::OrderReject {
                order_id: order_id.clone(),
                reason,
                timestamp,
            });
        }
        ("TRADE", "PARTIALLY_FILLED" | "FILLED") => {
            let quantity = value
                .get("l")
                .and_then(|entry| entry.as_str())
                .and_then(|value| Quantity::from_str(value).ok())
                .filter(|quantity| *quantity > Quantity::zero());
            let price = value
                .get("L")
                .and_then(|entry| entry.as_str())
                .and_then(|value| Price::from_str(value).ok())
                .filter(|price| *price > Price::zero());
            let execution_id = value
                .get("t")
                .and_then(|entry| entry.as_i64())
                .filter(|trade_id| *trade_id >= 0)
                .map(|trade_id| trade_id.to_string())
                .or_else(|| {
                    value
                        .get("I")
                        .and_then(|entry| entry.as_i64())
                        .map(|execution_id| execution_id.to_string())
                });
            if let (Some(quantity), Some(price), Some(execution_id)) =
                (quantity, price, execution_id)
            {
                publish_timing(PrivateOrderEventKind::Report);
                let _ = tx.send(ExecutionEvent::Fill {
                    fill_id: format!("BNFILL-{}-{execution_id}", order_id.0),
                    order_id,
                    price,
                    quantity,
                    timestamp,
                });
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
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

#[derive(serde::Deserialize)]
struct BinanceBalance {
    asset: String,
    free: String,
    locked: String,
}

#[derive(serde::Deserialize)]
struct BinanceAccountResponse {
    balances: Vec<BinanceBalance>,
}

#[derive(serde::Deserialize)]
#[allow(non_snake_case)]
struct BinanceSymbolInfo {
    symbol: String,
    status: String,
    baseAsset: String,
    quoteAsset: String,
}

#[derive(serde::Deserialize)]
struct BinanceExchangeInfo {
    symbols: Vec<BinanceSymbolInfo>,
}

#[derive(serde::Deserialize)]
struct BinanceTickerPrice {
    symbol: String,
    price: String,
}

fn binance_usd_rate(
    asset: &str,
    graph: &HashMap<String, Vec<(String, Decimal)>>,
) -> Option<Decimal> {
    const USD_ASSETS: &[&str] = &["USD", "USDT", "USDC", "FDUSD", "BUSD", "TUSD", "DAI"];
    if USD_ASSETS.contains(&asset) {
        return Some(Decimal::ONE);
    }
    let mut queue = VecDeque::from([(asset.to_string(), Decimal::ONE)]);
    let mut visited = HashSet::from([asset.to_string()]);
    while let Some((current, rate)) = queue.pop_front() {
        for (next, edge_rate) in graph.get(&current).into_iter().flatten() {
            let next_rate = rate * *edge_rate;
            if USD_ASSETS.contains(&next.as_str()) {
                return Some(next_rate);
            }
            if visited.insert(next.clone()) {
                queue.push_back((next.clone(), next_rate));
            }
        }
    }
    None
}

fn value_binance_balances(
    account: BinanceAccountResponse,
    exchange: BinanceExchangeInfo,
    tickers: Vec<BinanceTickerPrice>,
) -> HftResult<Vec<AccountBalance>> {
    let ticker_prices = tickers
        .into_iter()
        .filter_map(|ticker| {
            ticker
                .price
                .parse::<Decimal>()
                .ok()
                .filter(|price| *price > Decimal::ZERO)
                .map(|price| (ticker.symbol, price))
        })
        .collect::<HashMap<_, _>>();
    let mut graph: HashMap<String, Vec<(String, Decimal)>> = HashMap::new();
    for symbol in exchange
        .symbols
        .into_iter()
        .filter(|symbol| symbol.status == "TRADING")
    {
        let Some(price) = ticker_prices.get(&symbol.symbol).copied() else {
            continue;
        };
        graph
            .entry(symbol.baseAsset.clone())
            .or_default()
            .push((symbol.quoteAsset.clone(), price));
        graph
            .entry(symbol.quoteAsset)
            .or_default()
            .push((symbol.baseAsset, Decimal::ONE / price));
    }

    account
        .balances
        .into_iter()
        .map(|balance| {
            let available = balance.free.parse::<Decimal>().map_err(|error| {
                hft_core::HftError::Parse(format!(
                    "Binance {} free balance: {error}",
                    balance.asset
                ))
            })?;
            let frozen = balance.locked.parse::<Decimal>().map_err(|error| {
                hft_core::HftError::Parse(format!(
                    "Binance {} locked balance: {error}",
                    balance.asset
                ))
            })?;
            let total = available + frozen;
            Ok(AccountBalance {
                usd_value: binance_usd_rate(&balance.asset, &graph).map(|rate| total * rate),
                asset: balance.asset,
                available,
                frozen,
                total,
            })
        })
        .collect()
}

fn parse_binance_open_order(order: BinanceOrder) -> HftResult<OpenOrder> {
    if order.order_id == 0 || order.symbol.is_empty() || order.client_order_id.is_empty() {
        return Err(hft_core::HftError::Parse(
            "Binance open order is missing an identifier or symbol".to_string(),
        ));
    }

    let side = match order.side.as_str() {
        "BUY" => hft_core::Side::Buy,
        "SELL" => hft_core::Side::Sell,
        value => {
            return Err(hft_core::HftError::Parse(format!(
                "Binance open order {} has unknown side {value}",
                order.order_id
            )))
        }
    };
    let order_type = match order.r#type.as_str() {
        "MARKET" => hft_core::OrderType::Market,
        "LIMIT" => hft_core::OrderType::Limit,
        value => {
            return Err(hft_core::HftError::Parse(format!(
                "Binance open order {} has unsupported order type {value}",
                order.order_id
            )))
        }
    };
    let original = order.orig_qty.parse::<Decimal>().map_err(|error| {
        hft_core::HftError::Parse(format!(
            "Binance open order {} has invalid origQty: {error}",
            order.order_id
        ))
    })?;
    let filled = order.executed_qty.parse::<Decimal>().map_err(|error| {
        hft_core::HftError::Parse(format!(
            "Binance open order {} has invalid executedQty: {error}",
            order.order_id
        ))
    })?;
    if original <= Decimal::ZERO || filled < Decimal::ZERO || filled > original {
        return Err(hft_core::HftError::Parse(format!(
            "Binance open order {} has inconsistent quantities",
            order.order_id
        )));
    }

    let price = match order_type {
        hft_core::OrderType::Market => None,
        hft_core::OrderType::Limit => Some(Price::from_str(&order.price).map_err(|error| {
            hft_core::HftError::Parse(format!(
                "Binance open order {} has invalid price: {error}",
                order.order_id
            ))
        })?),
    };
    let status = match order.status.as_str() {
        "NEW" => ports::OrderStatus::New,
        "PARTIALLY_FILLED" => ports::OrderStatus::PartiallyFilled,
        "FILLED" => ports::OrderStatus::Filled,
        "CANCELED" => ports::OrderStatus::Canceled,
        "REJECTED" => ports::OrderStatus::Rejected,
        "EXPIRED" => ports::OrderStatus::Expired,
        value => {
            return Err(hft_core::HftError::Parse(format!(
                "Binance open order {} has unknown status {value}",
                order.order_id
            )))
        }
    };
    let created_at = order.time.checked_mul(1_000).ok_or_else(|| {
        hft_core::HftError::Parse(format!(
            "Binance open order {} creation timestamp overflow",
            order.order_id
        ))
    })?;
    let updated_at = order.update_time.checked_mul(1_000).ok_or_else(|| {
        hft_core::HftError::Parse(format!(
            "Binance open order {} update timestamp overflow",
            order.order_id
        ))
    })?;
    if created_at == 0 || updated_at < created_at {
        return Err(hft_core::HftError::Parse(format!(
            "Binance open order {} has invalid timestamps",
            order.order_id
        )));
    }

    Ok(OpenOrder {
        order_id: hft_core::OrderId(order.order_id.to_string()),
        client_order_id: Some(order.client_order_id),
        symbol: hft_core::Symbol::from(order.symbol),
        side,
        order_type,
        original_quantity: Quantity(original),
        remaining_quantity: Quantity(original - filled),
        filled_quantity: Quantity(filled),
        price,
        status,
        created_at,
        updated_at,
    })
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
            account_capability: cfg.account_capability,
            timeout_ms: cfg.timeout_ms,
            order_symbol: HashMap::new(),
            listen_key: None,
            resilient_executor: None,
            alert_callback: None,
            next_client_order_id: None,
            ws_order: None,
            shutdown_tx: None,
            last_submission_timing: None,
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
        self.signer.as_ref().ok_or_else(|| {
            hft_core::HftError::Execution(
                "Signer not initialized - missing credentials".to_string(),
            )
        })
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

    fn validate_product_gate(&self, intent: &ports::OrderIntent) -> HftResult<()> {
        match intent.product_type {
            ProductType::Spot => {
                if self.account_capability.can_trade_crypto_spot {
                    Ok(())
                } else {
                    Err(hft_core::HftError::Execution(
                        "Binance account is not enabled for crypto spot trading".to_string(),
                    ))
                }
            }
            ProductType::TokenizedSecuritySpot => {
                let ctx = &intent.compliance_context;
                if intent.asset_class != AssetClass::TokenizedSecurity {
                    return Err(hft_core::HftError::Execution(
                        "Binance tokenized security order must set asset_class=TokenizedSecurity"
                            .to_string(),
                    ));
                }
                if !ctx.allow_tokenized_securities || !ctx.eligibility_confirmed {
                    return Err(hft_core::HftError::Execution(
                        "Binance tokenized securities require explicit account eligibility and allow_tokenized_securities=true".to_string(),
                    ));
                }
                if !self.account_capability.can_trade_tokenized_securities {
                    return Err(hft_core::HftError::Execution(
                        "Binance account is not enabled for tokenized securities".to_string(),
                    ));
                }
                Ok(())
            }
            ProductType::Futures | ProductType::Perp => Err(hft_core::HftError::Execution(
                "Binance futures/perp orders must use a dedicated derivatives adapter, not /api/v3/order"
                    .to_string(),
            )),
            ProductType::BrokerageEquity => Err(hft_core::HftError::Execution(
                "Binance brokerage equities must use a dedicated equities adapter, not /api/v3/order"
                    .to_string(),
            )),
            ProductType::PredictionMarket => Err(hft_core::HftError::Execution(
                "Binance prediction-market orders must use the dedicated W3W Prediction adapter"
                    .to_string(),
            )),
        }
    }
}

#[async_trait]
impl ExecutionClient for BinanceExecutionClient {
    async fn place_order(&mut self, intent: ports::OrderIntent) -> HftResult<OrderId> {
        self.last_submission_timing = None;
        let client_order_id = self.next_client_order_id.take().unwrap_or_else(|| {
            if matches!(self.mode, ExecutionMode::Paper) {
                format!("BINANCE_PAPER_{:x}", hft_core::now_micros())
            } else {
                format!("BINANCE_{:x}", hft_core::now_micros())
            }
        });
        if client_order_id.is_empty()
            || client_order_id.len() > 36
            || !client_order_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/".contains(&byte))
        {
            return Err(hft_core::HftError::InvalidOrder(
                "Binance client order id must be 1-36 ASCII characters [A-Za-z0-9-_.:/]"
                    .to_string(),
            ));
        }
        self.validate_product_gate(&intent)?;

        // Live/Testnet 模式都必須有 signer，不能靜默退回 Paper。
        if uses_exchange_api(self.mode) && self.signer.is_none() {
            return Err(hft_core::HftError::Authentication(
                "Binance live/testnet order requires API credentials".to_string(),
            ));
        }

        if uses_exchange_api(self.mode) {
            self.order_symbol
                .insert(client_order_id.clone(), intent.symbol.as_str().to_string());
            let signer = self.get_signer()?;
            let payload = build_binance_ws_order_request(signer, &intent, &client_order_id);
            let ws_order = self.ws_order.as_ref().ok_or_else(|| {
                hft_core::HftError::Network("Binance WS order channel is not connected".to_string())
            })?;
            let receipt = ws_order.submit(client_order_id.clone(), payload).await?;
            let response = match receipt.outcome {
                Ok(response) => response,
                Err(error) => {
                    self.last_submission_timing = Some((
                        receipt.write_started_mono_us,
                        receipt.write_returned_mono_us,
                        None,
                    ));
                    return Err(error);
                }
            };
            let outcome = parse_binance_ws_order_response(response, &client_order_id);
            // Preserve the decoded-message boundary, but expose it only after semantic validation.
            let response_received_mono_us = (receipt.decoded_response_mono_us.is_some()
                && matches!(
                    &outcome,
                    Ok(_)
                        | Err(hft_core::HftError::RateLimit(_))
                        | Err(hft_core::HftError::Authentication(_))
                        | Err(hft_core::HftError::Exchange(_))
                ))
            .then_some(receipt.decoded_response_mono_us)
            .flatten();
            self.last_submission_timing = Some((
                receipt.write_started_mono_us,
                receipt.write_returned_mono_us,
                response_received_mono_us,
            ));
            return outcome;
        }

        // Paper: 立即回傳訂單ID並廣播 ACK/Fill
        let order_id = OrderId(client_order_id);
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

    async fn place_order_envelope(&mut self, envelope: &OrderIntentEnvelope) -> HftResult<OrderId> {
        self.next_client_order_id = Some(envelope.client_order_id.clone());
        self.place_order(envelope.intent.clone()).await
    }

    async fn place_order_envelope_traced(
        &mut self,
        envelope: &OrderIntentEnvelope,
    ) -> ExecutionSubmissionAttempt {
        self.next_client_order_id = Some(envelope.client_order_id.clone());
        let outcome = self.place_order(envelope.intent.clone()).await;
        let Some((write_started, write_returned, response_received)) =
            self.last_submission_timing.take()
        else {
            return ExecutionSubmissionAttempt::without_transport_timing(outcome);
        };
        ExecutionSubmissionAttempt {
            outcome,
            userspace_write_started_mono_us: write_started,
            userspace_write_returned_mono_us: write_returned,
            response_received_mono_us: response_received,
        }
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        if uses_exchange_api(self.mode) {
            if self.signer.is_none() {
                return Err(hft_core::HftError::Authentication(
                    "Binance live/testnet cancel requires API credentials".to_string(),
                ));
            }
            let symbol = self.order_symbol.get(&order_id.0).cloned().ok_or_else(|| {
                hft_core::HftError::OrderNotFound(format!(
                    "Binance cancel requires known symbol metadata for order {}",
                    order_id.0
                ))
            })?;
            if self.http_client.is_none() {
                self.ensure_http()?;
            }
            let signer = self
                .signer
                .as_ref()
                .ok_or_else(|| hft_core::HftError::Authentication("缺少API憑證".to_string()))?;
            let http_client = self.get_http()?.clone();
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
                        params.insert("symbol".to_string(), sym.clone());
                        if oid.parse::<u64>().is_ok() {
                            params.insert("orderId".to_string(), oid.clone());
                        } else {
                            params.insert("origClientOrderId".to_string(), oid.clone());
                        }
                        params.insert("recvWindow".to_string(), "5000".to_string());
                        let signed_query = sig.sign_request(&mut params);
                        let path = format!("/api/v3/order?{}", signed_query);
                        let headers = sig.generate_headers();
                        let response = http
                            .signed_request(reqwest::Method::DELETE, &path, Some(headers), None)
                            .await
                            .map_err(|e| hft_core::HftError::Network(e.to_string()))?;
                        if !response.status().is_success() {
                            let status = response.status();
                            let retry_after = response
                                .headers()
                                .get(reqwest::header::RETRY_AFTER)
                                .and_then(|value| value.to_str().ok())
                                .map(str::to_owned);
                            let body = response.text().await.unwrap_or_default();
                            return Err(classify_binance_http_error(
                                status,
                                retry_after.as_deref(),
                                &body,
                            ));
                        }
                        #[derive(serde::Deserialize)]
                        struct CancelResponse {
                            symbol: String,
                            #[serde(rename = "orderId")]
                            order_id: u64,
                            #[serde(rename = "clientOrderId")]
                            client_order_id: String,
                        }
                        let response: CancelResponse =
                            HttpClient::parse_json(response).await.map_err(|error| {
                                hft_core::HftError::Execution(format!(
                                    "Binance cancel response could not be decoded: {error}"
                                ))
                            })?;
                        let identifier_matches = if oid.parse::<u64>().is_ok() {
                            response.order_id.to_string() == oid
                        } else {
                            response.client_order_id == oid
                        };
                        if response.symbol != sym || !identifier_matches {
                            return Err(hft_core::HftError::Execution(
                                "Binance cancel response did not match the requested order"
                                    .to_string(),
                            ));
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
                                "binance",
                                "cancel_order",
                                format!("撤單失敗且熔斷器已開啟 (order_id={}): {}", order_id.0, e),
                            )
                            .with_error(e.to_string()),
                        );
                    }
                }
            }

            if result.is_ok() {
                self.order_symbol.remove(&order_id.0);
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
        if uses_exchange_api(self.mode) {
            if new_quantity.is_none() && new_price.is_none() {
                return Ok(());
            }
            return Err(hft_core::HftError::Config(format!(
                "Binance live modify is disabled for order {}; use an explicit cancel-then-new intent",
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
            let stream =
                tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|result| async move {
                    match result {
                        Ok(event) => Some(Ok(event)),
                        Err(e) => Some(Ok(ExecutionEvent::ReconciliationRequired {
                            reason: format!(
                                "Binance private execution stream lagged and has no in-process watermark recovery; restart is required: {e}"
                            ),
                            timestamp: hft_core::now_micros(),
                        })),
                    }
                });
            Ok(Box::pin(stream))
        } else {
            Ok(Box::pin(stream::empty()))
        }
    }

    async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
        if !uses_exchange_api(self.mode) {
            return Err(hft_core::HftError::Config(
                "Binance authoritative balances require live/testnet mode".to_string(),
            ));
        }
        let signer = self.get_signer()?;
        let http = self.get_http()?;
        let mut params = HashMap::from([
            ("omitZeroBalances".to_string(), "true".to_string()),
            ("recvWindow".to_string(), "5000".to_string()),
        ]);
        let account_path = format!("/api/v3/account?{}", signer.sign_request(&mut params));
        let account_response = http
            .get(&account_path, Some(signer.generate_headers()))
            .await
            .map_err(|error| hft_core::HftError::Network(error.to_string()))?;
        if !account_response.status().is_success() {
            let status = account_response.status();
            let retry_after = account_response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = account_response.text().await.unwrap_or_default();
            return Err(classify_binance_http_error(
                status,
                retry_after.as_deref(),
                &body,
            ));
        }
        let account: BinanceAccountResponse = HttpClient::parse_json(account_response)
            .await
            .map_err(|error| {
                hft_core::HftError::Parse(format!("Binance account response: {error}"))
            })?;

        let exchange_response = http
            .get("/api/v3/exchangeInfo", None)
            .await
            .map_err(|error| hft_core::HftError::Network(error.to_string()))?;
        if !exchange_response.status().is_success() {
            let status = exchange_response.status();
            let body = exchange_response.text().await.unwrap_or_default();
            return Err(classify_binance_http_error(status, None, &body));
        }
        let exchange: BinanceExchangeInfo = HttpClient::parse_json(exchange_response)
            .await
            .map_err(|error| {
                hft_core::HftError::Parse(format!("Binance exchangeInfo response: {error}"))
            })?;

        let ticker_response = http
            .get("/api/v3/ticker/price", None)
            .await
            .map_err(|error| hft_core::HftError::Network(error.to_string()))?;
        if !ticker_response.status().is_success() {
            let status = ticker_response.status();
            let body = ticker_response.text().await.unwrap_or_default();
            return Err(classify_binance_http_error(status, None, &body));
        }
        let tickers: Vec<BinanceTickerPrice> = HttpClient::parse_json(ticker_response)
            .await
            .map_err(|error| {
                hft_core::HftError::Parse(format!("Binance ticker response: {error}"))
            })?;
        value_binance_balances(account, exchange, tickers)
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

                let alert =
                    ExecutionAlert::new(alert_type, "binance", "execution", &cb_alert.message)
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

        // 啟動私有 WS（Live/Testnet）
        if uses_exchange_api(self.mode) {
            self.ws_order = Some(
                BinanceWsOrderClient::connect(
                    binance_ws_order_url(self.mode).to_string(),
                    std::time::Duration::from_millis(self.timeout_ms),
                )
                .await?,
            );
            let http = self.get_http()?.clone();
            let signer = self.get_signer()?.clone();
            let (ws, listen_key) =
                connect_binance_private_ws(&http, &signer, &self.ws_base_url).await?;
            self.listen_key = Some(listen_key.clone());
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            self.shutdown_tx = Some(shutdown_tx);
            tokio::spawn(run_binance_private_ws(
                http,
                signer,
                self.ws_base_url.clone(),
                tx.clone(),
                shutdown_rx,
                ws,
                listen_key,
            ));
        } else {
            warn!("Binance Paper mode: 跳過私有 WS 建立");
        }

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
        if !uses_exchange_api(self.mode) {
            return Err(hft_core::HftError::Execution(
                "Binance list_open_orders is only supported in Live or Testnet mode".to_string(),
            ));
        }
        let signer = self.signer.as_ref().ok_or_else(|| {
            hft_core::HftError::Authentication(
                "Binance live/testnet list_open_orders requires API credentials".to_string(),
            )
        })?;

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

        if !resp.status().is_success() {
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let body = resp.text().await.unwrap_or_default();
            return Err(classify_binance_http_error(
                status,
                retry_after.as_deref(),
                &body,
            ));
        }

        let items: Vec<BinanceOrder> = HttpClient::parse_json(resp)
            .await
            .map_err(|e| hft_core::HftError::Serialization(e.to_string()))?;

        // 映射到統一 OpenOrder
        let mut out = Vec::new();
        for item in items {
            out.push(parse_binance_open_order(item)?);
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderType, Side, Symbol, TimeInForce};
    use integration::signing::BinanceCredentials;
    use ports::{ExecutionClient, OrderIntent};

    fn make_test_config(mode: ExecutionMode) -> BinanceExecutionConfig {
        BinanceExecutionConfig {
            credentials: BinanceCredentials {
                api_key: String::new(),
                secret_key: String::new(),
            },
            rest_base_url: "https://api.binance.com".to_string(),
            ws_base_url: "wss://stream.binance.com:9443/ws".to_string(),
            timeout_ms: 5000,
            mode,
            account_capability: AccountCapability::default(),
        }
    }

    #[test]
    fn http_rate_limits_are_known_rejections_with_retry_after() {
        let error = classify_binance_http_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some("7"),
            r#"{"code":-1003,"msg":"Too many requests"}"#,
        );

        assert!(
            matches!(error, hft_core::HftError::RateLimit(message) if message.contains("retry-after=7s"))
        );
        assert!(matches!(
            classify_binance_http_error(reqwest::StatusCode::IM_A_TEAPOT, None, "IP banned"),
            hft_core::HftError::RateLimit(_)
        ));
    }

    #[test]
    fn private_fills_use_exchange_trade_id_and_expiry_is_terminal() {
        let (tx, mut rx) = broadcast::channel(8);
        for trade_id in [41, 42] {
            publish_binance_execution_report(
                &tx,
                &serde_json::json!({
                    "e": "executionReport",
                    "E": 123,
                    "x": "TRADE",
                    "X": "PARTIALLY_FILLED",
                    "i": 7,
                    "t": trade_id,
                    "l": "0.1",
                    "L": "100"
                }),
                hft_core::monotonic_micros(),
            );
        }
        assert!(matches!(
            rx.try_recv().expect("first private timing"),
            ExecutionEvent::PrivateOrderTiming {
                kind: PrivateOrderEventKind::Report,
                ..
            }
        ));
        let first = rx.try_recv().expect("first fill");
        assert!(matches!(
            rx.try_recv().expect("second private timing"),
            ExecutionEvent::PrivateOrderTiming {
                kind: PrivateOrderEventKind::Report,
                ..
            }
        ));
        let second = rx.try_recv().expect("second fill");
        let (
            ExecutionEvent::Fill { fill_id: first, .. },
            ExecutionEvent::Fill {
                fill_id: second, ..
            },
        ) = (first, second)
        else {
            panic!("expected two fills");
        };
        assert_ne!(first, second);
        assert!(first.ends_with("-41"));
        assert!(second.ends_with("-42"));

        publish_binance_execution_report(
            &tx,
            &serde_json::json!({
                "e": "executionReport",
                "E": 124,
                "x": "EXPIRED",
                "X": "EXPIRED",
                "i": 7,
                "t": -1,
                "l": "0",
                "L": "0"
            }),
            hft_core::monotonic_micros(),
        );
        assert!(matches!(
            rx.try_recv().expect("expiry timing"),
            ExecutionEvent::PrivateOrderTiming {
                kind: PrivateOrderEventKind::Report,
                ..
            }
        ));
        assert!(matches!(
            rx.try_recv().expect("expiry event"),
            ExecutionEvent::OrderCanceled { .. }
        ));
    }

    #[test]
    fn private_reports_prefer_the_stable_client_order_id() {
        let (tx, mut rx) = broadcast::channel(2);
        publish_binance_execution_report(
            &tx,
            &serde_json::json!({
                "e": "executionReport",
                "E": 123,
                "x": "NEW",
                "X": "NEW",
                "i": 7,
                "c": "client-42"
            }),
            42,
        );

        assert!(matches!(
            rx.try_recv().expect("ack timing"),
            ExecutionEvent::PrivateOrderTiming {
                order_id,
                kind: PrivateOrderEventKind::Ack,
                received_mono_us: 42,
            } if order_id == OrderId("client-42".to_string())
        ));
        assert!(matches!(
            rx.try_recv().expect("ack event"),
            ExecutionEvent::OrderAck { order_id, .. }
                if order_id == OrderId("client-42".to_string())
        ));
    }

    #[test]
    fn malformed_private_reports_do_not_publish_authoritative_timing() {
        let (tx, mut rx) = broadcast::channel(4);
        for report in [
            serde_json::json!({
                "e": "executionReport",
                "x": "TRADE",
                "X": "PARTIALLY_FILLED",
                "i": 7,
                "t": 41,
                "l": "0",
                "L": "100"
            }),
            serde_json::json!({
                "e": "executionReport",
                "x": "UNKNOWN",
                "X": "UNKNOWN",
                "i": 7
            }),
            serde_json::json!({
                "e": "executionReport",
                "x": "TRADE",
                "X": "NEW",
                "i": 7,
                "t": 42,
                "l": "0.1",
                "L": "100"
            }),
            serde_json::json!({
                "e": "executionReport",
                "x": "TRADE",
                "X": "CANCELED",
                "i": 7,
                "t": 43,
                "l": "0.1",
                "L": "100"
            }),
        ] {
            publish_binance_execution_report(&tx, &report, 42);
        }

        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn websocket_order_request_is_signed_and_correlated_by_client_id() {
        let signer = BinanceSigner::new(BinanceCredentials::new(
            "api-key".to_string(),
            "secret".to_string(),
        ));
        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            asset_class: hft_core::AssetClass::Crypto,
            product_type: ProductType::Spot,
            compliance_context: hft_core::ComplianceContext::default(),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::from_f64(0.01).unwrap(),
            price: Some(Price::from_f64(100.0).unwrap()),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test".to_string(),
            target_venue: None,
        };
        let request = build_binance_ws_order_request(&signer, &intent, "client-42");

        assert_eq!(request["method"], "order.place");
        assert_eq!(request["id"], "client-42");
        assert_eq!(request["params"]["newClientOrderId"], "client-42");
        assert_eq!(request["params"]["apiKey"], "api-key");
        assert_eq!(
            request["params"]["signature"].as_str().map(str::len),
            Some(64)
        );
        assert_eq!(
            parse_binance_ws_order_response(
                serde_json::json!({
                    "status": 200,
                    "result": {"clientOrderId": "client-42", "orderId": 7}
                }),
                "client-42"
            )
            .unwrap(),
            OrderId("client-42".to_string())
        );
    }

    #[test]
    fn balance_valuation_uses_multi_hop_exchange_prices() {
        let balances = value_binance_balances(
            BinanceAccountResponse {
                balances: vec![BinanceBalance {
                    asset: "ETH".to_string(),
                    free: "2".to_string(),
                    locked: "0.5".to_string(),
                }],
            },
            BinanceExchangeInfo {
                symbols: vec![
                    BinanceSymbolInfo {
                        symbol: "ETHBTC".to_string(),
                        status: "TRADING".to_string(),
                        baseAsset: "ETH".to_string(),
                        quoteAsset: "BTC".to_string(),
                    },
                    BinanceSymbolInfo {
                        symbol: "BTCUSDT".to_string(),
                        status: "TRADING".to_string(),
                        baseAsset: "BTC".to_string(),
                        quoteAsset: "USDT".to_string(),
                    },
                ],
            },
            vec![
                BinanceTickerPrice {
                    symbol: "ETHBTC".to_string(),
                    price: "0.05".to_string(),
                },
                BinanceTickerPrice {
                    symbol: "BTCUSDT".to_string(),
                    price: "60000".to_string(),
                },
            ],
        )
        .unwrap();

        assert_eq!(balances[0].total, Decimal::new(25, 1));
        assert_eq!(balances[0].usd_value, Some(Decimal::new(7500, 0)));
    }

    #[test]
    fn test_config_creation() {
        let config = make_test_config(ExecutionMode::Paper);
        assert_eq!(config.rest_base_url, "https://api.binance.com");
        assert_eq!(config.ws_base_url, "wss://stream.binance.com:9443/ws");
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
        assert!(debug_str.contains("BinanceExecutionConfig"));
    }

    #[test]
    fn test_client_creation() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BinanceExecutionClient::new(config);
        assert!(!client.connected);
        assert!(client.event_tx.is_none());
        assert!(client.http_client.is_none());
    }

    #[test]
    fn test_client_creation_with_empty_credentials() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BinanceExecutionClient::new(config);
        // Empty credentials should result in None signer
        assert!(client.signer.is_none());
    }

    #[test]
    fn test_client_creation_with_credentials() {
        let config = BinanceExecutionConfig {
            credentials: BinanceCredentials {
                api_key: "test_key".to_string(),
                secret_key: "test_secret".to_string(),
            },
            rest_base_url: "https://api.binance.com".to_string(),
            ws_base_url: "wss://stream.binance.com:9443/ws".to_string(),
            timeout_ms: 5000,
            mode: ExecutionMode::Paper,
            account_capability: AccountCapability::default(),
        };
        let client = BinanceExecutionClient::new(config);
        // With credentials, signer should be Some
        assert!(client.signer.is_some());
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
        let client = BinanceExecutionClient::new(config);
        assert!(client.resilience_stats().is_none());
    }

    #[test]
    fn test_with_alert_callback() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let config = make_test_config(ExecutionMode::Paper);
        let called = Arc::new(AtomicBool::new(false));
        let called_clone = Arc::clone(&called);

        let client = BinanceExecutionClient::new(config).with_alert_callback(move |_alert| {
            called_clone.store(true, Ordering::SeqCst);
        });

        // Alert callback should be set
        assert!(client.alert_callback.is_some());
    }

    #[tokio::test]
    async fn test_health_check_initial() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BinanceExecutionClient::new(config);
        let health = client.health().await;

        assert!(!health.connected);
        assert!(health.latency_ms.is_some());
        assert!(health.last_heartbeat > 0);
    }

    #[tokio::test]
    async fn test_connect_paper_mode() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);

        let result = client.connect().await;
        assert!(result.is_ok());
        assert!(client.connected);
        assert!(client.event_tx.is_some());
        assert!(client.resilient_executor.is_some());
    }

    #[tokio::test]
    async fn test_disconnect() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);

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
        let mut client = BinanceExecutionClient::new(config);

        client.connect().await.unwrap();
        let health = client.health().await;

        assert!(health.connected);
    }

    #[tokio::test]
    async fn test_paper_mode_place_order() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);
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
        assert!(order_id.0.starts_with("BINANCE_PAPER_"));
    }

    #[tokio::test]
    async fn live_mode_without_credentials_rejects_order_instead_of_paper_fallback() {
        let config = make_test_config(ExecutionMode::Live);
        let mut client = BinanceExecutionClient::new(config);

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

        let err = client.place_order(intent).await.unwrap_err();
        assert!(err.to_string().contains("requires API credentials"));
    }

    #[tokio::test]
    async fn testnet_without_credentials_rejects_order_instead_of_paper_fallback() {
        let config = make_test_config(ExecutionMode::Testnet);
        let mut client = BinanceExecutionClient::new(config);
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

        let err = client.place_order(intent).await.unwrap_err();
        assert!(err.to_string().contains("requires API credentials"));
    }

    #[tokio::test]
    async fn list_open_orders_requires_live_mode() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BinanceExecutionClient::new(config);

        let err = client.list_open_orders().await.unwrap_err();
        assert!(err
            .to_string()
            .contains("only supported in Live or Testnet mode"));
    }

    #[tokio::test]
    async fn list_open_orders_requires_live_credentials() {
        let config = make_test_config(ExecutionMode::Live);
        let client = BinanceExecutionClient::new(config);

        let err = client.list_open_orders().await.unwrap_err();
        assert!(err.to_string().contains("requires API credentials"));
    }

    #[test]
    fn open_order_parser_rejects_unknown_side_and_invalid_quantities() {
        let mut order = BinanceOrder {
            symbol: "BTCUSDT".to_string(),
            order_id: 42,
            client_order_id: "client-42".to_string(),
            price: "50000".to_string(),
            orig_qty: "1".to_string(),
            executed_qty: "0".to_string(),
            status: "NEW".to_string(),
            time: 1,
            update_time: 2,
            side: "UNKNOWN".to_string(),
            r#type: "LIMIT".to_string(),
        };
        assert!(parse_binance_open_order(order.clone()).is_err());

        order.side = "BUY".to_string();
        order.executed_qty = "2".to_string();
        assert!(parse_binance_open_order(order).is_err());
    }

    #[test]
    fn open_order_parser_rejects_unknown_status_and_order_type() {
        let mut order = BinanceOrder {
            symbol: "BTCUSDT".to_string(),
            order_id: 42,
            client_order_id: "client-42".to_string(),
            price: "50000".to_string(),
            orig_qty: "1".to_string(),
            executed_qty: "0".to_string(),
            status: "MYSTERY".to_string(),
            time: 1,
            update_time: 2,
            side: "BUY".to_string(),
            r#type: "LIMIT".to_string(),
        };
        assert!(parse_binance_open_order(order.clone()).is_err());

        order.status = "NEW".to_string();
        order.r#type = "STOP_LOSS_LIMIT".to_string();
        assert!(parse_binance_open_order(order).is_err());
    }

    #[test]
    fn open_order_deserialization_rejects_missing_required_fields() {
        let value = serde_json::json!({
            "symbol": "BTCUSDT",
            "orderId": 42,
            "clientOrderId": "client-42"
        });

        assert!(serde_json::from_value::<BinanceOrder>(value).is_err());
    }

    #[tokio::test]
    async fn tokenized_security_requires_explicit_eligibility() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);
        client.connect().await.unwrap();

        let intent = OrderIntent {
            symbol: Symbol::new("TSLABUSDT"),
            asset_class: hft_core::AssetClass::TokenizedSecurity,
            product_type: hft_core::ProductType::TokenizedSecuritySpot,
            compliance_context: hft_core::ComplianceContext::default(),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::from_f64(1.0).unwrap(),
            price: Some(Price::from_f64(250.0).unwrap()),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(hft_core::VenueId::BINANCE_TOKENIZED_SECURITIES),
        };

        let err = client.place_order(intent).await.unwrap_err();
        assert!(err.to_string().contains("explicit account eligibility"));
    }

    #[tokio::test]
    async fn tokenized_security_requires_account_capability() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);
        client.connect().await.unwrap();

        let intent = OrderIntent {
            symbol: Symbol::new("TSLABUSDT"),
            asset_class: hft_core::AssetClass::TokenizedSecurity,
            product_type: hft_core::ProductType::TokenizedSecuritySpot,
            compliance_context: hft_core::ComplianceContext {
                eligibility_confirmed: true,
                allow_tokenized_securities: true,
                ..Default::default()
            },
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::from_f64(1.0).unwrap(),
            price: Some(Price::from_f64(250.0).unwrap()),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(hft_core::VenueId::BINANCE_TOKENIZED_SECURITIES),
        };

        let err = client.place_order(intent).await.unwrap_err();
        assert!(err
            .to_string()
            .contains("account is not enabled for tokenized securities"));
    }

    #[tokio::test]
    async fn tokenized_security_allowed_when_order_and_account_are_enabled() {
        let mut config = make_test_config(ExecutionMode::Paper);
        config.account_capability.can_trade_tokenized_securities = true;
        let mut client = BinanceExecutionClient::new(config);
        client.connect().await.unwrap();

        let intent = OrderIntent {
            symbol: Symbol::new("TSLABUSDT"),
            asset_class: hft_core::AssetClass::TokenizedSecurity,
            product_type: hft_core::ProductType::TokenizedSecuritySpot,
            compliance_context: hft_core::ComplianceContext {
                eligibility_confirmed: true,
                allow_tokenized_securities: true,
                ..Default::default()
            },
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Quantity::from_f64(1.0).unwrap(),
            price: Some(Price::from_f64(250.0).unwrap()),
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(hft_core::VenueId::BINANCE_TOKENIZED_SECURITIES),
        };

        assert!(client.place_order(intent).await.is_ok());
    }

    #[tokio::test]
    async fn brokerage_equity_cannot_use_spot_order_api() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);
        client.connect().await.unwrap();

        let intent = OrderIntent {
            symbol: Symbol::new("AAPL"),
            asset_class: hft_core::AssetClass::Equity,
            product_type: hft_core::ProductType::BrokerageEquity,
            compliance_context: hft_core::ComplianceContext::default(),
            side: Side::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::from_f64(1.0).unwrap(),
            price: None,
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(hft_core::VenueId::BINANCE_BROKERAGE_EQUITIES),
        };

        let err = client.place_order(intent).await.unwrap_err();
        assert!(err.to_string().contains("dedicated equities adapter"));
    }

    #[tokio::test]
    async fn derivatives_cannot_use_spot_order_api() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);
        client.connect().await.unwrap();

        let intent = OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            asset_class: hft_core::AssetClass::Crypto,
            product_type: hft_core::ProductType::Perp,
            compliance_context: hft_core::ComplianceContext::default(),
            side: Side::Buy,
            order_type: OrderType::Market,
            quantity: Quantity::from_f64(0.001).unwrap(),
            price: None,
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_strategy".to_string(),
            target_venue: Some(hft_core::VenueId::BINANCE_FUTURES),
        };

        let err = client.place_order(intent).await.unwrap_err();
        assert!(err.to_string().contains("dedicated derivatives adapter"));
    }

    #[tokio::test]
    async fn test_paper_mode_cancel_order() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);
        client.connect().await.unwrap();

        let order_id = OrderId("test_order_123".to_string());
        let result = client.cancel_order(&order_id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn live_cancel_never_falls_back_to_paper_without_credentials() {
        let mut client = BinanceExecutionClient::new(make_test_config(ExecutionMode::Live));

        let error = client
            .cancel_order(&OrderId("1".to_string()))
            .await
            .unwrap_err();

        assert!(matches!(error, hft_core::HftError::Authentication(_)));
    }

    #[tokio::test]
    async fn live_cancel_rejects_unknown_symbol_instead_of_defaulting_to_btc() {
        let mut config = make_test_config(ExecutionMode::Live);
        config.credentials = BinanceCredentials::new("key".to_string(), "secret".to_string());
        let mut client = BinanceExecutionClient::new(config);

        let error = client
            .cancel_order(&OrderId("1".to_string()))
            .await
            .unwrap_err();

        assert!(matches!(error, hft_core::HftError::OrderNotFound(_)));
    }

    #[tokio::test]
    async fn live_modify_is_rejected_before_canceling_the_original_order() {
        let mut client = BinanceExecutionClient::new(make_test_config(ExecutionMode::Live));

        let error = client
            .modify_order(
                &OrderId("1".to_string()),
                None,
                Some(Price::from_f64(100.0).unwrap()),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, hft_core::HftError::Config(_)));
    }

    #[tokio::test]
    async fn test_paper_mode_modify_order() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);
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
        let client = BinanceExecutionClient::new(config);

        let result = client.execution_stream().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_execution_stream_after_connect() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);
        client.connect().await.unwrap();

        let result = client.execution_stream().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_circuit_state_before_connect() {
        let config = make_test_config(ExecutionMode::Paper);
        let client = BinanceExecutionClient::new(config);
        let state = client.circuit_state().await;
        assert!(state.is_none());
    }

    #[tokio::test]
    async fn test_circuit_state_after_connect() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);
        client.connect().await.unwrap();

        let state = client.circuit_state().await;
        assert!(state.is_some());
        assert_eq!(state.unwrap(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_reset_circuit_breaker() {
        let config = make_test_config(ExecutionMode::Paper);
        let mut client = BinanceExecutionClient::new(config);
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
        let mut client = BinanceExecutionClient::new(config);
        client.connect().await.unwrap();

        let stats = client.resilience_stats();
        assert!(stats.is_some());
        let stats = stats.unwrap();
        assert_eq!(stats.total_calls, 0);
        assert_eq!(stats.failed_calls, 0);
    }
}
