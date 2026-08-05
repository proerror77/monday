use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use engine::{
    create_execution_queues, spawn_execution_worker, ExecutionQueueConfig, ExecutionWorkerConfig,
};
use futures::stream;
use hft_core::{
    AccountId, ComplianceContext, HftResult, OrderId, OrderType, Price, Quantity,
    RegulatoryProfile, Side, Symbol, TimeInForce, VenueId,
};
use ports::{BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent, OpenOrder, OrderIntent};
use rust_decimal::Decimal;
use tokio::sync::Mutex;

#[derive(Default)]
struct RecordingExecutionClient {
    seen: Arc<Mutex<Vec<OrderIntent>>>,
}

#[async_trait]
impl ExecutionClient for RecordingExecutionClient {
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId> {
        self.seen.lock().await.push(intent);
        Ok(OrderId("paper-tsla-1".to_string()))
    }

    async fn cancel_order(&mut self, _order_id: &OrderId) -> HftResult<()> {
        Ok(())
    }

    async fn modify_order(
        &mut self,
        _order_id: &OrderId,
        _new_quantity: Option<Quantity>,
        _new_price: Option<Price>,
    ) -> HftResult<()> {
        Ok(())
    }

    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
        Ok(Box::pin(stream::iter([
            Ok(ExecutionEvent::OrderAck {
                order_id: OrderId("paper-tsla-1".to_string()),
                timestamp: hft_core::now_micros(),
            }),
            Ok(ExecutionEvent::Fill {
                order_id: OrderId("paper-tsla-1".to_string()),
                price: Price::from_f64(400.25).expect("valid price"),
                quantity: Quantity::from_f64(0.1).expect("valid quantity"),
                timestamp: hft_core::now_micros(),
                fill_id: "fill-1".to_string(),
            }),
        ])))
    }

    fn execution_stream_may_complete(&self) -> bool {
        true
    }

    fn is_simulated_execution(&self) -> bool {
        true
    }

    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
        Ok(Vec::new())
    }

    async fn connect(&mut self) -> HftResult<()> {
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        Ok(())
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: true,
            latency_ms: Some(0.0),
            last_heartbeat: hft_core::now_micros(),
        }
    }
}

fn tokenized_security_intent() -> OrderIntent {
    OrderIntent::crypto_spot(
        Symbol::new("TSLABUSDT"),
        Side::Buy,
        Quantity::from_f64(0.1).expect("valid quantity"),
        OrderType::Limit,
        Some(Price::from_f64(400.25).expect("valid price")),
        TimeInForce::GTC,
        "bstocks-paper-e2e".to_string(),
        None,
    )
    .tokenized_security_spot(ComplianceContext {
        regulatory_profile: RegulatoryProfile::AdgmTokenizedSecurity,
        jurisdiction: Some("AE".to_string()),
        eligibility_confirmed: true,
        allow_tokenized_securities: true,
        top_depth_usd: Some(Decimal::from(100_000)),
        spread_bps: Some(Decimal::ONE),
        corporate_action_active: Some(false),
        evidence_source: Some("paper-reference-feed".to_string()),
        evidence_venue: Some(VenueId::BINANCE_TOKENIZED_SECURITIES),
        evidence_observed_at: Some(hft_core::now_micros()),
    })
}

#[tokio::test]
async fn tokenized_security_is_rejected_before_simulated_execution() {
    let (mut engine_queues, worker_queues) = create_execution_queues(ExecutionQueueConfig {
        intent_queue_capacity: 8,
        event_queue_capacity: 8,
        batch_size: 8,
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = RecordingExecutionClient { seen: seen.clone() };
    let worker_config = ExecutionWorkerConfig {
        ack_timeout_ms: 0,
        reconcile_interval_ms: 0,
        ..Default::default()
    };

    let handle = spawn_execution_worker(worker_config, worker_queues, vec![Box::new(client)]);
    engine_queues
        .send_intent(
            AccountId("bstocks-unbound-test".to_string()),
            tokenized_security_intent(),
        )
        .expect("intent enters execution queue");

    let mut events = Vec::new();
    for _ in 0..50 {
        engine_queues.receive_events_into(&mut events);
        if !events.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.abort();

    let seen = seen.lock().await;
    assert!(seen.is_empty());
    assert!(matches!(
        events.as_slice(),
        [ExecutionEvent::OrderReject { reason, .. }]
            if reason == "product requires a venue-specific account admission policy"
    ));
}
