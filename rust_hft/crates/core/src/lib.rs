//! Core primitives（契約最小集）
//! - Newtypes / IDs / Errors / Constants
//! - 穩定介面依賴最小化

pub mod types;
pub mod error;
pub mod fixed_point;
pub mod latency;
pub mod unified_timestamp;

// 對外穩定導出
pub use error::{HftError, HftResult};
pub use types::*;
pub use fixed_point::{FixedPrice, FixedQuantity, FixedBps};
pub use latency::{LatencyStage, LatencyTracker, LatencyMeasurement, LatencyStats, MicrosTimestamp, now_micros};
pub use unified_timestamp::UnifiedTimestamp;

#[cfg(test)]
mod tests {
    use super::*;
    use ports::ExecutionEvent;
    use std::collections::HashSet;
    
    #[test]
    fn test_price_precision_parsing() {
        // 测试高精度价格解析，确保不经过f64中间态
        let price_str = "67188.123456789012345";
        
        // 直接从字符串解析到Decimal
        let price = Price::from_str(price_str).unwrap();
        assert_eq!(price.to_string(), price_str);
        
        // 验证f64转换会丢失精度
        let price_via_f64 = price_str.parse::<f64>().unwrap();
        let price_from_f64 = Price::from_f64(price_via_f64).unwrap();
        
        // 直接解析的精度应该更高
        assert_ne!(price, price_from_f64);
        assert!(price.0.to_string().len() > price_from_f64.0.to_string().len());
    }

    #[test]
    fn test_quantity_precision_parsing() {
        // 测试高精度数量解析
        let qty_str = "0.123456789012345678";
        
        let qty = Quantity::from_str(qty_str).unwrap();
        assert_eq!(qty.to_string(), qty_str);
        
        // 验证精度保持
        let qty_via_f64 = qty_str.parse::<f64>().unwrap();
        let qty_from_f64 = Quantity::from_f64(qty_via_f64).unwrap();
        
        assert_ne!(qty, qty_from_f64);
    }

    #[test]
    fn test_unified_timestamp_creation() {
        let exchange_ts = 1640995200_000_000u64;
        let local_ts = 1640995200_001_500u64;
        
        let unified = UnifiedTimestamp::new(exchange_ts, local_ts);
        
        assert_eq!(unified.exchange_ts, exchange_ts);
        assert_eq!(unified.local_ts, local_ts);
        assert_eq!(unified.primary_ts(), exchange_ts); // 优先交易所时间
        
        // 测试网络延迟计算
        assert_eq!(unified.network_latency_us(), Some(1500)); // 1.5ms
    }

    #[test]
    fn test_unified_timestamp_fallback() {
        // 测试无交易所时间戳时的降级处理
        let local_ts = 1640995200_001_500u64;
        let unified = UnifiedTimestamp::local_only(local_ts);
        
        assert_eq!(unified.exchange_ts, 0);
        assert_eq!(unified.local_ts, local_ts);
        assert_eq!(unified.primary_ts(), local_ts); // 降级到本地时间
        assert_eq!(unified.network_latency_us(), None); // 无网络延迟
    }

    #[test]
    fn test_timestamp_validation() {
        let now = UnifiedTimestamp::current_timestamp();
        
        // 正常时间戳应该验证通过
        let normal = UnifiedTimestamp::new(now - 1000, now);
        assert!(normal.validate());
        
        // 过于未来的时间戳应该失败
        let far_future = UnifiedTimestamp::new(now + 120_000_000, now); // 2分钟后
        assert!(!far_future.validate());
        
        // 过于过去的时间戳应该失败
        let far_past = UnifiedTimestamp::new(now - 25 * 3600 * 1_000_000, now); // 25小时前
        assert!(!far_past.validate());
    }

    #[test]
    fn test_stale_detection() {
        let now = UnifiedTimestamp::current_timestamp();
        let old_ts = now - 10_000; // 10ms前
        
        let unified = UnifiedTimestamp::new(old_ts, now);
        
        // 5ms阈值 - 应该是陈旧的
        assert!(unified.is_stale(5_000));
        
        // 20ms阈值 - 不应该是陈旧的
        assert!(!unified.is_stale(20_000));
    }

    #[test]
    fn test_milliseconds_conversion() {
        let millis_ts = 1640995200000u64; // 2022-01-01 00:00:00 UTC in ms
        let unified = UnifiedTimestamp::from_millis(millis_ts);
        
        assert_eq!(unified.exchange_ts, millis_ts * 1000); // 转换为微秒
    }

    #[test]
    fn test_error_handling_invalid_price() {
        // 测试无效价格的错误处理
        let invalid_prices = vec![
            "",
            "invalid",
            "NaN",
            "Infinity",
            "-67188.123",  // 负价格
            "0.0",         // 零价格
        ];
        
        for invalid_price in invalid_prices {
            let result = Price::from_str(invalid_price);
            assert!(result.is_err(), "应该拒绝无效价格: {}", invalid_price);
        }
    }
    
    #[test]
    fn test_error_handling_invalid_quantity() {
        // 测试无效数量的错误处理
        let invalid_quantities = vec![
            "",
            "invalid", 
            "NaN",
            "Infinity",
            "-0.123",  // 负数量
        ];
        
        for invalid_qty in invalid_quantities {
            let result = Quantity::from_str(invalid_qty);
            assert!(result.is_err(), "应该拒绝无效数量: {}", invalid_qty);
        }
    }
    
    #[test]
    fn test_execution_event_completeness() {
        // 测试ExecutionEvent的完整性
        let order_id = OrderId("TEST-001".to_string());
        let price = Price::from_str("67188.123").unwrap();
        let quantity = Quantity::from_str("0.123").unwrap();
        let timestamp = 1640995200_123_456u64;
        
        // 测试不同类型的ExecutionEvent
        let events = vec![
            ExecutionEvent::OrderAck {
                order_id: order_id.clone(),
                timestamp,
            },
            ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price,
                quantity,
                timestamp,
                fill_id: "FILL-001".to_string(),
            },
            ExecutionEvent::OrderRejected {
                order_id: order_id.clone(),
                reason: "Insufficient funds".to_string(),
                timestamp,
            },
            ExecutionEvent::OrderCompleted {
                order_id: order_id.clone(),
                final_price: price,
                total_filled: quantity,
                timestamp,
            },
        ];
        
        // 验证每个事件都有正确的字段
        for event in events {
            match event {
                ExecutionEvent::OrderAck { order_id, timestamp } => {
                    assert!(!order_id.0.is_empty());
                    assert!(timestamp > 0);
                }
                ExecutionEvent::Fill { order_id, price, quantity, timestamp, fill_id } => {
                    assert!(!order_id.0.is_empty());
                    assert!(price.0 > rust_decimal::Decimal::ZERO);
                    assert!(quantity.0 > rust_decimal::Decimal::ZERO);
                    assert!(timestamp > 0);
                    assert!(!fill_id.is_empty());
                }
                ExecutionEvent::OrderRejected { order_id, reason, timestamp } => {
                    assert!(!order_id.0.is_empty());
                    assert!(!reason.is_empty());
                    assert!(timestamp > 0);
                }
                ExecutionEvent::OrderCompleted { order_id, final_price, total_filled, timestamp } => {
                    assert!(!order_id.0.is_empty());
                    assert!(final_price.0 > rust_decimal::Decimal::ZERO);
                    assert!(total_filled.0 > rust_decimal::Decimal::ZERO);
                    assert!(timestamp > 0);
                }
                _ => {} // 其他事件类型
            }
        }
    }
}
