//! 背壓安全默認值測試

use engine::dataflow::{IngestionConfig, BackpressurePolicy};

#[test]
fn test_default_backpressure_policy_is_safe() {
    let config = IngestionConfig::default();
    
    // 確保默認背壓策略是安全的自適應策略
    match config.backpressure_policy {
        BackpressurePolicy::Adaptive { low_threshold, high_threshold } => {
            // 驗證閾值設置合理
            assert!(low_threshold > 0.0 && low_threshold < 1.0, "Low threshold should be between 0 and 1");
            assert!(high_threshold > 0.0 && high_threshold < 1.0, "High threshold should be between 0 and 1");
            assert!(low_threshold < high_threshold, "Low threshold should be less than high threshold");
            
            // 驗證推薦的閾值範圍
            assert!(low_threshold >= 0.5, "Low threshold should be at least 50% for safety");
            assert!(high_threshold <= 0.9, "High threshold should be at most 90% to prevent saturation");
            
            println!("✓ Default adaptive backpressure policy: {}% -> {}%", 
                     low_threshold * 100.0, high_threshold * 100.0);
        },
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
        },
        _ => panic!("Production config should use DropNew policy for maximum safety"),
    }
    
    // 驗證其他保守設置
    assert!(config.queue_capacity >= 65536, "Production should have large queue capacity");
    assert!(config.stale_threshold_us <= 2000, "Production should have strict staleness threshold");
}

#[test]
fn test_high_performance_config() {
    let config = IngestionConfig::high_performance();
    
    // 高性能配置應該使用自適應策略但更激進的閾值
    match config.backpressure_policy {
        BackpressurePolicy::Adaptive { low_threshold, high_threshold } => {
            assert!(low_threshold <= 0.6, "High performance config should switch to LastWins earlier");
            assert!(high_threshold <= 0.85, "High performance config should switch to DropNew earlier");
            
            println!("✓ High performance adaptive policy: {}% -> {}%", 
                     low_threshold * 100.0, high_threshold * 100.0);
        },
        _ => panic!("High performance config should use Adaptive policy"),
    }
    
    // 驗證高性能設置
    assert!(config.stale_threshold_us <= 1000, "High performance should have very strict staleness");
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
    assert_eq!(status.is_under_pressure, true);
    assert_eq!(status.events_dropped_total, 100);
    assert_eq!(status.queue_capacity, 32768);
    assert!(!status.recommended_action.is_empty());
    
    println!("✓ BackpressureStatus structure is properly defined");
}