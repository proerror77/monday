//! Binance USDⓈ-M Futures execution adapter.
//!
//! This module is deliberately separate from the Spot adapter.  Binance exposes the two
//! products through different REST paths and different private-stream payloads; sharing a
//! client instance would make it possible to submit a perpetual intent to the Spot endpoint.
//! The adapter only submits the one-way/BOTH account mode and standard MARKET/LIMIT intents
//! represented by Monday's `OrderIntent`.  Venue conditional/algo orders are read back through
//! the typed cancellation-reference surface so reconciliation never misclassifies them.

use async_trait::async_trait;
use execution::{
    AlertCallback, CircuitBreakerConfig, CircuitState, ExecutionAlert, ExecutionAlertType,
    ExecutorStats, ResilientExecutor, RetryConfig,
};
use futures::{stream, SinkExt, StreamExt};
use hft_core::{
    AccountCapability, HftError, HftResult, OrderId, Price, ProductType, Quantity, Side, Symbol,
};
use integration::{
    http::{HttpClient, HttpClientConfig},
    signing::{BinanceCredentials, BinanceSigner},
};
use ports::{
    AccountBalance, AssetInventoryCapability, BoxStream, CancellableOrderRef, ExecutionClient,
    ExecutionEvent, ExecutionSubmissionAttempt, OpenOrder, OrderIntentEnvelope, OrderStatus,
    Position as PortPosition, PrivateOrderEventKind,
};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tracing::{info, warn};

use super::ExecutionMode;

const REST_LISTEN_KEY_PATH: &str = "/fapi/v1/listenKey";
const REST_ACCOUNT_PATH: &str = "/fapi/v3/account";
const REST_ORDER_PATH: &str = "/fapi/v1/order";
const REST_OPEN_ORDERS_PATH: &str = "/fapi/v1/openOrders";
const REST_OPEN_ALGO_ORDERS_PATH: &str = "/fapi/v1/openAlgoOrders";
const REST_ALGO_ORDER_PATH: &str = "/fapi/v1/algoOrder";
const USER_STREAM_KEEPALIVE: Duration = Duration::from_secs(30 * 60);
const DEFAULT_RECV_WINDOW: &str = "5000";

#[derive(Debug, Clone)]
pub struct BinanceUsdMExecutionConfig {
    pub credentials: BinanceCredentials,
    pub rest_base_url: String,
    pub ws_base_url: String,
    pub timeout_ms: u64,
    pub mode: ExecutionMode,
    /// Retained for parity with the existing Binance Spot config.  The USD-M adapter itself
    /// accepts only crypto perpetual/futures intents; account-mode admission is performed by the
    /// runtime configuration and an unsupported mode is rejected before this client is built.
    pub account_capability: AccountCapability,
}

#[derive(Debug, Clone)]
struct UsdMOrderRecord {
    symbol: String,
    client_order_id: String,
    order_type: String,
    native_id: String,
    canonical_order_id: String,
    algo_id: Option<u64>,
    is_algo: bool,
}

pub struct BinanceUsdMExecutionClient {
    event_tx: Option<broadcast::Sender<ExecutionEvent>>,
    connected: bool,
    http_client: Option<HttpClient>,
    signer: Option<BinanceSigner>,
    rest_base_url: String,
    ws_base_url: String,
    mode: ExecutionMode,
    timeout_ms: u64,
    order_records: std::sync::Mutex<HashMap<String, UsdMOrderRecord>>,
    listen_key: Option<String>,
    private_stream_connected: Arc<AtomicBool>,
    resilient_executor: Option<Arc<ResilientExecutor>>,
    alert_callback: Option<AlertCallback>,
    next_client_order_id: Option<String>,
    next_reduce_only: Option<bool>,
    shutdown_tx: Option<watch::Sender<bool>>,
    private_event_receiver: std::sync::Mutex<Option<broadcast::Receiver<ExecutionEvent>>>,
}

fn uses_exchange_api(mode: ExecutionMode) -> bool {
    matches!(mode, ExecutionMode::Live | ExecutionMode::Testnet)
}

fn mode_label(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Paper => "Paper",
        ExecutionMode::Live => "Live",
        ExecutionMode::Testnet => "Testnet",
    }
}

fn has_private_credentials(credentials: &BinanceCredentials) -> bool {
    !credentials.api_key.trim().is_empty() && !credentials.secret_key.trim().is_empty()
}

fn validate_client_order_id(client_order_id: &str) -> HftResult<()> {
    if client_order_id.is_empty()
        || client_order_id.len() > 36
        || !client_order_id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        return Err(HftError::InvalidOrder(
            "Binance USD-M client order id must be 1-36 ASCII characters [A-Za-z0-9-_.:/]"
                .to_string(),
        ));
    }
    Ok(())
}

fn timestamp_us(milliseconds: u64, context: &str) -> HftResult<u64> {
    milliseconds.checked_mul(1_000).ok_or_else(|| {
        HftError::Parse(format!(
            "Binance USD-M {context} timestamp overflows microseconds"
        ))
    })
}

fn canonical_standard_order_id(symbol: &str, native_id: impl std::fmt::Display) -> OrderId {
    OrderId(format!("BNUSDM:standard:{symbol}:{native_id}"))
}

fn canonical_algo_order_id(symbol: &str, native_id: impl std::fmt::Display) -> OrderId {
    OrderId(format!("BNUSDM:algo:{symbol}:{native_id}"))
}

fn parse_native_standard_order_id(order_id: &OrderId, symbol: &str) -> Option<String> {
    order_id
        .0
        .strip_prefix(&format!("BNUSDM:standard:{symbol}:"))
        .filter(|native_id| !native_id.is_empty())
        .map(str::to_owned)
}

fn parse_decimal(value: &str, field: &str) -> HftResult<Decimal> {
    value
        .parse::<Decimal>()
        .map_err(|error| HftError::Parse(format!("Binance USD-M {field}: {error}")))
}

fn classify_http_error(
    status: reqwest::StatusCode,
    retry_after: Option<&str>,
    body: &str,
) -> HftError {
    // Keep Binance's existing status-to-error taxonomy.  The USD-M endpoint family must not
    // introduce a second interpretation of authentication, rate-limit, or exchange errors.
    super::classify_binance_http_error(status, retry_after, body)
}

fn classify_transport_error(operation: &str, error: impl std::fmt::Display) -> HftError {
    HftError::Network(format!(
        "Binance USD-M {operation} transport outcome unknown; reconciliation required: {error}"
    ))
}

async fn response_error(response: reqwest::Response, operation: &str) -> HftError {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.text().await.unwrap_or_default();
    let error = classify_http_error(status, retry_after.as_deref(), &body);
    if status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT {
        HftError::Network(format!(
            "Binance USD-M {operation} outcome unknown; reconciliation required: {error}"
        ))
    } else {
        error
    }
}

#[derive(Debug, Deserialize)]
struct ListenKeyResponse {
    #[serde(rename = "listenKey")]
    listen_key: String,
}

fn validate_listen_key(listen_key: &str) -> HftResult<()> {
    if listen_key.is_empty()
        || listen_key.len() > 256
        || !listen_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(HftError::Parse(
            "Binance USD-M listenKey has an invalid identity".to_string(),
        ));
    }
    Ok(())
}

type BinanceUsdMPrivateSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start_private_stream(
    http: &HttpClient,
    signer: &BinanceSigner,
    ws_base_url: &str,
) -> HftResult<(BinanceUsdMPrivateSocket, String)> {
    let response = http
        .signed_request(
            reqwest::Method::POST,
            REST_LISTEN_KEY_PATH,
            Some(signer.generate_headers()),
            None,
        )
        .await
        .map_err(|error| classify_transport_error("listen-key start", error))?;
    if !response.status().is_success() {
        return Err(response_error(response, "listen-key start").await);
    }
    let key: ListenKeyResponse = HttpClient::parse_json(response)
        .await
        .map_err(|error| HftError::Serialization(format!("Binance USD-M listenKey: {error}")))?;
    validate_listen_key(&key.listen_key)?;
    let url = format!("{}/{}", ws_base_url.trim_end_matches('/'), key.listen_key);
    let (socket, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|error| classify_transport_error("private stream connect", error))?;
    integration::ws::set_ws_tcp_nodelay(socket.get_ref(), true)
        .map_err(|error| classify_transport_error("private stream TCP_NODELAY", error))?;
    Ok((socket, key.listen_key))
}

async fn keepalive_private_stream(
    http: &HttpClient,
    signer: &BinanceSigner,
    listen_key: &str,
) -> HftResult<()> {
    let path = format!("{REST_LISTEN_KEY_PATH}?listenKey={listen_key}");
    let response = http
        .signed_request(
            reqwest::Method::PUT,
            &path,
            Some(signer.generate_headers()),
            None,
        )
        .await
        .map_err(|error| classify_transport_error("listen-key keepalive", error))?;
    if !response.status().is_success() {
        return Err(response_error(response, "listen-key keepalive").await);
    }
    Ok(())
}

fn emit_reconciliation(tx: &broadcast::Sender<ExecutionEvent>, reason: impl Into<String>) {
    let _ = tx.send(ExecutionEvent::ReconciliationRequired {
        reason: reason.into(),
        timestamp: hft_core::now_micros(),
    });
}

fn parse_order_identity(order: &serde_json::Map<String, Value>) -> Option<OrderId> {
    let symbol = order
        .get("s")
        .and_then(Value::as_str)
        .filter(|symbol| !symbol.is_empty() && symbol.trim() == *symbol)?;
    order
        .get("i")
        .and_then(Value::as_u64)
        .filter(|order_id| *order_id > 0)
        .map(|order_id| canonical_standard_order_id(symbol, order_id))
}

fn parse_order_report_events(
    value: &Value,
    received_mono_us: u64,
) -> HftResult<Vec<ExecutionEvent>> {
    let order = value.get("o").and_then(Value::as_object).ok_or_else(|| {
        HftError::Parse("Binance USD-M order event omitted o payload".to_string())
    })?;
    let order_id = parse_order_identity(order).ok_or_else(|| {
        HftError::Parse("Binance USD-M order event omitted order identity".to_string())
    })?;
    let event_ms = value
        .get("E")
        .and_then(Value::as_u64)
        .or_else(|| order.get("T").and_then(Value::as_u64))
        .ok_or_else(|| {
            HftError::Parse("Binance USD-M order event omitted event time".to_string())
        })?;
    if event_ms == 0 {
        return Err(HftError::Parse(
            "Binance USD-M order event has a zero event time".to_string(),
        ));
    }
    let timestamp = timestamp_us(event_ms, "order event")?;
    let status = order.get("X").and_then(Value::as_str).unwrap_or_default();
    let execution_type = order.get("x").and_then(Value::as_str).unwrap_or_default();
    let timing_kind = if execution_type == "NEW" {
        PrivateOrderEventKind::Ack
    } else {
        PrivateOrderEventKind::Report
    };
    let timing = ExecutionEvent::PrivateOrderTiming {
        order_id: order_id.clone(),
        kind: timing_kind,
        received_mono_us,
    };

    let mut events = vec![timing];
    match (execution_type, status) {
        ("NEW", "NEW") => events.push(ExecutionEvent::OrderAck {
            order_id,
            timestamp,
        }),
        (_, "CANCELED" | "EXPIRED" | "EXPIRED_IN_MATCH") => {
            events.push(ExecutionEvent::OrderCanceled { order_id, timestamp })
        }
        (_, "REJECTED") => {
            let reason = order
                .get("r")
                .and_then(Value::as_str)
                .filter(|reason| !reason.is_empty() && *reason != "NONE")
                .unwrap_or("Exchange rejected")
                .to_string();
            events.push(ExecutionEvent::OrderReject {
                order_id,
                reason,
                timestamp,
            });
        }
        ("TRADE", "PARTIALLY_FILLED" | "FILLED") => {
            let quantity = order
                .get("l")
                .and_then(Value::as_str)
                .ok_or_else(|| HftError::Parse("Binance USD-M fill omitted last quantity".to_string()))
                .and_then(|value| parse_decimal(value, "fill last quantity"))?;
            let price = order
                .get("L")
                .and_then(Value::as_str)
                .ok_or_else(|| HftError::Parse("Binance USD-M fill omitted last price".to_string()))
                .and_then(|value| parse_decimal(value, "fill last price"))?;
            let trade_id = order
                .get("t")
                .and_then(Value::as_i64)
                .filter(|trade_id| *trade_id >= 0)
                .ok_or_else(|| HftError::Parse("Binance USD-M fill omitted trade id".to_string()))?;
            if quantity <= Decimal::ZERO || price <= Decimal::ZERO {
                return Err(HftError::Parse(
                    "Binance USD-M fill has non-positive price or quantity".to_string(),
                ));
            }
            let fill_id = format!("BNUSDMFILL-{}-{trade_id}", order_id.0);
            events.push(ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price: Price(price),
                quantity: Quantity(quantity),
                timestamp,
                fill_id: fill_id.clone(),
            });
            if let Some(commission) = order.get("n").and_then(Value::as_str) {
                let commission = parse_decimal(commission, "fill commission")?;
                let commission_asset = order.get("N").and_then(Value::as_str).unwrap_or_default();
                if commission < Decimal::ZERO {
                    return Err(HftError::Parse(
                        "Binance USD-M fill commission is negative".to_string(),
                    ));
                }
                if commission > Decimal::ZERO
                    && matches!(commission_asset, "USDT" | "USDC" | "BUSD" | "FDUSD")
                {
                    events.push(ExecutionEvent::FeeCharged {
                        order_id: order_id.clone(),
                        amount: commission,
                        timestamp,
                        fill_id: fill_id.clone(),
                    });
                }
            }
            if status == "FILLED" {
                let total_filled = order
                    .get("z")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        HftError::Parse("Binance USD-M filled order omitted cumulative quantity".to_string())
                    })
                    .and_then(|value| parse_decimal(value, "filled cumulative quantity"))?;
                let final_price = match order.get("ap").and_then(Value::as_str) {
                    Some(value) => parse_decimal(value, "average fill price")?,
                    None => price,
                };
                if total_filled <= Decimal::ZERO || final_price <= Decimal::ZERO {
                    return Err(HftError::Parse(
                        "Binance USD-M filled order has invalid terminal quantities".to_string(),
                    ));
                }
                events.push(ExecutionEvent::OrderCompleted {
                    order_id,
                    final_price: Price(final_price),
                    total_filled: Quantity(total_filled),
                    timestamp,
                });
            }
        }
        ("AMENDMENT", "NEW" | "PARTIALLY_FILLED") => {
            let new_quantity = order
                .get("q")
                .and_then(Value::as_str)
                .ok_or_else(|| HftError::Parse("Binance USD-M amendment omitted quantity".to_string()))
                .and_then(|value| parse_decimal(value, "amended quantity"))?;
            if new_quantity <= Decimal::ZERO {
                return Err(HftError::Parse(
                    "Binance USD-M amendment has non-positive quantity".to_string(),
                ));
            }
            let new_price = order
                .get("p")
                .and_then(Value::as_str)
                .and_then(|value| parse_decimal(value, "amended price").ok())
                .filter(|value| *value > Decimal::ZERO)
                .map(Price);
            events.push(ExecutionEvent::OrderModified {
                order_id,
                new_quantity: Some(Quantity(new_quantity)),
                new_price,
                timestamp,
            });
        }
        (_, "NEW") if execution_type == "CALCULATED" => {
            // Liquidation/ADL reports are not a local order acknowledgement.  Force the
            // account reconciliation path to establish the resulting position truth.
            events.push(ExecutionEvent::ReconciliationRequired {
                reason: "Binance USD-M liquidation or ADL report requires account reconciliation"
                    .to_string(),
                timestamp,
            });
        }
        _ => {
            return Err(HftError::Parse(format!(
                "Binance USD-M order event has unsupported execution/status pair {execution_type}/{status}"
            )))
        }
    }
    Ok(events)
}

fn parse_account_update_events(value: &Value) -> HftResult<Vec<ExecutionEvent>> {
    let event_ms = value
        .get("E")
        .and_then(Value::as_u64)
        .or_else(|| value.get("T").and_then(Value::as_u64))
        .ok_or_else(|| {
            HftError::Parse("Binance USD-M account event omitted event time".to_string())
        })?;
    if event_ms == 0 {
        return Err(HftError::Parse(
            "Binance USD-M account event has a zero event time".to_string(),
        ));
    }
    let timestamp = timestamp_us(event_ms, "account event")?;
    let account = value.get("a").and_then(Value::as_object).ok_or_else(|| {
        HftError::Parse("Binance USD-M account event omitted a payload".to_string())
    })?;
    let balances = account.get("B").and_then(Value::as_array).ok_or_else(|| {
        HftError::Parse("Binance USD-M account event omitted B balances".to_string())
    })?;
    let mut seen_assets = HashSet::new();
    let mut events = Vec::with_capacity(balances.len());
    for balance in balances {
        let asset = balance
            .get("a")
            .and_then(Value::as_str)
            .filter(|asset| !asset.is_empty() && asset.trim() == *asset)
            .ok_or_else(|| {
                HftError::Parse("Binance USD-M account event has invalid asset".to_string())
            })?;
        if !seen_assets.insert(asset.to_string()) {
            return Err(HftError::Parse(format!(
                "Binance USD-M account event repeated asset {asset}"
            )));
        }
        let wallet_balance = balance
            .get("wb")
            .and_then(Value::as_str)
            .ok_or_else(|| HftError::Parse(format!("Binance USD-M {asset} update omitted wb")))
            .and_then(|value| parse_decimal(value, &format!("{asset} wallet balance")))?;
        if wallet_balance < Decimal::ZERO {
            return Err(HftError::Parse(format!(
                "Binance USD-M {asset} wallet balance is negative"
            )));
        }
        events.push(ExecutionEvent::BalanceUpdate {
            asset: asset.to_string(),
            balance: Quantity(wallet_balance),
            timestamp,
        });
    }
    Ok(events)
}

#[allow(clippy::too_many_arguments)]
async fn run_private_stream(
    http: HttpClient,
    signer: BinanceSigner,
    ws_base_url: String,
    event_tx: broadcast::Sender<ExecutionEvent>,
    private_stream_connected: Arc<AtomicBool>,
    mut shutdown: watch::Receiver<bool>,
    mut socket: BinanceUsdMPrivateSocket,
    mut listen_key: String,
) {
    let mut backoff = Duration::from_millis(100);
    loop {
        let mut keepalive = tokio::time::interval(USER_STREAM_KEEPALIVE);
        // `interval` emits immediately; consume that tick so the first keepalive is 30 minutes
        // after creation rather than an unnecessary second request during startup.
        keepalive.tick().await;
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        private_stream_connected.store(false, Ordering::Release);
                        return;
                    }
                }
                _ = keepalive.tick() => {
                    if let Err(error) = keepalive_private_stream(&http, &signer, &listen_key).await {
                        warn!(%error, "Binance USD-M private listenKey keepalive failed");
                        break;
                    }
                }
                message = socket.next() => match message {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        let received_mono_us = hft_core::monotonic_micros();
                        let value = match serde_json::from_str::<Value>(&text) {
                            Ok(value) => value,
                            Err(error) => {
                                emit_reconciliation(&event_tx, format!("Binance USD-M private event JSON invalid: {error}"));
                                break;
                            }
                        };
                        let event_type = value.get("e").and_then(Value::as_str).unwrap_or_default();
                        let parsed = match event_type {
                            "ORDER_TRADE_UPDATE" => parse_order_report_events(&value, received_mono_us),
                            "ACCOUNT_UPDATE" => parse_account_update_events(&value),
                            "listenKeyExpired" => Err(HftError::Execution(
                                "Binance USD-M private listenKey expired".to_string(),
                            )),
                            "MARGIN_CALL"
                            | "ACCOUNT_CONFIG_UPDATE"
                            | "ALGO_UPDATE"
                            | "CONDITIONAL_ORDER_TRIGGER_REJECT" => Err(HftError::Execution(
                                format!("Binance USD-M private event {event_type} requires reconciliation"),
                            )),
                            _ => Ok(Vec::new()),
                        };
                        match parsed {
                            Ok(events) => {
                                for event in events {
                                    let _ = event_tx.send(event);
                                }
                            }
                            Err(error) => {
                                emit_reconciliation(&event_tx, format!("Binance USD-M private event requires reconciliation: {error}"));
                                break;
                            }
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(payload))) => {
                        if socket.send(tokio_tungstenite::tungstenite::Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => {
                        break;
                    }
                    Some(Err(error)) => {
                        warn!(%error, "Binance USD-M private WS read failed");
                        break;
                    }
                    _ => {}
                }
            }
        }
        if *shutdown.borrow() {
            private_stream_connected.store(false, Ordering::Release);
            return;
        }
        private_stream_connected.store(false, Ordering::Release);
        let _ = event_tx.send(ExecutionEvent::ConnectionStatus {
            connected: false,
            timestamp: hft_core::now_micros(),
        });
        emit_reconciliation(
            &event_tx,
            "Binance USD-M private stream disconnected; open orders and positions require reconciliation",
        );
        loop {
            if *shutdown.borrow() {
                return;
            }
            match start_private_stream(&http, &signer, &ws_base_url).await {
                Ok((new_socket, new_listen_key)) => {
                    socket = new_socket;
                    listen_key = new_listen_key;
                    private_stream_connected.store(true, Ordering::Release);
                    backoff = Duration::from_millis(100);
                    let _ = event_tx.send(ExecutionEvent::ConnectionStatus {
                        connected: true,
                        timestamp: hft_core::now_micros(),
                    });
                    break;
                }
                Err(error) => {
                    warn!(%error, "Binance USD-M private WS reconnect failed");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                }
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct BinanceUsdMOrder {
    symbol: String,
    #[serde(default, rename = "orderId")]
    order_id: u64,
    #[serde(default, rename = "clientOrderId")]
    client_order_id: String,
    #[serde(default, rename = "algoId")]
    algo_id: Option<u64>,
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

#[derive(Debug)]
enum UsdMOpenOrderReadback {
    Standard {
        order: OpenOrder,
        record: UsdMOrderRecord,
    },
    Conditional(UsdMOrderRecord),
}

fn is_conditional_order_type(order_type: &str) -> bool {
    matches!(
        order_type,
        "STOP" | "STOP_MARKET" | "TAKE_PROFIT" | "TAKE_PROFIT_MARKET" | "TRAILING_STOP_MARKET"
    )
}

fn is_algo_order_type(order_type: &str) -> bool {
    is_conditional_order_type(order_type) || matches!(order_type, "LIMIT" | "MARKET")
}

fn parse_usdm_order_readback(order: BinanceUsdMOrder) -> HftResult<UsdMOpenOrderReadback> {
    if order.symbol.trim() != order.symbol || order.symbol.is_empty() {
        return Err(HftError::Parse(
            "Binance USD-M open order has an invalid symbol".to_string(),
        ));
    }
    if order.client_order_id.is_empty() && order.order_id == 0 {
        return Err(HftError::Parse(
            "Binance USD-M open order omitted both order identity fields".to_string(),
        ));
    }
    if !order.client_order_id.is_empty() {
        validate_client_order_id(&order.client_order_id)?;
    }
    if is_conditional_order_type(&order.r#type) {
        let client_order_id = if order.client_order_id.is_empty() {
            order.order_id.to_string()
        } else {
            order.client_order_id
        };
        let native_id = order
            .algo_id
            .map(|algo_id| algo_id.to_string())
            .filter(|value| value != "0")
            .or_else(|| (order.order_id > 0).then(|| order.order_id.to_string()))
            .unwrap_or_else(|| client_order_id.clone());
        let canonical_order_id = canonical_algo_order_id(&order.symbol, &native_id);
        return Ok(UsdMOpenOrderReadback::Conditional(UsdMOrderRecord {
            symbol: order.symbol,
            client_order_id,
            order_type: order.r#type,
            native_id,
            canonical_order_id: canonical_order_id.0,
            algo_id: order.algo_id,
            is_algo: true,
        }));
    }
    let symbol = order.symbol.clone();
    let client_order_id = order.client_order_id.clone();
    let native_id = order.order_id.to_string();
    let canonical_order_id = canonical_standard_order_id(&symbol, order.order_id);
    let parsed = parse_usdm_open_order(order)?;
    Ok(UsdMOpenOrderReadback::Standard {
        order: parsed,
        record: UsdMOrderRecord {
            symbol,
            client_order_id,
            order_type: "STANDARD".to_string(),
            native_id,
            canonical_order_id: canonical_order_id.0,
            algo_id: None,
            is_algo: false,
        },
    })
}

#[derive(Debug, Deserialize)]
struct BinanceUsdMAlgoOrder {
    #[serde(rename = "algoId")]
    algo_id: u64,
    #[serde(rename = "clientAlgoId")]
    client_algo_id: String,
    #[serde(rename = "orderType")]
    order_type: String,
    symbol: String,
}

fn parse_usdm_algo_order_record(order: BinanceUsdMAlgoOrder) -> HftResult<UsdMOrderRecord> {
    if order.algo_id == 0
        || order.symbol.is_empty()
        || order.symbol.trim() != order.symbol
        || !is_algo_order_type(&order.order_type)
    {
        return Err(HftError::Parse(
            "Binance USD-M algo order has an invalid identity, symbol, or order type".to_string(),
        ));
    }
    validate_client_order_id(&order.client_algo_id)?;
    let canonical_order_id = canonical_algo_order_id(&order.symbol, order.algo_id);
    Ok(UsdMOrderRecord {
        symbol: order.symbol,
        client_order_id: order.client_algo_id,
        order_type: order.order_type,
        native_id: order.algo_id.to_string(),
        canonical_order_id: canonical_order_id.0,
        algo_id: Some(order.algo_id),
        is_algo: true,
    })
}

fn parse_usdm_open_order(order: BinanceUsdMOrder) -> HftResult<OpenOrder> {
    if order.order_id == 0 || order.symbol.trim() != order.symbol || order.symbol.is_empty() {
        return Err(HftError::Parse(
            "Binance USD-M open order is missing a valid order identity or symbol".to_string(),
        ));
    }
    validate_client_order_id(&order.client_order_id)?;
    let side = match order.side.as_str() {
        "BUY" => Side::Buy,
        "SELL" => Side::Sell,
        value => {
            return Err(HftError::Parse(format!(
                "Binance USD-M open order {} has unknown side {value}",
                order.order_id
            )))
        }
    };
    let order_type = match order.r#type.as_str() {
        "MARKET" => hft_core::OrderType::Market,
        "LIMIT" => hft_core::OrderType::Limit,
        value => {
            return Err(HftError::Parse(format!(
                "Binance USD-M open order {} has unsupported order type {value}",
                order.order_id
            )))
        }
    };
    let original = parse_decimal(&order.orig_qty, "open order origQty")?;
    let filled = parse_decimal(&order.executed_qty, "open order executedQty")?;
    if original <= Decimal::ZERO || filled < Decimal::ZERO || filled > original {
        return Err(HftError::Parse(format!(
            "Binance USD-M open order {} has inconsistent quantities",
            order.order_id
        )));
    }
    let price = match order_type {
        hft_core::OrderType::Market => None,
        hft_core::OrderType::Limit => Some(Price::from_str(&order.price).map_err(|error| {
            HftError::Parse(format!(
                "Binance USD-M open order {} has invalid price: {error}",
                order.order_id
            ))
        })?),
    };
    let status = match order.status.as_str() {
        "NEW" => OrderStatus::New,
        "PARTIALLY_FILLED" => OrderStatus::PartiallyFilled,
        "FILLED" => OrderStatus::Filled,
        "CANCELED" => OrderStatus::Canceled,
        "REJECTED" => OrderStatus::Rejected,
        "EXPIRED" | "EXPIRED_IN_MATCH" => OrderStatus::Expired,
        value => {
            return Err(HftError::Parse(format!(
                "Binance USD-M open order {} has unknown status {value}",
                order.order_id
            )))
        }
    };
    let created_at = timestamp_us(order.time, "open order creation")?;
    let updated_at = timestamp_us(order.update_time, "open order update")?;
    if created_at == 0 || updated_at < created_at {
        return Err(HftError::Parse(format!(
            "Binance USD-M open order {} has invalid timestamps",
            order.order_id
        )));
    }
    Ok(OpenOrder {
        order_id: canonical_standard_order_id(&order.symbol, order.order_id),
        client_order_id: Some(order.client_order_id),
        symbol: Symbol::from(order.symbol),
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

#[derive(Debug, Deserialize)]
struct BinanceUsdMPlaceResponse {
    symbol: String,
    #[serde(rename = "orderId")]
    order_id: u64,
    #[serde(rename = "clientOrderId")]
    client_order_id: String,
    #[serde(rename = "updateTime")]
    update_time: u64,
}

fn parse_usdm_place_response(
    placed: BinanceUsdMPlaceResponse,
    expected_symbol: &str,
    expected_client_order_id: &str,
) -> HftResult<OrderId> {
    if placed.order_id == 0
        || placed.symbol != expected_symbol
        || placed.client_order_id != expected_client_order_id
    {
        return Err(HftError::Execution(
            "Binance USD-M place response did not match the submitted order identity".to_string(),
        ));
    }
    if placed.update_time == 0 {
        return Err(HftError::Parse(
            "Binance USD-M place response has a zero updateTime".to_string(),
        ));
    }
    let _ = timestamp_us(placed.update_time, "place response")?;
    Ok(canonical_standard_order_id(&placed.symbol, placed.order_id))
}

#[derive(Debug, Deserialize)]
struct BinanceUsdMCancelResponse {
    symbol: String,
    #[serde(rename = "orderId")]
    order_id: u64,
    #[serde(rename = "clientOrderId")]
    client_order_id: String,
}

#[derive(Debug, Deserialize)]
struct BinanceUsdMCancelAlgoResponse {
    #[serde(default, rename = "algoId")]
    algo_id: Option<u64>,
    #[serde(default, rename = "clientAlgoId")]
    client_algo_id: Option<String>,
    #[serde(default)]
    symbol: Option<String>,
}

fn validate_usdm_cancel_response(
    canceled: &BinanceUsdMCancelResponse,
    requested_order_id: &OrderId,
    record: &UsdMOrderRecord,
) -> HftResult<()> {
    let canonical_identity = canonical_standard_order_id(&canceled.symbol, canceled.order_id);
    let identity_matches = record.canonical_order_id == requested_order_id.0
        && canonical_identity.0 == requested_order_id.0;
    if canceled.symbol != record.symbol
        || canceled.client_order_id != record.client_order_id
        || !identity_matches
        || canceled.order_id == 0
    {
        return Err(HftError::Execution(
            "Binance USD-M cancel response did not match the requested order".to_string(),
        ));
    }
    Ok(())
}

fn validate_usdm_cancel_algo_response(
    canceled: &BinanceUsdMCancelAlgoResponse,
    requested_order_id: &OrderId,
    record: &UsdMOrderRecord,
) -> HftResult<()> {
    if requested_order_id.0 != record.canonical_order_id {
        return Err(HftError::Execution(
            "Binance USD-M algo cancel request identity did not match the canonical order"
                .to_string(),
        ));
    }
    let mut saw_identity = false;
    if let Some(algo_id) = canceled.algo_id {
        saw_identity = true;
        if record.algo_id != Some(algo_id) {
            return Err(HftError::Execution(
                "Binance USD-M algo cancel response had a mismatched algoId".to_string(),
            ));
        }
    }
    if let Some(client_id) = canceled.client_algo_id.as_deref() {
        saw_identity = true;
        if client_id != record.client_order_id {
            return Err(HftError::Execution(
                "Binance USD-M algo cancel response had a mismatched clientAlgoId".to_string(),
            ));
        }
    }
    if !saw_identity
        || canceled
            .symbol
            .as_deref()
            .is_some_and(|symbol| symbol != record.symbol.as_str())
    {
        return Err(HftError::Execution(
            "Binance USD-M algo cancel response did not match the requested order".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct BinanceUsdMBalance {
    asset: String,
    #[serde(rename = "walletBalance")]
    wallet_balance: String,
    #[serde(rename = "availableBalance")]
    available_balance: String,
    #[serde(default, rename = "marginBalance")]
    margin_balance: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BinanceUsdMPosition {
    symbol: String,
    #[serde(rename = "positionAmt")]
    position_amt: String,
    #[serde(rename = "entryPrice")]
    entry_price: String,
    #[serde(rename = "unrealizedProfit", alias = "unRealizedProfit")]
    unrealized_profit: String,
    #[serde(rename = "positionSide")]
    position_side: String,
}

#[derive(Debug, Deserialize)]
struct BinanceUsdMAccountResponse {
    #[serde(default)]
    assets: Vec<BinanceUsdMBalance>,
    positions: Vec<BinanceUsdMPosition>,
}

fn parse_usdm_balances(account: BinanceUsdMAccountResponse) -> HftResult<Vec<AccountBalance>> {
    if account.assets.is_empty() {
        return Err(HftError::Parse(
            "Binance USD-M account response contains no balance assets".to_string(),
        ));
    }
    let mut seen_assets = HashSet::new();
    account
        .assets
        .into_iter()
        .map(|asset| {
            if asset.asset.is_empty()
                || asset.asset.trim() != asset.asset
                || !seen_assets.insert(asset.asset.clone())
            {
                return Err(HftError::Parse(
                    "Binance USD-M account response contains an empty or duplicate asset"
                        .to_string(),
                ));
            }
            let wallet_balance = parse_decimal(
                &asset.wallet_balance,
                &format!("{} walletBalance", asset.asset),
            )?;
            let total = asset
                .margin_balance
                .as_deref()
                .map(|value| parse_decimal(value, &format!("{} marginBalance", asset.asset)))
                .transpose()?
                .unwrap_or(wallet_balance);
            let available = parse_decimal(
                &asset.available_balance,
                &format!("{} availableBalance", asset.asset),
            )?;
            if wallet_balance < Decimal::ZERO
                || total < Decimal::ZERO
                || available < Decimal::ZERO
                || available > total
            {
                return Err(HftError::Parse(format!(
                    "Binance USD-M {} has inconsistent wallet and available balances",
                    asset.asset
                )));
            }
            let frozen = total - available;
            let usd_value = matches!(
                asset.asset.as_str(),
                "USD" | "USDT" | "USDC" | "BUSD" | "FDUSD" | "TUSD" | "DAI"
            )
            .then_some(total);
            Ok(AccountBalance {
                asset: asset.asset,
                available,
                frozen,
                total,
                usd_value,
            })
        })
        .collect()
}

fn parse_usdm_positions(account: BinanceUsdMAccountResponse) -> HftResult<Vec<PortPosition>> {
    let mut seen_symbols = HashSet::new();
    account
        .positions
        .into_iter()
        .filter_map(|position| {
            if position.symbol.is_empty() || position.symbol.trim() != position.symbol {
                return Some(Err(HftError::Parse(
                    "Binance USD-M position has an invalid symbol".to_string(),
                )));
            }
            if !seen_symbols.insert(position.symbol.clone()) {
                return Some(Err(HftError::Parse(format!(
                    "Binance USD-M account response repeated position {}",
                    position.symbol
                ))));
            }
            if position.position_side != "BOTH" {
                return Some(Err(HftError::Config(
                    "Binance USD-M hedge-mode positions are unsupported; configure one-way/BOTH mode"
                        .to_string(),
                )));
            }
            let amount = match parse_decimal(&position.position_amt, "positionAmt") {
                Ok(amount) => amount,
                Err(error) => return Some(Err(error)),
            };
            if amount == Decimal::ZERO {
                return None;
            }
            let entry_price = match Price::from_str(&position.entry_price) {
                Ok(price) => price,
                Err(error) => {
                    return Some(Err(HftError::Parse(format!(
                        "Binance USD-M {} entryPrice: {error}",
                        position.symbol
                    ))))
                }
            };
            let unrealized_pnl = match parse_decimal(&position.unrealized_profit, "unrealizedProfit") {
                Ok(value) => value,
                Err(error) => return Some(Err(error)),
            };
            Some(Ok(PortPosition {
                symbol: Symbol::from(position.symbol),
                quantity: Quantity(amount),
                avg_price: entry_price,
                unrealized_pnl,
                realized_pnl: Decimal::ZERO,
            }))
        })
        .collect()
}

impl BinanceUsdMExecutionClient {
    pub fn new(cfg: BinanceUsdMExecutionConfig) -> Self {
        let signer = has_private_credentials(&cfg.credentials)
            .then(|| BinanceSigner::new(cfg.credentials.clone()));
        Self {
            event_tx: None,
            connected: false,
            http_client: None,
            signer,
            rest_base_url: cfg.rest_base_url,
            ws_base_url: cfg.ws_base_url,
            mode: cfg.mode,
            timeout_ms: cfg.timeout_ms,
            order_records: std::sync::Mutex::new(HashMap::new()),
            listen_key: None,
            private_stream_connected: Arc::new(AtomicBool::new(false)),
            resilient_executor: None,
            alert_callback: None,
            next_client_order_id: None,
            next_reduce_only: None,
            shutdown_tx: None,
            private_event_receiver: std::sync::Mutex::new(None),
        }
    }

    pub fn with_alert_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(ExecutionAlert) + Send + Sync + 'static,
    {
        self.alert_callback = Some(Arc::new(callback));
        self
    }

    pub fn resilience_stats(&self) -> Option<ExecutorStats> {
        self.resilient_executor
            .as_ref()
            .map(|executor| executor.stats())
    }

    pub async fn circuit_state(&self) -> Option<CircuitState> {
        if let Some(executor) = &self.resilient_executor {
            Some(executor.circuit_breaker.state().await)
        } else {
            None
        }
    }

    pub async fn reset_circuit_breaker(&self) {
        if let Some(executor) = &self.resilient_executor {
            executor.circuit_breaker.reset().await;
        }
    }

    fn ensure_http(&mut self) -> HftResult<()> {
        if self.http_client.is_none() {
            self.http_client = Some(
                HttpClient::new(HttpClientConfig {
                    base_url: self.rest_base_url.clone(),
                    timeout_ms: self.timeout_ms,
                    user_agent: "hft-binance-usdm-exec/1.0".to_string(),
                })
                .map_err(|error| HftError::Network(error.to_string()))?,
            );
        }
        Ok(())
    }

    fn snapshot_http(&self) -> HftResult<HttpClient> {
        if let Some(http) = &self.http_client {
            return Ok(http.clone());
        }
        HttpClient::new(HttpClientConfig {
            base_url: self.rest_base_url.clone(),
            timeout_ms: self.timeout_ms,
            user_agent: "hft-binance-usdm-exec/1.0".to_string(),
        })
        .map_err(|error| HftError::Network(error.to_string()))
    }

    fn signer(&self, operation: &str) -> HftResult<&BinanceSigner> {
        self.signer.as_ref().ok_or_else(|| {
            HftError::Authentication(format!(
                "Binance USD-M {operation} requires API credentials"
            ))
        })
    }

    fn require_private_access(&self, operation: &str) -> HftResult<()> {
        if !uses_exchange_api(self.mode) {
            return Err(HftError::Config(format!(
                "Binance USD-M {operation} is not available in {} mode; use the canonical simulated execution client",
                mode_label(self.mode),
            )));
        }
        self.signer(operation).map(|_| ())
    }

    fn validate_intent(&self, intent: &ports::OrderIntent) -> HftResult<()> {
        if !matches!(
            intent.product_type,
            ProductType::Perp | ProductType::Futures
        ) {
            return Err(HftError::InvalidOrder(
                "Binance USD-M execution adapter accepts Perp/Futures intents only".to_string(),
            ));
        }
        if intent.asset_class != hft_core::AssetClass::Crypto {
            return Err(HftError::InvalidOrder(
                "Binance USD-M execution adapter accepts crypto intents only".to_string(),
            ));
        }
        if intent
            .target_venue
            .is_some_and(|venue| venue != hft_core::VenueId::BINANCE_FUTURES)
        {
            return Err(HftError::InvalidOrder(
                "Binance USD-M intent target venue must be BINANCE_FUTURES".to_string(),
            ));
        }
        if intent.symbol.as_str().is_empty()
            || intent.symbol.as_str().trim() != intent.symbol.as_str()
        {
            return Err(HftError::InvalidOrder(
                "Binance USD-M intent has an invalid symbol".to_string(),
            ));
        }
        if intent.quantity.0 <= Decimal::ZERO {
            return Err(HftError::InvalidOrder(
                "Binance USD-M intent quantity must be positive".to_string(),
            ));
        }
        match intent.order_type {
            hft_core::OrderType::Market if intent.price.is_some() => {
                return Err(HftError::InvalidOrder(
                    "Binance USD-M market orders cannot carry an exchange-enforced price"
                        .to_string(),
                ))
            }
            hft_core::OrderType::Limit => {
                if intent.price.is_none_or(|price| price.0 <= Decimal::ZERO) {
                    return Err(HftError::InvalidOrder(
                        "Binance USD-M limit orders require a positive price".to_string(),
                    ));
                }
            }
            hft_core::OrderType::Market => {}
        }
        if intent.order_type == hft_core::OrderType::Market
            && intent.time_in_force != hft_core::TimeInForce::GTC
        {
            return Err(HftError::InvalidOrder(
                "Binance USD-M market orders support only GTC time-in-force".to_string(),
            ));
        }
        Ok(())
    }

    fn next_client_order_id(&mut self) -> HftResult<String> {
        let id = self
            .next_client_order_id
            .take()
            .unwrap_or_else(|| format!("BINANCE_USDM_{:x}", hft_core::now_micros()));
        validate_client_order_id(&id)?;
        Ok(id)
    }

    fn remember_order(&self, order_id: &OrderId, mut record: UsdMOrderRecord) -> HftResult<()> {
        let mut records = self
            .order_records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        record.canonical_order_id = order_id.0.clone();
        records.retain(|_, existing| existing.canonical_order_id != order_id.0);
        records.insert(order_id.0.clone(), record);
        Ok(())
    }

    fn order_record(&self, order_id: &OrderId) -> Option<UsdMOrderRecord> {
        self.order_records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&order_id.0)
            .cloned()
    }

    fn forget_order(&self, order_id: &OrderId) {
        let mut records = self
            .order_records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        records.remove(&order_id.0);
    }

    fn signed_query(&self, mut params: HashMap<String, String>) -> HftResult<String> {
        Ok(self.signer("signed request")?.sign_request(&mut params))
    }

    async fn signed_json<T: for<'de> Deserialize<'de>>(
        &self,
        method: reqwest::Method,
        path: &str,
        operation: &str,
    ) -> HftResult<T> {
        let http = self.snapshot_http()?;
        let signer = self.signer(operation)?;
        let response = http
            .signed_request(method, path, Some(signer.generate_headers()), None)
            .await
            .map_err(|error| classify_transport_error(operation, error))?;
        if !response.status().is_success() {
            return Err(response_error(response, operation).await);
        }
        HttpClient::parse_json(response)
            .await
            .map_err(|error| HftError::Serialization(format!("Binance USD-M {operation}: {error}")))
    }
}

#[async_trait]
impl ExecutionClient for BinanceUsdMExecutionClient {
    async fn place_order(&mut self, intent: ports::OrderIntent) -> HftResult<OrderId> {
        self.validate_intent(&intent)?;
        self.require_private_access("place_order")?;
        let client_order_id = self.next_client_order_id()?;
        let reduce_only = self.next_reduce_only.take().unwrap_or(false);
        self.ensure_http()?;
        let mut params = HashMap::from([
            ("symbol".to_string(), intent.symbol.as_str().to_string()),
            (
                "side".to_string(),
                match intent.side {
                    Side::Buy => "BUY".to_string(),
                    Side::Sell => "SELL".to_string(),
                },
            ),
            (
                "type".to_string(),
                match intent.order_type {
                    hft_core::OrderType::Market => "MARKET".to_string(),
                    hft_core::OrderType::Limit => "LIMIT".to_string(),
                },
            ),
            ("quantity".to_string(), intent.quantity.0.to_string()),
            ("positionSide".to_string(), "BOTH".to_string()),
            ("newClientOrderId".to_string(), client_order_id.clone()),
            ("recvWindow".to_string(), DEFAULT_RECV_WINDOW.to_string()),
            ("newOrderRespType".to_string(), "ACK".to_string()),
        ]);
        if let Some(price) = intent.price {
            params.insert("price".to_string(), price.0.to_string());
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
        if reduce_only {
            params.insert("reduceOnly".to_string(), "true".to_string());
        }
        let path = format!("{REST_ORDER_PATH}?{}", self.signed_query(params)?);
        let signer = self.signer("place_order")?;
        let http = self.snapshot_http()?;
        // Order placement is intentionally a single request.  A timeout or HTTP 5xx leaves
        // the venue outcome unknown; automatically replaying the signed order could create a
        // duplicate position.  Reconciliation owns recovery of this boundary.
        let response = http
            .signed_request(
                reqwest::Method::POST,
                &path,
                Some(signer.generate_headers()),
                None,
            )
            .await
            .map_err(|error| classify_transport_error("place_order", error))?;
        if !response.status().is_success() {
            return Err(response_error(response, "place_order").await);
        }
        let placed: BinanceUsdMPlaceResponse =
            HttpClient::parse_json(response).await.map_err(|error| {
                HftError::Serialization(format!("Binance USD-M place_order: {error}"))
            })?;
        let order_id = parse_usdm_place_response(placed, intent.symbol.as_str(), &client_order_id)?;
        self.remember_order(
            &order_id,
            UsdMOrderRecord {
                symbol: intent.symbol.as_str().to_string(),
                client_order_id,
                order_type: match intent.order_type {
                    hft_core::OrderType::Market => "MARKET",
                    hft_core::OrderType::Limit => "LIMIT",
                }
                .to_string(),
                native_id: parse_native_standard_order_id(&order_id, intent.symbol.as_str())
                    .ok_or_else(|| {
                        HftError::Execution(
                            "Binance USD-M canonical order identity is malformed".to_string(),
                        )
                    })?,
                canonical_order_id: order_id.0.clone(),
                algo_id: None,
                is_algo: false,
            },
        )?;
        Ok(order_id)
    }

    async fn place_order_envelope(&mut self, envelope: &OrderIntentEnvelope) -> HftResult<OrderId> {
        envelope
            .validate_cex_pre_execution(hft_core::now_micros(), None)
            .map_err(|reason| {
                HftError::Execution(format!("execution envelope rejected: {reason:?}"))
            })?;
        self.next_client_order_id = Some(envelope.client_order_id.clone());
        self.next_reduce_only = Some(envelope.lifecycle.reduce_only);
        self.place_order(envelope.intent.clone()).await
    }

    async fn place_order_envelope_traced(
        &mut self,
        envelope: &OrderIntentEnvelope,
    ) -> ExecutionSubmissionAttempt {
        if let Err(reason) = envelope.validate_cex_pre_execution(hft_core::now_micros(), None) {
            return ExecutionSubmissionAttempt::without_transport_timing(Err(HftError::Execution(
                format!("execution envelope rejected: {reason:?}"),
            )));
        }
        self.next_client_order_id = Some(envelope.client_order_id.clone());
        self.next_reduce_only = Some(envelope.lifecycle.reduce_only);
        ExecutionSubmissionAttempt::without_transport_timing(
            self.place_order(envelope.intent.clone()).await,
        )
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        if !uses_exchange_api(self.mode) {
            return Err(HftError::Config(format!(
                "Binance USD-M cancel_order is not available in {} mode; use the canonical simulated execution client",
                mode_label(self.mode)
            )));
        }
        self.require_private_access("cancel_order")?;
        let record = self.order_record(order_id).ok_or_else(|| {
            HftError::OrderNotFound(format!(
                "Binance USD-M cancel requires known symbol metadata for order {}",
                order_id.0
            ))
        })?;
        self.ensure_http()?;
        let mut params = HashMap::from([
            ("symbol".to_string(), record.symbol.clone()),
            ("recvWindow".to_string(), DEFAULT_RECV_WINDOW.to_string()),
        ]);
        let path_prefix = if record.is_algo {
            if let Some(algo_id) = record.algo_id {
                params.insert("algoId".to_string(), algo_id.to_string());
            } else {
                params.insert("clientAlgoId".to_string(), record.client_order_id.clone());
            }
            REST_ALGO_ORDER_PATH
        } else {
            params.insert("orderId".to_string(), record.native_id.clone());
            REST_ORDER_PATH
        };
        let path = format!("{path_prefix}?{}", self.signed_query(params)?);
        let signer = self.signer("cancel_order")?;
        let http = self.snapshot_http()?;
        let response = http
            .signed_request(
                reqwest::Method::DELETE,
                &path,
                Some(signer.generate_headers()),
                None,
            )
            .await
            .map_err(|error| classify_transport_error("cancel_order", error))?;
        if !response.status().is_success() {
            return Err(response_error(response, "cancel_order").await);
        }
        if record.is_algo {
            let canceled: BinanceUsdMCancelAlgoResponse =
                HttpClient::parse_json(response).await.map_err(|error| {
                    HftError::Serialization(format!("Binance USD-M algo cancel_order: {error}"))
                })?;
            validate_usdm_cancel_algo_response(&canceled, order_id, &record)?;
        } else {
            let canceled: BinanceUsdMCancelResponse =
                HttpClient::parse_json(response).await.map_err(|error| {
                    HftError::Serialization(format!("Binance USD-M cancel_order: {error}"))
                })?;
            validate_usdm_cancel_response(&canceled, order_id, &record)?;
        }
        self.forget_order(order_id);
        if let Some(tx) = &self.event_tx {
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
            return Err(HftError::Config(format!(
                "Binance USD-M live modify is disabled for order {}; use an explicit cancel-then-new intent",
                order_id.0
            )));
        }
        Err(HftError::Config(format!(
            "Binance USD-M modify_order is not available in {} mode; use the canonical simulated execution client",
            mode_label(self.mode)
        )))
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        if let Some(tx) = &self.event_tx {
            let receiver = self
                .private_event_receiver
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .unwrap_or_else(|| tx.subscribe());
            let stream = tokio_stream::wrappers::BroadcastStream::new(receiver).filter_map(
                |result| async move {
                    match result {
                        Ok(event) => Some(Ok(event)),
                        Err(error) => Some(Ok(ExecutionEvent::ReconciliationRequired {
                            reason: format!(
                                "Binance USD-M private execution stream lagged; restart/reconciliation required: {error}"
                            ),
                            timestamp: hft_core::now_micros(),
                        })),
                    }
                },
            );
            Ok(Box::pin(stream))
        } else {
            Ok(Box::pin(stream::empty()))
        }
    }

    async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
        self.require_private_access("get_balance")?;
        let params = HashMap::from([("recvWindow".to_string(), DEFAULT_RECV_WINDOW.to_string())]);
        let path = format!("{REST_ACCOUNT_PATH}?{}", self.signed_query(params)?);
        let account: BinanceUsdMAccountResponse = self
            .signed_json(reqwest::Method::GET, &path, "get_balance")
            .await?;
        parse_usdm_balances(account)
    }

    fn asset_inventory_capability(&self) -> AssetInventoryCapability {
        AssetInventoryCapability::PositionSnapshotRequired
    }

    fn supports_position_snapshot(&self) -> bool {
        true
    }

    async fn get_positions(&self) -> HftResult<Vec<PortPosition>> {
        self.require_private_access("get_positions")?;
        let params = HashMap::from([("recvWindow".to_string(), DEFAULT_RECV_WINDOW.to_string())]);
        let path = format!("{REST_ACCOUNT_PATH}?{}", self.signed_query(params)?);
        let account: BinanceUsdMAccountResponse = self
            .signed_json(reqwest::Method::GET, &path, "get_positions")
            .await?;
        parse_usdm_positions(account)
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        self.require_private_access("list_open_orders")?;
        let params = HashMap::from([("recvWindow".to_string(), DEFAULT_RECV_WINDOW.to_string())]);
        let path = format!("{REST_OPEN_ORDERS_PATH}?{}", self.signed_query(params)?);
        let orders: Vec<BinanceUsdMOrder> = self
            .signed_json(reqwest::Method::GET, &path, "list_open_orders")
            .await?;
        let mut parsed = Vec::new();
        for order in orders {
            match parse_usdm_order_readback(order)? {
                UsdMOpenOrderReadback::Standard { order, record } => {
                    self.remember_order(&order.order_id, record)?;
                    parsed.push(order);
                }
                UsdMOpenOrderReadback::Conditional(record) => {
                    let identity = OrderId(record.canonical_order_id.clone());
                    self.remember_order(&identity, record)?;
                }
            }
        }
        Ok(parsed)
    }

    async fn list_cancellable_orders(&self) -> HftResult<Vec<CancellableOrderRef>> {
        self.require_private_access("list_cancellable_orders")?;
        let params = HashMap::from([("recvWindow".to_string(), DEFAULT_RECV_WINDOW.to_string())]);
        let path = format!(
            "{REST_OPEN_ALGO_ORDERS_PATH}?{}",
            self.signed_query(params)?
        );
        let algo_orders: Vec<BinanceUsdMAlgoOrder> = self
            .signed_json(reqwest::Method::GET, &path, "list_cancellable_orders")
            .await?;
        for algo_order in algo_orders {
            let record = parse_usdm_algo_order_record(algo_order)?;
            let identity = OrderId(record.canonical_order_id.clone());
            self.remember_order(&identity, record)?;
        }

        let mut seen = HashSet::new();
        let records = self
            .order_records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(records
            .values()
            .filter(|record| record.is_algo)
            .filter_map(|record| {
                let order_id = OrderId(record.canonical_order_id.clone());
                seen.insert(order_id.0.clone())
                    .then(|| CancellableOrderRef {
                        order_id,
                        client_order_id: Some(record.client_order_id.clone()),
                        symbol: Symbol::from(record.symbol.clone()),
                        order_type: record.order_type.clone(),
                        algo_id: record.algo_id,
                    })
            })
            .collect())
    }

    async fn connect(&mut self) -> HftResult<()> {
        if !uses_exchange_api(self.mode) {
            return Err(HftError::Config(
                "Binance USD-M Paper connect is disabled; use the canonical simulated execution client"
                    .to_string(),
            ));
        }
        if uses_exchange_api(self.mode) && self.signer.is_none() {
            return Err(HftError::Authentication(
                "Binance USD-M Live/Testnet connect requires API credentials".to_string(),
            ));
        }
        let retry_config = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 100,
            max_delay_ms: 5000,
            backoff_multiplier: 2.0,
            retry_on_init_error: false,
        };
        let cb_config = CircuitBreakerConfig {
            failure_threshold: 5,
            open_duration_secs: 30,
            half_open_max_requests: 3,
            half_open_success_threshold: 2,
        };
        let mut executor = ResilientExecutor::new("binance-usdm", retry_config, cb_config);
        if let Some(alert_callback) = &self.alert_callback {
            let alert_callback = Arc::clone(alert_callback);
            executor = executor.with_alert_callback(move |cb_alert| {
                let alert_type = match cb_alert.state {
                    CircuitState::Open => ExecutionAlertType::CircuitOpen,
                    CircuitState::Closed => ExecutionAlertType::CircuitRecovered,
                    CircuitState::HalfOpen => return,
                };
                alert_callback(
                    ExecutionAlert::new(alert_type, "binance-usdm", "execution", &cb_alert.message)
                        .with_failure_count(cb_alert.failure_count),
                );
            });
        }
        self.resilient_executor = Some(Arc::new(executor));
        let (event_tx, event_rx) = broadcast::channel(1000);
        self.event_tx = Some(event_tx.clone());
        *self
            .private_event_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(event_rx);
        self.ensure_http()?;

        if uses_exchange_api(self.mode) {
            let http = self.snapshot_http()?;
            let signer = self.signer("connect")?.clone();
            let (socket, listen_key) =
                start_private_stream(&http, &signer, &self.ws_base_url).await?;
            self.listen_key = Some(listen_key.clone());
            self.private_stream_connected.store(true, Ordering::Release);
            self.connected = true;
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            self.shutdown_tx = Some(shutdown_tx);
            tokio::spawn(run_private_stream(
                http,
                signer,
                self.ws_base_url.clone(),
                event_tx,
                Arc::clone(&self.private_stream_connected),
                shutdown_rx,
                socket,
                listen_key,
            ));
        }
        info!(
            mode = mode_label(self.mode),
            "Binance USD-M execution client connected"
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        if let Some(shutdown) = self.shutdown_tx.take() {
            let _ = shutdown.send(true);
        }
        self.private_stream_connected
            .store(false, Ordering::Release);
        self.listen_key = None;
        self.event_tx = None;
        self.private_event_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.connected = false;
        Ok(())
    }

    async fn health(&self) -> ports::ConnectionHealth {
        ports::ConnectionHealth {
            connected: self.connected
                && (!uses_exchange_api(self.mode)
                    || self.private_stream_connected.load(Ordering::Acquire)),
            latency_ms: Some(1.0),
            last_heartbeat: hft_core::now_micros(),
        }
    }
}

// Compatibility spelling for callers that use the lowercase market acronym.
pub type BinanceUsdmExecutionClient = BinanceUsdMExecutionClient;
pub type BinanceUsdmExecutionConfig = BinanceUsdMExecutionConfig;

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use integration::signing::BinanceCredentials;
    use ports::{ExecutionClient, OrderIntentLifecycle};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn config(mode: ExecutionMode) -> BinanceUsdMExecutionConfig {
        BinanceUsdMExecutionConfig {
            credentials: BinanceCredentials::new(String::new(), String::new()),
            rest_base_url: "https://fapi.binance.com".to_string(),
            ws_base_url: "wss://fstream.binance.com/private/ws".to_string(),
            timeout_ms: 5_000,
            mode,
            account_capability: AccountCapability::default(),
        }
    }

    fn perp_intent() -> ports::OrderIntent {
        ports::OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            asset_class: hft_core::AssetClass::Crypto,
            product_type: ProductType::Perp,
            compliance_context: Default::default(),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.001).unwrap(),
            order_type: hft_core::OrderType::Limit,
            price: Some(Price::from_f64(50_000.0).unwrap()),
            time_in_force: hft_core::TimeInForce::GTC,
            strategy_id: "test".to_string(),
            target_venue: Some(hft_core::VenueId::BINANCE_FUTURES),
        }
    }

    fn valid_order() -> BinanceUsdMOrder {
        BinanceUsdMOrder {
            symbol: "BTCUSDT".to_string(),
            order_id: 42,
            client_order_id: "client-42".to_string(),
            algo_id: None,
            price: "50000".to_string(),
            orig_qty: "1".to_string(),
            executed_qty: "0.25".to_string(),
            status: "PARTIALLY_FILLED".to_string(),
            time: 1,
            update_time: 2,
            side: "BUY".to_string(),
            r#type: "LIMIT".to_string(),
        }
    }

    async fn rest_server(
        responses: Vec<(&'static str, String)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 32 * 1024];
                let size = socket.read(&mut request).await.unwrap();
                requests.push(String::from_utf8_lossy(&request[..size]).to_string());
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (format!("http://{address}"), task)
    }

    #[test]
    fn usd_m_open_order_preserves_numeric_and_client_identity() {
        let order = parse_usdm_open_order(valid_order()).unwrap();
        assert_eq!(
            order.order_id,
            OrderId("BNUSDM:standard:BTCUSDT:42".to_string())
        );
        assert_eq!(order.client_order_id.as_deref(), Some("client-42"));
        assert_eq!(order.filled_quantity, Quantity::from_f64(0.25).unwrap());
        assert_eq!(order.remaining_quantity, Quantity::from_f64(0.75).unwrap());
        assert_eq!(order.status, OrderStatus::PartiallyFilled);
    }

    #[test]
    fn usd_m_private_reports_emit_partial_fill_and_terminal_completion() {
        let partial = serde_json::json!({
            "e":"ORDER_TRADE_UPDATE", "E":1000,
            "o":{"s":"BTCUSDT","c":"client-42","i":42,"x":"TRADE","X":"PARTIALLY_FILLED","l":"0.25","L":"50000","z":"0.25","ap":"50000","t":7,"n":"0.01","N":"USDT"}
        });
        let events = parse_order_report_events(&partial, 99).unwrap();
        assert!(events.iter().any(|event| matches!(event, ExecutionEvent::Fill { order_id, fill_id, .. } if order_id == &OrderId("BNUSDM:standard:BTCUSDT:42".into()) && fill_id == "BNUSDMFILL-BNUSDM:standard:BTCUSDT:42-7")));
        assert!(events.iter().any(|event| matches!(event, ExecutionEvent::FeeCharged { order_id, amount, fill_id, .. } if order_id == &OrderId("BNUSDM:standard:BTCUSDT:42".into()) && *amount == Decimal::new(1, 2) && fill_id == "BNUSDMFILL-BNUSDM:standard:BTCUSDT:42-7")));
        assert!(!events
            .iter()
            .any(|event| matches!(event, ExecutionEvent::OrderCompleted { .. })));

        let filled = serde_json::json!({
            "e":"ORDER_TRADE_UPDATE", "E":1001,
            "o":{"s":"BTCUSDT","c":"client-42","i":42,"x":"TRADE","X":"FILLED","l":"0.75","L":"50100","z":"1","ap":"50075","t":8}
        });
        let events = parse_order_report_events(&filled, 100).unwrap();
        assert!(events.iter().any(|event| matches!(event, ExecutionEvent::OrderCompleted { order_id, final_price, total_filled, .. } if order_id == &OrderId("BNUSDM:standard:BTCUSDT:42".into()) && *final_price == Price::from_f64(50075.0).unwrap() && *total_filled == Quantity::from_f64(1.0).unwrap())));
    }

    #[test]
    fn usd_m_private_reports_map_cancel_and_reject_to_terminal_events() {
        let canceled = serde_json::json!({
            "e":"ORDER_TRADE_UPDATE", "E":1000,
            "o":{"s":"BTCUSDT","c":"client-42","i":42,"x":"CANCELED","X":"CANCELED"}
        });
        let events = parse_order_report_events(&canceled, 1).unwrap();
        assert!(events.iter().any(|event| matches!(event, ExecutionEvent::OrderCanceled { order_id, .. } if order_id == &OrderId("BNUSDM:standard:BTCUSDT:42".into()))));

        let rejected = serde_json::json!({
            "e":"ORDER_TRADE_UPDATE", "E":1000,
            "o":{"s":"BTCUSDT","c":"client-42","i":42,"x":"NEW","X":"REJECTED","r":"MARGIN_NOT_SUFFICIENT"}
        });
        let events = parse_order_report_events(&rejected, 1).unwrap();
        assert!(events.iter().any(|event| matches!(event, ExecutionEvent::OrderReject { order_id, reason, .. } if order_id == &OrderId("BNUSDM:standard:BTCUSDT:42".into()) && reason == "MARGIN_NOT_SUFFICIENT")));
    }

    #[tokio::test]
    async fn usd_m_rest_submission_is_signed_and_cancel_uses_the_authoritative_order_identity() {
        let (base_url, server) = rest_server(vec![
            (
                "200 OK",
                r#"{"symbol":"BTCUSDT","orderId":9001,"clientOrderId":"client-usdm","updateTime":123}"#.to_string(),
            ),
            (
                "200 OK",
                r#"{"symbol":"BTCUSDT","orderId":9001,"clientOrderId":"client-usdm"}"#.to_string(),
            ),
        ])
        .await;
        let mut cfg = config(ExecutionMode::Testnet);
        cfg.credentials =
            BinanceCredentials::new("test-key".to_string(), "test-secret".to_string());
        cfg.rest_base_url = base_url;
        let mut client = BinanceUsdMExecutionClient::new(cfg);
        let lifecycle = OrderIntentLifecycle {
            reduce_only: true,
            ..Default::default()
        };
        let envelope =
            OrderIntentEnvelope::new(perp_intent(), lifecycle).with_client_order_id("client-usdm");

        let order_id = client.place_order_envelope(&envelope).await.unwrap();
        assert_eq!(
            order_id,
            OrderId("BNUSDM:standard:BTCUSDT:9001".to_string())
        );
        client.cancel_order(&order_id).await.unwrap();
        let requests = server.await.unwrap();

        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("POST /fapi/v1/order?"));
        assert!(requests[0].contains("newClientOrderId=client-usdm"));
        assert!(requests[0].contains("positionSide=BOTH"));
        assert!(requests[0].contains("reduceOnly=true"));
        assert!(requests[0].contains("signature="));
        assert!(!requests[0].contains("/api/v3/order"));
        assert!(requests[1].starts_with("DELETE /fapi/v1/order?"));
        assert!(requests[1].contains("orderId=9001"));
    }

    #[tokio::test]
    async fn usd_m_open_orders_use_futures_endpoint_and_preserve_client_ids() {
        let (base_url, server) = rest_server(vec![
            (
                "200 OK",
                r#"[{"symbol":"BTCUSDT","orderId":9001,"clientOrderId":"client-usdm","price":"50000","origQty":"1","executedQty":"0.25","status":"PARTIALLY_FILLED","time":1,"updateTime":2,"side":"BUY","type":"LIMIT"}]"#.to_string(),
            ),
        ])
        .await;
        let mut cfg = config(ExecutionMode::Testnet);
        cfg.credentials =
            BinanceCredentials::new("test-key".to_string(), "test-secret".to_string());
        cfg.rest_base_url = base_url;
        let client = BinanceUsdMExecutionClient::new(cfg);

        let orders = client.list_open_orders().await.unwrap();
        let requests = server.await.unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(
            orders[0].order_id,
            OrderId("BNUSDM:standard:BTCUSDT:9001".to_string())
        );
        assert_eq!(orders[0].client_order_id.as_deref(), Some("client-usdm"));
        assert!(requests[0].starts_with("GET /fapi/v1/openOrders?"));
    }

    #[tokio::test]
    async fn usd_m_open_order_readback_seeds_symbol_for_canonical_cancellation() {
        let (base_url, server) = rest_server(vec![
            (
                "200 OK",
                r#"[{"symbol":"BTCUSDT","orderId":9001,"clientOrderId":"client-usdm","price":"50000","origQty":"1","executedQty":"0","status":"NEW","time":1,"updateTime":2,"side":"BUY","type":"LIMIT"}]"#.to_string(),
            ),
            (
                "200 OK",
                r#"{"symbol":"BTCUSDT","orderId":9001,"clientOrderId":"client-usdm"}"#.to_string(),
            ),
        ])
        .await;
        let mut cfg = config(ExecutionMode::Testnet);
        cfg.credentials =
            BinanceCredentials::new("test-key".to_string(), "test-secret".to_string());
        cfg.rest_base_url = base_url;
        let mut client = BinanceUsdMExecutionClient::new(cfg);

        let orders = client.list_open_orders().await.unwrap();
        let order_id = orders[0].order_id.clone();
        client.cancel_order(&order_id).await.unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].starts_with("DELETE /fapi/v1/order?"));
        assert!(requests[1].contains("orderId=9001"));
    }

    #[tokio::test]
    async fn usd_m_same_native_order_id_is_namespaced_by_symbol_for_cache_and_cancel() {
        let (base_url, server) = rest_server(vec![
            (
                "200 OK",
                r#"[{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"btc-42","price":"50000","origQty":"1","executedQty":"0","status":"NEW","time":1,"updateTime":2,"side":"BUY","type":"LIMIT"}]"#.to_string(),
            ),
            (
                "200 OK",
                r#"[{"symbol":"ETHUSDT","orderId":42,"clientOrderId":"eth-42","price":"3000","origQty":"1","executedQty":"0","status":"NEW","time":1,"updateTime":2,"side":"BUY","type":"LIMIT"}]"#.to_string(),
            ),
            (
                "200 OK",
                r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"btc-42"}"#.to_string(),
            ),
            (
                "200 OK",
                r#"{"symbol":"ETHUSDT","orderId":42,"clientOrderId":"eth-42"}"#.to_string(),
            ),
        ])
        .await;
        let mut cfg = config(ExecutionMode::Testnet);
        cfg.credentials =
            BinanceCredentials::new("test-key".to_string(), "test-secret".to_string());
        cfg.rest_base_url = base_url;
        let mut client = BinanceUsdMExecutionClient::new(cfg);

        let btc = client.list_open_orders().await.unwrap()[0].order_id.clone();
        let eth = client.list_open_orders().await.unwrap()[0].order_id.clone();
        assert_eq!(btc, OrderId("BNUSDM:standard:BTCUSDT:42".to_string()));
        assert_eq!(eth, OrderId("BNUSDM:standard:ETHUSDT:42".to_string()));
        let btc_events = parse_order_report_events(
            &serde_json::json!({
                "e":"ORDER_TRADE_UPDATE", "E":1000,
                "o":{"s":"BTCUSDT","c":"btc-42","i":42,"x":"CANCELED","X":"CANCELED"}
            }),
            1,
        )
        .unwrap();
        let eth_events = parse_order_report_events(
            &serde_json::json!({
                "e":"ORDER_TRADE_UPDATE", "E":1000,
                "o":{"s":"ETHUSDT","c":"eth-42","i":42,"x":"CANCELED","X":"CANCELED"}
            }),
            1,
        )
        .unwrap();
        assert!(btc_events.iter().any(|event| matches!(
            event,
            ExecutionEvent::OrderCanceled { order_id, .. }
                if order_id == &btc
        )));
        assert!(eth_events.iter().any(|event| matches!(
            event,
            ExecutionEvent::OrderCanceled { order_id, .. }
                if order_id == &eth
        )));
        client.cancel_order(&btc).await.unwrap();
        client.cancel_order(&eth).await.unwrap();

        let requests = server.await.unwrap();
        assert!(requests[2].contains("symbol=BTCUSDT"));
        assert!(requests[3].contains("symbol=ETHUSDT"));
        assert!(requests[2].contains("orderId=42"));
        assert!(requests[3].contains("orderId=42"));
    }

    #[tokio::test]
    async fn usd_m_client_id_metadata_can_repeat_when_canonical_id_is_distinct() {
        let (base_url, server) = rest_server(vec![
            (
                "200 OK",
                r#"[{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"same-id","price":"50000","origQty":"1","executedQty":"0","status":"NEW","time":1,"updateTime":2,"side":"BUY","type":"LIMIT"}]"#.to_string(),
            ),
            (
                "200 OK",
                r#"[{"symbol":"ETHUSDT","orderId":42,"clientOrderId":"same-id","price":"3000","origQty":"1","executedQty":"0","status":"NEW","time":1,"updateTime":2,"side":"BUY","type":"LIMIT"}]"#.to_string(),
            ),
            (
                "200 OK",
                r#"{"symbol":"BTCUSDT","orderId":42,"clientOrderId":"same-id"}"#.to_string(),
            ),
            (
                "200 OK",
                r#"{"symbol":"ETHUSDT","orderId":42,"clientOrderId":"same-id"}"#.to_string(),
            ),
        ])
        .await;
        let mut cfg = config(ExecutionMode::Testnet);
        cfg.credentials =
            BinanceCredentials::new("test-key".to_string(), "test-secret".to_string());
        cfg.rest_base_url = base_url;
        let mut client = BinanceUsdMExecutionClient::new(cfg);
        let btc = client.list_open_orders().await.unwrap()[0].order_id.clone();
        let eth = client.list_open_orders().await.unwrap()[0].order_id.clone();
        assert_eq!(btc, OrderId("BNUSDM:standard:BTCUSDT:42".to_string()));
        assert_eq!(eth, OrderId("BNUSDM:standard:ETHUSDT:42".to_string()));
        assert!(matches!(
            client.cancel_order(&OrderId("same-id".to_string())).await,
            Err(HftError::OrderNotFound(_))
        ));
        client.cancel_order(&btc).await.unwrap();
        client.cancel_order(&eth).await.unwrap();
        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[2].contains("symbol=BTCUSDT"));
        assert!(requests[3].contains("symbol=ETHUSDT"));
    }

    #[tokio::test]
    async fn usd_m_unknown_submission_response_is_rejected_without_a_retry() {
        let (base_url, server) = rest_server(vec![(
            "200 OK",
            r#"{"symbol":"ETHUSDT","orderId":9001,"clientOrderId":"client-usdm","updateTime":123}"#
                .to_string(),
        )])
        .await;
        let mut cfg = config(ExecutionMode::Testnet);
        cfg.credentials =
            BinanceCredentials::new("test-key".to_string(), "test-secret".to_string());
        cfg.rest_base_url = base_url;
        let mut client = BinanceUsdMExecutionClient::new(cfg);
        let envelope = OrderIntentEnvelope::new(perp_intent(), OrderIntentLifecycle::default())
            .with_client_order_id("client-usdm");

        let error = client.place_order_envelope(&envelope).await.unwrap_err();
        assert!(
            matches!(error, HftError::Execution(message) if message.contains("order identity"))
        );
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn usd_m_timeout_or_server_failure_is_unknown_and_never_replayed() {
        let (base_url, server) = rest_server(vec![(
            "500 Internal Server Error",
            r#"{"code":-1000,"msg":"internal error"}"#.to_string(),
        )])
        .await;
        let mut cfg = config(ExecutionMode::Testnet);
        cfg.credentials =
            BinanceCredentials::new("test-key".to_string(), "test-secret".to_string());
        cfg.rest_base_url = base_url;
        let mut client = BinanceUsdMExecutionClient::new(cfg);
        let envelope = OrderIntentEnvelope::new(perp_intent(), OrderIntentLifecycle::default())
            .with_client_order_id("client-usdm");

        let error = client.place_order_envelope(&envelope).await.unwrap_err();
        assert!(matches!(error, HftError::Network(message) if message.contains("outcome unknown")));
        assert_eq!(server.await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn usd_m_account_snapshot_uses_futures_endpoint_and_converts_positions() {
        let (base_url, server) = rest_server(vec![
            (
                "200 OK",
                r#"{"assets":[{"asset":"USDT","walletBalance":"100","availableBalance":"90"}],"positions":[{"symbol":"BTCUSDT","positionAmt":"0.01","entryPrice":"50000","unrealizedProfit":"-2.5","positionSide":"BOTH"}]}"#.to_string(),
            ),
            (
                "200 OK",
                r#"{"assets":[{"asset":"USDT","walletBalance":"100","availableBalance":"90"}],"positions":[{"symbol":"BTCUSDT","positionAmt":"0.01","entryPrice":"50000","unrealizedProfit":"-2.5","positionSide":"BOTH"}]}"#.to_string(),
            ),
        ])
        .await;
        let mut cfg = config(ExecutionMode::Testnet);
        cfg.credentials =
            BinanceCredentials::new("test-key".to_string(), "test-secret".to_string());
        cfg.rest_base_url = base_url;
        let client = BinanceUsdMExecutionClient::new(cfg);

        let balances = client.get_balance().await.unwrap();
        let positions = client.get_positions().await.unwrap();
        let requests = server.await.unwrap();
        assert_eq!(balances[0].asset, "USDT");
        assert_eq!(balances[0].available, Decimal::from(90));
        assert_eq!(balances[0].frozen, Decimal::from(10));
        assert_eq!(positions[0].quantity, Quantity::from_f64(0.01).unwrap());
        assert_eq!(positions[0].avg_price, Price::from_f64(50_000.0).unwrap());
        assert_eq!(
            positions[0].unrealized_pnl,
            Decimal::from_f64_retain(-2.5).unwrap()
        );
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.starts_with("GET /fapi/v3/account?")));
    }

    #[test]
    fn usd_m_place_and_cancel_response_identity_mismatches_are_rejected() {
        let place = BinanceUsdMPlaceResponse {
            symbol: "BTCUSDT".to_string(),
            order_id: 1,
            client_order_id: "different".to_string(),
            update_time: 1,
        };
        assert!(matches!(
            parse_usdm_place_response(place, "BTCUSDT", "expected"),
            Err(HftError::Execution(message)) if message.contains("order identity")
        ));
        let cancel = BinanceUsdMCancelResponse {
            symbol: "ETHUSDT".to_string(),
            order_id: 1,
            client_order_id: "expected".to_string(),
        };
        let cancel_record = UsdMOrderRecord {
            symbol: "BTCUSDT".to_string(),
            client_order_id: "expected".to_string(),
            order_type: "STANDARD".to_string(),
            native_id: "1".to_string(),
            canonical_order_id: "BNUSDM:standard:BTCUSDT:1".to_string(),
            algo_id: None,
            is_algo: false,
        };
        assert!(matches!(
            validate_usdm_cancel_response(
                &cancel,
                &OrderId("BNUSDM:standard:BTCUSDT:1".to_string()),
                &cancel_record
            ),
            Err(HftError::Execution(message)) if message.contains("cancel response")
        ));
    }

    #[test]
    fn usd_m_algo_cancel_response_requires_all_returned_identity_fields_to_match() {
        let record = UsdMOrderRecord {
            symbol: "BTCUSDT".to_string(),
            client_order_id: "algo-42".to_string(),
            order_type: "STOP_MARKET".to_string(),
            native_id: "42".to_string(),
            canonical_order_id: "BNUSDM:algo:BTCUSDT:42".to_string(),
            algo_id: Some(42),
            is_algo: true,
        };
        let requested = OrderId(record.canonical_order_id.clone());
        let wrong_algo = BinanceUsdMCancelAlgoResponse {
            algo_id: Some(43),
            client_algo_id: Some("algo-42".to_string()),
            symbol: Some("BTCUSDT".to_string()),
        };
        assert!(validate_usdm_cancel_algo_response(&wrong_algo, &requested, &record).is_err());
        let wrong_client = BinanceUsdMCancelAlgoResponse {
            algo_id: Some(42),
            client_algo_id: Some("algo-43".to_string()),
            symbol: Some("BTCUSDT".to_string()),
        };
        assert!(validate_usdm_cancel_algo_response(&wrong_client, &requested, &record).is_err());
    }

    #[test]
    fn usd_m_place_response_requires_nonzero_update_time() {
        let zero = BinanceUsdMPlaceResponse {
            symbol: "BTCUSDT".to_string(),
            order_id: 1,
            client_order_id: "expected".to_string(),
            update_time: 0,
        };
        assert!(matches!(
            parse_usdm_place_response(zero, "BTCUSDT", "expected"),
            Err(HftError::Parse(message)) if message.contains("updateTime")
        ));

        let missing = serde_json::json!({
            "symbol": "BTCUSDT",
            "orderId": 1,
            "clientOrderId": "expected"
        });
        assert!(serde_json::from_value::<BinanceUsdMPlaceResponse>(missing).is_err());
    }

    #[test]
    fn usd_m_account_update_converts_wallet_balance_without_inventing_positions() {
        let value = serde_json::json!({
            "e":"ACCOUNT_UPDATE", "E":1000, "T":999,
            "a":{"m":"ORDER","B":[{"a":"USDT","wb":"12.5","cw":"12.5","bc":"0"}],"P":[]}
        });
        let events = parse_account_update_events(&value).unwrap();
        assert!(
            matches!(&events[0], ExecutionEvent::BalanceUpdate { asset, balance, timestamp } if asset == "USDT" && *balance == Quantity::from_f64(12.5).unwrap() && *timestamp == 1_000_000)
        );
    }

    #[tokio::test]
    async fn usd_m_direct_paper_client_rejects_writes_without_emitting_fills() {
        let mut client = BinanceUsdMExecutionClient::new(config(ExecutionMode::Paper));
        let connect_error = client.connect().await.unwrap_err();
        assert!(connect_error
            .to_string()
            .contains("canonical simulated execution client"));
        assert!(!client.connected);

        let mut spot = perp_intent();
        spot.product_type = ProductType::Spot;
        assert!(matches!(
            client.place_order(spot).await,
            Err(HftError::InvalidOrder(_))
        ));
        let place_error = client.place_order(perp_intent()).await.unwrap_err();
        assert!(place_error
            .to_string()
            .contains("canonical simulated execution client"));
        let cancel_error = client
            .cancel_order(&OrderId("paper-order".to_string()))
            .await
            .unwrap_err();
        assert!(cancel_error
            .to_string()
            .contains("canonical simulated execution client"));
        let modify_error = client
            .modify_order(
                &OrderId("paper-order".to_string()),
                Some(Quantity::from_f64(0.001).unwrap()),
                None,
            )
            .await
            .unwrap_err();
        assert!(modify_error
            .to_string()
            .contains("canonical simulated execution client"));

        let mut stream = client.execution_stream().await.unwrap();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn usd_m_live_connect_rejects_missing_credentials_before_network_io() {
        let mut client = BinanceUsdMExecutionClient::new(config(ExecutionMode::Live));
        let error = client.connect().await.unwrap_err();
        assert!(
            matches!(error, HftError::Authentication(message) if message.contains("API credentials"))
        );
        assert!(!client.connected);
    }

    #[tokio::test]
    async fn usd_m_both_envelope_entrypoints_apply_cex_gate_before_submission() {
        let lifecycle = OrderIntentLifecycle {
            max_slippage_bps: Some(25),
            ..Default::default()
        };
        let envelope = OrderIntentEnvelope::new(perp_intent(), lifecycle)
            .with_client_order_id("usdm-envelope");

        let mut first = BinanceUsdMExecutionClient::new(config(ExecutionMode::Paper));
        let first_error = first.place_order_envelope(&envelope).await.unwrap_err();
        assert!(first_error.to_string().contains("MissingSlippageReference"));

        let mut second = BinanceUsdMExecutionClient::new(config(ExecutionMode::Paper));
        let attempt = second.place_order_envelope_traced(&envelope).await;
        assert!(attempt
            .outcome
            .unwrap_err()
            .to_string()
            .contains("MissingSlippageReference"));
    }

    #[tokio::test]
    async fn usd_m_envelope_limits_and_expiry_are_rejected_before_transport() {
        let expired = OrderIntentEnvelope::new(
            perp_intent(),
            OrderIntentLifecycle {
                valid_until: 1,
                ..Default::default()
            },
        )
        .with_client_order_id("expired");
        let mut client = BinanceUsdMExecutionClient::new(config(ExecutionMode::Paper));
        assert!(client
            .place_order_envelope(&expired)
            .await
            .unwrap_err()
            .to_string()
            .contains("Expired"));

        let quantity_limited = OrderIntentEnvelope::new(
            perp_intent(),
            OrderIntentLifecycle {
                max_order_quantity: Some(Decimal::from_f64_retain(0.0001).unwrap()),
                ..Default::default()
            },
        )
        .with_client_order_id("quantity-limit");
        let mut client = BinanceUsdMExecutionClient::new(config(ExecutionMode::Paper));
        assert!(client
            .place_order_envelope(&quantity_limited)
            .await
            .unwrap_err()
            .to_string()
            .contains("MaxOrderQuantityExceeded"));

        let notional_limited = OrderIntentEnvelope::new(
            perp_intent(),
            OrderIntentLifecycle {
                max_order_notional: Some(Decimal::ONE),
                ..Default::default()
            },
        )
        .with_client_order_id("notional-limit");
        let mut client = BinanceUsdMExecutionClient::new(config(ExecutionMode::Paper));
        assert!(client
            .place_order_envelope(&notional_limited)
            .await
            .unwrap_err()
            .to_string()
            .contains("MaxOrderNotionalExceeded"));
    }

    #[test]
    fn usd_m_order_report_rejects_missing_fill_identity() {
        let malformed = serde_json::json!({
            "e":"ORDER_TRADE_UPDATE", "E":1000,
            "o":{"s":"BTCUSDT","c":"client-42","i":42,"x":"TRADE","X":"FILLED","l":"0.1","L":"50000","z":"0.1","ap":"50000"}
        });
        assert!(
            matches!(parse_order_report_events(&malformed, 1), Err(HftError::Parse(message)) if message.contains("trade id"))
        );
    }

    #[test]
    fn usd_m_hedge_mode_position_is_fail_closed() {
        let account = BinanceUsdMAccountResponse {
            assets: vec![],
            positions: vec![BinanceUsdMPosition {
                symbol: "BTCUSDT".to_string(),
                position_amt: "1".to_string(),
                entry_price: "50000".to_string(),
                unrealized_profit: "0".to_string(),
                position_side: "LONG".to_string(),
            }],
        };
        assert!(
            matches!(parse_usdm_positions(account), Err(HftError::Config(message)) if message.contains("hedge-mode"))
        );
    }

    #[test]
    fn usd_m_account_response_distinguishes_missing_positions_from_empty_positions() {
        let missing = serde_json::json!({
            "assets": []
        });
        assert!(serde_json::from_value::<BinanceUsdMAccountResponse>(missing).is_err());

        let empty = serde_json::json!({
            "assets": [],
            "positions": []
        });
        assert!(serde_json::from_value::<BinanceUsdMAccountResponse>(empty).is_ok());
    }

    #[test]
    fn usd_m_position_response_requires_position_side() {
        let missing = serde_json::json!({
            "assets": [],
            "positions": [{
                "symbol": "BTCUSDT",
                "positionAmt": "1",
                "entryPrice": "50000",
                "unRealizedProfit": "0"
            }]
        });
        assert!(serde_json::from_value::<BinanceUsdMAccountResponse>(missing).is_err());

        let empty = BinanceUsdMAccountResponse {
            assets: vec![],
            positions: vec![BinanceUsdMPosition {
                symbol: "BTCUSDT".to_string(),
                position_amt: "1".to_string(),
                entry_price: "50000".to_string(),
                unrealized_profit: "0".to_string(),
                position_side: String::new(),
            }],
        };
        assert!(matches!(
            parse_usdm_positions(empty),
            Err(HftError::Config(message)) if message.contains("hedge-mode")
        ));
    }

    #[tokio::test]
    async fn usd_m_execution_stream_is_empty_before_connect() {
        let client = BinanceUsdMExecutionClient::new(config(ExecutionMode::Paper));
        let mut stream = client.execution_stream().await.unwrap();
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn usd_m_private_stream_retains_reports_before_execution_stream_subscription() {
        let mut client = BinanceUsdMExecutionClient::new(config(ExecutionMode::Testnet));
        let (tx, rx) = broadcast::channel(8);
        client.event_tx = Some(tx.clone());
        *client
            .private_event_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rx);
        tx.send(ExecutionEvent::ConnectionStatus {
            connected: true,
            timestamp: 1,
        })
        .unwrap();

        let mut stream = client.execution_stream().await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap().unwrap(),
            ExecutionEvent::ConnectionStatus {
                connected: true,
                timestamp: 1
            }
        ));
    }

    #[tokio::test]
    async fn usd_m_algo_orders_are_read_back_and_cancelled_via_algo_endpoint() {
        let (base_url, server) = rest_server(vec![
            (
                "200 OK",
                r#"[{"symbol":"BTCUSDT","orderId":9002,"clientOrderId":"same-id","price":"50000","origQty":"1","executedQty":"0","status":"NEW","time":1,"updateTime":2,"side":"SELL","type":"LIMIT"}]"#.to_string(),
            ),
            (
                "200 OK",
                r#"[{"algoId":9002,"clientAlgoId":"same-id","orderType":"STOP_MARKET","symbol":"BTCUSDT"}]"#.to_string(),
            ),
            (
                "200 OK",
                r#"{"symbol":"BTCUSDT","orderId":9002,"clientOrderId":"same-id"}"#.to_string(),
            ),
            (
                "200 OK",
                r#"{"algoId":9002,"clientAlgoId":"same-id","symbol":"BTCUSDT","code":"200"}"#.to_string(),
            ),
        ])
        .await;
        let mut cfg = config(ExecutionMode::Testnet);
        cfg.credentials =
            BinanceCredentials::new("test-key".to_string(), "test-secret".to_string());
        cfg.rest_base_url = base_url;
        let mut client = BinanceUsdMExecutionClient::new(cfg);

        let open_orders = client.list_open_orders().await.unwrap();
        assert_eq!(open_orders.len(), 1);
        assert_eq!(
            open_orders[0].order_id,
            OrderId("BNUSDM:standard:BTCUSDT:9002".to_string())
        );
        let cancellable = client.list_cancellable_orders().await.unwrap();
        assert_eq!(cancellable.len(), 1);
        assert_eq!(
            cancellable[0].order_id,
            OrderId("BNUSDM:algo:BTCUSDT:9002".to_string())
        );
        assert_eq!(cancellable[0].order_type, "STOP_MARKET");
        client.cancel_order(&open_orders[0].order_id).await.unwrap();
        client.cancel_order(&cancellable[0].order_id).await.unwrap();

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[1].starts_with("GET /fapi/v1/openAlgoOrders?"));
        assert!(requests[2].starts_with("DELETE /fapi/v1/order?"));
        assert!(requests[3].starts_with("DELETE /fapi/v1/algoOrder?"));
        assert!(requests[3].contains("algoId=9002"));
    }
}
