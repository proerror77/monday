use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use hft_core::{HftError, OrderId, Price, Quantity, Timestamp, VenueId};
use ports::{
    BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent, HftResult, OpenOrder,
    OrderIntent, OrderStatus,
};
use tokio::{
    sync::mpsc::{channel, error::TrySendError, Receiver, Sender},
    sync::Mutex,
    time::Duration,
};
use tokio_stream::wrappers::ReceiverStream;
use tracing::warn;

#[allow(dead_code)]
const DEFAULT_FILL_DELAY_MS: u64 = 50;
const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 4096;

pub struct SimulatedExecutionClient {
    venue: VenueId,
    events_tx: Sender<ExecutionEvent>,
    events_rx: Arc<Mutex<Option<Receiver<ExecutionEvent>>>>,
    id_counter: Arc<AtomicU64>,
    connected: Arc<AtomicBool>,
    dropped_events: Arc<AtomicU64>,
    open_orders: Arc<Mutex<HashMap<OrderId, OpenOrder>>>,
    fill_delay_ms: u64,
}

impl SimulatedExecutionClient {
    #[allow(dead_code)]
    pub fn new(venue: VenueId) -> Self {
        Self::new_with_event_queue_capacity(venue, DEFAULT_EVENT_QUEUE_CAPACITY)
    }

    #[allow(dead_code)]
    pub fn new_with_event_queue_capacity(venue: VenueId, event_queue_capacity: usize) -> Self {
        let capacity = event_queue_capacity.max(1);
        let (tx, rx) = channel(capacity);
        Self {
            venue,
            events_tx: tx,
            events_rx: Arc::new(Mutex::new(Some(rx))),
            id_counter: Arc::new(AtomicU64::new(1)),
            connected: Arc::new(AtomicBool::new(false)),
            dropped_events: Arc::new(AtomicU64::new(0)),
            open_orders: Arc::new(Mutex::new(HashMap::new())),
            fill_delay_ms: DEFAULT_FILL_DELAY_MS,
        }
    }

    fn next_order_id(&self) -> OrderId {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed);
        OrderId(format!("sim_{}_{}", self.venue.as_str(), id))
    }

    fn current_timestamp() -> Timestamp {
        Utc::now().timestamp_micros() as u64
    }

    fn send_event(&self, event: ExecutionEvent) -> HftResult<()> {
        Self::try_send_event(&self.events_tx, &self.dropped_events, event)
    }

    fn try_send_event(
        sender: &Sender<ExecutionEvent>,
        dropped_events: &AtomicU64,
        event: ExecutionEvent,
    ) -> HftResult<()> {
        match sender.try_send(event) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                dropped_events.fetch_add(1, Ordering::Relaxed);
                Err(HftError::Execution(
                    "simulated execution event queue full".to_string(),
                ))
            }
            Err(TrySendError::Closed(_)) => {
                dropped_events.fetch_add(1, Ordering::Relaxed);
                Err(HftError::Execution(
                    "simulated execution event queue closed".to_string(),
                ))
            }
        }
    }

    #[allow(dead_code)]
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl ExecutionClient for SimulatedExecutionClient {
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        let order_id = self.next_order_id();
        let now = Self::current_timestamp();

        self.send_event(ExecutionEvent::OrderNew {
            order_id: order_id.clone(),
            symbol: intent.symbol.clone(),
            side: intent.side,
            quantity: intent.quantity,
            requested_price: intent.price,
            timestamp: now,
            venue: Some(self.venue),
            strategy_id: intent.strategy_id.clone(),
        })?;

        self.send_event(ExecutionEvent::OrderAck {
            order_id: order_id.clone(),
            timestamp: now,
        })?;

        self.open_orders.lock().await.insert(
            order_id.clone(),
            OpenOrder {
                order_id: order_id.clone(),
                symbol: intent.symbol.clone(),
                side: intent.side,
                order_type: intent.order_type,
                original_quantity: intent.quantity,
                remaining_quantity: intent.quantity,
                filled_quantity: Quantity::from_f64(0.0).expect("valid zero quantity"),
                price: intent.price,
                status: OrderStatus::Accepted,
                created_at: now,
                updated_at: now,
            },
        );

        let sender = self.events_tx.clone();
        let dropped_events = Arc::clone(&self.dropped_events);
        let open_orders = Arc::clone(&self.open_orders);
        let delay = Duration::from_millis(self.fill_delay_ms);
        let order_id_in_flight = order_id.clone();

        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let Some(open_order) = open_orders.lock().await.remove(&order_id_in_flight) else {
                return;
            };
            let qty = open_order.remaining_quantity;
            let price = open_order
                .price
                .unwrap_or_else(|| Price::from_f64(0.0).expect("valid price"));
            let ts = Utc::now().timestamp_micros() as u64;
            let fill_id = format!("sim_fill_{}", ts);
            if let Err(e) = Self::try_send_event(
                &sender,
                &dropped_events,
                ExecutionEvent::Fill {
                    order_id: order_id_in_flight.clone(),
                    price,
                    quantity: qty,
                    timestamp: ts,
                    fill_id,
                },
            ) {
                warn!("simulated execution fill event dropped: {}", e);
            }
            if let Err(e) = Self::try_send_event(
                &sender,
                &dropped_events,
                ExecutionEvent::OrderCompleted {
                    order_id: order_id_in_flight,
                    final_price: price,
                    total_filled: qty,
                    timestamp: ts,
                },
            ) {
                warn!("simulated execution completed event dropped: {}", e);
            }
            if let Err(e) = Self::try_send_event(
                &sender,
                &dropped_events,
                ExecutionEvent::ConnectionStatus {
                    connected: true,
                    timestamp: ts,
                },
            ) {
                warn!("simulated execution connection event dropped: {}", e);
            }
        });

        Ok(order_id)
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        if self.open_orders.lock().await.remove(order_id).is_none() {
            return Err(HftError::OrderNotFound(order_id.0.clone()));
        }
        self.send_event(ExecutionEvent::OrderCanceled {
            order_id: order_id.clone(),
            timestamp: Self::current_timestamp(),
        })?;
        Ok(())
    }

    async fn modify_order(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> HftResult<()> {
        let mut open_orders = self.open_orders.lock().await;
        let open_order = open_orders
            .get_mut(order_id)
            .ok_or_else(|| HftError::OrderNotFound(order_id.0.clone()))?;
        if let Some(quantity) = new_quantity {
            open_order.original_quantity = quantity;
            open_order.remaining_quantity = quantity;
        }
        if let Some(price) = new_price {
            open_order.price = Some(price);
        }
        open_order.updated_at = Self::current_timestamp();
        drop(open_orders);
        self.send_event(ExecutionEvent::OrderModified {
            order_id: order_id.clone(),
            new_quantity,
            new_price,
            timestamp: Self::current_timestamp(),
        })?;
        Ok(())
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        let mut guard = self.events_rx.lock().await;
        let rx = guard
            .take()
            .ok_or_else(|| HftError::new("Simulated execution stream already taken"))?;
        let stream = ReceiverStream::new(rx).map(Ok);
        Ok(Box::pin(stream))
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        Ok(self.open_orders.lock().await.values().cloned().collect())
    }

    async fn connect(&mut self) -> HftResult<()> {
        self.connected.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.connected.store(false, Ordering::Relaxed);
        Ok(())
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.connected.load(Ordering::Relaxed),
            latency_ms: Some(0.1),
            last_heartbeat: Self::current_timestamp(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::{OrderType, Side, Symbol, TimeInForce};

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
    async fn simulated_execution_event_queue_is_bounded() {
        let mut client = SimulatedExecutionClient::new_with_event_queue_capacity(VenueId::MOCK, 1);

        let result = client.place_order(test_intent()).await;

        assert!(result.is_err());
        assert_eq!(client.dropped_events(), 1);
    }

    #[tokio::test]
    async fn simulated_open_orders_are_authoritative_and_cancelable() {
        let mut client = SimulatedExecutionClient::new(VenueId::MOCK);
        client.fill_delay_ms = 5_000;

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
}
