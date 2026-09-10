use super::super::Portfolio;
use super::fills::{FillLedger, FillRecord};
use super::intents::{IntentPurpose, TradeSide, TradingIntent};
use super::orders::OrderRecord;
use super::orders::{OrderLedger, OrderState};
use super::pnl::PnlSnapshot;
use super::positions::{PositionLedger, PositionSnapshot};
use super::risk::{snapshot_from_state, RiskSnapshot};
use chrono::{DateTime, Utc};
use hft_core::{
    OrderId as CanonicalOrderId, Price as CanonicalPrice, Quantity as CanonicalQuantity,
    Side as CanonicalSide, Symbol as CanonicalSymbol,
};
use hft_oms_core::{
    OmsCore, OrderStatus as CanonicalOmsStatus, RegisterOrderParams as CanonicalRegisterOrderParams,
};
use ports::ExecutionEvent;
use ports::{OrderManager as CanonicalOrderManager, PortfolioManager as CanonicalPortfolioManager};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TradingRuntimeError {
    #[error("{0} must not be empty")]
    EmptyIdentifier(&'static str),
    #[error("{0} already exists")]
    DuplicateIdentifier(&'static str),
    #[error("{0}")]
    InvalidIntent(&'static str),
    #[error("canonical checkpoint missing from trading runtime snapshot")]
    MissingCanonicalCheckpoint,
    #[error("invalid canonical checkpoint: {0}")]
    InvalidCanonicalCheckpoint(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TradingRuntimeSnapshot {
    pub intents: Vec<TradingIntent>,
    pub orders: Vec<super::orders::OrderRecord>,
    pub fills: Vec<FillRecord>,
    pub positions: Vec<PositionSnapshot>,
    pub pnl: PnlSnapshot,
    pub risk: RiskSnapshot,
    #[serde(default)]
    pub canonical_oms: Option<ports::OmsCheckpoint>,
    #[serde(default)]
    pub canonical_portfolio: Option<ports::PortfolioState>,
    #[serde(default)]
    pub canonical_snapshot_digest: Option<String>,
}

impl PartialEq for TradingRuntimeSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.intents == other.intents
            && self.orders == other.orders
            && self.fills == other.fills
            && self.positions == other.positions
            && self.pnl == other.pnl
            && self.risk == other.risk
            && self.canonical_snapshot_digest == other.canonical_snapshot_digest
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TradeCashflowSummary {
    pub buy_shares: Decimal,
    pub sell_shares: Decimal,
    pub gross_buy_cost: Decimal,
    pub gross_sell_proceeds: Decimal,
    pub total_fees: Decimal,
}

impl TradeCashflowSummary {
    pub fn deployed_capital(&self) -> Decimal {
        self.gross_buy_cost
    }

    pub fn net_pnl(&self) -> Decimal {
        self.gross_sell_proceeds - self.gross_buy_cost - self.total_fees
    }

    pub fn roi_on_deployed_capital(&self) -> Option<Decimal> {
        let deployed = self.deployed_capital();
        if deployed.is_zero() {
            None
        } else {
            Some(self.net_pnl() / deployed)
        }
    }
}

impl TradingRuntimeSnapshot {
    pub fn fill_cashflow_summary(&self) -> TradeCashflowSummary {
        let mut summary = TradeCashflowSummary {
            total_fees: self.pnl.total_fees,
            ..TradeCashflowSummary::default()
        };
        // Canonical Portfolio is authoritative for fees. Fill rows may be
        // observed before a separate FeeCharged event or may be replayed from
        // REST after that event has already been applied.

        for fill in &self.fills {
            let notional = fill.quantity * fill.price;

            match fill.side {
                TradeSide::Buy => {
                    summary.buy_shares += fill.quantity;
                    summary.gross_buy_cost += notional;
                }
                TradeSide::Sell => {
                    summary.sell_shares += fill.quantity;
                    summary.gross_sell_proceeds += notional;
                }
            }
        }

        summary
    }

    pub fn integrity_digest(&self) -> String {
        let mut material = String::new();
        for intent in &self.intents {
            material.push_str(&format!(
                "intent:{}:{}:{}:{}:{:?}:{}:{:?}:{:?}:{:?};",
                intent.intent_id,
                intent.deployment_id,
                intent.market_id,
                intent.token_id,
                intent.side,
                intent.quantity,
                intent.limit_price,
                intent.purpose,
                intent.created_at
            ));
        }
        let mut orders = self.orders.clone();
        orders.sort_by(|left, right| left.order_id.cmp(&right.order_id));
        for order in orders {
            material.push_str(&format!(
                "projection-order:{}:{}:{}:{}:{}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?};",
                order.order_id,
                order.intent_id,
                order.deployment_id,
                order.token_id,
                order.requested_qty,
                order.limit_price,
                order.venue_order_id,
                order.venue_order_history,
                order.revision,
                order.state,
                order.state_changed_at,
                order.filled_qty,
                order.rejection_reason,
                order.last_error
            ));
        }
        for fill in &self.fills {
            material.push_str(&format!(
                "fill:{}:{}:{}:{:?}:{}:{}:{}:{};",
                fill.fill_id,
                fill.order_id,
                fill.token_id,
                fill.side,
                fill.quantity,
                fill.price,
                fill.fee,
                fill.timestamp
            ));
        }
        let mut positions = self.positions.clone();
        positions.sort_by(|left, right| left.token_id.cmp(&right.token_id));
        for position in positions {
            material.push_str(&format!(
                "position:{}:{}:{}:{};",
                position.token_id,
                position.net_qty,
                position.avg_entry_price,
                position.realized_pnl
            ));
        }
        material.push_str(&format!(
            "pnl:{}:{}:{};risk:{:?};",
            self.pnl.realized_pnl, self.pnl.unrealized_pnl, self.pnl.total_fees, self.risk
        ));
        if let Some(oms) = &self.canonical_oms {
            let mut canonical_orders = oms.orders.iter().collect::<Vec<_>>();
            canonical_orders.sort_by_key(|(order_id, _)| order_id.0.clone());
            for (order_id, order) in canonical_orders {
                material.push_str(&format!("oms-order:{}:{:?};", order_id.0, order));
            }
            let mut contracts = oms.notional_contracts.iter().collect::<Vec<_>>();
            contracts.sort_by_key(|(order_id, _)| order_id.0.clone());
            for (order_id, contract) in contracts {
                material.push_str(&format!("oms-contract:{}:{:?};", order_id.0, contract));
            }
            material.push_str(&format!(
                "oms-exceptions:{:?};",
                oms.reconciliation_exceptions
            ));
        }
        if let Some(portfolio) = &self.canonical_portfolio {
            material.push_str(&format!(
                "portfolio-digest:{:?};",
                portfolio.canonical_state_digest
            ));
        }
        format!("sha256:{:x}", Sha256::digest(material.as_bytes()))
    }
}

#[derive(Default)]
pub struct TradingRuntime {
    intents: Vec<TradingIntent>,
    intent_by_id: BTreeMap<String, usize>,
    order_by_idempotency_key: BTreeMap<String, (String, TradingIntent)>,
    orders: OrderLedger,
    fills: FillLedger,
    positions: PositionLedger,
    /// Canonical Monday OMS/portfolio are the authoritative state transition
    /// engines. The legacy-shaped ledgers below are read-only projections kept
    /// temporarily for the prediction caller migration.
    canonical_oms: OmsCore,
    canonical_portfolio: RefCell<Portfolio>,
    reconciliation_required: bool,
}

impl std::fmt::Debug for TradingRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TradingRuntime")
            .field("intents", &self.intents.len())
            .field("orders", &self.orders.active_orders())
            .field("fills", &self.fills.all().len())
            .field("positions", &self.positions.positions().count())
            .finish()
    }
}

impl TradingRuntime {
    pub fn restore(snapshot: TradingRuntimeSnapshot) -> Result<Self, TradingRuntimeError> {
        let expected_digest = snapshot
            .canonical_snapshot_digest
            .as_deref()
            .ok_or_else(|| {
                TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "trading runtime snapshot is missing canonical integrity digest".to_string(),
                )
            })?;
        if expected_digest != snapshot.integrity_digest() {
            return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                "trading runtime snapshot integrity digest mismatch".to_string(),
            ));
        }
        let oms_checkpoint = snapshot
            .canonical_oms
            .clone()
            .ok_or(TradingRuntimeError::MissingCanonicalCheckpoint)?;
        let portfolio_checkpoint = snapshot
            .canonical_portfolio
            .clone()
            .ok_or(TradingRuntimeError::MissingCanonicalCheckpoint)?;
        let mut intent_ids = HashSet::new();
        for intent in &snapshot.intents {
            if intent.intent_id.is_empty() || !intent_ids.insert(intent.intent_id.clone()) {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "snapshot has duplicate or empty intent identity".to_string(),
                ));
            }
        }
        let projected_order_ids = snapshot
            .orders
            .iter()
            .map(|order| order.order_id.as_str())
            .collect::<HashSet<_>>();
        for order in oms_checkpoint.orders.values() {
            let Some(strategy_id) = order.strategy_id.as_deref() else {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "OMS checkpoint order has no canonical intent identity".to_string(),
                ));
            };
            let Some(intent) = snapshot
                .intents
                .iter()
                .find(|intent| intent.intent_id == strategy_id)
            else {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "OMS checkpoint order has no matching intent metadata".to_string(),
                ));
            };
            if order.symbol.as_str() != intent.token_id
                || order.side != canonical_side(intent.side)
                || !projected_order_ids.contains(order.order_id.0.as_str())
            {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "OMS order identity disagrees with its intent or projection".to_string(),
                ));
            }
        }
        let mut idempotency_keys = HashSet::new();
        for order_id in portfolio_checkpoint.order_meta.keys() {
            if !oms_checkpoint.orders.contains_key(order_id) {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "Portfolio checkpoint references an order absent from OMS".to_string(),
                ));
            }
        }
        for projected in &snapshot.orders {
            if let Some(key) = projected.idempotency_key.as_deref() {
                if key.is_empty() || !idempotency_keys.insert(key.to_string()) {
                    return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                        "snapshot has duplicate or empty idempotency identity".to_string(),
                    ));
                }
            }
            let Some(canonical) = oms_checkpoint
                .orders
                .get(&CanonicalOrderId(projected.order_id.clone()))
            else {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "projection order is absent from OMS checkpoint".to_string(),
                ));
            };
            let Some(intent) = snapshot
                .intents
                .iter()
                .find(|intent| intent.intent_id == projected.intent_id)
            else {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "projection order has no matching intent identity".to_string(),
                ));
            };
            let canonical_state = match canonical.status {
                ports::OrderStatus::Unknown => OrderState::Unknown,
                ports::OrderStatus::New => OrderState::Pending,
                ports::OrderStatus::Acknowledged | ports::OrderStatus::Accepted => {
                    OrderState::Acknowledged
                }
                ports::OrderStatus::PartiallyFilled => OrderState::PartiallyFilled,
                ports::OrderStatus::Filled => OrderState::Filled,
                ports::OrderStatus::Canceled
                | ports::OrderStatus::Expired
                | ports::OrderStatus::Replaced => OrderState::Canceled,
                ports::OrderStatus::Rejected => OrderState::Rejected,
            };
            let canonical_changed_at = canonical.state_changed_at.and_then(|timestamp| {
                i64::try_from(timestamp)
                    .ok()
                    .and_then(DateTime::from_timestamp_micros)
            });
            if projected.requested_qty != canonical.qty.0
                || projected.limit_price != canonical.limit_price.map(|price| price.0)
                || projected.venue_order_id != canonical.venue_order_id
                || projected.venue_order_history != canonical.venue_order_history
                || projected.revision != canonical.revision
                || projected.filled_qty != canonical.cum_qty.0
                || projected.state != canonical_state
                || projected.state_changed_at != canonical_changed_at
                || projected.rejection_reason != canonical.rejection_reason
                || projected.last_error != canonical.last_error
                || projected.idempotency_key != canonical.client_order_id
                || projected.intent_id != intent.intent_id
                || projected.deployment_id != intent.deployment_id
                || projected.token_id != intent.token_id
                || canonical.symbol.as_str() != intent.token_id
                || canonical.side != canonical_side(intent.side)
            {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "projection order metadata disagrees with OMS checkpoint".to_string(),
                ));
            }
        }
        let mut fill_ids = HashSet::new();
        for fill in &snapshot.fills {
            if fill.fill_id.is_empty()
                || !fill_ids.insert(fill.fill_id.clone())
                || fill.quantity <= Decimal::ZERO
                || fill.price <= Decimal::ZERO
                || fill.fee < Decimal::ZERO
            {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "snapshot has invalid or duplicate fill audit identity".to_string(),
                ));
            }
            let order_id = CanonicalOrderId(fill.order_id.clone());
            let Some(order) = oms_checkpoint.orders.get(&order_id) else {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "fill audit references an order absent from OMS".to_string(),
                ));
            };
            let Some(intent) = order.strategy_id.as_deref().and_then(|id| {
                snapshot
                    .intents
                    .iter()
                    .find(|intent| intent.intent_id == id)
            }) else {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "fill audit references an order without intent identity".to_string(),
                ));
            };
            if fill.token_id != intent.token_id || fill.side != intent.side {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "fill audit identity disagrees with its order intent".to_string(),
                ));
            }
            if !portfolio_checkpoint
                .processed_fill_ids
                .get(&order_id)
                .is_some_and(|ids| ids.contains(&fill.fill_id))
            {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "fill audit identity is absent from canonical Portfolio".to_string(),
                ));
            }
        }
        for (order_id, processed_ids) in &portfolio_checkpoint.processed_fill_ids {
            if processed_ids
                .iter()
                .any(|fill_id| !fill_ids.contains(fill_id))
            {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "canonical Portfolio contains a fill identity absent from audit".to_string(),
                ));
            }
            if !oms_checkpoint.orders.contains_key(order_id) {
                return Err(TradingRuntimeError::InvalidCanonicalCheckpoint(
                    "canonical Portfolio fill namespace references unknown OMS order".to_string(),
                ));
            }
        }
        let mut canonical_oms = OmsCore::new();
        CanonicalOrderManager::import_checkpoint(&mut canonical_oms, oms_checkpoint)
            .map_err(TradingRuntimeError::InvalidCanonicalCheckpoint)?;
        let mut canonical_portfolio = Portfolio::new();
        CanonicalPortfolioManager::try_import_state(&mut canonical_portfolio, portfolio_checkpoint)
            .map_err(TradingRuntimeError::InvalidCanonicalCheckpoint)?;

        let intent_by_id = snapshot
            .intents
            .iter()
            .enumerate()
            .map(|(index, intent)| (intent.intent_id.clone(), index))
            .collect();

        let order_by_idempotency_key = snapshot
            .orders
            .iter()
            .filter_map(|order| {
                let key = order.idempotency_key.clone()?;
                let intent = snapshot
                    .intents
                    .iter()
                    .find(|intent| intent.intent_id == order.intent_id)?
                    .clone();
                Some((key, (order.order_id.clone(), intent)))
            })
            .collect();
        let mut runtime = Self {
            intents: snapshot.intents,
            intent_by_id,
            order_by_idempotency_key,
            orders: OrderLedger::restore(snapshot.orders),
            fills: FillLedger::restore(snapshot.fills),
            positions: PositionLedger::default(),
            canonical_oms,
            canonical_portfolio: RefCell::new(canonical_portfolio),
            reconciliation_required: false,
        };
        runtime.reconciliation_required =
            !runtime.canonical_oms.reconciliation_exceptions().is_empty()
                || CanonicalPortfolioManager::reconciliation_exception_count(
                    &*runtime.canonical_portfolio.borrow(),
                ) > 0;
        runtime.sync_projections();
        Ok(runtime)
    }

    /// Rebuild the strategy-facing views from the canonical OMS/Portfolio
    /// checkpoints. These views contain no transition or accounting logic.
    fn sync_projections(&mut self) {
        let canonical = CanonicalOrderManager::export_checkpoint(&self.canonical_oms);
        let mut projected_orders = Vec::new();
        for record in canonical.orders.values() {
            let Some(intent) = self
                .intents
                .iter()
                .find(|intent| record.strategy_id.as_deref() == Some(intent.intent_id.as_str()))
            else {
                continue;
            };
            let state = match record.status {
                ports::OrderStatus::Unknown => OrderState::Unknown,
                ports::OrderStatus::New => OrderState::Pending,
                ports::OrderStatus::Acknowledged => OrderState::Acknowledged,
                ports::OrderStatus::Accepted => OrderState::Acknowledged,
                ports::OrderStatus::PartiallyFilled => OrderState::PartiallyFilled,
                ports::OrderStatus::Filled => OrderState::Filled,
                ports::OrderStatus::Canceled => OrderState::Canceled,
                ports::OrderStatus::Rejected => OrderState::Rejected,
                ports::OrderStatus::Expired | ports::OrderStatus::Replaced => OrderState::Canceled,
            };
            projected_orders.push(OrderRecord {
                order_id: record.order_id.0.clone(),
                intent_id: intent.intent_id.clone(),
                deployment_id: intent.deployment_id.clone(),
                token_id: intent.token_id.clone(),
                requested_qty: record.qty.0,
                limit_price: record.limit_price.map(|price| price.0),
                venue_order_id: record.venue_order_id.clone(),
                venue_order_history: record.venue_order_history.clone(),
                revision: record.revision,
                state,
                state_changed_at: record.state_changed_at.and_then(|timestamp| {
                    i64::try_from(timestamp)
                        .ok()
                        .and_then(DateTime::from_timestamp_micros)
                }),
                filled_qty: record.cum_qty.0,
                rejection_reason: record.rejection_reason.clone(),
                last_error: record.last_error.clone(),
                idempotency_key: record.client_order_id.clone(),
            });
        }
        self.orders = OrderLedger::restore(projected_orders);

        let canonical_portfolio = self.canonical_portfolio.borrow();
        let portfolio_state = CanonicalPortfolioManager::export_state(&*canonical_portfolio);
        let positions = portfolio_state
            .account_view
            .positions
            .values()
            .map(|position| PositionSnapshot {
                token_id: position.symbol.as_str().to_string(),
                net_qty: position.quantity.0,
                avg_entry_price: position.avg_price.0,
                realized_pnl: position.realized_pnl,
            })
            .collect::<Vec<_>>();
        self.positions = PositionLedger::restore(positions);
    }

    pub fn submit_intent(
        &mut self,
        intent: TradingIntent,
        order_id: impl Into<String>,
        idempotency_key: Option<&str>,
    ) -> Result<&super::orders::OrderRecord, TradingRuntimeError> {
        if self.reconciliation_required {
            return Err(TradingRuntimeError::InvalidIntent(
                "canonical state requires independent reconciliation",
            ));
        }
        let idempotency_key = idempotency_key.map(str::trim).filter(|key| !key.is_empty());
        if let Some(existing_order_id) = self.idempotent_order_id(&intent, idempotency_key)? {
            return Ok(self
                .orders
                .order(&existing_order_id)
                .expect("idempotency key references an existing order"));
        }
        let order_id = order_id.into();
        if intent.intent_id.trim().is_empty() {
            return Err(TradingRuntimeError::EmptyIdentifier("intent_id"));
        }
        if order_id.trim().is_empty() {
            return Err(TradingRuntimeError::EmptyIdentifier("order_id"));
        }
        if self
            .orders
            .orders()
            .any(|order| order.intent_id == intent.intent_id)
        {
            return Err(TradingRuntimeError::DuplicateIdentifier("intent_id"));
        }
        if self.orders.contains(&order_id) {
            return Err(TradingRuntimeError::DuplicateIdentifier("order_id"));
        }
        if intent.quantity <= Decimal::ZERO {
            return Err(TradingRuntimeError::InvalidIntent(
                "quantity must be greater than zero",
            ));
        }
        if intent
            .limit_price
            .is_some_and(|price| price <= Decimal::ZERO || price >= Decimal::ONE)
        {
            return Err(TradingRuntimeError::InvalidIntent(
                "limit_price must be between zero and one",
            ));
        }
        if intent.purpose == IntentPurpose::Cancel {
            return Err(TradingRuntimeError::InvalidIntent(
                "cancel purpose cannot submit an order",
            ));
        }
        if matches!(intent.purpose, IntentPurpose::Reduce | IntentPurpose::Exit)
            && !self.positions.can_reduce(
                &intent.token_id,
                intent.side,
                intent.quantity + self.reserved_reduction_qty(&intent, None),
            )
        {
            return Err(TradingRuntimeError::InvalidIntent(
                "reduce or exit must decrease an existing position without flipping it",
            ));
        }

        // Canonical OMS retains terminal orders; keep intent metadata so
        // restore can validate every order owner without legacy replay.
        let index = self.intents.len();
        self.intent_by_id.insert(intent.intent_id.clone(), index);
        self.intents.push(intent.clone());
        let projection = OrderRecord {
            order_id: order_id.clone(),
            intent_id: intent.intent_id.clone(),
            deployment_id: intent.deployment_id.clone(),
            token_id: intent.token_id.clone(),
            requested_qty: intent.quantity,
            limit_price: intent.limit_price,
            venue_order_id: None,
            venue_order_history: Vec::new(),
            revision: 0,
            state: OrderState::Pending,
            state_changed_at: Some(Utc::now()),
            filled_qty: Decimal::ZERO,
            rejection_reason: None,
            last_error: None,
            idempotency_key: idempotency_key.map(str::to_string),
        };
        if !canonical_register(
            &mut self.canonical_oms,
            &mut self.canonical_portfolio.borrow_mut(),
            &projection,
            &intent,
        ) {
            return Err(TradingRuntimeError::InvalidIntent(
                "canonical OMS/Portfolio refused order registration",
            ));
        }
        if let Some(key) = idempotency_key {
            self.order_by_idempotency_key
                .insert(key.to_string(), (order_id.clone(), intent.clone()));
        }
        self.sync_projections();
        Ok(self.orders.order(&order_id).expect("order inserted"))
    }

    pub fn idempotent_order(
        &self,
        intent: &TradingIntent,
        idempotency_key: Option<&str>,
    ) -> Result<Option<&super::orders::OrderRecord>, TradingRuntimeError> {
        let Some(existing_order_id) = self.idempotent_order_id(intent, idempotency_key)? else {
            return Ok(None);
        };
        Ok(Some(
            self.orders
                .order(&existing_order_id)
                .expect("idempotency key references an existing order"),
        ))
    }

    fn idempotent_order_id(
        &self,
        intent: &TradingIntent,
        idempotency_key: Option<&str>,
    ) -> Result<Option<String>, TradingRuntimeError> {
        let Some((existing_order_id, existing_intent)) = idempotency_key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .and_then(|key| self.order_by_idempotency_key.get(key))
        else {
            return Ok(None);
        };
        if !same_idempotent_payload(existing_intent, intent) {
            return Err(TradingRuntimeError::InvalidIntent(
                "idempotency key payload mismatch",
            ));
        }
        Ok(Some(existing_order_id.clone()))
    }

    fn reserved_reduction_qty(
        &self,
        intent: &TradingIntent,
        excluded_order_id: Option<&str>,
    ) -> Decimal {
        self.orders
            .orders()
            .filter(|order| {
                matches!(
                    order.state,
                    OrderState::Pending
                        | OrderState::Unknown
                        | OrderState::Acknowledged
                        | OrderState::PartiallyFilled
                ) && order.token_id == intent.token_id
                    && excluded_order_id != Some(order.order_id.as_str())
            })
            .filter(|order| {
                self.intent(&order.intent_id).is_some_and(|existing| {
                    existing.side == intent.side
                        && matches!(
                            existing.purpose,
                            IntentPurpose::Reduce | IntentPurpose::Exit
                        )
                })
            })
            .map(|order| (order.requested_qty - order.filled_qty).max(Decimal::ZERO))
            .sum()
    }

    pub fn validate_order_replacement(
        &self,
        order_id: &str,
        requested_qty: Decimal,
        limit_price: Option<Decimal>,
    ) -> Result<(), TradingRuntimeError> {
        let order = self
            .orders
            .order(order_id)
            .ok_or(TradingRuntimeError::InvalidIntent("order not found"))?;
        if requested_qty <= Decimal::ZERO {
            return Err(TradingRuntimeError::InvalidIntent(
                "quantity must be greater than zero",
            ));
        }
        if requested_qty < order.filled_qty {
            return Err(TradingRuntimeError::InvalidIntent(
                "replacement quantity cannot be below filled quantity",
            ));
        }
        if limit_price.is_some_and(|price| price <= Decimal::ZERO || price >= Decimal::ONE) {
            return Err(TradingRuntimeError::InvalidIntent(
                "limit_price must be between zero and one",
            ));
        }
        let intent = self
            .intent(&order.intent_id)
            .ok_or(TradingRuntimeError::InvalidIntent("intent not found"))?;
        if matches!(intent.purpose, IntentPurpose::Reduce | IntentPurpose::Exit) {
            let replacement_remaining = (requested_qty - order.filled_qty).max(Decimal::ZERO);
            let reserved = self.reserved_reduction_qty(intent, Some(order_id));
            if !self.positions.can_reduce(
                &intent.token_id,
                intent.side,
                replacement_remaining + reserved,
            ) {
                return Err(TradingRuntimeError::InvalidIntent(
                    "reduce or exit replacement must not increase or flip the position",
                ));
            }
        }
        Ok(())
    }

    pub fn acknowledge_order(
        &mut self,
        order_id: &str,
        venue_order_id: impl Into<String>,
    ) -> Option<&super::orders::OrderRecord> {
        self.acknowledge_order_at(order_id, venue_order_id, now_timestamp())
    }

    fn acknowledge_order_at(
        &mut self,
        order_id: &str,
        venue_order_id: impl Into<String>,
        timestamp: u64,
    ) -> Option<&super::orders::OrderRecord> {
        self.canonical_oms
            .on_execution_event(&ExecutionEvent::OrderAck {
                order_id: CanonicalOrderId(order_id.to_string()),
                timestamp,
            })?;
        if !self.canonical_oms.set_venue_order_id(
            &CanonicalOrderId(order_id.to_string()),
            venue_order_id.into(),
        ) {
            self.reconciliation_required = true;
            self.refresh_reconciliation_latch();
            self.sync_projections();
            return None;
        }
        self.sync_projections();
        self.orders.order(order_id)
    }

    pub fn replace_order(
        &mut self,
        order_id: &str,
        requested_qty: Decimal,
        limit_price: Option<Decimal>,
        venue_order_id: impl Into<String>,
    ) -> Option<&super::orders::OrderRecord> {
        self.replace_order_at(
            order_id,
            requested_qty,
            limit_price,
            venue_order_id,
            now_timestamp(),
        )
    }

    fn replace_order_at(
        &mut self,
        order_id: &str,
        requested_qty: Decimal,
        limit_price: Option<Decimal>,
        venue_order_id: impl Into<String>,
        timestamp: u64,
    ) -> Option<&super::orders::OrderRecord> {
        if self
            .canonical_oms
            .apply_order_modification(
                &CanonicalOrderId(order_id.to_string()),
                Some(CanonicalQuantity(requested_qty)),
                limit_price.map(CanonicalPrice),
                timestamp,
            )
            .is_none()
        {
            self.refresh_reconciliation_latch();
            self.sync_projections();
            return None;
        }
        if !self.canonical_oms.replace_venue_order_id(
            &CanonicalOrderId(order_id.to_string()),
            venue_order_id.into(),
        ) {
            self.reconciliation_required = true;
            self.refresh_reconciliation_latch();
            self.sync_projections();
            return None;
        }
        self.sync_projections();
        self.orders.order(order_id)
    }

    pub fn reject_order(
        &mut self,
        order_id: &str,
        reason: impl Into<String>,
    ) -> Option<&super::orders::OrderRecord> {
        self.reject_order_at(order_id, reason, now_timestamp())
    }

    fn reject_order_at(
        &mut self,
        order_id: &str,
        reason: impl Into<String>,
        timestamp: u64,
    ) -> Option<&super::orders::OrderRecord> {
        let reason = reason.into();
        self.canonical_oms
            .on_execution_event(&ExecutionEvent::OrderReject {
                order_id: CanonicalOrderId(order_id.to_string()),
                reason: reason.clone(),
                timestamp,
            })?;
        self.canonical_oms
            .set_rejection_reason(&CanonicalOrderId(order_id.to_string()), reason);
        self.sync_projections();
        self.orders.order(order_id)
    }

    pub fn record_order_error(
        &mut self,
        order_id: &str,
        error: impl Into<String>,
    ) -> Option<&super::orders::OrderRecord> {
        self.canonical_oms
            .set_last_error(&CanonicalOrderId(order_id.to_string()), error.into());
        self.sync_projections();
        self.orders.order(order_id)
    }

    pub fn mark_order_unknown(
        &mut self,
        order_id: &str,
        error: impl Into<String>,
    ) -> Option<&super::orders::OrderRecord> {
        let error = error.into();
        self.canonical_oms
            .set_last_error(&CanonicalOrderId(order_id.to_string()), error);
        self.canonical_oms.update_status(
            &CanonicalOrderId(order_id.to_string()),
            CanonicalOmsStatus::Unknown,
        )?;
        self.sync_projections();
        self.orders.order(order_id)
    }

    pub fn cancel_order(&mut self, order_id: &str) -> Option<&super::orders::OrderRecord> {
        self.cancel_order_at(order_id, now_timestamp())
    }

    fn cancel_order_at(
        &mut self,
        order_id: &str,
        timestamp: u64,
    ) -> Option<&super::orders::OrderRecord> {
        self.canonical_oms
            .on_execution_event(&ExecutionEvent::OrderCanceled {
                order_id: CanonicalOrderId(order_id.to_string()),
                timestamp,
            })?;
        self.sync_projections();
        self.orders.order(order_id)
    }

    pub fn cancel_active_entry_orders_for_market(&mut self, market_id: &str) -> usize {
        let order_ids = self
            .orders
            .orders()
            .filter(|order| {
                matches!(
                    order.state,
                    OrderState::Pending
                        | OrderState::Unknown
                        | OrderState::Acknowledged
                        | OrderState::PartiallyFilled
                )
            })
            .filter(|order| {
                self.intent(&order.intent_id).is_some_and(|intent| {
                    intent.market_id == market_id
                        && matches!(intent.purpose, IntentPurpose::Entry | IntentPurpose::Hedge)
                })
            })
            .map(|order| order.order_id.clone())
            .collect::<Vec<_>>();

        for order_id in &order_ids {
            let _ = self
                .canonical_oms
                .on_execution_event(&ExecutionEvent::OrderCanceled {
                    order_id: CanonicalOrderId(order_id.clone()),
                    timestamp: now_timestamp(),
                });
        }
        self.sync_projections();
        order_ids.len()
    }

    pub fn order(&self, order_id: &str) -> Option<&super::orders::OrderRecord> {
        self.orders.order(order_id)
    }

    pub fn intent(&self, intent_id: &str) -> Option<&TradingIntent> {
        self.intent_by_id
            .get(intent_id)
            .and_then(|index| self.intents.get(*index))
    }

    pub fn record_fill(&mut self, fill: FillRecord) -> bool {
        if fill.fill_id.trim().is_empty() {
            return false;
        }
        if self.fills.contains(&fill.fill_id) {
            // A REST duplicate may carry the fee after the private stream
            // already recorded the fill. The canonical fee namespace makes
            // this safe and idempotent.
            if fill.fee != Decimal::ZERO {
                self.apply_fee_observation(&fill);
            }
            return false;
        }
        if fill.fee < Decimal::ZERO {
            self.apply_fee_observation(&fill);
            return false;
        }
        if fill.quantity <= Decimal::ZERO || fill.price <= Decimal::ZERO {
            return false;
        }
        let Some(order) = self.orders.order(&fill.order_id) else {
            return false;
        };
        let Some(intent) = self.intent(&order.intent_id) else {
            return false;
        };
        if fill.token_id != order.token_id || fill.side != intent.side {
            return false;
        }
        let canonical_order_id = CanonicalOrderId(fill.order_id.clone());
        let canonical_event = ExecutionEvent::Fill {
            order_id: canonical_order_id.clone(),
            price: CanonicalPrice(fill.price),
            quantity: CanonicalQuantity(fill.quantity),
            timestamp: fill.timestamp.timestamp_micros().max(0) as u64,
            fill_id: fill.fill_id.clone(),
        };
        let oms_checkpoint = CanonicalOrderManager::export_checkpoint(&self.canonical_oms);
        let portfolio_checkpoint =
            CanonicalPortfolioManager::export_state(&*self.canonical_portfolio.borrow());
        let portfolio_exception_count = CanonicalPortfolioManager::reconciliation_exception_count(
            &*self.canonical_portfolio.borrow(),
        );
        if !CanonicalPortfolioManager::register_order(
            &mut *self.canonical_portfolio.borrow_mut(),
            canonical_order_id.clone(),
            CanonicalSymbol::new(&fill.token_id),
            canonical_side(fill.side),
        ) {
            self.reconciliation_required = true;
            return false;
        }
        let Some(_) = self.canonical_oms.on_execution_event(&canonical_event) else {
            // Canonical OMS retained malformed/overfill events for
            // reconciliation; roll back Portfolio metadata while preserving
            // the concrete exception evidence.
            let mut restore = portfolio_checkpoint;
            restore.reconciliation_exceptions.extend(
                CanonicalPortfolioManager::export_state(&*self.canonical_portfolio.borrow())
                    .reconciliation_exceptions,
            );
            restore.canonical_state_digest = None;
            let _ = CanonicalPortfolioManager::try_import_state(
                &mut *self.canonical_portfolio.borrow_mut(),
                restore,
            );
            self.reconciliation_required = true;
            return false;
        };
        self.canonical_portfolio
            .borrow_mut()
            .on_execution_event(&canonical_event);
        if fill.fee > Decimal::ZERO {
            self.canonical_portfolio
                .borrow_mut()
                .on_execution_event(&ExecutionEvent::FeeCharged {
                    order_id: canonical_order_id,
                    amount: fill.fee,
                    timestamp: fill.timestamp.timestamp_micros().max(0) as u64,
                    fill_id: fill.fill_id.clone(),
                });
        }
        if CanonicalPortfolioManager::reconciliation_exception_count(
            &*self.canonical_portfolio.borrow(),
        ) > portfolio_exception_count
        {
            let mut restore = portfolio_checkpoint;
            restore.reconciliation_exceptions.extend(
                CanonicalPortfolioManager::export_state(&*self.canonical_portfolio.borrow())
                    .reconciliation_exceptions,
            );
            restore.canonical_state_digest = None;
            let _ = CanonicalPortfolioManager::try_import_state(
                &mut *self.canonical_portfolio.borrow_mut(),
                restore,
            );
            let _ =
                CanonicalOrderManager::import_checkpoint(&mut self.canonical_oms, oms_checkpoint);
            self.reconciliation_required = true;
            return false;
        }
        self.fills.record(fill);
        self.sync_projections();
        true
    }

    /// Apply a terminal or acknowledgement observed on the authoritative
    /// execution stream to the canonical OMS projection.  Reconciliation
    /// supplies the local order identity because venue streams may report a
    /// venue-native order id while the runtime indexes local ids.
    pub fn apply_reconciliation_event(
        &mut self,
        local_order_id: &str,
        venue_order_id: Option<&str>,
        event: &ExecutionEvent,
    ) -> bool {
        match event {
            ExecutionEvent::OrderAck { timestamp, .. } => self
                .acknowledge_order_at(
                    local_order_id,
                    venue_order_id.unwrap_or(local_order_id),
                    *timestamp,
                )
                .is_some(),
            ExecutionEvent::OrderReject {
                reason, timestamp, ..
            } => self
                .reject_order_at(local_order_id, reason.clone(), *timestamp)
                .is_some(),
            ExecutionEvent::OrderCanceled { timestamp, .. } => {
                self.cancel_order_at(local_order_id, *timestamp).is_some()
            }
            ExecutionEvent::OrderModified {
                new_quantity,
                new_price,
                timestamp,
                ..
            } => {
                let Some(order) = self.order(local_order_id).cloned() else {
                    return false;
                };
                self.replace_order_at(
                    local_order_id,
                    new_quantity
                        .map(|quantity| quantity.0)
                        .unwrap_or(order.requested_qty),
                    new_price.map(|price| price.0).or(order.limit_price),
                    venue_order_id.unwrap_or(local_order_id),
                    *timestamp,
                )
                .is_some()
            }
            ExecutionEvent::Fill {
                price,
                quantity,
                timestamp,
                fill_id,
                ..
            } => {
                let Some(order) = self.order(local_order_id).cloned() else {
                    return false;
                };
                let Some(intent) = self.intent(&order.intent_id).cloned() else {
                    return false;
                };
                let Some(timestamp) = DateTime::from_timestamp_micros(*timestamp as i64) else {
                    return false;
                };
                self.record_fill(FillRecord {
                    fill_id: fill_id.clone(),
                    order_id: local_order_id.to_string(),
                    token_id: intent.token_id,
                    side: intent.side,
                    quantity: quantity.0,
                    price: price.0,
                    fee: Decimal::ZERO,
                    timestamp,
                })
            }
            ExecutionEvent::FeeCharged {
                amount,
                timestamp,
                fill_id,
                ..
            } => {
                let before =
                    CanonicalPortfolioManager::export_state(&*self.canonical_portfolio.borrow());
                let canonical_event = ExecutionEvent::FeeCharged {
                    order_id: CanonicalOrderId(local_order_id.to_string()),
                    amount: *amount,
                    timestamp: *timestamp,
                    fill_id: fill_id.clone(),
                };
                self.canonical_portfolio
                    .borrow_mut()
                    .on_execution_event(&canonical_event);
                let after =
                    CanonicalPortfolioManager::export_state(&*self.canonical_portfolio.borrow());
                let changed = before.processed_fee_ids != after.processed_fee_ids;
                self.refresh_reconciliation_latch();
                if changed || self.reconciliation_required {
                    self.sync_projections();
                }
                changed && !self.reconciliation_required
            }
            _ => false,
        }
    }

    pub fn reconciliation_required(&self) -> bool {
        self.reconciliation_required
    }

    fn refresh_reconciliation_latch(&mut self) {
        if !CanonicalOrderManager::export_checkpoint(&self.canonical_oms)
            .reconciliation_exceptions
            .is_empty()
            || !CanonicalPortfolioManager::export_state(&*self.canonical_portfolio.borrow())
                .reconciliation_exceptions
                .is_empty()
        {
            self.reconciliation_required = true;
        }
    }

    fn apply_fee_observation(&mut self, fill: &FillRecord) {
        self.canonical_portfolio
            .borrow_mut()
            .on_execution_event(&ExecutionEvent::FeeCharged {
                order_id: CanonicalOrderId(fill.order_id.clone()),
                amount: fill.fee,
                timestamp: fill.timestamp.timestamp_micros().max(0) as u64,
                fill_id: fill.fill_id.clone(),
            });
        self.refresh_reconciliation_latch();
        self.sync_projections();
    }

    pub fn last_fill_time(&self) -> Option<DateTime<Utc>> {
        self.fills.all().iter().map(|fill| fill.timestamp).max()
    }

    /// Read-only access to the position ledger.
    pub fn positions(&self) -> &PositionLedger {
        &self.positions
    }

    /// Read-only access to the order ledger.
    pub fn orders(&self) -> &OrderLedger {
        &self.orders
    }

    pub fn snapshot(&self, _mark_prices: &BTreeMap<String, Decimal>) -> TradingRuntimeSnapshot {
        if !_mark_prices.is_empty() {
            let marks = _mark_prices
                .iter()
                .map(|(symbol, price)| (CanonicalSymbol::new(symbol), CanonicalPrice(*price)))
                .collect::<HashMap<_, _>>();
            CanonicalPortfolioManager::update_market_prices(
                &mut *self.canonical_portfolio.borrow_mut(),
                &marks,
            );
        }
        let orders = self.orders.orders().cloned().collect::<Vec<_>>();
        let active_intents = self
            .intents
            .iter()
            .filter(|intent| {
                orders.iter().any(|order| {
                    order.intent_id == intent.intent_id
                        && matches!(
                            order.state,
                            super::orders::OrderState::Pending
                                | super::orders::OrderState::Unknown
                                | super::orders::OrderState::Acknowledged
                                | super::orders::OrderState::PartiallyFilled
                        )
                })
            })
            .cloned()
            .collect::<Vec<_>>();

        let mut snapshot = TradingRuntimeSnapshot {
            intents: self.intents.clone(),
            orders,
            fills: self.fills.all().to_vec(),
            positions: self.positions.positions().cloned().collect(),
            pnl: {
                let canonical_portfolio = self.canonical_portfolio.borrow();
                let view = CanonicalPortfolioManager::reader(&*canonical_portfolio).load();
                let portfolio_state =
                    CanonicalPortfolioManager::export_state(&*canonical_portfolio);
                PnlSnapshot {
                    realized_pnl: view.realized_pnl,
                    unrealized_pnl: view.unrealized_pnl,
                    total_fees: portfolio_state.total_fees,
                }
            },
            risk: snapshot_from_state(&active_intents, &self.orders, &self.positions),
            canonical_oms: Some(CanonicalOrderManager::export_checkpoint(
                &self.canonical_oms,
            )),
            canonical_portfolio: Some(CanonicalPortfolioManager::export_state(
                &*self.canonical_portfolio.borrow(),
            )),
            canonical_snapshot_digest: None,
        };
        snapshot.canonical_snapshot_digest = Some(snapshot.integrity_digest());
        snapshot
    }
}

fn now_timestamp() -> u64 {
    Utc::now().timestamp_micros().max(0) as u64
}

fn canonical_side(side: TradeSide) -> CanonicalSide {
    match side {
        TradeSide::Buy => CanonicalSide::Buy,
        TradeSide::Sell => CanonicalSide::Sell,
    }
}

fn canonical_register(
    oms: &mut OmsCore,
    portfolio: &mut Portfolio,
    order: &super::orders::OrderRecord,
    intent: &TradingIntent,
) -> bool {
    let order_id = CanonicalOrderId(order.order_id.clone());
    let symbol = CanonicalSymbol::new(&order.token_id);
    let side = canonical_side(intent.side);
    if !oms.register_order(CanonicalRegisterOrderParams {
        order_id: order_id.clone(),
        client_order_id: order.idempotency_key.clone(),
        account_id: None,
        symbol: symbol.clone(),
        side,
        qty: CanonicalQuantity(order.requested_qty),
        venue: None,
        strategy_id: Some(intent.intent_id.clone()),
    }) {
        return false;
    }
    if let Some(limit_price) = order.limit_price {
        if !oms.set_limit_price(&order_id, CanonicalPrice(limit_price)) {
            return false;
        }
    }
    if !CanonicalPortfolioManager::register_order(portfolio, order_id.clone(), symbol, side) {
        return false;
    }
    if let Some(limit_price) = order.limit_price {
        if intent.side == TradeSide::Buy
            && !matches!(intent.purpose, IntentPurpose::Reduce | IntentPurpose::Exit)
        {
            let _ =
                oms.register_notional_fill_contract(&order_id, CanonicalPrice(limit_price), false);
        }
    }
    true
}

fn same_idempotent_payload(left: &TradingIntent, right: &TradingIntent) -> bool {
    left.deployment_id == right.deployment_id
        && left.market_id == right.market_id
        && left.token_id == right.token_id
        && left.side == right.side
        && left.quantity == right.quantity
        && left.limit_price == right.limit_price
        && left.purpose == right.purpose
}

#[cfg(test)]
mod tests {
    use super::super::{
        FillRecord, IntentPurpose, OrderState, PnlSnapshot, TradeSide, TradingIntent,
        TradingRuntimeSnapshot,
    };
    use super::{TradingRuntime, TradingRuntimeError};
    use chrono::{DateTime, Utc};
    use ports::ExecutionEvent;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;

    #[test]
    fn restore_rebuilds_positions_and_active_risk_from_snapshot() {
        let mut source = TradingRuntime::default();
        source
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-1".into(),
                    deployment_id: "example.live".into(),
                    market_id: "market-1".into(),
                    token_id: "token-1".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.45)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-1",
                None,
            )
            .unwrap();
        source.acknowledge_order("order-1", "venue-1");
        assert!(source.record_fill(FillRecord {
            fill_id: "fill-1".into(),
            order_id: "order-1".into(),
            token_id: "token-1".into(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.45),
            fee: dec!(0.02),
            timestamp: Utc::now(),
        }));
        let snapshot = source.snapshot(&BTreeMap::new());
        let runtime = TradingRuntime::restore(snapshot).unwrap();
        let restored = runtime.snapshot(&BTreeMap::new());
        assert_eq!(restored.orders.len(), 1);
        assert_eq!(restored.fills.len(), 1);
        assert_eq!(restored.positions.len(), 1);
        assert_eq!(restored.positions[0].net_qty, dec!(1));
        assert_eq!(restored.risk.active_orders, 1);
    }

    #[test]
    fn cancel_active_entry_orders_for_market_releases_expired_event_reserve() {
        let mut runtime = TradingRuntime::default();
        let opened_at = Utc::now();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-expired".to_string(),
                    deployment_id: "example.replay".to_string(),
                    market_id: "expired-event".to_string(),
                    token_id: "token-expired".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(10),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Entry,
                    created_at: opened_at,
                },
                "order-expired",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-expired", "venue-expired");
        assert!(runtime.record_fill(FillRecord {
            fill_id: "fill-expired-partial".to_string(),
            order_id: "order-expired".to_string(),
            token_id: "token-expired".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(4),
            price: dec!(0.60),
            fee: Decimal::ZERO,
            timestamp: opened_at,
        }));

        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-live".to_string(),
                    deployment_id: "example.replay".to_string(),
                    market_id: "live-event".to_string(),
                    token_id: "token-live".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.50)),
                    purpose: IntentPurpose::Entry,
                    created_at: opened_at,
                },
                "order-live",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-live", "venue-live");

        let before = runtime.snapshot(&BTreeMap::new()).risk;
        assert_eq!(before.active_orders, 2);
        assert_eq!(before.reserved_order_exposure, dec!(4.60));

        assert_eq!(
            runtime.cancel_active_entry_orders_for_market("expired-event"),
            1
        );
        assert_eq!(
            runtime.order("order-expired").expect("expired order").state,
            OrderState::Canceled
        );
        assert_eq!(
            runtime.order("order-live").expect("live order").state,
            OrderState::Acknowledged
        );

        let after = runtime.snapshot(&BTreeMap::new()).risk;
        assert_eq!(after.active_orders, 1);
        assert_eq!(after.reserved_order_exposure, dec!(1.00));
        assert_eq!(after.gross_exposure, dec!(2.40));
    }

    #[test]
    fn restore_preserves_persisted_positions_when_fills_are_absent() {
        let mut source = TradingRuntime::default();
        source
            .submit_intent(
                TradingIntent {
                    intent_id: "position-source".into(),
                    deployment_id: "example.live".into(),
                    market_id: "market-1".into(),
                    token_id: "token-1".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(5),
                    limit_price: Some(dec!(0.42)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "position-order",
                None,
            )
            .unwrap();
        source.acknowledge_order("position-order", "venue-position");
        assert!(source.record_fill(FillRecord {
            fill_id: "position-open".into(),
            order_id: "position-order".into(),
            token_id: "token-1".into(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            price: dec!(0.42),
            fee: dec!(0.03),
            timestamp: Utc::now(),
        }));
        source
            .submit_intent(
                TradingIntent {
                    intent_id: "position-close".into(),
                    deployment_id: "example.live".into(),
                    market_id: "market-1".into(),
                    token_id: "token-1".into(),
                    side: TradeSide::Sell,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.77)),
                    purpose: IntentPurpose::Exit,
                    created_at: Utc::now(),
                },
                "position-close-order",
                None,
            )
            .unwrap();
        source.acknowledge_order("position-close-order", "venue-close");
        assert!(source.record_fill(FillRecord {
            fill_id: "position-close-fill".into(),
            order_id: "position-close-order".into(),
            token_id: "token-1".into(),
            side: TradeSide::Sell,
            quantity: dec!(2),
            price: dec!(0.77),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        let snapshot = source.snapshot(&BTreeMap::new());
        let runtime = TradingRuntime::restore(snapshot).unwrap();
        let restored = runtime.snapshot(&BTreeMap::new());

        assert_eq!(restored.positions.len(), 1);
        assert_eq!(restored.positions[0].net_qty, dec!(3));
        // Canonical Portfolio nets the 0.03 fee into realized PnL exactly
        // once; the disclosure field keeps total_fees separate.
        assert_eq!(restored.pnl.realized_pnl, dec!(0.67));
        assert_eq!(restored.pnl.total_fees, dec!(0.03));
        assert_eq!(restored.risk.open_positions, 1);
        assert_eq!(restored.risk.gross_exposure, dec!(1.26));
    }

    #[test]
    fn reconciliation_fill_fee_and_duplicate_rest_fill_charge_once() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "stream-intent".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "stream-order",
                None,
            )
            .unwrap();
        runtime.acknowledge_order("stream-order", "venue-stream");
        let timestamp = Utc::now().timestamp_micros().max(0) as u64;
        assert!(runtime.apply_reconciliation_event(
            "stream-order",
            Some("venue-stream"),
            &ExecutionEvent::Fill {
                order_id: hft_core::OrderId("venue-stream".to_string()),
                price: hft_core::Price(dec!(0.5)),
                quantity: hft_core::Quantity(dec!(1)),
                timestamp,
                fill_id: "stream-fill".to_string(),
            },
        ));
        assert!(runtime.apply_reconciliation_event(
            "stream-order",
            Some("venue-stream"),
            &ExecutionEvent::FeeCharged {
                order_id: hft_core::OrderId("venue-stream".to_string()),
                amount: dec!(0.03),
                timestamp,
                fill_id: "stream-fill".to_string(),
            },
        ));
        assert!(!runtime.apply_reconciliation_event(
            "stream-order",
            Some("venue-stream"),
            &ExecutionEvent::FeeCharged {
                order_id: hft_core::OrderId("venue-stream".to_string()),
                amount: dec!(0.03),
                timestamp,
                fill_id: "stream-fill".to_string(),
            },
        ));
        assert!(!runtime.record_fill(FillRecord {
            fill_id: "stream-fill".to_string(),
            order_id: "stream-order".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: dec!(0.03),
            timestamp: Utc::now(),
        }));

        let snapshot = runtime.snapshot(&BTreeMap::new());
        assert_eq!(snapshot.pnl.total_fees, dec!(0.03));
        assert_eq!(snapshot.pnl.net_pnl(), dec!(-0.03));
        assert_eq!(snapshot.fill_cashflow_summary().total_fees, dec!(0.03));
        assert_eq!(snapshot.fill_cashflow_summary().net_pnl(), dec!(-0.53));
        let restored =
            TradingRuntime::restore(snapshot).expect("canonical fee checkpoint restores");
        let restored_snapshot = restored.snapshot(&BTreeMap::new());
        assert_eq!(restored_snapshot.pnl.total_fees, dec!(0.03));
        assert_eq!(restored_snapshot.pnl.net_pnl(), dec!(-0.03));
        assert_eq!(
            restored_snapshot.fill_cashflow_summary().total_fees,
            dec!(0.03)
        );
        assert_eq!(
            restored_snapshot.fill_cashflow_summary().net_pnl(),
            dec!(-0.53)
        );
    }

    #[test]
    fn duplicate_fill_fee_for_unknown_order_latches_and_blocks_new_intents() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "fee-intent".into(),
                    deployment_id: "dep".into(),
                    market_id: "market".into(),
                    token_id: "token".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "fee-order",
                None,
            )
            .unwrap();
        assert!(runtime.record_fill(FillRecord {
            fill_id: "duplicate-fee".into(),
            order_id: "fee-order".into(),
            token_id: "token".into(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        assert!(!runtime.record_fill(FillRecord {
            fill_id: "duplicate-fee".into(),
            order_id: "missing-order".into(),
            token_id: "token".into(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: dec!(0.01),
            timestamp: Utc::now(),
        }));
        assert!(runtime.reconciliation_required());
        let error = runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "blocked-intent".into(),
                    deployment_id: "dep".into(),
                    market_id: "market".into(),
                    token_id: "token".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "blocked-order",
                None,
            )
            .expect_err("reconciliation evidence must block admission");
        assert!(error.to_string().contains("reconciliation"));
    }

    #[test]
    fn negative_fee_observation_keeps_evidence_for_known_and_duplicate_fills() {
        let make_runtime = || {
            let mut runtime = TradingRuntime::default();
            runtime
                .submit_intent(
                    TradingIntent {
                        intent_id: "negative-fee-intent".into(),
                        deployment_id: "dep".into(),
                        market_id: "market".into(),
                        token_id: "token".into(),
                        side: TradeSide::Buy,
                        quantity: dec!(1),
                        limit_price: Some(dec!(0.5)),
                        purpose: IntentPurpose::Entry,
                        created_at: Utc::now(),
                    },
                    "negative-fee-order",
                    None,
                )
                .unwrap();
            runtime
        };

        let mut known = make_runtime();
        assert!(!known.record_fill(FillRecord {
            fill_id: "known-negative-fee".into(),
            order_id: "negative-fee-order".into(),
            token_id: "token".into(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: dec!(-0.01),
            timestamp: Utc::now(),
        }));
        assert!(known.reconciliation_required());
        assert_eq!(
            known
                .snapshot(&BTreeMap::new())
                .canonical_portfolio
                .unwrap()
                .reconciliation_exceptions
                .len(),
            1
        );

        let mut duplicate = make_runtime();
        assert!(duplicate.record_fill(FillRecord {
            fill_id: "duplicate-negative-fee".into(),
            order_id: "negative-fee-order".into(),
            token_id: "token".into(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        assert!(!duplicate.record_fill(FillRecord {
            fill_id: "duplicate-negative-fee".into(),
            order_id: "negative-fee-order".into(),
            token_id: "token".into(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: dec!(-0.01),
            timestamp: Utc::now(),
        }));
        assert!(duplicate.reconciliation_required());
    }

    #[test]
    fn clearing_reconciliation_exceptions_cannot_restore_runtime() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "restore-intent".into(),
                    deployment_id: "dep".into(),
                    market_id: "market".into(),
                    token_id: "token".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "restore-order",
                None,
            )
            .unwrap();
        assert!(!runtime.apply_reconciliation_event(
            "missing-order",
            None,
            &ExecutionEvent::FeeCharged {
                order_id: hft_core::OrderId("restore-order".into()),
                amount: dec!(0.01),
                timestamp: 1,
                fill_id: "fee-without-fill".into(),
            }
        ));
        let mut snapshot = runtime.snapshot(&BTreeMap::new());
        snapshot
            .canonical_portfolio
            .as_mut()
            .unwrap()
            .reconciliation_exceptions
            .clear();
        assert!(TradingRuntime::restore(snapshot).is_err());
    }

    #[test]
    fn restore_rejects_canonical_oms_metadata_tampering() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "metadata-intent".into(),
                    deployment_id: "dep".into(),
                    market_id: "market".into(),
                    token_id: "token".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "metadata-order",
                None,
            )
            .unwrap();
        runtime.acknowledge_order("metadata-order", "venue-1");
        let mut snapshot = runtime.snapshot(&BTreeMap::new());
        snapshot
            .canonical_oms
            .as_mut()
            .unwrap()
            .orders
            .get_mut(&hft_core::OrderId("metadata-order".into()))
            .unwrap()
            .revision = 99;
        assert!(TradingRuntime::restore(snapshot).is_err());
    }

    #[test]
    fn restore_rejects_fill_and_intent_identity_tampering() {
        let mut source = TradingRuntime::default();
        source
            .submit_intent(
                TradingIntent {
                    intent_id: "integrity-intent".into(),
                    deployment_id: "dep".into(),
                    market_id: "market".into(),
                    token_id: "token".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "integrity-order",
                None,
            )
            .unwrap();
        assert!(source.record_fill(FillRecord {
            fill_id: "integrity-fill".into(),
            order_id: "integrity-order".into(),
            token_id: "token".into(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        let snapshot = source.snapshot(&BTreeMap::new());

        let mut missing_digest = snapshot.clone();
        missing_digest.canonical_snapshot_digest = None;
        assert!(TradingRuntime::restore(missing_digest).is_err());

        let mut tampered_fill = snapshot.clone();
        tampered_fill.fills[0].quantity = dec!(0.5);
        assert!(TradingRuntime::restore(tampered_fill).is_err());

        let mut tampered_intent = snapshot;
        tampered_intent.intents[0].token_id = "other-token".into();
        assert!(TradingRuntime::restore(tampered_intent).is_err());
    }

    #[test]
    fn canonical_replace_exception_latches_and_blocks_admission() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "replace-error-intent".into(),
                    deployment_id: "dep".into(),
                    market_id: "market".into(),
                    token_id: "token".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "replace-error-order",
                None,
            )
            .unwrap();
        assert!(runtime
            .replace_order(
                "replace-error-order",
                Decimal::ZERO,
                Some(dec!(0.4)),
                "venue"
            )
            .is_none());
        assert!(runtime.reconciliation_required());
        assert!(
            runtime
                .snapshot(&BTreeMap::new())
                .canonical_oms
                .unwrap()
                .reconciliation_exceptions
                .len()
                > 0
        );
    }

    #[test]
    fn reconciliation_terminal_events_preserve_authoritative_timestamps() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "timestamp-intent".into(),
                    deployment_id: "dep".into(),
                    market_id: "market".into(),
                    token_id: "token".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "timestamp-order",
                None,
            )
            .unwrap();
        assert!(runtime.apply_reconciliation_event(
            "timestamp-order",
            Some("venue-timestamp"),
            &ExecutionEvent::OrderAck {
                order_id: hft_core::OrderId("venue-timestamp".into()),
                timestamp: 101,
            }
        ));
        assert!(runtime.apply_reconciliation_event(
            "timestamp-order",
            Some("venue-timestamp"),
            &ExecutionEvent::OrderCanceled {
                order_id: hft_core::OrderId("venue-timestamp".into()),
                timestamp: 202,
            }
        ));
        let canceled_at = runtime.order("timestamp-order").unwrap().state_changed_at;
        assert_eq!(canceled_at, DateTime::from_timestamp_micros(202));
        assert!(runtime.apply_reconciliation_event(
            "timestamp-order",
            Some("venue-timestamp"),
            &ExecutionEvent::OrderCanceled {
                order_id: hft_core::OrderId("venue-timestamp".into()),
                timestamp: 999,
            }
        ));
        assert_eq!(
            runtime.order("timestamp-order").unwrap().state_changed_at,
            canceled_at
        );
    }

    #[test]
    fn closed_position_intents_are_pruned_from_lookup() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-1",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-1", "venue-1");
        assert!(runtime.intent("intent-1").is_some());

        runtime.record_fill(FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        });
        assert!(runtime.intent("intent-1").is_some());

        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-exit".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Exit,
                    created_at: Utc::now(),
                },
                "order-exit",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-exit", "venue-exit");
        runtime.record_fill(FillRecord {
            fill_id: "fill-2".to_string(),
            order_id: "order-exit".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Sell,
            quantity: dec!(1),
            price: dec!(0.60),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        });
        assert!(runtime.intent("intent-1").is_some());
        // Canonical checkpoints retain terminal intent metadata so restore can
        // validate every OMS order owner without replaying legacy projections.
        assert_eq!(runtime.snapshot(&BTreeMap::new()).intents.len(), 2);
    }

    #[test]
    fn price_improved_buy_fill_can_exceed_requested_shares_within_notional_cap() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-buy".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(28.30),
                    limit_price: Some(dec!(0.53)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-buy",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-buy", "venue-buy");

        let recorded = runtime.record_fill(FillRecord {
            fill_id: "fill-buy".to_string(),
            order_id: "order-buy".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            // Exact canonical notional cap: 28.30 * 0.53 = 14.9990.
            quantity: dec!(31.464233),
            price: dec!(0.4767),
            fee: dec!(0.12),
            timestamp: Utc::now(),
        });

        assert!(recorded);
        let order = runtime.order("order-buy").expect("order");
        assert_eq!(order.state, OrderState::Filled);
        assert_eq!(order.filled_qty, dec!(31.464233));
        let snapshot = runtime.snapshot(&BTreeMap::new());
        assert_eq!(snapshot.fills.len(), 1);
        assert_eq!(snapshot.positions[0].net_qty, dec!(31.464233));
    }

    #[test]
    fn price_improved_buy_overfill_still_rejects_above_notional_cap() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-buy".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(28.30),
                    limit_price: Some(dec!(0.53)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-buy",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-buy", "venue-buy");

        let recorded = runtime.record_fill(FillRecord {
            fill_id: "fill-buy".to_string(),
            order_id: "order-buy".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(40),
            price: dec!(0.53),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        });

        assert!(!recorded);
        let order = runtime.order("order-buy").expect("order");
        assert_eq!(order.state, OrderState::Acknowledged);
        assert_eq!(order.filled_qty, Decimal::ZERO);
    }

    #[test]
    fn price_improved_exit_buy_overfill_cannot_flip_short_position() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-short".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-entry",
                None,
            )
            .expect("short entry");
        assert!(runtime.record_fill(FillRecord {
            fill_id: "fill-entry".to_string(),
            order_id: "order-entry".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Sell,
            quantity: dec!(2),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "exit-short".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Exit,
                    created_at: Utc::now(),
                },
                "order-exit",
                None,
            )
            .expect("valid exit");
        let before = runtime.snapshot(&BTreeMap::new());

        assert!(!runtime.record_fill(FillRecord {
            fill_id: "fill-exit".to_string(),
            order_id: "order-exit".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(3),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        let after = runtime.snapshot(&BTreeMap::new());
        assert_eq!(after.fills, before.fills);
        assert_eq!(after.positions, before.positions);
        assert!(runtime.reconciliation_required());
        assert!(!after
            .canonical_oms
            .unwrap()
            .reconciliation_exceptions
            .is_empty());
    }

    #[test]
    fn cashflow_summary_treats_quantity_as_shares_not_dollars() {
        let now = Utc::now();
        let snapshot = TradingRuntimeSnapshot {
            fills: vec![
                FillRecord {
                    fill_id: "fill-buy".to_string(),
                    order_id: "order-buy".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(25),
                    price: dec!(0.40),
                    fee: dec!(0.05),
                    timestamp: now,
                },
                FillRecord {
                    fill_id: "fill-sell".to_string(),
                    order_id: "order-sell".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(25),
                    price: dec!(1.00),
                    fee: Decimal::ZERO,
                    timestamp: now,
                },
            ],
            pnl: PnlSnapshot {
                total_fees: dec!(0.05),
                ..Default::default()
            },
            ..Default::default()
        };

        let summary = snapshot.fill_cashflow_summary();
        assert_eq!(summary.buy_shares, dec!(25));
        assert_eq!(summary.sell_shares, dec!(25));
        assert_eq!(summary.gross_buy_cost, dec!(10.00));
        assert_eq!(summary.gross_sell_proceeds, dec!(25.00));
        assert_eq!(summary.deployed_capital(), dec!(10.00));
        assert_eq!(summary.net_pnl(), dec!(14.95));
        assert_eq!(
            summary.roi_on_deployed_capital().expect("roi").round_dp(4),
            dec!(1.4950)
        );
    }

    #[test]
    fn duplicate_intent_and_order_ids_do_not_overwrite() {
        let mut runtime = TradingRuntime::default();
        let intent = TradingIntent {
            intent_id: "intent-1".to_string(),
            deployment_id: "dep-1".to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            limit_price: Some(dec!(0.40)),
            purpose: IntentPurpose::Entry,
            created_at: Utc::now(),
        };
        runtime
            .submit_intent(intent.clone(), "order-1", None)
            .expect("valid intent");
        let before = runtime.snapshot(&BTreeMap::new());

        let mut duplicate_intent = intent.clone();
        duplicate_intent.quantity = dec!(2);
        assert_eq!(
            runtime.submit_intent(duplicate_intent, "order-2", None),
            Err(TradingRuntimeError::DuplicateIdentifier("intent_id"))
        );
        assert_eq!(runtime.snapshot(&BTreeMap::new()), before);

        let mut duplicate_order = intent;
        duplicate_order.intent_id = "intent-2".to_string();
        assert_eq!(
            runtime.submit_intent(duplicate_order, "order-1", None),
            Err(TradingRuntimeError::DuplicateIdentifier("order_id"))
        );
        assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
    }

    #[test]
    fn cancel_purpose_cannot_submit_order() {
        let mut runtime = TradingRuntime::default();
        let before = runtime.snapshot(&BTreeMap::new());

        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "cancel-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Cancel,
                    created_at: Utc::now(),
                },
                "order-cancel",
                None,
            )
            .expect_err("cancel cannot submit");

        assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
    }

    #[test]
    fn exit_cannot_increase_or_flip_position() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-entry",
                None,
            )
            .expect("valid intent");
        assert!(runtime.record_fill(FillRecord {
            fill_id: "fill-entry".to_string(),
            order_id: "order-entry".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        let before = runtime.snapshot(&BTreeMap::new());

        for (intent_id, order_id, side, quantity) in [
            ("exit-increase", "order-increase", TradeSide::Buy, dec!(1)),
            ("exit-flip", "order-flip", TradeSide::Sell, dec!(3)),
        ] {
            runtime
                .submit_intent(
                    TradingIntent {
                        intent_id: intent_id.to_string(),
                        deployment_id: "dep-1".to_string(),
                        market_id: "market-1".to_string(),
                        token_id: "token-1".to_string(),
                        side,
                        quantity,
                        limit_price: Some(dec!(0.40)),
                        purpose: IntentPurpose::Exit,
                        created_at: Utc::now(),
                    },
                    order_id,
                    None,
                )
                .expect_err("invalid exit");
            assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
        }
    }

    #[test]
    fn active_exit_quantity_is_reserved_before_accepting_another_exit() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-entry",
                None,
            )
            .expect("valid entry");
        assert!(runtime.record_fill(FillRecord {
            fill_id: "fill-entry".to_string(),
            order_id: "order-entry".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "exit-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Exit,
                    created_at: Utc::now(),
                },
                "order-exit-1",
                None,
            )
            .expect("first exit");
        let before = runtime.snapshot(&BTreeMap::new());

        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "exit-2".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Reduce,
                    created_at: Utc::now(),
                },
                "order-exit-2",
                None,
            )
            .expect_err("second exit exceeds unreserved position");
        assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
    }

    #[test]
    fn fill_token_side_and_numeric_invariants_are_enforced() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-1",
                None,
            )
            .expect("valid intent");
        let before = runtime.snapshot(&BTreeMap::new());
        let valid = FillRecord {
            fill_id: String::new(),
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        };
        let invalid = [
            FillRecord {
                fill_id: "   ".to_string(),
                ..valid.clone()
            },
            FillRecord {
                fill_id: "zero-qty".to_string(),
                quantity: Decimal::ZERO,
                ..valid.clone()
            },
            FillRecord {
                fill_id: "zero-price".to_string(),
                price: Decimal::ZERO,
                ..valid.clone()
            },
            FillRecord {
                fill_id: "wrong-token".to_string(),
                token_id: "token-2".to_string(),
                ..valid.clone()
            },
            FillRecord {
                fill_id: "wrong-side".to_string(),
                side: TradeSide::Sell,
                ..valid.clone()
            },
        ];

        for fill in invalid {
            assert!(!runtime.record_fill(fill));
            assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
        }
        assert!(!runtime.record_fill(FillRecord {
            fill_id: "overfill".to_string(),
            quantity: dec!(3),
            ..valid.clone()
        }));
        assert!(runtime.reconciliation_required());
        assert!(!runtime.record_fill(FillRecord {
            fill_id: "negative-fee".to_string(),
            fee: dec!(-0.01),
            ..valid
        }));
        assert!(runtime.reconciliation_required());
        assert!(runtime
            .snapshot(&BTreeMap::new())
            .canonical_portfolio
            .unwrap()
            .reconciliation_exceptions
            .iter()
            .any(|exception| exception.reason.contains("negative")));
    }
}
