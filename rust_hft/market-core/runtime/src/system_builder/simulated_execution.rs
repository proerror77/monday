use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use hft_core::{HftError, OrderId, Price, Quantity, Timestamp, VenueId};
use ports::{
    BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent, HftResult, OpenOrder, OrderIntent,
};
use tokio::{
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    sync::Mutex,
    time::Duration,
};
use tokio_stream::wrappers::UnboundedReceiverStream;

#[allow(dead_code)]
const DEFAULT_FILL_DELAY_MS: u64 = 50;

pub struct SimulatedExecutionClient {
    venue: VenueId,
    events_tx: UnboundedSender<ExecutionEvent>,
    events_rx: Arc<Mutex<Option<UnboundedReceiver<ExecutionEvent>>>>,
    id_counter: Arc<AtomicU64>,
    connected: Arc<AtomicBool>,
    fill_delay_ms: u64,
}

impl SimulatedExecutionClient {
    #[allow(dead_code)]
    pub fn new(venue: VenueId) -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            venue,
            events_tx: tx,
            events_rx: Arc::new(Mutex::new(Some(rx))),
            id_counter: Arc::new(AtomicU64::new(1)),
            connected: Arc::new(AtomicBool::new(false)),
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

    fn send_event(&self, event: ExecutionEvent) {
        let _ = self.events_tx.send(event);
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
            quantity: intent.quantity.clone(),
            requested_price: intent.price.clone(),
            timestamp: now,
            venue: Some(self.venue),
            strategy_id: intent.strategy_id.clone(),
        });

        self.send_event(ExecutionEvent::OrderAck {
            order_id: order_id.clone(),
            timestamp: now,
        });

        let sender = self.events_tx.clone();
        let qty = intent.quantity.clone();
        let price = intent
            .price
            .clone()
            .unwrap_or_else(|| Price::from_f64(0.0).expect("valid price"));
        let delay = Duration::from_millis(self.fill_delay_ms);
        let order_id_in_flight = order_id.clone();

        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let ts = Utc::now().timestamp_micros() as u64;
            let fill_id = format!("sim_fill_{}", ts);
            let _ = sender.send(ExecutionEvent::Fill {
                order_id: order_id_in_flight.clone(),
                price: price.clone(),
                quantity: qty.clone(),
                timestamp: ts,
                fill_id,
            });
            let _ = sender.send(ExecutionEvent::OrderCompleted {
                order_id: order_id_in_flight,
                final_price: price,
                total_filled: qty,
                timestamp: ts,
            });
            let _ = sender.send(ExecutionEvent::ConnectionStatus {
                connected: true,
                timestamp: ts,
            });
        });

        Ok(order_id)
    }

    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()> {
        self.send_event(ExecutionEvent::OrderCanceled {
            order_id: order_id.clone(),
            timestamp: Self::current_timestamp(),
        });
        Ok(())
    }

    async fn modify_order(
        &mut self,
        order_id: &OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
    ) -> HftResult<()> {
        self.send_event(ExecutionEvent::OrderModified {
            order_id: order_id.clone(),
            new_quantity,
            new_price,
            timestamp: Self::current_timestamp(),
        });
        Ok(())
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        let mut guard = self.events_rx.lock().await;
        let rx = guard
            .take()
            .ok_or_else(|| HftError::new("Simulated execution stream already taken"))?;
        let stream = UnboundedReceiverStream::new(rx).map(|evt| Ok(evt));
        Ok(Box::pin(stream))
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        Ok(Vec::new())
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
