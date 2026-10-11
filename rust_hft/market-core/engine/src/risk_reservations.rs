//! Engine-owned admission metadata. OMS remains the only submitted-order lifecycle writer.
//! A queue entry is retained until OrderNew binds it to OMS. Quantities then come from OMS;
//! cancellation or rejection without final-fill evidence retains its remainder until reconciliation.
use hft_core::{
    AccountId, HftError, HftResult, OrderId, OrderType, Price, Quantity, Side, Symbol, VenueId,
};
use ports::{
    AccountView, ExecutionEvent, OrderIntentEnvelope, OrderManager, OrderRecord, OrderRiskView,
    OrderStatus,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Recovery metadata from the controlled engine owner, not a new order grant.
/// Persist it with the matching OMS/Portfolio checkpoints. Import always pauses;
/// missing queued submission evidence cannot be cleared by an empty exchange snapshot.
pub struct RiskReservationCheckpoint {
    schema: u32,
    queued: HashMap<String, Valuation>,
    orders: HashMap<OrderId, Valuation>,
    awaiting_final_readback: HashSet<OrderId>,
}

impl Default for RiskReservationCheckpoint {
    fn default() -> Self {
        Self {
            schema: 2,
            queued: HashMap::new(),
            orders: HashMap::new(),
            awaiting_final_readback: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Valuation {
    client_order_id: String,
    strategy_id: String,
    account_id: Option<AccountId>,
    venue: Option<VenueId>,
    symbol: Symbol,
    side: Side,
    asset_class: hft_core::AssetClass,
    product_type: hft_core::ProductType,
    quantity: Quantity,
    unit_price: Price,
    // Preserve every admitted/observed price while delayed fills can still arrive.
    observed_unit_price: Price,
    // Submitted metadata is valid only for this exact canonical amendment history.
    observed_revision: Option<u32>,
}

impl Valuation {
    pub(super) fn from_envelope(envelope: &OrderIntentEnvelope) -> HftResult<Self> {
        let intent = &envelope.intent;
        let price = match intent.order_type {
            OrderType::Limit => intent.price,
            OrderType::Market => {
                let reference = envelope.price_reference.as_ref().ok_or_else(|| {
                    risk_error("market reservation requires the admitted price reference")
                })?;
                let slippage = envelope
                    .lifecycle
                    .max_slippage_bps
                    .filter(|value| (1..=10_000).contains(value))
                    .ok_or_else(|| {
                        risk_error("market reservation requires an admitted slippage bound")
                    })?;
                if reference.symbol != intent.symbol
                    || reference.side != intent.side
                    || intent
                        .target_venue
                        .is_some_and(|venue| reference.venue != venue)
                {
                    return Err(risk_error("market reservation reference identity changed"));
                }
                reference
                    .price
                    .0
                    .checked_mul(Decimal::from(10_000 + slippage))
                    .and_then(|value| value.checked_div(Decimal::from(10_000)))
                    .map(Price)
            }
        }
        .ok_or_else(|| risk_error("pending order risk cannot be valued"))?;
        let valuation = Self {
            client_order_id: envelope.client_order_id.clone(),
            strategy_id: intent.strategy_id.clone(),
            account_id: envelope.account_id.clone(),
            venue: intent.target_venue,
            symbol: intent.symbol.clone(),
            side: intent.side,
            asset_class: intent.asset_class,
            product_type: intent.product_type,
            quantity: intent.quantity,
            unit_price: price,
            observed_unit_price: price,
            observed_revision: None,
        };
        valuation.validate()?;
        Ok(valuation)
    }

    fn validate(&self) -> HftResult<()> {
        if self.client_order_id.trim().is_empty()
            || self.strategy_id.trim().is_empty()
            || self.quantity.0 <= Decimal::ZERO
            || self.unit_price.0 <= Decimal::ZERO
            || self.observed_unit_price.0 < self.unit_price.0
            || self
                .quantity
                .0
                .checked_mul(self.observed_unit_price.0)
                .is_none()
        {
            return Err(risk_error("invalid pending risk identity or valuation"));
        }
        Ok(())
    }

    pub(super) fn risk_price(&self) -> Price {
        self.unit_price
    }

    pub(super) fn add_to_view(&self, view: &mut AdmissionExposure) -> HftResult<()> {
        self.add_remaining_to_view(view, self.quantity, self.unit_price)
    }

    fn add_remaining_to_view(
        &self,
        view: &mut AdmissionExposure,
        quantity: Quantity,
        price: Price,
    ) -> HftResult<()> {
        add_gross(&mut view.account, &self.symbol, quantity, price)?;
        if quantity.0 > Decimal::ZERO {
            view.pending.push(ports::PendingRiskExposure {
                strategy_id: self.strategy_id.clone(),
                symbol: self.symbol.clone(),
                quantity,
                asset_class: self.asset_class,
                product_type: self.product_type,
                notional: quantity
                    .0
                    .checked_mul(price.0)
                    .ok_or_else(|| risk_error("pending exposure overflow"))?,
            });
        }
        Ok(())
    }

    fn matches_record(&self, order: OrderRiskView<'_>) -> bool {
        order.revision < u32::MAX
            && self.observed_revision == Some(order.revision)
            && order.client_order_id == Some(self.client_order_id.as_str())
            && order.strategy_id == Some(self.strategy_id.as_str())
            && self
                .account_id
                .as_ref()
                .is_none_or(|id| order.account_id == Some(id))
            && self.venue.is_none_or(|venue| order.venue == Some(venue))
            && order.symbol == &self.symbol
            && order.side == self.side
            && order.qty.0 > Decimal::ZERO
            && order.qty.0 <= self.quantity.0
            && order.cum_qty.0 >= Decimal::ZERO
    }

    fn order_exposure(&self, order: OrderRiskView<'_>) -> HftResult<(Quantity, Price)> {
        if !self.matches_record(order) {
            return Err(risk_error(
                "OMS identity differs from admitted risk valuation",
            ));
        }
        let quantity = Quantity((order.qty.0 - order.cum_qty.0).max(Decimal::ZERO));
        // Never shrink the approval-time price bound after a favorable market move or amend.
        let price = Price(
            order
                .limit_price
                .map_or(self.observed_unit_price.0, |price| {
                    price.0.max(self.observed_unit_price.0)
                }),
        );
        Ok((quantity, price))
    }
}

pub(super) struct AdmissionExposure {
    pub account: AccountView,
    pub pending: Vec<ports::PendingRiskExposure>,
}

#[derive(Default)]
pub(super) struct RiskReservations {
    checkpoint: RiskReservationCheckpoint,
    recovery_required: bool,
    checkpoint_supplied: bool,
}

fn risk_error(message: &str) -> HftError {
    HftError::Risk(message.to_string())
}

fn open(status: OrderStatus) -> bool {
    matches!(
        status,
        OrderStatus::New
            | OrderStatus::Acknowledged
            | OrderStatus::Accepted
            | OrderStatus::PartiallyFilled
            | OrderStatus::Unknown
    )
}

impl RiskReservations {
    pub(super) fn export(&self) -> RiskReservationCheckpoint {
        self.checkpoint.clone()
    }

    pub(super) fn restore(&mut self, checkpoint: RiskReservationCheckpoint) -> HftResult<()> {
        self.recovery_required = true;
        if (!self.checkpoint.queued.is_empty() || !self.checkpoint.orders.is_empty())
            && self.checkpoint != checkpoint
        {
            return Err(risk_error(
                "restore cannot discard current unresolved risk reservations",
            ));
        }
        if checkpoint.schema != 2 {
            return Err(risk_error("unsupported risk reservation checkpoint"));
        }
        for (id, valuation) in &checkpoint.queued {
            valuation.validate()?;
            if id != &valuation.client_order_id || valuation.observed_revision.is_some() {
                return Err(risk_error("queued risk identity changed"));
            }
        }
        let mut clients: HashSet<_> = checkpoint.queued.keys().map(String::as_str).collect();
        for (id, valuation) in &checkpoint.orders {
            valuation.validate()?;
            if id.0.is_empty()
                || valuation.observed_revision.is_none()
                || !clients.insert(&valuation.client_order_id)
            {
                return Err(risk_error("duplicate restored risk identity"));
            }
        }
        if checkpoint
            .awaiting_final_readback
            .iter()
            .any(|id| !checkpoint.orders.contains_key(id))
        {
            return Err(risk_error(
                "terminal risk hold lacks its admitted valuation",
            ));
        }
        self.checkpoint = checkpoint;
        self.checkpoint_supplied = true;
        Ok(())
    }

    pub(super) fn owners_restored(&mut self) {
        self.recovery_required = true;
        self.checkpoint_supplied = false;
    }

    pub(super) fn has_queued_client(&self, client_id: &str) -> bool {
        self.checkpoint.queued.contains_key(client_id)
    }

    pub(super) fn require_recovery(&mut self) {
        self.recovery_required = true;
    }

    pub(super) fn is_pristine(&self) -> bool {
        !self.recovery_required
            && !self.checkpoint_supplied
            && self.checkpoint.queued.is_empty()
            && self.checkpoint.orders.is_empty()
            && self.checkpoint.awaiting_final_readback.is_empty()
    }

    pub(super) fn is_ready(&self) -> bool {
        !self.recovery_required
    }

    pub(super) fn has_queued(&self) -> bool {
        !self.checkpoint.queued.is_empty()
    }

    pub(super) fn reserve(&mut self, valuation: Valuation) -> HftResult<()> {
        if self
            .checkpoint
            .queued
            .contains_key(&valuation.client_order_id)
            || self
                .checkpoint
                .orders
                .values()
                .any(|v| v.client_order_id == valuation.client_order_id)
        {
            return Err(risk_error(
                "risk reservation already exists for this client order",
            ));
        }
        self.checkpoint
            .queued
            .insert(valuation.client_order_id.clone(), valuation);
        Ok(())
    }

    pub(super) fn remove_unsent(&mut self, client_id: &str) {
        self.checkpoint.queued.remove(client_id);
    }

    pub(super) fn validate_new(&self, event: &ExecutionEvent) -> HftResult<()> {
        let ExecutionEvent::OrderNew {
            order_id,
            client_order_id: Some(id),
            account_id,
            symbol,
            side,
            quantity,
            venue,
            requested_price,
            strategy_id,
            ..
        } = event
        else {
            return Ok(());
        };
        if self
            .checkpoint
            .orders
            .get(order_id)
            .is_some_and(|valuation| valuation.client_order_id != *id)
        {
            return Err(risk_error(
                "OrderNew reused another admitted order identity",
            ));
        }
        if let Some(valuation) = self.checkpoint.queued.get(id) {
            if valuation
                .account_id
                .as_ref()
                .is_some_and(|expected| account_id.as_ref() != Some(expected))
                || valuation
                    .venue
                    .is_some_and(|expected| *venue != Some(expected))
                || &valuation.strategy_id != strategy_id
                || &valuation.symbol != symbol
                || valuation.side != *side
                || valuation.quantity != *quantity
                || requested_price.is_some_and(|price| {
                    price.0 <= Decimal::ZERO || price.0 > valuation.unit_price.0
                })
            {
                return Err(risk_error("OrderNew changed its queued risk reservation"));
            }
        }
        Ok(())
    }

    pub(super) fn observe(
        &mut self,
        event: &ExecutionEvent,
        order: Option<OrderRiskView<'_>>,
        final_cancel_venues: &HashSet<VenueId>,
        queued_rejection: bool,
    ) {
        match event {
            ExecutionEvent::OrderNew {
                order_id,
                client_order_id: Some(id),
                ..
            } => {
                if self.checkpoint.queued.contains_key(id) {
                    // A fresh queue admission cannot reconstruct earlier amendments.
                    if let Some(order) = order.filter(|order| order.revision == 0) {
                        let mut valuation = self.checkpoint.queued.remove(id).unwrap();
                        valuation.observed_revision = Some(order.revision);
                        self.checkpoint.orders.insert(order_id.clone(), valuation);
                    } else {
                        self.recovery_required = true;
                    }
                } else if !self.checkpoint.orders.contains_key(order_id)
                    && order.is_none_or(|order| open(order.status))
                {
                    self.recovery_required = true;
                }
            }
            ExecutionEvent::OrderNew {
                client_order_id: None,
                ..
            } => {
                // OMS accepted a submitted order, but no admission identity can bind it.
                // Keep any queued reservations and reject further admission until recovery.
                self.recovery_required = true;
            }
            ExecutionEvent::OrderModified { order_id, .. } => {
                if let (Some(valuation), Some(order)) =
                    (self.checkpoint.orders.get_mut(order_id), order)
                {
                    if let Some(price) = order.limit_price {
                        valuation.observed_unit_price.0 =
                            valuation.observed_unit_price.0.max(price.0);
                    }
                    // A newer event cannot fill a gap in the imported price history.
                    if order.revision < u32::MAX
                        && valuation
                            .observed_revision
                            .and_then(|revision| revision.checked_add(1))
                            == Some(order.revision)
                    {
                        valuation.observed_revision = Some(order.revision);
                    } else {
                        self.recovery_required = true;
                    }
                } else {
                    self.recovery_required = true;
                }
            }
            ExecutionEvent::OrderReject { order_id, .. } => {
                // Only typed worker provenance proves the intent was never submitted.
                // A private venue order ID may collide with an unrelated queued client ID.
                if queued_rejection {
                    self.checkpoint.queued.remove(&order_id.0);
                } else if order.is_some_and(|order| order.status == OrderStatus::Filled) {
                    self.checkpoint.orders.remove(order_id);
                    self.checkpoint.awaiting_final_readback.remove(order_id);
                } else if self.checkpoint.orders.contains_key(order_id)
                    && order.is_some_and(|order| order.status == OrderStatus::Rejected)
                {
                    // Submitted rejection and fills can arrive on different private topics.
                    self.checkpoint
                        .awaiting_final_readback
                        .insert(order_id.clone());
                } else if order.is_none() {
                    self.recovery_required = true;
                }
            }
            ExecutionEvent::OrderCanceled { order_id, .. } => {
                if self.checkpoint.orders.contains_key(order_id) {
                    if order
                        .and_then(|order| order.venue)
                        .is_some_and(|venue| final_cancel_venues.contains(&venue))
                    {
                        self.checkpoint.orders.remove(order_id);
                        self.checkpoint.awaiting_final_readback.remove(order_id);
                    } else {
                        self.checkpoint
                            .awaiting_final_readback
                            .insert(order_id.clone());
                    }
                }
            }
            ExecutionEvent::Fill { order_id, .. }
                if order.is_some_and(|order| order.cum_qty.0 >= order.qty.0) =>
            {
                self.checkpoint.orders.remove(order_id);
                self.checkpoint.awaiting_final_readback.remove(order_id);
            }
            _ => {}
        }
    }

    pub(super) fn view(
        &self,
        account: &AccountView,
        manager: Option<&dyn OrderManager>,
    ) -> HftResult<AdmissionExposure> {
        if self.recovery_required {
            return Err(risk_error(
                "risk reservations require independent reconciliation",
            ));
        }
        self.project(
            account,
            self.checkpoint.orders.keys().map(|id| {
                manager
                    .and_then(|manager| manager.order_risk_view(id))
                    .ok_or_else(|| risk_error("admitted order lacks canonical OMS risk readback"))
            }),
        )
    }

    fn project<'a>(
        &self,
        account: &AccountView,
        orders: impl IntoIterator<Item = HftResult<OrderRiskView<'a>>>,
    ) -> HftResult<AdmissionExposure> {
        let mut view = AdmissionExposure {
            account: account.clone(),
            pending: Vec::new(),
        };
        for valuation in self.checkpoint.queued.values() {
            valuation.add_to_view(&mut view)?;
        }
        let mut observed = HashSet::new();
        for order in orders {
            let order = order?;
            let id = order.order_id;
            observed.insert(id);
            let valuation = self.checkpoint.orders.get(id);
            if let Some(valuation) = valuation {
                if !valuation.matches_record(order) {
                    return Err(risk_error(
                        "OMS identity differs from admitted risk valuation",
                    ));
                }
            }
            if open(order.status) || self.checkpoint.awaiting_final_readback.contains(id) {
                let valuation = valuation
                    .ok_or_else(|| risk_error("OMS order lacks approval-time risk valuation"))?;
                let (quantity, price) = valuation.order_exposure(order)?;
                valuation.add_remaining_to_view(&mut view, quantity, price)?;
            }
        }
        if self
            .checkpoint
            .orders
            .keys()
            .any(|id| !observed.contains(id))
        {
            return Err(risk_error("admitted order disappeared from canonical OMS"));
        }
        Ok(view)
    }

    /// Called only after complete order, holdings and recent-fill reconciliation succeeds.
    /// A recovered queue entry is an unknown submission, not proof of zero consumption.
    pub(super) fn confirm_readback(
        &mut self,
        account: &AccountView,
        orders: &HashMap<OrderId, OrderRecord>,
        exchange_order_ids: &HashSet<&OrderId>,
        exchange_client_ids: &HashSet<&str>,
    ) -> HftResult<()> {
        if self.recovery_required && !self.checkpoint_supplied {
            return Err(risk_error(
                "restored owners require an explicit risk reservation checkpoint",
            ));
        }
        if !self.checkpoint.queued.is_empty() {
            return Err(risk_error(
                "queued or unknown submission still lacks an OMS binding",
            ));
        }
        self.project(account, orders.values().map(|order| Ok(order.into())))?;
        for id in &self.checkpoint.awaiting_final_readback {
            let valuation = self
                .checkpoint
                .orders
                .get(id)
                .ok_or_else(|| risk_error("terminal order lacks its admitted risk identity"))?;
            if exchange_order_ids.contains(id)
                || exchange_client_ids.contains(valuation.client_order_id.as_str())
            {
                return Err(risk_error(
                    "terminal order is still open in authoritative readback",
                ));
            }
        }
        for id in self.checkpoint.awaiting_final_readback.drain() {
            self.checkpoint.orders.remove(&id);
        }
        self.recovery_required = false;
        Ok(())
    }

    pub(super) fn validate_replace(
        &self,
        order: OrderRiskView<'_>,
        quantity: Option<Quantity>,
        price: Option<Price>,
    ) -> HftResult<()> {
        let valuation = self
            .checkpoint
            .orders
            .get(order.order_id)
            .ok_or_else(|| risk_error("replacement lacks approval-time risk valuation"))?;
        let (old_quantity, old_price) = valuation.order_exposure(order)?;
        if quantity
            .is_some_and(|quantity| quantity.0 <= Decimal::ZERO || quantity.0 < order.cum_qty.0)
            || price.is_some_and(|price| price.0 <= Decimal::ZERO)
        {
            return Err(risk_error("replacement has invalid quantity or price"));
        }
        let quantity = quantity.map_or(old_quantity, |total| Quantity(total.0 - order.cum_qty.0));
        let price = Price(price.map_or(old_price.0, |price| price.0.max(old_price.0)));
        let previous = old_quantity
            .0
            .checked_mul(old_price.0)
            .ok_or_else(|| risk_error("risk valuation overflow"))?;
        let next = quantity
            .0
            .checked_mul(price.0)
            .ok_or_else(|| risk_error("replacement risk overflow"))?;
        if next > previous {
            return Err(risk_error(
                "replacement increases reserved notional; a new reviewed intent is required",
            ));
        }
        Ok(())
    }
}

fn add_gross(
    view: &mut AccountView,
    symbol: &Symbol,
    quantity: Quantity,
    price: Price,
) -> HftResult<()> {
    if quantity.0 == Decimal::ZERO {
        return Ok(());
    }
    let position = view
        .positions
        .entry(symbol.clone())
        .or_insert_with(|| ports::Position {
            symbol: symbol.clone(),
            quantity: Quantity::zero(),
            avg_price: price,
            unrealized_pnl: Decimal::ZERO,
            realized_pnl: Decimal::ZERO,
        });
    let old_notional = position
        .avg_price
        .0
        .checked_mul(position.quantity.0)
        .and_then(|value| value.checked_add(position.unrealized_pnl))
        .map(|value| value.abs())
        .ok_or_else(|| risk_error("existing risk exposure overflow"))?;
    let notional = quantity
        .0
        .checked_mul(price.0)
        .and_then(|value| old_notional.checked_add(value))
        .ok_or_else(|| risk_error("pending risk exposure overflow"))?;
    let gross = position
        .quantity
        .0
        .abs()
        .checked_add(quantity.0)
        .ok_or_else(|| risk_error("pending risk quantity overflow"))?;
    let basis = position
        .avg_price
        .0
        .checked_mul(gross)
        .ok_or_else(|| risk_error("pending risk basis overflow"))?;
    position.quantity = Quantity(gross);
    // Risk-only projection: preserve the exact gross notional without rounding an average price.
    position.unrealized_pnl = notional
        .checked_sub(basis)
        .ok_or_else(|| risk_error("pending risk mark overflow"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(side: Side) -> OrderIntentEnvelope {
        OrderIntentEnvelope::new(
            ports::OrderIntent::crypto_spot(
                Symbol::new("BTCUSDT"),
                side,
                Quantity(Decimal::ONE),
                OrderType::Limit,
                Some(Price(Decimal::from(60))),
                hft_core::TimeInForce::GTC,
                "risk-test".into(),
                Some(VenueId::BINANCE),
            ),
            ports::OrderIntentLifecycle::new(hft_core::now_micros(), u64::MAX),
        )
    }

    #[test]
    fn opposite_unfilled_orders_are_gross_exposure() {
        let mut reservations = RiskReservations::default();
        reservations
            .reserve(Valuation::from_envelope(&intent(Side::Buy)).unwrap())
            .unwrap();
        reservations
            .reserve(Valuation::from_envelope(&intent(Side::Sell)).unwrap())
            .unwrap();
        let view = reservations.view(&AccountView::default(), None).unwrap();
        let position = &view.account.positions[&Symbol::new("BTCUSDT")];
        assert_eq!(position.quantity.0, Decimal::from(2));
        assert_eq!(
            position.avg_price.0 * position.quantity.0 + position.unrealized_pnl,
            Decimal::from(120)
        );
    }

    #[test]
    fn market_valuation_preserves_admitted_quote_and_slippage_bound() {
        let mut envelope = intent(Side::Buy);
        envelope.intent.order_type = OrderType::Market;
        assert!(Valuation::from_envelope(&envelope).is_err());
        envelope.lifecycle.max_slippage_bps = Some(500);
        envelope.price_reference = Some(ports::ExecutionPriceReference {
            venue: VenueId::BINANCE,
            symbol: envelope.intent.symbol.clone(),
            side: Side::Buy,
            price: Price(Decimal::from(60)),
            book_sequence: 1,
            received_at: hft_core::LocalReceiveTimestamp::new(hft_core::now_micros()),
        });
        let valuation = Valuation::from_envelope(&envelope).unwrap();
        assert_eq!(valuation.unit_price.0, Decimal::from(63));
        envelope.price_reference.as_mut().unwrap().price = Price(Decimal::from(30));
        let mut reservations = RiskReservations::default();
        reservations.reserve(valuation).unwrap();
        let view = reservations.view(&AccountView::default(), None).unwrap();
        let position = &view.account.positions[&Symbol::new("BTCUSDT")];
        assert_eq!(
            position.avg_price.0 * position.quantity.0 + position.unrealized_pnl,
            Decimal::from(63)
        );
        envelope.price_reference.as_mut().unwrap().symbol = Symbol::new("ETHUSDT");
        assert!(Valuation::from_envelope(&envelope).is_err());
    }
}
