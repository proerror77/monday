//! 背壓安全默認值測試

use engine::dataflow::{BackpressurePolicy, IngestionConfig};
use engine::{create_execution_queues, ExecutionQueueConfig, LifecycleIntentSubmitError};
use hft_core::{OrderType, Quantity, Side, Symbol, TimeInForce};
use ports::{
    ExecutionEvent, OrderIntent, OrderIntentEnvelope, OrderIntentLifecycle, OrderIntentRejectReason,
};

fn test_intent() -> OrderIntent {
    OrderIntent {
        symbol: Symbol::new("BTCUSDT"),
        side: Side::Buy,
        quantity: Quantity::from_f64(1.0).expect("valid quantity"),
        order_type: OrderType::Market,
        price: None,
        time_in_force: TimeInForce::IOC,
        strategy_id: "lifecycle-test".to_string(),
        target_venue: None,
    }
}

#[test]
fn test_default_backpressure_policy_is_safe() {
    let config = IngestionConfig::default();

    // 確保默認背壓策略是安全的自適應策略
    match config.backpressure_policy {
        BackpressurePolicy::Adaptive {
            low_threshold,
            high_threshold,
        } => {
            // 驗證閾值設置合理
            assert!(
                low_threshold > 0.0 && low_threshold < 1.0,
                "Low threshold should be between 0 and 1"
            );
            assert!(
                high_threshold > 0.0 && high_threshold < 1.0,
                "High threshold should be between 0 and 1"
            );
            assert!(
                low_threshold < high_threshold,
                "Low threshold should be less than high threshold"
            );

            // 驗證推薦的閾值範圍
            assert!(
                low_threshold >= 0.5,
                "Low threshold should be at least 50% for safety"
            );
            assert!(
                high_threshold <= 0.9,
                "High threshold should be at most 90% to prevent saturation"
            );

            println!(
                "✓ Default adaptive backpressure policy: {}% -> {}%",
                low_threshold * 100.0,
                high_threshold * 100.0
            );
        }
        _ => panic!("Default backpressure policy should be Adaptive for safety"),
    }
}

#[test]
fn test_production_safe_config() {
    let config = IngestionConfig::production_safe();

    // 生產環境配置應該使用最保守的 DropNew 策略
    match config.backpressure_policy {
        BackpressurePolicy::DropNew => {
            println!("✓ Production config uses safe DropNew policy");
        }
        _ => panic!("Production config should use DropNew policy for maximum safety"),
    }

    // 驗證其他保守設置
    assert!(
        config.queue_capacity >= 65536,
        "Production should have large queue capacity"
    );
    assert!(
        config.stale_threshold_us <= 2000,
        "Production should have strict staleness threshold"
    );
}

#[test]
fn test_high_performance_config() {
    let config = IngestionConfig::high_performance();

    // 高性能配置應該使用自適應策略但更激進的閾值
    match config.backpressure_policy {
        BackpressurePolicy::Adaptive {
            low_threshold,
            high_threshold,
        } => {
            assert!(
                low_threshold <= 0.6,
                "High performance config should switch to LastWins earlier"
            );
            assert!(
                high_threshold <= 0.85,
                "High performance config should switch to DropNew earlier"
            );

            println!(
                "✓ High performance adaptive policy: {}% -> {}%",
                low_threshold * 100.0,
                high_threshold * 100.0
            );
        }
        _ => panic!("High performance config should use Adaptive policy"),
    }

    // 驗證高性能設置
    assert!(
        config.stale_threshold_us <= 1000,
        "High performance should have very strict staleness"
    );
}

#[test]
fn test_backpressure_status_fields() {
    use engine::BackpressureStatus;

    // 測試背壓狀態結構體包含必要字段
    let status = BackpressureStatus {
        utilization: 0.75,
        is_under_pressure: true,
        events_dropped_total: 100,
        recommended_action: "Test action".to_string(),
        queue_capacity: 32768,
    };

    assert_eq!(status.utilization, 0.75);
    assert!(status.is_under_pressure);
    assert_eq!(status.events_dropped_total, 100);
    assert_eq!(status.queue_capacity, 32768);
    assert!(!status.recommended_action.is_empty());

    println!("✓ BackpressureStatus structure is properly defined");
}

#[test]
fn test_expired_lifecycle_intent_never_enters_execution_queue() {
    let config = ExecutionQueueConfig {
        intent_queue_capacity: 4,
        event_queue_capacity: 4,
        batch_size: 8,
    };
    let (mut engine_queues, mut worker_queues) = create_execution_queues(config);
    let envelope = OrderIntentEnvelope::new(test_intent(), OrderIntentLifecycle::new(1_000, 1_100));

    let result = engine_queues.send_lifecycle_intent(envelope, 1_100);

    match result {
        Err(LifecycleIntentSubmitError::LifecycleRejected { reason, .. }) => {
            assert_eq!(
                reason,
                OrderIntentRejectReason::Expired {
                    now: 1_100,
                    valid_until: 1_100,
                }
            );
        }
        other => panic!("expected lifecycle rejection, got {other:?}"),
    }
    assert_eq!(worker_queues.receive_intents().len(), 0);
    assert_eq!(engine_queues.stats().intents_sent, 0);
    assert_eq!(engine_queues.stats().intent_lifecycle_rejected_count, 1);
    assert_eq!(engine_queues.stats().intent_expired_count, 1);
}

#[test]
fn test_current_lifecycle_intent_enters_execution_queue() {
    let config = ExecutionQueueConfig {
        intent_queue_capacity: 4,
        event_queue_capacity: 4,
        batch_size: 8,
    };
    let (mut engine_queues, mut worker_queues) = create_execution_queues(config);
    let mut lifecycle = OrderIntentLifecycle::new(1_000, 1_100);
    lifecycle.max_latency_us = Some(99);
    let envelope = OrderIntentEnvelope::new(test_intent(), lifecycle);

    assert!(engine_queues.send_lifecycle_intent(envelope, 1_099).is_ok());

    let intents = worker_queues.receive_intents();
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].symbol.as_str(), "BTCUSDT");
    assert_eq!(engine_queues.stats().intents_sent, 1);
    assert_eq!(engine_queues.stats().intent_lifecycle_rejected_count, 0);
}

#[test]
fn test_stale_book_lifecycle_intent_never_enters_execution_queue() {
    let config = ExecutionQueueConfig {
        intent_queue_capacity: 4,
        event_queue_capacity: 4,
        batch_size: 8,
    };
    let (mut engine_queues, mut worker_queues) = create_execution_queues(config);
    let mut lifecycle = OrderIntentLifecycle::new(1_000, 1_100);
    lifecycle.source_book_seq = Some(42);
    let envelope = OrderIntentEnvelope::new(test_intent(), lifecycle);

    let result = engine_queues.send_lifecycle_intent_with_book_seq(envelope, 1_050, Some(43));

    match result {
        Err(LifecycleIntentSubmitError::LifecycleRejected { reason, .. }) => {
            assert_eq!(
                reason,
                OrderIntentRejectReason::SourceBookStale {
                    source_book_seq: 42,
                    latest_book_seq: 43,
                }
            );
        }
        other => panic!("expected stale-source rejection, got {other:?}"),
    }
    assert_eq!(worker_queues.receive_intents().len(), 0);
    assert_eq!(engine_queues.stats().intents_sent, 0);
    assert_eq!(engine_queues.stats().intent_stale_count, 1);
}

#[test]
fn test_execution_intent_queue_full_rejects_new_intent() {
    let config = ExecutionQueueConfig {
        intent_queue_capacity: 4,
        event_queue_capacity: 4,
        batch_size: 8,
    };
    let (mut engine_queues, mut worker_queues) = create_execution_queues(config);

    assert!(engine_queues.send_intent(test_intent()).is_ok());
    assert!(engine_queues.send_intent(test_intent()).is_ok());
    assert!(engine_queues.send_intent(test_intent()).is_ok());
    assert!(engine_queues.send_intent(test_intent()).is_err());

    assert_eq!(engine_queues.stats().intents_sent, 3);
    assert_eq!(engine_queues.stats().intent_queue_full_count, 1);
    assert_eq!(worker_queues.receive_intents().len(), 3);
}

#[test]
fn test_execution_event_queue_full_is_visible_to_stats() {
    let config = ExecutionQueueConfig {
        intent_queue_capacity: 4,
        event_queue_capacity: 4,
        batch_size: 8,
    };
    let (mut engine_queues, mut worker_queues) = create_execution_queues(config);
    let event = ExecutionEvent::ConnectionStatus {
        connected: true,
        timestamp: 1,
    };

    assert!(worker_queues.send_event(event.clone()).is_ok());
    assert!(worker_queues.send_event(event.clone()).is_ok());
    assert!(worker_queues.send_event(event.clone()).is_ok());
    assert!(worker_queues.send_event(event).is_err());

    let mut events = Vec::new();
    engine_queues.receive_events_into(&mut events);
    assert_eq!(events.len(), 3);
    assert_eq!(worker_queues.stats().events_sent, 3);
    assert_eq!(worker_queues.stats().event_queue_full_count, 1);
}
