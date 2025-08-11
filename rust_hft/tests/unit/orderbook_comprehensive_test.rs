/*!
 * Comprehensive OrderBook Tests
 * 
 * 全面测试订单簿系统，包括：
 * - 订单簿更新逻辑
 * - 价格层级管理
 * - 性能测试（延遲要求）
 * - 数据完整性验证
 * - 并发安全性
 */

use rust_hft::core::orderbook::{OrderBook, OrderBookStats};
use rust_hft::core::types::*;
use std::time::Instant;
use tokio::time::{timeout, Duration};
use std::sync::Arc;
use tokio::sync::Mutex;
use approx::assert_relative_eq;

use crate::common::{PerformanceHelper, LatencyTracker, test_data};

#[tokio::test]
async fn test_orderbook_initialization() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    assert_eq!(ob.symbol, "BTCUSDT");
    assert_eq!(ob.bids.len(), 0);
    assert_eq!(ob.asks.len(), 0);
    assert!(!ob.is_valid); // Empty orderbook is invalid
    assert_eq!(ob.update_count, 0);
    
    Ok(())
}

#[tokio::test]
async fn test_snapshot_initialization() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![
            PriceLevel { price: 50000.0.to_price(), quantity: "1.5".to_quantity(), side: Side::Bid },
            PriceLevel { price: 49999.0.to_price(), quantity: "2.0".to_quantity(), side: Side::Bid },
            PriceLevel { price: 49998.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid },
        ],
        asks: vec![
            PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask },
            PriceLevel { price: 50002.0.to_price(), quantity: "1.5".to_quantity(), side: Side::Ask },
            PriceLevel { price: 50003.0.to_price(), quantity: "2.0".to_quantity(), side: Side::Ask },
        ],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    ob.init_snapshot(snapshot)?;
    
    // Verify initialization
    assert_eq!(ob.bids.len(), 3);
    assert_eq!(ob.asks.len(), 3);
    assert!(ob.is_valid);
    assert_eq!(ob.update_count, 1);
    
    // Verify best prices
    assert_eq!(ob.best_bid(), Some(50000.0.to_price()));
    assert_eq!(ob.best_ask(), Some(50001.0.to_price()));
    
    // Verify spread
    assert_eq!(ob.spread(), Some(1.0));
    
    Ok(())
}

#[tokio::test]
async fn test_incremental_updates() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    // Initialize with snapshot
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![
            PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid },
        ],
        asks: vec![
            PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask },
        ],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    ob.init_snapshot(snapshot)?;
    
    // Apply incremental update
    let update = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![
            PriceLevel { price: 49999.0.to_price(), quantity: "2.0".to_quantity(), side: Side::Bid },
        ],
        asks: vec![
            PriceLevel { price: 50002.0.to_price(), quantity: "1.5".to_quantity(), side: Side::Ask },
        ],
        timestamp: now_micros(),
        sequence_start: 2,
        sequence_end: 2,
        is_snapshot: false,
    };
    
    ob.apply_update(update)?;
    
    // Verify updates
    assert_eq!(ob.bids.len(), 2);
    assert_eq!(ob.asks.len(), 2);
    assert_eq!(ob.update_count, 2);
    assert_eq!(ob.last_sequence, 2);
    
    Ok(())
}

#[tokio::test]
async fn test_price_level_removal() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    // Initialize with levels
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![
            PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid },
            PriceLevel { price: 49999.0.to_price(), quantity: "2.0".to_quantity(), side: Side::Bid },
        ],
        asks: vec![
            PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask },
        ],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    ob.init_snapshot(snapshot)?;
    assert_eq!(ob.bids.len(), 2);
    
    // Remove a level by setting quantity to zero
    let update = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![
            PriceLevel { price: 49999.0.to_price(), quantity: "0".to_quantity(), side: Side::Bid },
        ],
        asks: vec![],
        timestamp: now_micros(),
        sequence_start: 2,
        sequence_end: 2,
        is_snapshot: false,
    };
    
    ob.apply_update(update)?;
    
    // Verify removal
    assert_eq!(ob.bids.len(), 1);
    assert_eq!(ob.best_bid(), Some(50000.0.to_price()));
    
    Ok(())
}

#[tokio::test]
async fn test_sequence_validation() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    // Initialize
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    ob.init_snapshot(snapshot)?;
    
    // Try to apply update with wrong sequence
    let invalid_update = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![],
        asks: vec![],
        timestamp: now_micros(),
        sequence_start: 3, // Should be 2
        sequence_end: 3,
        is_snapshot: false,
    };
    
    let result = ob.apply_update(invalid_update);
    assert!(result.is_err()); // Should fail due to sequence gap
    
    Ok(())
}

#[tokio::test]
async fn test_orderbook_metrics() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![
            PriceLevel { price: 50000.0.to_price(), quantity: "2.0".to_quantity(), side: Side::Bid },
            PriceLevel { price: 49999.0.to_price(), quantity: "1.5".to_quantity(), side: Side::Bid },
            PriceLevel { price: 49998.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid },
        ],
        asks: vec![
            PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask },
            PriceLevel { price: 50002.0.to_price(), quantity: "1.5".to_quantity(), side: Side::Ask },
            PriceLevel { price: 50003.0.to_price(), quantity: "2.0".to_quantity(), side: Side::Ask },
        ],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    ob.init_snapshot(snapshot)?;
    
    // Test mid price
    let mid = ob.mid_price().unwrap();
    assert_relative_eq!(mid.0, 50000.5, epsilon = 0.01);
    
    // Test spread in basis points
    let spread_bps = ob.spread_bps().unwrap();
    assert!(spread_bps > 0.0);
    
    // Test depth calculation
    let bid_depth_l1 = ob.calculate_depth(1, Side::Bid);
    assert_relative_eq!(bid_depth_l1, 50000.0 * 2.0, epsilon = 0.01); // price * quantity
    
    let ask_depth_l1 = ob.calculate_depth(1, Side::Ask);
    assert_relative_eq!(ask_depth_l1, 50001.0 * 1.0, epsilon = 0.01);
    
    // Test Order Book Imbalance
    let obi = ob.calculate_obi(1);
    assert!(obi.abs() <= 1.0); // OBI should be between -1 and 1
    
    // Test multi-level OBI
    let (obi_l1, obi_l5, obi_l10, obi_l20) = ob.calculate_multi_obi();
    assert!(obi_l1.abs() <= 1.0);
    assert!(obi_l5.abs() <= 1.0);
    
    Ok(())
}

#[tokio::test]
async fn test_market_impact_estimation() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![
            PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid },
            PriceLevel { price: 49999.0.to_price(), quantity: "2.0".to_quantity(), side: Side::Bid },
        ],
        asks: vec![
            PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask },
            PriceLevel { price: 50002.0.to_price(), quantity: "2.0".to_quantity(), side: Side::Ask },
        ],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    ob.init_snapshot(snapshot)?;
    
    // Test market impact for buy order (consumes ask side)
    let buy_impact = ob.estimate_market_impact(Side::Bid, "0.5".to_quantity());
    assert!(buy_impact > 0.0);
    
    // Test market impact for sell order (consumes bid side)
    let sell_impact = ob.estimate_market_impact(Side::Ask, "0.5".to_quantity());
    assert!(sell_impact > 0.0);
    
    Ok(())
}

#[tokio::test]
async fn test_orderbook_validation() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    // Test invalid orderbook (crossed spread)
    let invalid_snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 50002.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    ob.init_snapshot(invalid_snapshot)?;
    assert!(!ob.is_valid); // Should be invalid due to crossed spread
    
    Ok(())
}

#[tokio::test]
async fn test_orderbook_performance_benchmarks() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    let latency_tracker = LatencyTracker::new();
    
    // Initialize with larger orderbook
    let mut bids = Vec::new();
    let mut asks = Vec::new();
    
    for i in 0..100 {
        bids.push(PriceLevel { 
            price: (50000.0 - i as f64 * 0.01).to_price(), 
            quantity: "1.0".to_quantity(), 
            side: Side::Bid 
        });
        asks.push(PriceLevel { 
            price: (50001.0 + i as f64 * 0.01).to_price(), 
            quantity: "1.0".to_quantity(), 
            side: Side::Ask 
        });
    }
    
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids,
        asks,
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    let start = Instant::now();
    ob.init_snapshot(snapshot)?;
    let init_latency = start.elapsed().as_nanos() as u64;
    latency_tracker.record_latency(init_latency).await;
    
    // Test update performance
    for i in 0..1000 {
        let update = OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel { 
                price: (49900.0 - i as f64 * 0.01).to_price(), 
                quantity: "1.0".to_quantity(), 
                side: Side::Bid 
            }],
            asks: vec![],
            timestamp: now_micros(),
            sequence_start: (i + 2) as u64,
            sequence_end: (i + 2) as u64,
            is_snapshot: false,
        };
        
        let start = Instant::now();
        ob.apply_update(update)?;
        let update_latency = start.elapsed().as_nanos() as u64;
        latency_tracker.record_latency(update_latency).await;
    }
    
    let avg_latency = latency_tracker.get_avg_latency().await;
    let p99_latency = latency_tracker.get_p99_latency().await;
    
    println!("OrderBook Performance Results:");
    println!("  Average latency: {:.2} ns", avg_latency);
    println!("  P99 latency: {:.2} ns", p99_latency);
    
    // Performance requirements from CLAUDE.md: ≤ 25 μs p99
    assert!(p99_latency < 25_000.0); // 25 microseconds = 25,000 nanoseconds
    
    Ok(())
}

#[tokio::test]
async fn test_concurrent_orderbook_access() -> anyhow::Result<()> {
    let ob = Arc::new(Mutex::new(OrderBook::new("BTCUSDT".to_string())));
    
    // Initialize orderbook
    {
        let mut locked_ob = ob.lock().await;
        let snapshot = OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bids: vec![PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
            asks: vec![PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask }],
            timestamp: now_micros(),
            sequence_start: 1,
            sequence_end: 1,
            is_snapshot: true,
        };
        locked_ob.init_snapshot(snapshot)?;
    }
    
    // Spawn multiple concurrent tasks
    let mut handles = Vec::new();
    for i in 0..10 {
        let ob_clone = Arc::clone(&ob);
        let handle = tokio::spawn(async move {
            let mut locked_ob = ob_clone.lock().await;
            
            // Each task performs different operations
            match i % 3 {
                0 => {
                    // Read operations
                    let _best_bid = locked_ob.best_bid();
                    let _best_ask = locked_ob.best_ask();
                    let _spread = locked_ob.spread();
                },
                1 => {
                    // Calculation operations
                    let _obi = locked_ob.calculate_obi(5);
                    let _depth = locked_ob.calculate_depth(3, Side::Bid);
                },
                2 => {
                    // Validation operations
                    let _is_valid = locked_ob.validate_orderbook();
                    let _stats = locked_ob.get_stats();
                },
                _ => {}
            }
            
            Ok::<(), anyhow::Error>(())
        });
        handles.push(handle);
    }
    
    // Wait for all tasks to complete
    for handle in handles {
        handle.await??;
    }
    
    Ok(())
}

#[tokio::test]
async fn test_orderbook_statistics() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![
            PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid },
        ],
        asks: vec![
            PriceLevel { price: 50001.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Ask },
        ],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    ob.init_snapshot(snapshot)?;
    
    let stats = ob.get_stats();
    assert_eq!(stats.bid_levels, 1);
    assert_eq!(stats.ask_levels, 1);
    assert!(stats.is_valid);
    assert_eq!(stats.update_count, 1);
    assert!(stats.spread.is_some());
    assert!(stats.spread_bps.is_some());
    
    Ok(())
}

#[tokio::test]
async fn test_large_orderbook_handling() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    // Create large orderbook with 1000 levels on each side
    let mut bids = Vec::new();
    let mut asks = Vec::new();
    
    for i in 0..1000 {
        bids.push(PriceLevel { 
            price: (50000.0 - i as f64 * 0.01).to_price(), 
            quantity: (1.0 + i as f64 * 0.001).to_quantity(), 
            side: Side::Bid 
        });
        asks.push(PriceLevel { 
            price: (50001.0 + i as f64 * 0.01).to_price(), 
            quantity: (1.0 + i as f64 * 0.001).to_quantity(), 
            side: Side::Ask 
        });
    }
    
    let snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids,
        asks,
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    let start = Instant::now();
    ob.init_snapshot(snapshot)?;
    let elapsed = start.elapsed();
    
    println!("Large orderbook initialization: {:?}", elapsed);
    
    // Verify the orderbook is properly initialized
    assert_eq!(ob.bids.len(), 1000);
    assert_eq!(ob.asks.len(), 1000);
    assert!(ob.is_valid);
    
    // Test deep calculations
    let deep_obi = ob.calculate_obi(100);
    assert!(deep_obi.abs() <= 1.0);
    
    let deep_bid_depth = ob.calculate_depth(100, Side::Bid);
    assert!(deep_bid_depth > 0.0);
    
    Ok(())
}

#[tokio::test]
async fn test_orderbook_edge_cases() -> anyhow::Result<()> {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    // Test empty levels
    let empty_snapshot = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![],
        asks: vec![],
        timestamp: now_micros(),
        sequence_start: 1,
        sequence_end: 1,
        is_snapshot: true,
    };
    
    ob.init_snapshot(empty_snapshot)?;
    assert!(!ob.is_valid); // Empty orderbook should be invalid
    assert!(ob.best_bid().is_none());
    assert!(ob.best_ask().is_none());
    assert!(ob.mid_price().is_none());
    assert!(ob.spread().is_none());
    
    // Test single-sided orderbook
    let single_sided = OrderBookUpdate {
        symbol: "BTCUSDT".to_string(),
        bids: vec![PriceLevel { price: 50000.0.to_price(), quantity: "1.0".to_quantity(), side: Side::Bid }],
        asks: vec![],
        timestamp: now_micros(),
        sequence_start: 2,
        sequence_end: 2,
        is_snapshot: true,
    };
    
    ob.init_snapshot(single_sided)?;
    assert!(!ob.is_valid); // Single-sided orderbook should be invalid
    assert!(ob.best_bid().is_some());
    assert!(ob.best_ask().is_none());
    assert!(ob.mid_price().is_none());
    
    Ok(())
}