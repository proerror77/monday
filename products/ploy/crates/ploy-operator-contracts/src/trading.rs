use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntentPurpose {
    Entry,
    Exit,
    Reduce,
    Hedge,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TradingIntentSnapshot {
    pub intent_id: String,
    pub market_id: String,
    pub token_id: String,
    pub side: String,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
    pub purpose: IntentPurpose,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaperIntentRequest {
    #[serde(default)]
    pub idempotency_key: Option<String>,
    pub market_id: String,
    pub token_id: String,
    pub side: String,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
    pub purpose: IntentPurpose,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaperIntentResponse {
    pub deployment_id: String,
    pub intent_id: String,
    pub order_id: String,
    pub state: String,
    pub venue_order_id: Option<String>,
    pub rejection_reason: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OrderReplaceRequest {
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OrderControlResponse {
    pub deployment_id: String,
    pub order_id: String,
    pub state: String,
    pub venue_order_id: Option<String>,
    #[serde(default)]
    pub venue_order_history: Vec<String>,
    #[serde(default)]
    pub revision: u32,
    pub requested_qty: Decimal,
    pub limit_price: Option<Decimal>,
    pub rejection_reason: Option<String>,
    pub last_error: Option<String>,
    pub filled_qty: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OrderSnapshot {
    pub order_id: String,
    pub intent_id: String,
    pub token_id: String,
    pub requested_qty: Decimal,
    pub limit_price: Option<Decimal>,
    pub venue_order_id: Option<String>,
    #[serde(default)]
    pub venue_order_history: Vec<String>,
    #[serde(default)]
    pub revision: u32,
    pub state: String,
    #[serde(default)]
    pub state_changed_at: Option<DateTime<Utc>>,
    pub filled_qty: Decimal,
    pub rejection_reason: Option<String>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FillSnapshot {
    pub fill_id: String,
    pub order_id: String,
    pub token_id: String,
    pub side: String,
    pub quantity: Decimal,
    pub price: Decimal,
    pub fee: Decimal,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PositionSnapshotResponse {
    pub token_id: String,
    pub net_qty: Decimal,
    pub avg_entry_price: Decimal,
    pub realized_pnl: Decimal,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PnlSnapshotResponse {
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_fees: Decimal,
    pub net_pnl: Decimal,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RiskSnapshotResponse {
    pub pending_intents: usize,
    pub active_orders: usize,
    pub open_positions: usize,
    pub gross_exposure: Decimal,
    pub reserved_order_exposure: Decimal,
    pub total_gross_exposure: Decimal,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TradingStateSnapshot {
    pub deployment_id: String,
    pub runtime_mode: crate::DeploymentRuntimeMode,
    pub intents: Vec<TradingIntentSnapshot>,
    pub orders: Vec<OrderSnapshot>,
    pub fills: Vec<FillSnapshot>,
    pub positions: Vec<PositionSnapshotResponse>,
    pub pnl: PnlSnapshotResponse,
    pub risk: RiskSnapshotResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TradeResponse {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub token_id: String,
    pub token_name: String,
    pub side: String,
    pub shares: i32,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub pnl: Option<f64>,
    pub status: String,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PositionResponse {
    pub token_id: String,
    pub token_name: String,
    pub side: String,
    pub shares: i32,
    pub entry_price: f64,
    pub current_price: f64,
    pub unrealized_pnl: f64,
    pub entry_time: DateTime<Utc>,
    pub duration_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MarketData {
    pub token_id: String,
    pub token_name: String,
    pub best_bid: f64,
    pub best_ask: f64,
    pub spread: f64,
    pub last_price: f64,
    pub volume_24h: f64,
    pub timestamp: DateTime<Utc>,
}

/// Time-remaining regime for a binary option market.
/// Shared between research (backtesting) and live strategy runtime.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Regime {
    /// 181..=300 seconds remaining.
    Early,
    /// 61..=180 seconds remaining.
    Middle,
    /// 6..=60 seconds remaining.
    Late,
    /// 0..=5 seconds remaining.
    Expiry,
}

impl Regime {
    pub fn from_secs(t: i64) -> Self {
        match t {
            181..=300 => Regime::Early,
            61..=180 => Regime::Middle,
            6..=60 => Regime::Late,
            _ => Regime::Expiry,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Regime::Early => "early",
            Regime::Middle => "middle",
            Regime::Late => "late",
            Regime::Expiry => "expiry",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IntentPurpose, OrderControlResponse, OrderReplaceRequest, OrderSnapshot,
        PaperIntentRequest, PaperIntentResponse, PnlSnapshotResponse, RiskSnapshotResponse,
        TradeResponse, TradingIntentSnapshot, TradingStateSnapshot,
    };
    use chrono::Utc;
    use rust_decimal::Decimal;
    use serde_json::json;

    #[test]
    fn intent_purpose_serializes_as_snake_case() {
        let json = serde_json::to_string(&IntentPurpose::Entry).expect("serialize");
        assert_eq!(json, "\"entry\"");
    }

    #[test]
    fn trade_response_uses_existing_wire_fields() {
        let timestamp = Utc::now();
        let value = serde_json::to_value(TradeResponse {
            id: "cycle-1".to_string(),
            timestamp,
            token_id: "token-1".to_string(),
            token_name: "BTC".to_string(),
            side: "UP".to_string(),
            shares: 5,
            entry_price: 0.42,
            exit_price: Some(0.61),
            pnl: Some(0.95),
            status: "COMPLETED".to_string(),
            error_message: None,
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "id": "cycle-1",
                "timestamp": timestamp,
                "token_id": "token-1",
                "token_name": "BTC",
                "side": "UP",
                "shares": 5,
                "entry_price": 0.42,
                "exit_price": 0.61,
                "pnl": 0.95,
                "status": "COMPLETED",
                "error_message": null,
            })
        );
    }

    #[test]
    fn order_control_response_uses_stable_wire_keys() {
        let value = serde_json::to_value(OrderControlResponse {
            deployment_id: "example.live".to_string(),
            order_id: "order-1".to_string(),
            state: "canceled".to_string(),
            venue_order_id: Some("venue-1".to_string()),
            venue_order_history: vec!["venue-0".to_string()],
            revision: 1,
            requested_qty: Decimal::new(250, 2),
            limit_price: Some(Decimal::new(42, 2)),
            rejection_reason: None,
            last_error: None,
            filled_qty: Decimal::ZERO,
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "deployment_id": "example.live",
                "order_id": "order-1",
                "state": "canceled",
                "venue_order_id": "venue-1",
                "venue_order_history": ["venue-0"],
                "revision": 1,
                "requested_qty": "2.50",
                "limit_price": "0.42",
                "rejection_reason": null,
                "last_error": null,
                "filled_qty": "0",
            })
        );
    }

    #[test]
    fn order_replace_request_uses_stable_wire_keys() {
        let value = serde_json::to_value(OrderReplaceRequest {
            quantity: Decimal::new(250, 2),
            limit_price: Some(Decimal::new(42, 2)),
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "quantity": "2.50",
                "limit_price": "0.42",
            })
        );
    }

    #[test]
    fn trading_state_snapshot_uses_stable_wire_keys() {
        let timestamp = Utc::now();
        let value = serde_json::to_value(TradingStateSnapshot {
            deployment_id: "example.paper".to_string(),
            runtime_mode: crate::DeploymentRuntimeMode::Paper,
            intents: vec![TradingIntentSnapshot {
                intent_id: "intent-1".to_string(),
                market_id: "market-1".to_string(),
                token_id: "token-1".to_string(),
                side: "buy".to_string(),
                quantity: rust_decimal::Decimal::ONE,
                limit_price: None,
                purpose: IntentPurpose::Entry,
                created_at: timestamp,
            }],
            orders: vec![OrderSnapshot {
                order_id: "order-1".to_string(),
                intent_id: "intent-1".to_string(),
                token_id: "token-1".to_string(),
                requested_qty: rust_decimal::Decimal::ONE,
                limit_price: None,
                venue_order_id: Some("venue-1".to_string()),
                venue_order_history: vec!["venue-0".to_string()],
                revision: 1,
                state: "filled".to_string(),
                state_changed_at: Some(timestamp),
                filled_qty: rust_decimal::Decimal::ONE,
                rejection_reason: None,
                last_error: None,
                idempotency_key: None,
            }],
            fills: vec![],
            positions: vec![],
            pnl: PnlSnapshotResponse::default(),
            risk: RiskSnapshotResponse::default(),
        })
        .expect("to_value");

        assert_eq!(
            value,
            json!({
                "deployment_id": "example.paper",
                "runtime_mode": "paper",
                "intents": [{
                    "intent_id": "intent-1",
                    "market_id": "market-1",
                    "token_id": "token-1",
                    "side": "buy",
                    "quantity": "1",
                    "limit_price": null,
                    "purpose": "entry",
                    "created_at": timestamp,
                }],
                "orders": [{
                    "order_id": "order-1",
                    "intent_id": "intent-1",
                    "token_id": "token-1",
                    "requested_qty": "1",
                    "limit_price": null,
                    "venue_order_id": "venue-1",
                    "venue_order_history": ["venue-0"],
                    "revision": 1,
                    "state": "filled",
                    "state_changed_at": timestamp,
                    "filled_qty": "1",
                    "rejection_reason": null,
                    "last_error": null,
                    "idempotency_key": null,
                }],
                "fills": [],
                "positions": [],
                "pnl": {
                    "realized_pnl": "0",
                    "unrealized_pnl": "0",
                    "total_fees": "0",
                    "net_pnl": "0",
                },
                "risk": {
                    "pending_intents": 0,
                    "active_orders": 0,
                    "open_positions": 0,
                    "gross_exposure": "0",
                    "reserved_order_exposure": "0",
                    "total_gross_exposure": "0",
                },
            })
        );
    }

    #[test]
    fn paper_intent_contract_uses_stable_wire_keys() {
        let request = serde_json::to_value(PaperIntentRequest {
            idempotency_key: Some("request-1".to_string()),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: "buy".to_string(),
            quantity: rust_decimal::Decimal::ONE,
            limit_price: Some(rust_decimal::Decimal::ONE),
            purpose: IntentPurpose::Entry,
        })
        .expect("request json");
        assert_eq!(
            request,
            json!({
                "idempotency_key": "request-1",
                "market_id": "market-1",
                "token_id": "token-1",
                "side": "buy",
                "quantity": "1",
                "limit_price": "1",
                "purpose": "entry",
            })
        );

        let response = serde_json::to_value(PaperIntentResponse {
            deployment_id: "example.paper".to_string(),
            intent_id: "intent-1".to_string(),
            order_id: "order-intent-1".to_string(),
            state: "acknowledged".to_string(),
            venue_order_id: Some("venue-order-1".to_string()),
            rejection_reason: None,
            last_error: None,
        })
        .expect("response json");
        assert_eq!(
            response,
            json!({
                "deployment_id": "example.paper",
                "intent_id": "intent-1",
                "order_id": "order-intent-1",
                "state": "acknowledged",
                "venue_order_id": "venue-order-1",
                "rejection_reason": null,
                "last_error": null,
            })
        );
    }
}
