//! Ondo Perps REST execution adapter.

use std::{
    env,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use chrono::DateTime;
use futures::StreamExt;
use hft_core::{
    now_micros, AssetClass, HftError, HftResult, OrderId, OrderType, Price, ProductType, Quantity,
    RegulatoryProfile, Side, Symbol, TimeInForce, VenueId,
};
use hmac::{Hmac, Mac};
use ports::{
    AccountBalance, BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent, OpenOrder,
    OrderIntent, OrderStatus,
};
use reqwest::{Client, Method, Response};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

const DEFAULT_REST_BASE_URL: &str = "https://api.ondoperps.xyz";

#[derive(Debug, Clone)]
pub struct OndoPerpsExecutionConfig {
    pub rest_base_url: String,
    pub key_id: String,
    pub api_secret: String,
    pub timeout_ms: u64,
}

impl Default for OndoPerpsExecutionConfig {
    fn default() -> Self {
        Self {
            rest_base_url: env::var("ONDO_REST_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_REST_BASE_URL.to_string()),
            key_id: env::var("ONDO_KEY_ID").unwrap_or_default(),
            api_secret: env::var("ONDO_API_SECRET").unwrap_or_default(),
            timeout_ms: 5_000,
        }
    }
}

pub struct OndoPerpsExecutionClient {
    cfg: OndoPerpsExecutionConfig,
    http: Client,
    event_tx: broadcast::Sender<ExecutionEvent>,
    connected: Arc<AtomicBool>,
    last_heartbeat: Arc<AtomicU64>,
}

impl OndoPerpsExecutionClient {
    pub fn new(mut cfg: OndoPerpsExecutionConfig) -> HftResult<Self> {
        if cfg.rest_base_url.trim().is_empty() {
            cfg.rest_base_url = env::var("ONDO_REST_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_REST_BASE_URL.to_string());
        }
        if cfg.key_id.trim().is_empty() {
            cfg.key_id = env::var("ONDO_KEY_ID").unwrap_or_default();
        }
        if cfg.api_secret.trim().is_empty() {
            cfg.api_secret = env::var("ONDO_API_SECRET").unwrap_or_default();
        }
        if cfg.key_id.trim().is_empty() || cfg.api_secret.trim().is_empty() {
            return Err(HftError::Authentication(
                "Ondo Perps requires key_id/api_secret or ONDO_KEY_ID/ONDO_API_SECRET".into(),
            ));
        }

        let http = Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms.max(1_000)))
            .user_agent("hft-ondo-perps-exec/0.1")
            .build()
            .map_err(|e| HftError::Config(format!("build Ondo HTTP client: {e}")))?;
        let (event_tx, _) = broadcast::channel(256);

        Ok(Self {
            cfg,
            http,
            event_tx,
            connected: Arc::new(AtomicBool::new(false)),
            last_heartbeat: Arc::new(AtomicU64::new(now_micros())),
        })
    }

    fn current_timestamp_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn signature(
        secret: &str,
        timestamp_ms: u64,
        method: &str,
        path: &str,
        body: &str,
    ) -> HftResult<String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .map_err(|e| HftError::Authentication(format!("invalid Ondo API secret: {e}")))?;
        mac.update(timestamp_ms.to_string().as_bytes());
        mac.update(method.to_ascii_uppercase().as_bytes());
        mac.update(path.as_bytes());
        mac.update(body.as_bytes());
        Ok(hex::encode(mac.finalize().into_bytes()))
    }

    fn normalize_market(symbol: &Symbol) -> HftResult<String> {
        let mut market = symbol
            .as_str()
            .trim()
            .to_ascii_uppercase()
            .replace(['/', '_'], "-");
        if market.is_empty() {
            return Err(HftError::InvalidOrder("Ondo market cannot be empty".into()));
        }
        if let Some(base) = market.strip_suffix(".P") {
            market = base.to_string();
        }
        if !market.contains('-') {
            if let Some(base) = market.strip_suffix("USD") {
                market = format!("{base}-USD");
            } else {
                market.push_str("-USD");
            }
        }
        Ok(format!("{market}.P"))
    }

    fn time_in_force(tif: TimeInForce) -> HftResult<&'static str> {
        match tif {
            TimeInForce::GTC => Ok("GTC"),
            TimeInForce::IOC => Ok("IOC"),
            TimeInForce::FOK => Err(HftError::InvalidOrder(
                "Ondo Perps supports only GTC or IOC limit orders".into(),
            )),
        }
    }

    fn build_order_body(intent: &OrderIntent) -> HftResult<String> {
        Self::validate_intent(intent)?;
        let order_type = match intent.order_type {
            OrderType::Market => "market",
            OrderType::Limit => "limit",
        };
        let mut body = json!({
            "side": match intent.side { Side::Buy => "buy", Side::Sell => "sell" },
            "market": Self::normalize_market(&intent.symbol)?,
            "size": intent.quantity.0.to_string(),
            "type": order_type,
        });
        if intent.order_type == OrderType::Limit {
            let price = intent
                .price
                .ok_or_else(|| HftError::InvalidOrder("Ondo limit order requires price".into()))?;
            body["price"] = Value::String(price.0.to_string());
            body["timeInForce"] = Value::String(Self::time_in_force(intent.time_in_force)?.into());
        }
        serde_json::to_string(&body)
            .map_err(|e| HftError::Serialization(format!("serialize Ondo order: {e}")))
    }

    async fn send_signed(
        &self,
        method: Method,
        path: &str,
        body: Option<&str>,
    ) -> HftResult<Response> {
        let timestamp = Self::current_timestamp_ms();
        let exact_body = body.unwrap_or("");
        let signature = Self::signature(
            &self.cfg.api_secret,
            timestamp,
            method.as_str(),
            path,
            exact_body,
        )?;
        let url = format!("{}{}", self.cfg.rest_base_url.trim_end_matches('/'), path);
        let mut request = self
            .http
            .request(method, url)
            .header("ONDO-KEY-ID", &self.cfg.key_id)
            .header("ONDO-TIMESTAMP", timestamp.to_string())
            .header("ONDO-SIGN", signature);
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_owned());
        }
        let response = request
            .send()
            .await
            .map_err(|e| HftError::Network(format!("Ondo request failed: {e}")))?;
        self.last_heartbeat.store(now_micros(), Ordering::SeqCst);
        Ok(response)
    }

    async fn response_text(response: Response, operation: &str) -> HftResult<String> {
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| HftError::Network(format!("read Ondo {operation} response: {e}")))?;
        if !status.is_success() {
            return Err(HftError::Exchange(format!(
                "Ondo {operation} failed ({status}): {text}"
            )));
        }
        Ok(text)
    }

    fn parse_order_id(text: &str) -> HftResult<OrderId> {
        let response: ApiResponse<Value> = serde_json::from_str(text)
            .map_err(|e| HftError::Serialization(format!("parse Ondo order response: {e}")))?;
        if !response.success {
            return Err(HftError::Exchange(
                "Ondo order response reported failure".into(),
            ));
        }
        let value = response
            .result
            .get("orderId")
            .ok_or_else(|| HftError::Parse("Ondo order response missing result.orderId".into()))?;
        let id = match value {
            Value::String(id) if !id.is_empty() => id.clone(),
            Value::Number(id) => id.to_string(),
            _ => {
                return Err(HftError::Parse(
                    "Ondo result.orderId has invalid type".into(),
                ))
            }
        };
        Ok(OrderId(id))
    }

    fn parse_open_orders(text: &str) -> HftResult<Vec<OpenOrder>> {
        let response: PagedApiResponse<Vec<RawOpenOrder>> = serde_json::from_str(text)
            .map_err(|e| HftError::Serialization(format!("parse Ondo open orders: {e}")))?;
        if !response.success {
            return Err(HftError::Exchange(response.error.unwrap_or_else(|| {
                "Ondo open-orders response reported failure".into()
            })));
        }
        if response
            .page_info
            .and_then(|page| page.next_cursor)
            .is_some_and(|cursor| !cursor.is_empty())
        {
            return Err(HftError::Exchange(
                "Ondo open-orders response is paginated and therefore incomplete".into(),
            ));
        }
        response
            .result
            .into_iter()
            .map(RawOpenOrder::try_into)
            .collect()
    }

    fn validate_intent(intent: &OrderIntent) -> HftResult<()> {
        if intent.asset_class != AssetClass::TokenizedSecurity
            || intent.product_type != ProductType::Perp
            || intent.compliance_context.regulatory_profile
                != RegulatoryProfile::RestrictedJurisdiction
            || !intent.compliance_context.eligibility_confirmed
            || !intent.compliance_context.allow_tokenized_securities
        {
            return Err(HftError::InvalidOrder(
                "Ondo Perps requires an eligible restricted-jurisdiction tokenized-security perp intent"
                    .into(),
            ));
        }
        if intent
            .target_venue
            .is_some_and(|venue| venue != VenueId::ONDO_PERPS)
        {
            return Err(HftError::InvalidOrder(
                "Ondo Perps intent targets a different venue".into(),
            ));
        }
        if intent.quantity.0 <= Decimal::ZERO {
            return Err(HftError::InvalidOrder(
                "Ondo Perps order quantity must be positive".into(),
            ));
        }
        if intent.price.is_some_and(|price| price.0 <= Decimal::ZERO) {
            return Err(HftError::InvalidOrder(
                "Ondo Perps order price must be positive".into(),
            ));
        }
        Ok(())
    }

    fn parse_balance(text: &str) -> HftResult<Vec<AccountBalance>> {
        let response: ApiResponse<RawBalance> = serde_json::from_str(text)
            .map_err(|e| HftError::Serialization(format!("parse Ondo balance: {e}")))?;
        if !response.success {
            return Err(HftError::Exchange(response.error.unwrap_or_else(|| {
                "Ondo balance response reported failure".into()
            })));
        }
        let balance = response.result;
        if balance.under_liquidation
            || balance.margin_balance < Decimal::ZERO
            || balance.used_margin < Decimal::ZERO
            || balance.available_margin < Decimal::ZERO
            || balance.available_margin + balance.used_margin != balance.margin_balance
        {
            return Err(HftError::Exchange(
                "Ondo margin balance is unsafe or internally inconsistent".into(),
            ));
        }
        Ok(vec![AccountBalance {
            asset: "USDC".into(),
            available: balance.available_margin,
            frozen: balance.used_margin,
            total: balance.margin_balance,
            usd_value: Some(balance.margin_balance),
        }])
    }
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    success: bool,
    result: T,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PagedApiResponse<T> {
    success: bool,
    result: T,
    #[serde(default)]
    page_info: Option<PageInfo>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBalance {
    margin_balance: Decimal,
    used_margin: Decimal,
    available_margin: Decimal,
    under_liquidation: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOpenOrder {
    order_id: String,
    market: String,
    side: String,
    #[serde(rename = "type")]
    order_type: String,
    size: Decimal,
    filled_size: Decimal,
    price: Option<Decimal>,
    status: String,
    created_at: String,
    updated_at: Option<String>,
}

impl TryFrom<RawOpenOrder> for OpenOrder {
    type Error = HftError;

    fn try_from(order: RawOpenOrder) -> Result<Self, Self::Error> {
        let side = match order.side.as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            other => return Err(HftError::Parse(format!("unknown Ondo side: {other}"))),
        };
        let order_type = match order.order_type.as_str() {
            "market" => OrderType::Market,
            "limit" => OrderType::Limit,
            other => return Err(HftError::Parse(format!("unknown Ondo order type: {other}"))),
        };
        let status = match order.status.as_str() {
            "open" | "accepted" if order.filled_size > Decimal::ZERO => {
                OrderStatus::PartiallyFilled
            }
            "open" | "accepted" => OrderStatus::Accepted,
            "new" => OrderStatus::New,
            "partially_filled" => OrderStatus::PartiallyFilled,
            other => {
                return Err(HftError::Parse(format!(
                    "unknown Ondo open-order status: {other}"
                )))
            }
        };
        if order.filled_size < Decimal::ZERO || order.filled_size > order.size {
            return Err(HftError::Parse(format!(
                "Ondo order {} has invalid filled size",
                order.order_id
            )));
        }
        if order_type == OrderType::Limit && order.price.is_none() {
            return Err(HftError::Parse(format!(
                "Ondo limit order {} has no price",
                order.order_id
            )));
        }
        let created_at = parse_rfc3339_micros(&order.created_at, "createdAt")?;
        let updated_at = match order.updated_at {
            Some(value) => parse_rfc3339_micros(&value, "updatedAt")?,
            None => created_at,
        };
        Ok(OpenOrder {
            order_id: OrderId(order.order_id),
            client_order_id: None,
            symbol: Symbol::from(order.market),
            side,
            order_type,
            original_quantity: Quantity(order.size),
            remaining_quantity: Quantity(order.size - order.filled_size),
            filled_quantity: Quantity(order.filled_size),
            price: order.price.map(Price),
            status,
            created_at,
            updated_at,
        })
    }
}

fn parse_rfc3339_micros(value: &str, field: &str) -> HftResult<u64> {
    let timestamp = DateTime::parse_from_rfc3339(value)
        .map_err(|e| HftError::Parse(format!("invalid Ondo {field}: {e}")))?
        .timestamp_micros();
    u64::try_from(timestamp)
        .map_err(|_| HftError::Parse(format!("Ondo {field} predates Unix epoch")))
}

#[async_trait]
impl ExecutionClient for OndoPerpsExecutionClient {
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        let body = Self::build_order_body(&intent)?;
        let response = self
            .send_signed(Method::POST, "/v1/perps/orders", Some(&body))
            .await?;
        let text = Self::response_text(response, "place order").await?;
        let order_id = Self::parse_order_id(&text)?;
        let _ = self.event_tx.send(ExecutionEvent::OrderAck {
            order_id: order_id.clone(),
            timestamp: now_micros(),
        });
        Ok(order_id)
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        let path = format!("/v1/perps/orders/{}", order_id.0);
        let response = self.send_signed(Method::DELETE, &path, None).await?;
        let text = Self::response_text(response, "cancel order").await?;
        let response: ApiResponse<Value> = serde_json::from_str(&text)
            .map_err(|e| HftError::Serialization(format!("parse Ondo cancel response: {e}")))?;
        if !response.success {
            return Err(HftError::Exchange(
                "Ondo cancel response reported failure".into(),
            ));
        }
        let _ = self.event_tx.send(ExecutionEvent::OrderCanceled {
            order_id: order_id.clone(),
            timestamp: now_micros(),
        });
        Ok(())
    }

    async fn modify_order(
        &mut self,
        _order_id: &OrderId,
        _new_quantity: Option<Quantity>,
        _new_price: Option<Price>,
    ) -> HftResult<()> {
        Err(HftError::Execution(
            "Ondo Perps does not support order modification through this adapter".into(),
        ))
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        let stream =
            BroadcastStream::new(self.event_tx.subscribe()).filter_map(|event| async move {
                match event {
                    Ok(event) => Some(Ok(event)),
                    Err(_) => None,
                }
            });
        Ok(Box::pin(stream))
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        let path = "/v1/perps/orders?status=open&limit=1000";
        let response = self.send_signed(Method::GET, path, None).await?;
        let text = Self::response_text(response, "list open orders").await?;
        Self::parse_open_orders(&text)
    }

    async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
        let response = self
            .send_signed(Method::GET, "/v1/perps/balance", None)
            .await?;
        let text = Self::response_text(response, "get balance").await?;
        Self::parse_balance(&text)
    }

    async fn connect(&mut self) -> HftResult<()> {
        self.get_balance().await?;
        self.connected.store(true, Ordering::SeqCst);
        self.last_heartbeat.store(now_micros(), Ordering::SeqCst);
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.connected.load(Ordering::SeqCst),
            latency_ms: None,
            last_heartbeat: self.last_heartbeat.load(Ordering::SeqCst),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{AssetClass, ProductType, RegulatoryProfile, VenueId};

    fn intent(order_type: OrderType, price: Option<Price>) -> OrderIntent {
        OrderIntent {
            symbol: Symbol::from("aapl/usd"),
            asset_class: AssetClass::TokenizedSecurity,
            product_type: ProductType::Perp,
            compliance_context: hft_core::ComplianceContext {
                regulatory_profile: RegulatoryProfile::RestrictedJurisdiction,
                jurisdiction: Some("SG".into()),
                eligibility_confirmed: true,
                allow_tokenized_securities: true,
            },
            side: Side::Buy,
            quantity: Quantity(Decimal::from(2)),
            order_type,
            price,
            time_in_force: TimeInForce::GTC,
            strategy_id: "test".into(),
            target_venue: Some(VenueId::ONDO_PERPS),
        }
    }

    #[test]
    fn rejects_orders_outside_the_ondo_perps_compliance_boundary() {
        let mut invalid = intent(OrderType::Market, None);
        invalid.product_type = ProductType::Spot;
        assert!(OndoPerpsExecutionClient::build_order_body(&invalid).is_err());

        invalid = intent(OrderType::Market, None);
        invalid.compliance_context.eligibility_confirmed = false;
        assert!(OndoPerpsExecutionClient::build_order_body(&invalid).is_err());

        invalid = intent(OrderType::Market, None);
        invalid.target_venue = Some(VenueId::BINANCE);
        assert!(OndoPerpsExecutionClient::build_order_body(&invalid).is_err());

        invalid = intent(OrderType::Market, None);
        invalid.quantity = Quantity(Decimal::ZERO);
        assert!(OndoPerpsExecutionClient::build_order_body(&invalid).is_err());
    }

    #[test]
    fn signature_matches_known_hmac_vector() {
        let body = r#"{"side":"buy","market":"AAPL-USD.P","size":"2","type":"limit","price":"190.5","timeInForce":"GTC"}"#;
        let signature = OndoPerpsExecutionClient::signature(
            "test-secret",
            1_700_000_000_123,
            "post",
            "/v1/perps/orders",
            body,
        )
        .unwrap();
        assert_eq!(
            signature,
            "5d41493147f2c22efe84cea65913eca198792931abda5d66aa0f8353bbcdcf82"
        );
    }

    #[test]
    fn order_body_has_normalized_market_and_limit_fields() {
        let body = OndoPerpsExecutionClient::build_order_body(&intent(
            OrderType::Limit,
            Some(Price(Decimal::new(1905, 1))),
        ))
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({
                "side": "buy",
                "market": "AAPL-USD.P",
                "size": "2",
                "type": "limit",
                "price": "190.5",
                "timeInForce": "GTC"
            })
        );
    }

    #[test]
    fn market_body_omits_price_and_limit_requires_it() {
        let body =
            OndoPerpsExecutionClient::build_order_body(&intent(OrderType::Market, None)).unwrap();
        let body = serde_json::from_str::<Value>(&body).unwrap();
        assert!(body.get("price").is_none());
        assert!(body.get("timeInForce").is_none());
        assert!(
            OndoPerpsExecutionClient::build_order_body(&intent(OrderType::Limit, None)).is_err()
        );

        let mut unsupported = intent(OrderType::Limit, Some(Price(Decimal::ONE)));
        unsupported.time_in_force = TimeInForce::FOK;
        assert!(OndoPerpsExecutionClient::build_order_body(&unsupported).is_err());
    }

    #[test]
    fn order_response_parser_accepts_string_or_numeric_id_and_rejects_failure() {
        assert_eq!(
            OndoPerpsExecutionClient::parse_order_id(
                r#"{"success":true,"result":{"orderId":"ord-1"}}"#
            )
            .unwrap()
            .0,
            "ord-1"
        );
        assert_eq!(
            OndoPerpsExecutionClient::parse_order_id(r#"{"success":true,"result":{"orderId":42}}"#)
                .unwrap()
                .0,
            "42"
        );
        assert!(OndoPerpsExecutionClient::parse_order_id(
            r#"{"success":false,"result":{"orderId":"ord-1"}}"#
        )
        .is_err());
    }

    #[test]
    fn open_orders_parser_maps_complete_records_and_rejects_unknown_schema() {
        let orders = OndoPerpsExecutionClient::parse_open_orders(
            r#"{"success":true,"result":[{"orderId":"ord-1","market":"AAPL-USD.P","side":"buy","type":"limit","size":"2","filledSize":"0.5","price":"190.5","status":"open","createdAt":"2026-07-12T08:00:00.123456Z","clientOrderId":"client-1","reduceOnly":false}]}"#,
        )
        .unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].remaining_quantity.0, Decimal::new(15, 1));
        assert_eq!(orders[0].status, OrderStatus::PartiallyFilled);
        assert_eq!(orders[0].created_at, 1_783_843_200_123_456);
        assert_eq!(orders[0].updated_at, orders[0].created_at);

        assert!(OndoPerpsExecutionClient::parse_open_orders(
            r#"{"success":true,"result":[{"orderId":"ord-1"}]}"#
        )
        .is_err());

        assert!(OndoPerpsExecutionClient::parse_open_orders(
            r#"{"success":true,"result":[],"pageInfo":{"nextCursor":"more"}}"#
        )
        .is_err());
    }

    #[test]
    fn balance_parser_returns_authoritative_margin_equity() {
        let balances = OndoPerpsExecutionClient::parse_balance(
            r#"{"success":true,"result":{"walletBalance":"5000","realizedPnl":"250","unrealizedPnl":"-50","marginBalance":"4950","usedMargin":"1125","availableMargin":"3825","withdrawableMargin":"3825","maintenanceMarginRequirement":"112.5","totalMaintenanceMargin":"200","marginRatio":"0.04","leverage":"0.46","underLiquidation":false,"totalFundingPayments":"-5.67","totalTradingFees":"12.34","totalPnL":"232"}}"#,
        )
        .unwrap();

        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0].asset, "USDC");
        assert_eq!(balances[0].available, Decimal::from(3825));
        assert_eq!(balances[0].frozen, Decimal::from(1125));
        assert_eq!(balances[0].total, Decimal::from(4950));
        assert_eq!(balances[0].usd_value, Some(Decimal::from(4950)));
    }
}
