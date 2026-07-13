use async_trait::async_trait;
use binance_sdk::{
    config::ConfigurationRestApi,
    errors::ConnectorError,
    w3w_prediction::{
        rest_api::{
            BatchCancelOrdersCancelInfoListParameterInner, BatchCancelOrdersParams,
            GetMarketDetailParams, GetQuoteFundingSourceEnum, GetQuoteOrderTypeEnum,
            GetQuoteParams, GetQuoteSideEnum, ListPredictionWalletsParams,
            PlaceOrderAccountTypeEnum, PlaceOrderFundingSourceEnum, PlaceOrderOrderTypeEnum,
            PlaceOrderParams, QueryActiveOrdersParams, QueryPaymentOptionBalancesParams, RestApi,
        },
        W3WPredictionRestApi,
    },
};
pub use execution::ExecutionMode;
use futures::StreamExt;
use hft_core::{
    HftError, HftResult, OrderId, OrderType, Price, ProductType, Quantity, Side, Symbol,
    TimeInForce, VenueId,
};
use ports::{
    AccountBalance, BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent, OpenOrder,
    OrderIntent, OrderIntentEnvelope, OrderStatus,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PredictionAccountType {
    Spot,
    Funding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PredictionFundingSource {
    Mpc,
    Cex,
}

fn default_timeout_ms() -> u64 {
    1_000
}

#[derive(Debug, Clone, Deserialize)]
pub struct BinancePredictionVenueConfig {
    pub wallet_address: String,
    pub wallet_id: String,
    pub account_type: PredictionAccountType,
    pub funding_source: PredictionFundingSource,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct BinancePredictionExecutionConfig {
    pub api_key: String,
    pub api_secret: String,
    pub wallet_address: String,
    pub wallet_id: String,
    pub rest_base_url: String,
    pub timeout_ms: u64,
    pub mode: ExecutionMode,
    pub account_type: PredictionAccountType,
    pub funding_source: PredictionFundingSource,
}

#[derive(Debug)]
pub struct BinancePredictionExecutionClient {
    api: Option<RestApi>,
    config: BinancePredictionExecutionConfig,
    event_tx: broadcast::Sender<ExecutionEvent>,
    connected: bool,
}

impl BinancePredictionExecutionClient {
    pub fn new(config: BinancePredictionExecutionConfig) -> HftResult<Self> {
        if config.mode == ExecutionMode::Testnet {
            return Err(HftError::Config(
                "Binance Prediction does not expose a testnet; use Paper or Live".to_string(),
            ));
        }
        let api = if config.mode == ExecutionMode::Live {
            if config.api_key.is_empty() || config.api_secret.is_empty() {
                return Err(HftError::Authentication(
                    "Binance Prediction live execution requires API credentials".to_string(),
                ));
            }
            if config.wallet_address.is_empty() || config.wallet_id.is_empty() {
                return Err(HftError::Config(
                    "Binance Prediction live execution requires wallet_address and wallet_id"
                        .to_string(),
                ));
            }
            let sdk_config = ConfigurationRestApi::builder()
                .api_key(config.api_key.clone())
                .api_secret(config.api_secret.clone())
                .base_path(config.rest_base_url.clone())
                .timeout(config.timeout_ms)
                .keep_alive(false)
                // binance-sdk 61.0.0 underflows its retry counter at 0. A value of 1 still
                // performs no retry for POST, which keeps order submission non-retriable.
                .retries(1)
                .build()
                .map_err(|error| HftError::Config(error.to_string()))?;
            Some(W3WPredictionRestApi::from_config(sdk_config))
        } else {
            None
        };

        let (event_tx, _) = broadcast::channel(256);
        Ok(Self {
            api,
            config,
            event_tx,
            connected: false,
        })
    }

    fn validate_intent(intent: &OrderIntent) -> HftResult<()> {
        if intent.product_type != ProductType::PredictionMarket
            || intent.target_venue != Some(VenueId::BINANCE_PREDICTION)
        {
            return Err(HftError::InvalidOrder(
                "Binance Prediction adapter only accepts PredictionMarket intents for BINANCE_PREDICTION"
                    .to_string(),
            ));
        }
        match intent.order_type {
            OrderType::Market if intent.time_in_force != TimeInForce::FOK => Err(
                HftError::InvalidOrder("Binance Prediction MARKET orders require FOK".to_string()),
            ),
            OrderType::Limit if intent.time_in_force != TimeInForce::GTC => Err(
                HftError::InvalidOrder("Binance Prediction LIMIT orders require GTC".to_string()),
            ),
            OrderType::Limit if intent.price.is_none() => Err(HftError::InvalidOrder(
                "Binance Prediction LIMIT orders require a price".to_string(),
            )),
            _ => Ok(()),
        }
    }

    fn map_connector_error(error: ConnectorError, outcome_unknown: bool) -> HftError {
        match error {
            ConnectorError::UnauthorizedError { msg, .. }
            | ConnectorError::ForbiddenError { msg, .. } => HftError::Authentication(msg),
            ConnectorError::TooManyRequestsError { msg, .. }
            | ConnectorError::RateLimitBanError { msg, .. } => HftError::RateLimit(msg),
            ConnectorError::NetworkError(message) => {
                if outcome_unknown {
                    HftError::Network(format!(
                        "Binance Prediction submission outcome unknown: {message}"
                    ))
                } else {
                    HftError::Network(message)
                }
            }
            ConnectorError::ServerError { msg, .. } if outcome_unknown => HftError::Network(
                format!("Binance Prediction submission outcome unknown: {msg}"),
            ),
            ConnectorError::ConnectorClientError { msg, .. }
                if outcome_unknown && msg.contains("HTTP request failed") =>
            {
                HftError::Network(format!(
                    "Binance Prediction submission outcome unknown: {msg}"
                ))
            }
            other => HftError::Exchange(other.to_string()),
        }
    }

    fn map_sdk_error(error: anyhow::Error, outcome_unknown: bool) -> HftError {
        if let Some(connector) = error.downcast_ref::<ConnectorError>() {
            return match connector {
                ConnectorError::UnauthorizedError { msg, .. }
                | ConnectorError::ForbiddenError { msg, .. } => {
                    HftError::Authentication(msg.clone())
                }
                ConnectorError::TooManyRequestsError { msg, .. }
                | ConnectorError::RateLimitBanError { msg, .. } => HftError::RateLimit(msg.clone()),
                ConnectorError::NetworkError(message) => {
                    if outcome_unknown {
                        HftError::Network(format!(
                            "Binance Prediction submission outcome unknown: {message}"
                        ))
                    } else {
                        HftError::Network(message.clone())
                    }
                }
                ConnectorError::ServerError { msg, .. } if outcome_unknown => HftError::Network(
                    format!("Binance Prediction submission outcome unknown: {msg}"),
                ),
                ConnectorError::ConnectorClientError { msg, .. }
                    if outcome_unknown && msg.contains("HTTP request failed") =>
                {
                    HftError::Network(format!(
                        "Binance Prediction submission outcome unknown: {msg}"
                    ))
                }
                _ => HftError::Exchange(connector.to_string()),
            };
        }
        if outcome_unknown {
            HftError::Network(format!(
                "Binance Prediction submission outcome unknown: {error}"
            ))
        } else {
            HftError::Exchange(error.to_string())
        }
    }

    fn amount_in_wei(quantity: Quantity) -> HftResult<String> {
        let scaled = quantity.0 * Decimal::from_i128_with_scale(1_000_000_000_000_000_000, 0);
        if scaled.fract() != Decimal::ZERO {
            return Err(HftError::InvalidOrder(
                "Binance Prediction quantity supports at most 18 decimal places".to_string(),
            ));
        }
        Ok(scaled.trunc().to_string())
    }

    fn amount_from_wei(value: &str, field: &str) -> HftResult<Quantity> {
        let raw = value.parse::<Decimal>().map_err(|error| {
            HftError::Parse(format!("Binance Prediction {field} is invalid: {error}"))
        })?;
        if raw < Decimal::ZERO {
            return Err(HftError::Parse(format!(
                "Binance Prediction {field} must be non-negative"
            )));
        }
        Ok(Quantity(
            (raw / Decimal::from_i128_with_scale(1_000_000_000_000_000_000, 0)).normalize(),
        ))
    }

    fn quote_funding_source(&self) -> GetQuoteFundingSourceEnum {
        match self.config.funding_source {
            PredictionFundingSource::Mpc => GetQuoteFundingSourceEnum::Mpc,
            PredictionFundingSource::Cex => GetQuoteFundingSourceEnum::Cex,
        }
    }

    fn order_funding_source(&self) -> PlaceOrderFundingSourceEnum {
        match self.config.funding_source {
            PredictionFundingSource::Mpc => PlaceOrderFundingSourceEnum::Mpc,
            PredictionFundingSource::Cex => PlaceOrderFundingSourceEnum::Cex,
        }
    }

    fn account_type(&self) -> PlaceOrderAccountTypeEnum {
        match self.config.account_type {
            PredictionAccountType::Spot => PlaceOrderAccountTypeEnum::Spot,
            PredictionAccountType::Funding => PlaceOrderAccountTypeEnum::Funding,
        }
    }
}

#[async_trait]
impl ExecutionClient for BinancePredictionExecutionClient {
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        Self::validate_intent(&intent)?;
        if self.config.mode == ExecutionMode::Live {
            return Err(HftError::InvalidOrder(
                "Binance Prediction live orders require OrderIntentEnvelope.max_slippage_bps"
                    .to_string(),
            ));
        }
        Ok(OrderId(format!(
            "BINANCE_PREDICTION_PAPER_{:x}",
            hft_core::now_micros()
        )))
    }

    async fn place_order_envelope(&mut self, envelope: &OrderIntentEnvelope) -> HftResult<OrderId> {
        Self::validate_intent(&envelope.intent)?;
        envelope
            .validate_pre_execution(hft_core::now_micros(), None)
            .map_err(|reason| {
                HftError::InvalidOrder(format!("stale prediction order: {reason:?}"))
            })?;
        let slippage_bps = envelope.lifecycle.max_slippage_bps.ok_or_else(|| {
            HftError::InvalidOrder(
                "Binance Prediction live orders require max_slippage_bps".to_string(),
            )
        })?;
        if !(1..=10_000).contains(&slippage_bps) {
            return Err(HftError::InvalidOrder(
                "Binance Prediction max_slippage_bps must be in 1..=10000".to_string(),
            ));
        }
        if self.config.mode != ExecutionMode::Live {
            return self.place_order(envelope.intent.clone()).await;
        }
        if !self.connected {
            return Err(HftError::Network(
                "Binance Prediction client is not connected".to_string(),
            ));
        }
        let api = self.api.as_ref().ok_or_else(|| {
            HftError::Config("Binance Prediction API client is not configured".to_string())
        })?;
        let quote_side = match envelope.intent.side {
            hft_core::Side::Buy => GetQuoteSideEnum::Buy,
            hft_core::Side::Sell => GetQuoteSideEnum::Sell,
        };
        let expected_side = match envelope.intent.side {
            hft_core::Side::Buy => "BUY",
            hft_core::Side::Sell => "SELL",
        };
        let (quote_order_type, place_order_type, time_in_force) = match envelope.intent.order_type {
            OrderType::Market => (
                GetQuoteOrderTypeEnum::Market,
                PlaceOrderOrderTypeEnum::Market,
                "FOK",
            ),
            OrderType::Limit => (
                GetQuoteOrderTypeEnum::Limit,
                PlaceOrderOrderTypeEnum::Limit,
                "GTC",
            ),
        };
        let expected_order_type = match envelope.intent.order_type {
            OrderType::Market => "MARKET",
            OrderType::Limit => "LIMIT",
        };
        let price_limit = envelope.intent.price.map(|price| price.0.to_string());
        let amount_in = Self::amount_in_wei(envelope.intent.quantity)?;
        let mut quote = GetQuoteParams::builder(
            self.config.wallet_address.clone(),
            envelope.intent.symbol.as_str().to_string(),
            quote_side,
            amount_in.clone(),
            quote_order_type,
            slippage_bps,
        )
        .funding_source(self.quote_funding_source());
        if let Some(price_limit) = price_limit.clone() {
            quote = quote.price_limit(price_limit);
        }
        let quote = api
            .get_quote(
                quote
                    .build()
                    .map_err(|error| HftError::InvalidOrder(error.to_string()))?,
            )
            .await
            .map_err(|error| Self::map_sdk_error(error, false))?
            .data()
            .await
            .map_err(|error| Self::map_connector_error(error, false))?;
        let quote_id = quote
            .quote_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                HftError::Exchange("Binance Prediction quote response omitted quoteId".to_string())
            })?;
        if quote.token_id.as_deref() != Some(envelope.intent.symbol.as_str()) {
            return Err(HftError::Exchange(
                "Binance Prediction quote returned a mismatched tokenId".to_string(),
            ));
        }
        if quote.slippage_bps != Some(slippage_bps) {
            return Err(HftError::Exchange(
                "Binance Prediction quote returned mismatched slippage".to_string(),
            ));
        }
        if quote.wallet_address.as_deref() != Some(self.config.wallet_address.as_str()) {
            return Err(HftError::Exchange(
                "Binance Prediction quote returned a mismatched wallet".to_string(),
            ));
        }
        if quote.side.as_deref() != Some(expected_side)
            || quote.order_type.as_deref() != Some(expected_order_type)
            || quote.amount_in.as_deref() != Some(amount_in.as_str())
        {
            return Err(HftError::Exchange(
                "Binance Prediction quote did not match the requested side, type, or amount"
                    .to_string(),
            ));
        }
        if quote.expire_at.is_some_and(|expire_at| {
            expire_at <= i64::try_from(hft_core::now_micros() / 1_000).unwrap_or(i64::MAX)
        }) {
            return Err(HftError::Exchange(
                "Binance Prediction quote expired before placement".to_string(),
            ));
        }

        let mut place = PlaceOrderParams::builder(
            self.config.wallet_address.clone(),
            self.config.wallet_id.clone(),
            quote_id,
            time_in_force.to_string(),
            self.account_type(),
            place_order_type,
            slippage_bps,
        )
        .funding_source(self.order_funding_source());
        if let Some(price_limit) = price_limit {
            place = place.price_limit(price_limit);
        }
        let response = api
            .place_order(
                place
                    .build()
                    .map_err(|error| HftError::InvalidOrder(error.to_string()))?,
            )
            .await
            .map_err(|error| Self::map_sdk_error(error, true))?
            .data()
            .await
            .map_err(|error| Self::map_connector_error(error, true))?;
        let order_id = OrderId(
            response
                .order_id
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    HftError::Exchange("Binance Prediction response omitted orderId".to_string())
                })?,
        );
        let _ = self.event_tx.send(ExecutionEvent::OrderAck {
            order_id: order_id.clone(),
            timestamp: hft_core::now_micros(),
        });
        Ok(order_id)
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        if self.config.mode != ExecutionMode::Live {
            let _ = self.event_tx.send(ExecutionEvent::OrderCanceled {
                order_id: order_id.clone(),
                timestamp: hft_core::now_micros(),
            });
            return Ok(());
        }
        if !self.connected {
            return Err(HftError::Network(
                "Binance Prediction client is not connected".to_string(),
            ));
        }
        let api = self.api.as_ref().ok_or_else(|| {
            HftError::Config("Binance Prediction API client is not configured".to_string())
        })?;
        let response = api
            .batch_cancel_orders(
                BatchCancelOrdersParams::builder(
                    self.config.wallet_address.clone(),
                    self.config.wallet_id.clone(),
                )
                .cancel_info_list(vec![BatchCancelOrdersCancelInfoListParameterInner::new(
                    order_id.0.clone(),
                )])
                .build()
                .map_err(|error| HftError::InvalidOrder(error.to_string()))?,
            )
            .await
            .map_err(|error| Self::map_sdk_error(error, true))?
            .data()
            .await
            .map_err(|error| Self::map_connector_error(error, true))?;
        if !response
            .canceled
            .as_deref()
            .unwrap_or_default()
            .contains(&order_id.0)
        {
            let reason = response
                .failed
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find(|failed| failed.order_id.as_deref() == Some(order_id.0.as_str()))
                .and_then(|failed| failed.reason.clone())
                .unwrap_or_else(|| "cancel was not confirmed".to_string());
            return Err(HftError::Exchange(format!(
                "Binance Prediction cancel failed for {}: {reason}",
                order_id.0
            )));
        }
        let _ = self.event_tx.send(ExecutionEvent::OrderCanceled {
            order_id: order_id.clone(),
            timestamp: hft_core::now_micros(),
        });
        Ok(())
    }

    async fn modify_order(
        &mut self,
        _order_id: &OrderId,
        _new_quantity: Option<Quantity>,
        _new_price: Option<hft_core::Price>,
    ) -> HftResult<()> {
        Err(HftError::InvalidOrder(
            "Binance Prediction does not support in-place order modification; cancel and replace"
                .to_string(),
        ))
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        let stream =
            BroadcastStream::new(self.event_tx.subscribe()).filter_map(|item| async move {
                match item {
                    Ok(event) => Some(Ok(event)),
                    Err(error) => Some(Err(HftError::Network(format!(
                        "Binance Prediction execution stream lagged: {error}"
                    )))),
                }
            });
        Ok(Box::pin(stream))
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        if self.config.mode != ExecutionMode::Live {
            return Ok(Vec::new());
        }
        if !self.connected {
            return Err(HftError::Network(
                "Binance Prediction client is not connected".to_string(),
            ));
        }
        let api = self.api.as_ref().ok_or_else(|| {
            HftError::Config("Binance Prediction API client is not configured".to_string())
        })?;
        let response = api
            .query_active_orders(
                QueryActiveOrdersParams::builder(self.config.wallet_address.clone())
                    .limit(100)
                    .build()
                    .map_err(|error| HftError::Config(error.to_string()))?,
            )
            .await
            .map_err(|error| HftError::Exchange(error.to_string()))?
            .data()
            .await
            .map_err(|error| HftError::Exchange(error.to_string()))?;
        let mut open_orders = Vec::new();
        for order in response.orders.unwrap_or_default() {
            let order_id = order
                .order_id
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    HftError::Parse("Binance Prediction open order omitted orderId".to_string())
                })?;
            let side = match order.side.as_deref() {
                Some("BUY") => Side::Buy,
                Some("SELL") => Side::Sell,
                other => {
                    return Err(HftError::Parse(format!(
                        "Binance Prediction open order {order_id} has invalid side {other:?}"
                    )))
                }
            };
            let order_type = match order.order_type.as_deref() {
                Some("MARKET") => OrderType::Market,
                Some("LIMIT") => OrderType::Limit,
                other => {
                    return Err(HftError::Parse(format!(
                        "Binance Prediction open order {order_id} has invalid type {other:?}"
                    )))
                }
            };
            let (original_raw, filled_raw) = match side {
                Side::Buy => (order.maker_usdt_amount, order.filled_usdt_amount),
                Side::Sell => (order.maker_share_qty, order.filled_share_qty),
            };
            let original_quantity = Self::amount_from_wei(
                original_raw.as_deref().ok_or_else(|| {
                    HftError::Parse(format!(
                        "Binance Prediction open order {order_id} omitted original amount"
                    ))
                })?,
                "original amount",
            )?;
            let filled_quantity =
                Self::amount_from_wei(filled_raw.as_deref().unwrap_or("0"), "filled amount")?;
            if filled_quantity.0 > original_quantity.0 {
                return Err(HftError::Parse(format!(
                    "Binance Prediction open order {order_id} filled amount exceeds original amount"
                )));
            }
            let market_topic_id = order.market_topic_id.ok_or_else(|| {
                HftError::Parse(format!(
                    "Binance Prediction open order {order_id} omitted marketTopicId"
                ))
            })?;
            let market_id = order.market_id.ok_or_else(|| {
                HftError::Parse(format!(
                    "Binance Prediction open order {order_id} omitted marketId"
                ))
            })?;
            let outcome_index = order.outcome_index.ok_or_else(|| {
                HftError::Parse(format!(
                    "Binance Prediction open order {order_id} omitted outcomeIndex"
                ))
            })?;
            let detail = api
                .get_market_detail(
                    GetMarketDetailParams::builder(market_topic_id)
                        .build()
                        .map_err(|error| HftError::Config(error.to_string()))?,
                )
                .await
                .map_err(|error| HftError::Exchange(error.to_string()))?
                .data()
                .await
                .map_err(|error| HftError::Exchange(error.to_string()))?;
            let token_id = detail
                .markets
                .unwrap_or_default()
                .into_iter()
                .find(|market| market.market_id == Some(market_id))
                .and_then(|market| market.outcomes)
                .unwrap_or_default()
                .into_iter()
                .find(|outcome| outcome.index == Some(outcome_index))
                .and_then(|outcome| outcome.token_id)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    HftError::Parse(format!(
                        "Binance Prediction could not resolve tokenId for order {order_id}"
                    ))
                })?;
            let status = match order.status.as_deref() {
                Some("PARTIALLY_FILLED") => OrderStatus::PartiallyFilled,
                Some("NEW" | "OPEN" | "ACTIVE") | None => OrderStatus::New,
                Some(value) => {
                    return Err(HftError::Parse(format!(
                        "Binance Prediction open order {order_id} has invalid status {value}"
                    )))
                }
            };
            let price = match order_type {
                OrderType::Market => None,
                OrderType::Limit => Some(
                    Price::from_str(order.price.as_deref().ok_or_else(|| {
                        HftError::Parse(format!(
                            "Binance Prediction limit order {order_id} omitted price"
                        ))
                    })?)
                    .map_err(|error| HftError::Parse(error.to_string()))?,
                ),
            };
            let created_at = u64::try_from(order.create_time.unwrap_or_default())
                .ok()
                .and_then(|value| value.checked_mul(1_000))
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    HftError::Parse(format!(
                        "Binance Prediction open order {order_id} has invalid createTime"
                    ))
                })?;
            let updated_at = u64::try_from(order.modify_time.unwrap_or_default())
                .ok()
                .and_then(|value| value.checked_mul(1_000))
                .filter(|value| *value >= created_at)
                .ok_or_else(|| {
                    HftError::Parse(format!(
                        "Binance Prediction open order {order_id} has invalid modifyTime"
                    ))
                })?;
            open_orders.push(OpenOrder {
                order_id: OrderId(order_id),
                client_order_id: None,
                symbol: Symbol::new(token_id),
                side,
                order_type,
                original_quantity,
                remaining_quantity: Quantity(original_quantity.0 - filled_quantity.0),
                filled_quantity,
                price,
                status,
                created_at,
                updated_at,
            });
        }
        Ok(open_orders)
    }

    async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
        if self.config.mode != ExecutionMode::Live {
            return Ok(Vec::new());
        }
        if !self.connected {
            return Err(HftError::Network(
                "Binance Prediction client is not connected".to_string(),
            ));
        }
        let api = self.api.as_ref().ok_or_else(|| {
            HftError::Config("Binance Prediction API client is not configured".to_string())
        })?;
        let response = api
            .query_payment_option_balances(
                QueryPaymentOptionBalancesParams::builder()
                    .build()
                    .map_err(|error| HftError::Config(error.to_string()))?,
            )
            .await
            .map_err(|error| HftError::Exchange(error.to_string()))?
            .data()
            .await
            .map_err(|error| HftError::Exchange(error.to_string()))?;
        let account_type = match self.config.account_type {
            PredictionAccountType::Spot => "SPOT",
            PredictionAccountType::Funding => "FUNDING",
        };
        let item = response
            .items
            .unwrap_or_default()
            .into_iter()
            .find(|item| {
                item.enabled.unwrap_or(false) && item.account_type.as_deref() == Some(account_type)
            })
            .ok_or_else(|| {
                HftError::Config(format!(
                    "Binance Prediction payment account {account_type} is not enabled"
                ))
            })?;
        let available = item
            .available_balance_display
            .as_deref()
            .ok_or_else(|| {
                HftError::Parse(
                    "Binance Prediction payment balance omitted availableBalanceDisplay"
                        .to_string(),
                )
            })?
            .parse::<Decimal>()
            .map_err(|error| HftError::Parse(error.to_string()))?;
        if available < Decimal::ZERO {
            return Err(HftError::Parse(
                "Binance Prediction payment balance must be non-negative".to_string(),
            ));
        }
        Ok(vec![AccountBalance {
            asset: "USDT".to_string(),
            available,
            frozen: Decimal::ZERO,
            total: available,
            usd_value: Some(available),
        }])
    }

    async fn connect(&mut self) -> HftResult<()> {
        if self.config.mode == ExecutionMode::Live {
            let api = self.api.as_ref().ok_or_else(|| {
                HftError::Config("Binance Prediction API client is not configured".to_string())
            })?;
            let response = api
                .list_prediction_wallets(
                    ListPredictionWalletsParams::builder()
                        .build()
                        .map_err(|error| HftError::Config(error.to_string()))?,
                )
                .await
                .map_err(|error| HftError::Authentication(error.to_string()))?
                .data()
                .await
                .map_err(|error| HftError::Authentication(error.to_string()))?;
            let registered = response
                .wallets
                .unwrap_or_default()
                .into_iter()
                .any(|wallet| {
                    wallet.wallet_id.as_deref() == Some(self.config.wallet_id.as_str())
                        && wallet.wallet_address.as_deref().is_some_and(|address| {
                            address.eq_ignore_ascii_case(&self.config.wallet_address)
                        })
                });
            if !registered {
                return Err(HftError::Authentication(
                    "Binance Prediction configured wallet is not registered for this API account"
                        .to_string(),
                ));
            }
        }
        self.connected = true;
        let _ = self.event_tx.send(ExecutionEvent::ConnectionStatus {
            connected: true,
            timestamp: hft_core::now_micros(),
        });
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.connected = false;
        let _ = self.event_tx.send(ExecutionEvent::ConnectionStatus {
            connected: false,
            timestamp: hft_core::now_micros(),
        });
        Ok(())
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.connected,
            latency_ms: None,
            last_heartbeat: hft_core::now_micros(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{
        AssetClass, ComplianceContext, OrderType, ProductType, Quantity, Side, Symbol, TimeInForce,
        VenueId,
    };
    use ports::{ExecutionClient, OrderIntent, OrderIntentEnvelope, OrderIntentLifecycle};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn prediction_api_server() -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for body in [
                r#"{"wallets":[{"walletAddress":"0x1234","walletId":"wallet-1","registeredTime":1}]}"#,
                r#"{"quoteId":"quote-1","tokenId":"112233","side":"BUY","amountIn":"2500000000000000000","orderType":"MARKET","slippageBps":75,"walletAddress":"0x1234"}"#,
                r#"{"orderId":"prediction-order-1"}"#,
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 16 * 1024];
                let size = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..size]);
                requests.push(request.lines().next().unwrap_or_default().to_string());
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (format!("http://{address}"), task)
    }

    async fn authenticated_response_server(
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for body in [
                r#"{"wallets":[{"walletAddress":"0x1234","walletId":"wallet-1","registeredTime":1}]}"#,
                body,
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 16 * 1024];
                let size = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..size]);
                requests.push(request.lines().next().unwrap_or_default().to_string());
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (format!("http://{address}"), task)
    }

    async fn reconciliation_api_server() -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for body in [
                r#"{"wallets":[{"walletAddress":"0x1234","walletId":"wallet-1","registeredTime":1}]}"#,
                r#"{"total":1,"offset":0,"limit":20,"orders":[{"orderId":"prediction-order-1","marketTopicId":7,"marketId":42,"outcome":"YES","outcomeIndex":0,"status":"PARTIALLY_FILLED","side":"BUY","orderType":"LIMIT","createTime":1000,"modifyTime":2000,"makerUsdtAmount":"2500000000000000000","filledUsdtAmount":"1000000000000000000","price":"0.55"}]}"#,
                r#"{"marketTopicId":7,"markets":[{"marketId":42,"outcomes":[{"name":"YES","index":0,"tokenId":"112233"}]}]}"#,
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 16 * 1024];
                let size = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..size]);
                requests.push(request.lines().next().unwrap_or_default().to_string());
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (format!("http://{address}"), task)
    }

    async fn ambiguous_place_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for body in [
                r#"{"wallets":[{"walletAddress":"0x1234","walletId":"wallet-1","registeredTime":1}]}"#,
                r#"{"quoteId":"quote-1","tokenId":"112233","side":"BUY","amountIn":"2500000000000000000","orderType":"MARKET","slippageBps":75,"walletAddress":"0x1234"}"#,
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0_u8; 16 * 1024];
                let _ = socket.read(&mut request).await.unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 16 * 1024];
            let _ = socket.read(&mut request).await.unwrap();
        });
        format!("http://{address}")
    }

    fn live_config() -> BinancePredictionExecutionConfig {
        BinancePredictionExecutionConfig {
            api_key: "test-key".to_string(),
            api_secret: "test-secret".to_string(),
            wallet_address: "0x1234".to_string(),
            wallet_id: "wallet-1".to_string(),
            rest_base_url: "http://127.0.0.1:1".to_string(),
            timeout_ms: 100,
            mode: ExecutionMode::Live,
            account_type: PredictionAccountType::Spot,
            funding_source: PredictionFundingSource::Cex,
        }
    }

    fn prediction_intent() -> OrderIntent {
        OrderIntent {
            symbol: Symbol::new("112233"),
            asset_class: AssetClass::PredictionMarket,
            product_type: ProductType::PredictionMarket,
            compliance_context: ComplianceContext::default(),
            side: Side::Buy,
            quantity: Quantity::from_str("2.5").unwrap(),
            order_type: OrderType::Market,
            price: None,
            time_in_force: TimeInForce::FOK,
            strategy_id: "prediction-test".to_string(),
            target_venue: Some(VenueId::BINANCE_PREDICTION),
        }
    }

    #[test]
    fn live_configuration_requires_credentials_and_prediction_wallet() {
        let error = BinancePredictionExecutionClient::new(BinancePredictionExecutionConfig {
            api_key: String::new(),
            api_secret: String::new(),
            wallet_address: String::new(),
            wallet_id: String::new(),
            rest_base_url: "https://api.binance.com".to_string(),
            timeout_ms: 1_000,
            mode: ExecutionMode::Live,
            account_type: PredictionAccountType::Spot,
            funding_source: PredictionFundingSource::Cex,
        })
        .unwrap_err();

        assert!(error.to_string().contains("credentials"));

        let mut config = live_config();
        config.mode = ExecutionMode::Testnet;
        let error = BinancePredictionExecutionClient::new(config).unwrap_err();
        assert!(error.to_string().contains("does not expose a testnet"));
    }

    #[tokio::test]
    async fn live_order_requires_envelope_slippage_before_network_io() {
        let mut client = BinancePredictionExecutionClient::new(live_config()).unwrap();
        let envelope = OrderIntentEnvelope::new(
            prediction_intent(),
            OrderIntentLifecycle {
                created_ts: 1,
                valid_until: u64::MAX,
                ..OrderIntentLifecycle::default()
            },
        );

        let error = client.place_order_envelope(&envelope).await.unwrap_err();

        assert!(error.to_string().contains("max_slippage_bps"));
    }

    #[tokio::test]
    async fn live_envelope_gets_quote_then_places_prediction_order() {
        let (base_url, server) = prediction_api_server().await;
        let mut config = live_config();
        config.rest_base_url = base_url;
        let mut client = BinancePredictionExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();
        let envelope = OrderIntentEnvelope::new(
            prediction_intent(),
            OrderIntentLifecycle {
                created_ts: 1,
                valid_until: u64::MAX,
                max_slippage_bps: Some(75),
                ..OrderIntentLifecycle::default()
            },
        );

        let order_id = client.place_order_envelope(&envelope).await.unwrap();

        assert_eq!(order_id.0, "prediction-order-1");
        let requests = server.await.unwrap();
        assert!(requests[1].starts_with("POST /sapi/v1/w3w/wallet/prediction/trade/get-quote?"));
        assert!(requests[1].contains("amountIn=2500000000000000000"));
        assert!(requests[1].contains("tokenId=112233"));
        assert!(requests[2]
            .starts_with("POST /sapi/v1/w3w/wallet/prediction/trade/place-order-bundle?"));
        assert!(requests[2].contains("quoteId=quote-1"));
        assert!(requests[2].contains("walletId=wallet-1"));
    }

    #[tokio::test]
    async fn live_cancel_uses_prediction_batch_cancel_endpoint() {
        let (base_url, server) =
            authenticated_response_server(r#"{"canceled":["prediction-order-1"],"failed":[]}"#)
                .await;
        let mut config = live_config();
        config.rest_base_url = base_url;
        let mut client = BinancePredictionExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();

        client
            .cancel_order(&OrderId("prediction-order-1".to_string()))
            .await
            .unwrap();

        let requests = server.await.unwrap();
        assert!(requests[1].starts_with("POST /sapi/v1/w3w/wallet/prediction/trade/batch-cancel?"));
        assert!(requests[1].contains("prediction-order-1"));
        assert!(requests[1].contains("walletId=wallet-1"));
    }

    #[tokio::test]
    async fn open_orders_reconcile_to_outcome_token_and_input_amount() {
        let (base_url, server) = reconciliation_api_server().await;
        let mut config = live_config();
        config.rest_base_url = base_url;
        let mut client = BinancePredictionExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();

        let orders = client.list_open_orders().await.unwrap();

        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].order_id.0, "prediction-order-1");
        assert_eq!(orders[0].symbol.as_str(), "112233");
        assert_eq!(orders[0].original_quantity.0.to_string(), "2.5");
        assert_eq!(orders[0].filled_quantity.0.to_string(), "1");
        assert_eq!(orders[0].remaining_quantity.0.to_string(), "1.5");
        let requests = server.await.unwrap();
        assert!(requests[1].starts_with("GET /sapi/v1/w3w/wallet/prediction/order/list?"));
        assert!(requests[2]
            .starts_with("GET /sapi/v1/w3w/wallet/prediction/market/detail?marketTopicId=7"));
    }

    #[tokio::test]
    async fn balance_reconciliation_reads_prediction_payment_balance() {
        let (base_url, server) = authenticated_response_server(
            r#"{"items":[{"accountType":"SPOT","availableBalanceDisplay":"123.45","enabled":true}]}"#,
        )
        .await;
        let mut config = live_config();
        config.rest_base_url = base_url;
        let mut client = BinancePredictionExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();

        let balances = client.get_balance().await.unwrap();

        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0].asset, "USDT");
        assert_eq!(balances[0].available.to_string(), "123.45");
        assert_eq!(balances[0].usd_value.unwrap().to_string(), "123.45");
        let requests = server.await.unwrap();
        assert!(
            requests[1].starts_with("GET /sapi/v1/w3w/wallet/prediction/balance/payment-options?")
        );
    }

    #[tokio::test]
    async fn live_connect_rejects_unregistered_prediction_wallet() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 16 * 1024];
            let _ = socket.read(&mut request).await.unwrap();
            let body = r#"{"wallets":[{"walletAddress":"0x9999","walletId":"other-wallet","registeredTime":1}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let mut config = live_config();
        config.rest_base_url = format!("http://{address}");
        let mut client = BinancePredictionExecutionClient::new(config).unwrap();

        let error = client.connect().await.unwrap_err();

        assert!(error.to_string().contains("not registered"));
        assert!(!client.health().await.connected);
    }

    #[tokio::test]
    async fn place_transport_failure_is_reported_as_unknown_outcome() {
        let mut config = live_config();
        config.rest_base_url = ambiguous_place_server().await;
        let mut client = BinancePredictionExecutionClient::new(config).unwrap();
        client.connect().await.unwrap();
        let envelope = OrderIntentEnvelope::new(
            prediction_intent(),
            OrderIntentLifecycle {
                created_ts: hft_core::now_micros(),
                valid_until: u64::MAX,
                max_slippage_bps: Some(75),
                ..OrderIntentLifecycle::default()
            },
        );

        let error = client.place_order_envelope(&envelope).await.unwrap_err();

        assert!(matches!(error, HftError::Network(_)));
        assert!(error.to_string().contains("outcome unknown"));
    }
}
