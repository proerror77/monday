use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use engine::{
    create_execution_queues, spawn_execution_worker, ExecutionQueueConfig, ExecutionWorkerConfig,
};
use futures::stream;
use hft_core::{
    AssetClass, ComplianceContext, HftResult, OrderId, OrderType, Price, ProductType, Quantity,
    RegulatoryProfile, Side, Symbol, TimeInForce,
};
use ports::{BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent, OpenOrder, OrderIntent};
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
    })
}

#[tokio::test]
async fn tokenized_security_order_reaches_paper_execution_events() {
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
        .send_intent(tokenized_security_intent())
        .expect("intent enters execution queue");

    let mut events = Vec::new();
    for _ in 0..50 {
        engine_queues.receive_events_into(&mut events);
        if events.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.abort();

    let seen = seen.lock().await;
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].asset_class, AssetClass::TokenizedSecurity);
    assert_eq!(seen[0].product_type, ProductType::TokenizedSecuritySpot);

    assert!(events.iter().any(|event| matches!(
        event,
        ExecutionEvent::OrderNew {
            order_id,
            symbol,
            strategy_id,
            ..
        } if order_id.0 == "paper-tsla-1"
            && symbol.as_str() == "TSLABUSDT"
            && strategy_id == "bstocks-paper-e2e"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ExecutionEvent::OrderAck { order_id, .. } if order_id.0 == "paper-tsla-1"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ExecutionEvent::Fill { order_id, fill_id, .. }
            if order_id.0 == "paper-tsla-1" && fill_id == "fill-1"
    )));
}
