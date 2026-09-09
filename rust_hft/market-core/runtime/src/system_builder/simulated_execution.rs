mod book_matching;

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, OnceLock,
};

use async_trait::async_trait;
use engine::aggregation::MarketView;
use futures::StreamExt;
use hft_core::{
    BookBudget, HftError, OrderId, OrderType, Price, Quantity, Symbol, TimeInForce, Timestamp,
    VenueId, VenueSymbol,
};
use ports::{
    BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent, HftResult, OpenOrder,
    OrderIntent, OrderStatus,
};
use rust_decimal::Decimal;
use tokio::{
    sync::{
        mpsc::{channel, Receiver, Sender},
        Mutex,
    },
    task::JoinHandle,
    time::{Duration, Instant},
};
use tokio_stream::wrappers::ReceiverStream;

use book_matching::{displayed_snapshot, valid_book};

const DEFAULT_FILL_DELAY_MS: u64 = 50;
const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 4096;
const MAX_OPEN_ORDERS: usize = 4096;
pub(super) type MarketBinding = Arc<OnceLock<Arc<dyn snapshot::SnapshotReader<MarketView>>>>;

#[derive(Clone)]
struct PendingOrder {
    order: OpenOrder,
    tif: TimeInForce,
    ordinal: u64,
    eligible_at: Instant,
    filled_notional: Decimal,
}

#[derive(Default)]
struct PaperState {
    orders: HashMap<OrderId, PendingOrder>,
    books: HashMap<Symbol, BookBudget>,
}

pub struct SimulatedExecutionClient {
    venue: VenueId,
    events_tx: Sender<ExecutionEvent>,
    events_rx: Arc<Mutex<Option<Receiver<ExecutionEvent>>>>,
    id_counter: Arc<AtomicU64>,
    connected: Arc<AtomicBool>,
    dropped_events: Arc<AtomicU64>,
    state: Arc<Mutex<PaperState>>,
    market: MarketBinding,
    matcher: Option<JoinHandle<()>>,
    fill_delay_ms: u64,
}

impl SimulatedExecutionClient {
    #[allow(dead_code)]
    pub fn new(venue: VenueId) -> Self {
        Self::new_with_event_queue_capacity(venue, DEFAULT_EVENT_QUEUE_CAPACITY)
    }

    pub fn new_with_event_queue_capacity(venue: VenueId, event_queue_capacity: usize) -> Self {
        let (tx, rx) = channel(event_queue_capacity.max(1));
        Self {
            venue,
            events_tx: tx,
            events_rx: Arc::new(Mutex::new(Some(rx))),
            id_counter: Arc::new(AtomicU64::new(1)),
            connected: Arc::new(AtomicBool::new(false)),
            dropped_events: Arc::new(AtomicU64::new(0)),
            state: Arc::new(Mutex::new(PaperState::default())),
            market: Arc::new(OnceLock::new()),
            matcher: None,
            fill_delay_ms: DEFAULT_FILL_DELAY_MS,
        }
    }

    pub(super) fn market_binding(&self) -> MarketBinding {
        self.market.clone()
    }
    fn current_timestamp() -> Timestamp {
        hft_core::now_micros()
    }
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    fn reserve_events(
        &self,
        count: usize,
    ) -> HftResult<tokio::sync::mpsc::PermitIterator<'_, ExecutionEvent>> {
        reserve_events(&self.events_tx, &self.dropped_events, count)
    }

    fn start_matcher(&mut self) {
        if self
            .matcher
            .as_ref()
            .is_some_and(|task| !task.is_finished())
        {
            return;
        }
        let state = self.state.clone();
        let market = self.market.clone();
        let sender = self.events_tx.clone();
        let dropped = self.dropped_events.clone();
        let connected = self.connected.clone();
        let counter = self.id_counter.clone();
        let venue = self.venue;
        self.matcher = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(reader) = market.get() else {
                    continue;
                };
                let view = reader.load();
                let mut state = state.lock().await;
                if let Err(error) = match_orders(
                    &mut state,
                    &view,
                    venue,
                    &sender,
                    &dropped,
                    &counter,
                    Instant::now(),
                    Self::current_timestamp(),
                ) {
                    connected.store(false, Ordering::Release);
                    tracing::error!(%error, "paper matcher stopped before changing unreported order state");
                    break;
                }
            }
        }));
    }
}

impl Drop for SimulatedExecutionClient {
    fn drop(&mut self) {
        if let Some(task) = self.matcher.take() {
            task.abort();
        }
    }
}

fn reserve_events<'a>(
    sender: &'a Sender<ExecutionEvent>,
    dropped: &AtomicU64,
    count: usize,
) -> HftResult<tokio::sync::mpsc::PermitIterator<'a, ExecutionEvent>> {
    sender.try_reserve_many(count).map_err(|_| {
        dropped.fetch_add(1, Ordering::Relaxed);
        HftError::Execution(
            "simulated execution event queue lacks capacity for atomic update".into(),
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn match_orders(
    state: &mut PaperState,
    view: &MarketView,
    venue: VenueId,
    sender: &Sender<ExecutionEvent>,
    dropped: &AtomicU64,
    counter: &AtomicU64,
    instant: Instant,
    now: u64,
) -> HftResult<()> {
    // Observe every tracked symbol even during the arrival delay or between
    // orders, so disappearance/reappearance is not mistaken for unchanged depth.
    for (symbol, budget) in &mut state.books {
        if let Some(book) = view.get_orderbook(&VenueSymbol::new(venue, symbol.clone())) {
            if let Some(snapshot) = displayed_snapshot(book) {
                budget.observe(&snapshot, now);
            } else {
                budget.clear_levels();
            }
        } else {
            budget.clear_levels();
        }
    }
    let mut order_ids: Vec<_> = state
        .orders
        .iter()
        .map(|(id, pending)| (pending.ordinal, id.clone()))
        .collect();
    order_ids.sort_unstable_by_key(|(ordinal, _)| *ordinal);
    for (_, id) in order_ids {
        let pending = &state.orders[&id];
        if pending.eligible_at > instant {
            continue;
        }
        let symbol = pending.order.symbol.clone();
        let mut budget = state.books.get(&symbol).cloned().unwrap_or_default();
        let observed = view
            .get_orderbook(&VenueSymbol::new(venue, symbol.clone()))
            .and_then(displayed_snapshot)
            .is_some_and(|snapshot| budget.observe(&snapshot, now));
        let original_budget = budget.clone();
        let mut fills = if observed {
            budget
                .fills(
                    pending.order.side,
                    pending.order.order_type,
                    pending.order.price,
                    pending.order.remaining_quantity,
                )
                .into_iter()
                .map(|fill| (fill.price, fill.quantity))
                .collect()
        } else {
            Vec::new()
        };
        let total: Decimal = fills.iter().map(|(_, quantity)| quantity.0).sum();
        if pending.tif == TimeInForce::FOK && total < pending.order.remaining_quantity.0 {
            fills.clear();
            budget = original_budget;
        }
        let total: Decimal = fills.iter().map(|(_, quantity)| quantity.0).sum();
        let completed = total == pending.order.remaining_quantity.0;
        let canceled = !completed
            && (pending.tif != TimeInForce::GTC || pending.order.order_type == OrderType::Market);
        let count = fills.len() + usize::from(completed || canceled);
        if count == 0 {
            if observed {
                state.books.insert(symbol, budget);
            }
            continue;
        }
        let permits = reserve_events(sender, dropped, count)?;
        let mut pending = state.orders[&id].clone();
        let mut events = Vec::with_capacity(count);
        for (price, quantity) in fills {
            pending.order.filled_quantity.0 += quantity.0;
            pending.order.remaining_quantity.0 -= quantity.0;
            pending.filled_notional = price
                .0
                .checked_mul(quantity.0)
                .and_then(|notional| pending.filled_notional.checked_add(notional))
                .ok_or_else(|| HftError::Execution("paper fill notional overflow".into()))?;
            pending.order.updated_at = now;
            pending.order.status = OrderStatus::PartiallyFilled;
            events.push(ExecutionEvent::Fill {
                order_id: id.clone(),
                price,
                quantity,
                timestamp: now,
                fill_id: format!(
                    "sim_fill_{}_{}",
                    venue.as_str(),
                    counter.fetch_add(1, Ordering::Relaxed)
                ),
            });
        }
        if completed {
            events.push(ExecutionEvent::OrderCompleted {
                order_id: id.clone(),
                final_price: Price(pending.filled_notional / pending.order.filled_quantity.0),
                total_filled: pending.order.filled_quantity,
                timestamp: now,
            });
        } else if canceled {
            events.push(ExecutionEvent::OrderCanceled {
                order_id: id.clone(),
                timestamp: now,
            });
        }
        if observed {
            state.books.insert(symbol, budget);
        }
        if completed || canceled {
            state.orders.remove(&id);
        } else {
            state.orders.insert(id, pending);
        }
        for (permit, event) in permits.zip(events) {
            permit.send(event);
        }
    }
    Ok(())
}

#[async_trait]
impl ExecutionClient for SimulatedExecutionClient {
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        if !self.connected.load(Ordering::Acquire) || self.dropped_events() != 0 {
            return Err(HftError::Execution(
                "paper event delivery is unhealthy".into(),
            ));
        }
        if intent.target_venue != Some(self.venue)
            || intent.quantity.0 <= Decimal::ZERO
            || (intent.order_type == OrderType::Limit
                && intent.price.is_none_or(|price| price.0 <= Decimal::ZERO))
        {
            return Err(HftError::InvalidOrder(
                "paper order needs matching venue, positive quantity and a valid limit price"
                    .into(),
            ));
        }
        let now = Self::current_timestamp();
        let view = self
            .market
            .get()
            .ok_or_else(|| {
                HftError::Execution("paper execution requires the canonical market reader".into())
            })?
            .load();
        let book = view
            .get_orderbook(&VenueSymbol::new(self.venue, intent.symbol.clone()))
            .filter(|book| valid_book(book, now))
            .ok_or_else(|| {
                HftError::Execution("paper execution requires a fresh valid observed book".into())
            })?;
        let arrival_price = Some(Price::from(match intent.side {
            hft_core::Side::Buy => book.ask_prices[0],
            hft_core::Side::Sell => book.bid_prices[0],
        }));
        let ordinal = self.id_counter.fetch_add(1, Ordering::Relaxed);
        let id = OrderId(format!("sim_{}_{}", self.venue.as_str(), ordinal));
        {
            let mut state = self.state.lock().await;
            if state.orders.len() >= MAX_OPEN_ORDERS
                || (!state.books.contains_key(&intent.symbol)
                    && state.books.len() >= MAX_OPEN_ORDERS)
            {
                return Err(HftError::Execution(
                    "paper order or tracked-book bound reached".into(),
                ));
            }
            let permits = self.reserve_events(2)?;
            let events = [
                ExecutionEvent::OrderNew {
                    order_id: id.clone(),
                    client_order_id: None,
                    account_id: None,
                    symbol: intent.symbol.clone(),
                    side: intent.side,
                    quantity: intent.quantity,
                    requested_price: intent.price,
                    arrival_price,
                    timestamp: now,
                    venue: Some(self.venue),
                    strategy_id: intent.strategy_id,
                },
                ExecutionEvent::OrderAck {
                    order_id: id.clone(),
                    timestamp: now,
                },
            ];
            state.books.entry(intent.symbol.clone()).or_default();
            state.orders.insert(
                id.clone(),
                PendingOrder {
                    order: OpenOrder {
                        order_id: id.clone(),
                        client_order_id: None,
                        symbol: intent.symbol,
                        side: intent.side,
                        order_type: intent.order_type,
                        original_quantity: intent.quantity,
                        remaining_quantity: intent.quantity,
                        filled_quantity: Quantity(Decimal::ZERO),
                        price: intent.price,
                        status: OrderStatus::Accepted,
                        created_at: now,
                        updated_at: now,
                    },
                    tif: intent.time_in_force,
                    ordinal,
                    eligible_at: Instant::now() + Duration::from_millis(self.fill_delay_ms),
                    filled_notional: Decimal::ZERO,
                },
            );
            for (permit, event) in permits.zip(events) {
                permit.send(event);
            }
        }
        self.start_matcher();
        Ok(id)
    }

    async fn cancel_order(&mut self, id: &OrderId) -> HftResult<()> {
        let mut state = self.state.lock().await;
        if !state.orders.contains_key(id) {
            return Err(HftError::OrderNotFound(id.0.clone()));
        }
        let mut permits = self.reserve_events(1)?;
        state.orders.remove(id);
        permits.next().unwrap().send(ExecutionEvent::OrderCanceled {
            order_id: id.clone(),
            timestamp: Self::current_timestamp(),
        });
        Ok(())
    }

    async fn modify_order(
        &mut self,
        id: &OrderId,
        quantity: Option<Quantity>,
        price: Option<Price>,
    ) -> HftResult<()> {
        let mut state = self.state.lock().await;
        let pending = state
            .orders
            .get_mut(id)
            .ok_or_else(|| HftError::OrderNotFound(id.0.clone()))?;
        if quantity.is_some_and(|quantity| quantity.0 <= pending.order.filled_quantity.0)
            || price.is_some_and(|price| price.0 <= Decimal::ZERO)
        {
            return Err(HftError::InvalidOrder(
                "paper amendment needs quantity above filled total and positive price".into(),
            ));
        }
        let mut permits = self.reserve_events(1)?;
        if let Some(quantity) = quantity {
            pending.order.original_quantity = quantity;
            pending.order.remaining_quantity.0 = quantity.0 - pending.order.filled_quantity.0;
        }
        if let Some(price) = price {
            pending.order.price = Some(price);
        }
        pending.order.updated_at = Self::current_timestamp();
        pending.eligible_at = Instant::now() + Duration::from_millis(self.fill_delay_ms);
        pending.ordinal = self.id_counter.fetch_add(1, Ordering::Relaxed);
        permits.next().unwrap().send(ExecutionEvent::OrderModified {
            order_id: id.clone(),
            new_quantity: quantity,
            new_price: price,
            timestamp: pending.order.updated_at,
        });
        Ok(())
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        let rx = self
            .events_rx
            .lock()
            .await
            .take()
            .ok_or_else(|| HftError::new("Simulated execution stream already taken"))?;
        Ok(Box::pin(ReceiverStream::new(rx).map(Ok)))
    }
    fn is_simulated_execution(&self) -> bool {
        true
    }
    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        Ok(self
            .state
            .lock()
            .await
            .orders
            .values()
            .map(|pending| pending.order.clone())
            .collect())
    }
    async fn connect(&mut self) -> HftResult<()> {
        if self.dropped_events() != 0 {
            return Err(HftError::Execution(
                "paper event delivery is unhealthy".into(),
            ));
        }
        self.connected.store(true, Ordering::Release);
        self.start_matcher();
        Ok(())
    }
    async fn disconnect(&mut self) -> HftResult<()> {
        self.connected.store(false, Ordering::Release);
        if let Some(task) = self.matcher.take() {
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }
    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.connected.load(Ordering::Acquire) && self.dropped_events() == 0,
            latency_ms: Some(self.fill_delay_ms as f64),
            last_heartbeat: Self::current_timestamp(),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderType, Side, Symbol, TimeInForce};

    fn book_view(ask_quantity: i64, sequence: u64, received_at: u64) -> MarketView {
        let symbol = Symbol::new("BTCUSDT");
        let mut book = engine::aggregation::TopNSnapshot::new(symbol.clone(), 5);
        book.sequence = sequence;
        book.generation = 1;
        book.local_receive = Some(hft_core::LocalReceiveTimestamp::new(received_at));
        book.bid_prices.push(Price(Decimal::from(99)).into());
        book.bid_quantities.push(Quantity(Decimal::from(10)).into());
        book.ask_prices.push(Price(Decimal::from(101)).into());
        book.ask_quantities
            .push(Quantity(Decimal::from(ask_quantity)).into());
        book.ask_prices.push(Price(Decimal::from(102)).into());
        book.ask_quantities.push(Quantity(Decimal::from(2)).into());
        MarketView {
            orderbooks: [(VenueSymbol::new(VenueId::MOCK, symbol), Arc::new(book))]
                .into_iter()
                .collect(),
            arbitrage_opportunities: vec![],
            timestamp: received_at,
            version: sequence,
        }
    }

    fn bind_book(client: &SimulatedExecutionClient) -> Arc<snapshot::ArcSwapPublisher<MarketView>> {
        let publisher = Arc::new(snapshot::ArcSwapPublisher::new(book_view(
            2,
            1,
            hft_core::now_micros(),
        )));
        assert!(client.market_binding().set(publisher.clone()).is_ok());
        client.connected.store(true, Ordering::Release);
        publisher
    }

    async fn tick(client: &SimulatedExecutionClient, view: &MarketView) -> HftResult<()> {
        match_orders(
            &mut *client.state.lock().await,
            view,
            client.venue,
            &client.events_tx,
            &client.dropped_events,
            &client.id_counter,
            Instant::now() + Duration::from_secs(60),
            hft_core::now_micros(),
        )
    }

    async fn events(client: &SimulatedExecutionClient) -> Vec<ExecutionEvent> {
        let mut received = vec![];
        let mut guard = client.events_rx.lock().await;
        let receiver = guard.as_mut().unwrap();
        while let Ok(event) = receiver.try_recv() {
            received.push(event);
        }
        received
    }

    fn test_intent() -> OrderIntent {
        OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            asset_class: hft_core::AssetClass::Crypto,
            product_type: hft_core::ProductType::Spot,
            compliance_context: hft_core::ComplianceContext::default(),
            side: Side::Buy,
            quantity: Quantity::from_f64(1.0).expect("valid quantity"),
            order_type: OrderType::Market,
            price: Some(Price::from_f64(50_000.0).expect("valid price")),
            time_in_force: TimeInForce::IOC,
            strategy_id: "sim-test".to_string(),
            target_venue: Some(VenueId::MOCK),
        }
    }

    #[tokio::test]
    async fn simulated_market_order_requires_observed_book() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.connected.store(true, Ordering::Release);
        assert!(
            client.place_order(test_intent()).await.is_err(),
            "a market order cannot fill without an observed execution price"
        );
        assert!(client.list_open_orders().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn simulated_market_order_rejects_a_malformed_displayed_book() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        let mut malformed = book_view(2, 1, hft_core::now_micros());
        let book = Arc::make_mut(malformed.orderbooks.values_mut().next().unwrap());
        book.bid_quantities.pop();
        assert!(client
            .market_binding()
            .set(Arc::new(snapshot::ArcSwapPublisher::new(malformed)))
            .is_ok());
        client.connected.store(true, Ordering::Release);

        assert!(client.place_order(test_intent()).await.is_err());
        assert!(client.list_open_orders().await.unwrap().is_empty());
    }

    #[test]
    fn simulated_execution_is_marked_non_external() {
        let client = SimulatedExecutionClient::new(VenueId::MOCK);

        assert!(client.is_simulated_execution());
    }

    #[tokio::test]
    async fn simulated_execution_event_queue_is_bounded() {
        let mut client = SimulatedExecutionClient::new_with_event_queue_capacity(VenueId::MOCK, 1);
        bind_book(&client);

        let result = client.place_order(test_intent()).await;

        assert!(result.is_err());
        assert_eq!(client.dropped_events(), 1);
        assert!(client.list_open_orders().await.unwrap().is_empty());
        assert!(events(&client).await.is_empty());
    }

    #[tokio::test]
    async fn simulated_open_orders_are_authoritative_and_cancelable() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.fill_delay_ms = 5_000;
        bind_book(&client);

        let order_id = client
            .place_order(test_intent())
            .await
            .expect("place order");
        let open_orders = client.list_open_orders().await.expect("open orders");
        assert_eq!(open_orders.len(), 1);
        assert_eq!(open_orders[0].order_id, order_id);

        client.cancel_order(&order_id).await.expect("cancel order");
        assert!(client
            .list_open_orders()
            .await
            .expect("open orders after cancel")
            .is_empty());
    }
    #[tokio::test]
    async fn market_fills_use_visible_levels_and_ioc_cancels_unfilled_quantity() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.fill_delay_ms = 5_000;
        bind_book(&client);
        let mut intent = test_intent();
        intent.quantity = Quantity(Decimal::from(5));
        intent.price = Some(Price(Decimal::from(100)));
        client.place_order(intent).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(2, 1, hft_core::now_micros()))
            .await
            .unwrap();
        let received = events(&client).await;
        let fills: Vec<_> = received
            .iter()
            .filter_map(|event| match event {
                ExecutionEvent::Fill {
                    price, quantity, ..
                } => Some((price.0, quantity.0)),
                _ => None,
            })
            .collect();
        assert_eq!(
            fills,
            vec![
                (Decimal::from(101), Decimal::from(2)),
                (Decimal::from(102), Decimal::from(2))
            ]
        );
        assert!(matches!(
            received.last(),
            Some(ExecutionEvent::OrderCanceled { .. })
        ));
        assert!(client.list_open_orders().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn malformed_displayed_book_does_not_fill_an_existing_paper_order() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.fill_delay_ms = 5_000;
        bind_book(&client);
        let mut intent = test_intent();
        intent.order_type = OrderType::Limit;
        intent.time_in_force = TimeInForce::GTC;
        intent.price = Some(Price(Decimal::from(101)));
        intent.quantity = Quantity(Decimal::from(1));
        let order_id = client.place_order(intent).await.unwrap();
        events(&client).await;

        let mut malformed = book_view(2, 2, hft_core::now_micros());
        let book = Arc::make_mut(malformed.orderbooks.values_mut().next().unwrap());
        book.ask_quantities.pop();
        tick(&client, &malformed).await.unwrap();

        assert!(events(&client).await.is_empty());
        let open_orders = client.list_open_orders().await.unwrap();
        assert_eq!(open_orders.len(), 1);
        assert_eq!(open_orders[0].order_id, order_id);
        assert_eq!(open_orders[0].filled_quantity.0, Decimal::ZERO);
    }

    #[tokio::test]
    async fn limit_partial_fills_share_depth_and_unchanged_quotes_never_replenish_it() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.fill_delay_ms = 5_000;
        bind_book(&client);
        let mut intent = test_intent();
        intent.order_type = OrderType::Limit;
        intent.time_in_force = TimeInForce::GTC;
        intent.price = Some(Price(Decimal::from(101)));
        intent.quantity = Quantity(Decimal::from(3));
        let first = client.place_order(intent.clone()).await.unwrap();
        let second = client.place_order(intent).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(2, 1, hft_core::now_micros()))
            .await
            .unwrap();
        let state = client.list_open_orders().await.unwrap();
        assert_eq!(
            state
                .iter()
                .find(|order| order.order_id == first)
                .unwrap()
                .filled_quantity
                .0,
            Decimal::from(2)
        );
        assert_eq!(
            state
                .iter()
                .find(|order| order.order_id == second)
                .unwrap()
                .filled_quantity
                .0,
            Decimal::ZERO
        );
        events(&client).await;
        tick(&client, &book_view(2, 2, hft_core::now_micros()))
            .await
            .unwrap();
        assert!(events(&client).await.is_empty());
        tick(&client, &book_view(4, 3, hft_core::now_micros()))
            .await
            .unwrap();
        let state = client.list_open_orders().await.unwrap();
        assert_eq!(state.len(), 1);
        assert_eq!(state[0].order_id, second);
        assert_eq!(state[0].filled_quantity.0, Decimal::ONE);
        client
            .modify_order(&second, Some(Quantity(Decimal::from(2))), None)
            .await
            .unwrap();
        let state = client.list_open_orders().await.unwrap();
        assert_eq!(
            state[0].remaining_quantity.0,
            Decimal::ONE,
            "amend quantity is total, not remaining"
        );
        client.cancel_order(&second).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(20, 4, hft_core::now_micros()))
            .await
            .unwrap();
        assert!(
            events(&client).await.is_empty(),
            "canceled orders cannot fill later"
        );
    }

    #[tokio::test]
    async fn non_marketable_limit_waits_and_fok_does_not_consume_partial_depth() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.fill_delay_ms = 5_000;
        bind_book(&client);
        let mut intent = test_intent();
        intent.order_type = OrderType::Limit;
        intent.time_in_force = TimeInForce::GTC;
        intent.price = Some(Price(Decimal::from(100)));
        let waiting = client.place_order(intent.clone()).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(2, 1, hft_core::now_micros()))
            .await
            .unwrap();
        assert!(events(&client).await.is_empty());
        intent.price = Some(Price(Decimal::from(101)));
        intent.quantity = Quantity(Decimal::from(3));
        intent.time_in_force = TimeInForce::FOK;
        client.place_order(intent.clone()).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(2, 2, hft_core::now_micros()))
            .await
            .unwrap();
        let received = events(&client).await;
        assert_eq!(received.len(), 1);
        assert!(matches!(received[0], ExecutionEvent::OrderCanceled { .. }));
        intent.quantity = Quantity(Decimal::from(2));
        client.place_order(intent).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(2, 3, hft_core::now_micros()))
            .await
            .unwrap();
        assert!(events(&client)
            .await
            .iter()
            .any(|event| matches!(event, ExecutionEvent::OrderCompleted { .. })));
        assert_eq!(
            client.list_open_orders().await.unwrap()[0].order_id,
            waiting
        );
    }

    #[tokio::test]
    async fn simulated_review_new_generation_can_restart_its_sequence() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.fill_delay_ms = 5_000;
        bind_book(&client);
        let mut intent = test_intent();
        intent.order_type = OrderType::Limit;
        intent.time_in_force = TimeInForce::GTC;
        intent.price = Some(Price(Decimal::from(101)));
        intent.quantity = Quantity(Decimal::from(2));
        client.place_order(intent.clone()).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(2, 100, hft_core::now_micros()))
            .await
            .unwrap();
        events(&client).await;
        client.place_order(intent).await.unwrap();
        events(&client).await;
        let mut reconnected = book_view(2, 1, hft_core::now_micros());
        Arc::make_mut(reconnected.orderbooks.values_mut().next().unwrap()).generation = 2;
        tick(&client, &reconnected).await.unwrap();
        let filled: Decimal = events(&client)
            .await
            .into_iter()
            .filter_map(|event| match event {
                ExecutionEvent::Fill { quantity, .. } => Some(quantity.0),
                _ => None,
            })
            .sum();
        assert_eq!(filled, Decimal::from(2));
    }

    #[tokio::test]
    async fn simulated_review_depth_decrease_reduces_unspent_liquidity() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.fill_delay_ms = 5_000;
        bind_book(&client);
        let mut intent = test_intent();
        intent.order_type = OrderType::Limit;
        intent.time_in_force = TimeInForce::GTC;
        intent.price = Some(Price(Decimal::from(101)));
        intent.quantity = Quantity(Decimal::from(4));
        client.place_order(intent.clone()).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(10, 1, hft_core::now_micros()))
            .await
            .unwrap();
        events(&client).await;
        intent.quantity = Quantity(Decimal::from(6));
        client.place_order(intent).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(8, 2, hft_core::now_micros()))
            .await
            .unwrap();
        let filled: Decimal = events(&client)
            .await
            .into_iter()
            .filter_map(|event| match event {
                ExecutionEvent::Fill { quantity, .. } => Some(quantity.0),
                _ => None,
            })
            .sum();
        assert_eq!(filled, Decimal::from(4));
    }

    #[tokio::test]
    async fn simulated_review_tracks_level_removal_while_no_order_is_eligible() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.fill_delay_ms = 5_000;
        bind_book(&client);
        let mut intent = test_intent();
        intent.order_type = OrderType::Limit;
        intent.time_in_force = TimeInForce::GTC;
        intent.price = Some(Price(Decimal::from(101)));
        intent.quantity = Quantity(Decimal::from(2));
        client.place_order(intent.clone()).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(2, 1, hft_core::now_micros()))
            .await
            .unwrap();
        events(&client).await;
        assert!(client.list_open_orders().await.unwrap().is_empty());
        let mut removed = book_view(2, 2, hft_core::now_micros());
        let book = Arc::make_mut(removed.orderbooks.values_mut().next().unwrap());
        book.ask_prices.remove(0);
        book.ask_quantities.remove(0);
        tick(&client, &removed).await.unwrap();
        client.place_order(intent).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(2, 3, hft_core::now_micros()))
            .await
            .unwrap();
        let filled: Decimal = events(&client)
            .await
            .into_iter()
            .filter_map(|event| match event {
                ExecutionEvent::Fill { quantity, .. } => Some(quantity.0),
                _ => None,
            })
            .sum();
        assert_eq!(filled, Decimal::from(2));
    }

    #[tokio::test]
    async fn stale_book_and_event_backpressure_cannot_commit_unreported_fills() {
        let mut client = SimulatedExecutionClient::new_with_event_queue_capacity(VenueId::MOCK, 2);
        client.fill_delay_ms = 5_000;
        bind_book(&client);
        let mut intent = test_intent();
        intent.order_type = OrderType::Limit;
        intent.time_in_force = TimeInForce::GTC;
        intent.price = Some(Price(Decimal::from(102)));
        intent.quantity = Quantity(Decimal::from(3));
        client.place_order(intent).await.unwrap();
        events(&client).await;
        tick(
            &client,
            &book_view(2, 1, hft_core::now_micros() - 2_000_000),
        )
        .await
        .unwrap();
        assert!(events(&client).await.is_empty());
        assert!(tick(&client, &book_view(2, 2, hft_core::now_micros()))
            .await
            .is_err());
        assert!(
            events(&client).await.is_empty(),
            "two fills plus completion need three permits"
        );
        assert_eq!(
            client.list_open_orders().await.unwrap()[0]
                .filled_quantity
                .0,
            Decimal::ZERO
        );
        assert_eq!(client.dropped_events(), 1);
    }
    #[tokio::test]
    async fn sell_limits_use_bids_and_book_regression_cannot_replenish_depth() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.fill_delay_ms = 5_000;
        bind_book(&client);
        let mut intent = test_intent();
        intent.side = Side::Sell;
        intent.order_type = OrderType::Limit;
        intent.time_in_force = TimeInForce::GTC;
        intent.price = Some(Price(Decimal::from(100)));
        let id = client.place_order(intent).await.unwrap();
        events(&client).await;
        tick(&client, &book_view(2, 5, hft_core::now_micros()))
            .await
            .unwrap();
        assert!(events(&client).await.is_empty());
        client
            .modify_order(&id, None, Some(Price(Decimal::from(99))))
            .await
            .unwrap();
        events(&client).await;
        tick(&client, &book_view(20, 4, hft_core::now_micros()))
            .await
            .unwrap();
        assert!(
            events(&client).await.is_empty(),
            "regressed sequence is not fresh liquidity"
        );
        tick(&client, &book_view(2, 6, hft_core::now_micros()))
            .await
            .unwrap();
        assert!(events(&client).await.iter().any(|event| matches!(event,
            ExecutionEvent::Fill { price, quantity, .. } if price.0 == Decimal::from(99) && quantity.0 == Decimal::ONE)));
        client.disconnect().await.unwrap();
        assert!(client.place_order(test_intent()).await.is_err());
    }
}
