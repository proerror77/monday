/*!
 * OrderBook 性能基準測試
 * 
 * 比較標準版本和高性能版本的 OrderBook 管理器
 */

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use rust_hft::integrations::{
    BarterOrderBookManager, 
    HighPerfOrderBookManager,
    high_perf_orderbook_manager::{HighPerfOrderBook, HighPerfLevel},
    MultiLevelOrderBook, 
    ExtendedLevel,
    BitgetMessage,
};
use rust_decimal_macros::dec;
use chrono::Utc;
use serde_json::json;

/// 創建測試數據
fn create_test_orderbook_data(depth: usize) -> (Vec<ExtendedLevel>, Vec<ExtendedLevel>) {
    let mut bids = Vec::with_capacity(depth);
    let mut asks = Vec::with_capacity(depth);
    
    for i in 0..depth {
        bids.push(ExtendedLevel {
            price: dec!(50000) - rust_decimal::Decimal::from(i),
            amount: dec!(1.0) + rust_decimal::Decimal::from(i) / dec!(10.0),
            order_id: None,
            timestamp: Utc::now(),
        });
        
        asks.push(ExtendedLevel {
            price: dec!(50000) + rust_decimal::Decimal::from(i + 1),
            amount: dec!(1.0) + rust_decimal::Decimal::from(i) / dec!(10.0),
            order_id: None,
            timestamp: Utc::now(),
        });
    }
    
    (bids, asks)
}

/// 創建高性能測試數據
fn create_high_perf_test_data(depth: usize) -> (Vec<HighPerfLevel>, Vec<HighPerfLevel>) {
    let mut bids = Vec::with_capacity(depth);
    let mut asks = Vec::with_capacity(depth);
    
    for i in 0..depth {
        bids.push(HighPerfLevel {
            price: dec!(50000) - rust_decimal::Decimal::from(i),
            amount: dec!(1.0) + rust_decimal::Decimal::from(i) / dec!(10.0),
            order_id: None,
            timestamp: i as u64,
        });
        
        asks.push(HighPerfLevel {
            price: dec!(50000) + rust_decimal::Decimal::from(i + 1),
            amount: dec!(1.0) + rust_decimal::Decimal::from(i) / dec!(10.0),
            order_id: None,
            timestamp: i as u64,
        });
    }
    
    (bids, asks)
}

/// 創建 Bitget 消息用於測試
fn create_bitget_message(depth: usize) -> BitgetMessage {
    let mut bids_json = Vec::new();
    let mut asks_json = Vec::new();
    
    for i in 0..depth {
        let bid_price = 50000.0 - i as f64;
        let ask_price = 50000.0 + i as f64 + 1.0;
        let amount = 1.0 + i as f64 / 10.0;
        
        bids_json.push(json!([bid_price.to_string(), amount.to_string()]));
        asks_json.push(json!([ask_price.to_string(), amount.to_string()]));
    }
    
    let data = json!([{
        "bids": bids_json,
        "asks": asks_json,
        "checksum": 123456789,
        "seq": 1
    }]);
    
    BitgetMessage::OrderBook {
        symbol: "BTCUSDT".to_string(),
        data,
        action: Some("snapshot".to_string()),
        timestamp: Utc::now().timestamp_micros() as u64,
    }
}

/// 基準測試：標準 OrderBook 更新
fn bench_standard_orderbook_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook_update");
    
    for depth in [5, 10, 20, 50].iter() {
        group.bench_with_input(
            BenchmarkId::new("standard", depth),
            depth,
            |b, &depth| {
                let mut orderbook = MultiLevelOrderBook::new("BTCUSDT".to_string());
                let (bids, asks) = create_test_orderbook_data(depth);
                
                b.iter(|| {
                    orderbook.update_l2(black_box(bids.clone()), black_box(asks.clone()));
                });
            },
        );
        
        group.bench_with_input(
            BenchmarkId::new("high_perf", depth),
            depth,
            |b, &depth| {
                let mut orderbook = HighPerfOrderBook::new("BTCUSDT".to_string());
                let (bids, asks) = create_high_perf_test_data(depth);
                
                b.iter(|| {
                    orderbook.update_l2_fast(black_box(bids.clone()), black_box(asks.clone()));
                });
            },
        );
    }
    
    group.finish();
}

/// 基準測試：統計計算
fn bench_statistics_calculation(c: &mut Criterion) {
    let mut group = c.benchmark_group("statistics");
    
    // 標準版本統計計算
    group.bench_function("standard_stats", |b| {
        let mut orderbook = MultiLevelOrderBook::new("BTCUSDT".to_string());
        let (bids, asks) = create_test_orderbook_data(20);
        orderbook.update_l2(bids, asks);
        
        b.iter(|| {
            black_box(orderbook.get_spread());
            black_box(orderbook.get_mid_price());
            black_box(orderbook.get_l2_depth());
        });
    });
    
    // 高性能版本統計計算（緩存）
    group.bench_function("high_perf_cached_stats", |b| {
        let mut orderbook = HighPerfOrderBook::new("BTCUSDT".to_string());
        let (bids, asks) = create_high_perf_test_data(20);
        orderbook.update_l2_fast(bids, asks);
        
        b.iter(|| {
            let stats = black_box(orderbook.get_cached_stats());
            black_box(stats.spread);
            black_box(stats.weighted_mid_price);
            black_box(stats.total_bid_volume);
        });
    });
    
    group.finish();
}

/// 基準測試：VWAP 計算
fn bench_vwap_calculation(c: &mut Criterion) {
    let mut group = c.benchmark_group("vwap");
    
    for depth in [5, 10, 20].iter() {
        group.bench_with_input(
            BenchmarkId::new("vwap_calculation", depth),
            depth,
            |b, &depth| {
                let mut orderbook = HighPerfOrderBook::new("BTCUSDT".to_string());
                let (bids, asks) = create_high_perf_test_data(20);
                orderbook.update_l2_fast(bids, asks);
                
                b.iter(|| {
                    black_box(orderbook.calculate_vwap_simd(depth));
                });
            },
        );
    }
    
    group.finish();
}

/// 基準測試：消息處理
fn bench_message_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("message_processing");
    
    // 標準版本消息處理
    group.bench_function("standard_message", |b| {
        let mut manager = BarterOrderBookManager::new();
        let message = create_bitget_message(15);
        
        b.iter(|| {
            black_box(manager.handle_bitget_message(black_box(message.clone())));
        });
    });
    
    // 高性能版本單個消息處理
    group.bench_function("high_perf_single_message", |b| {
        let mut manager = HighPerfOrderBookManager::new(1, 1000);
        let sender = manager.get_message_sender();
        let message = create_bitget_message(15);
        
        b.iter(|| {
            // 發送消息
            black_box(sender.send(black_box(message.clone())).unwrap());
            // 處理批量
            black_box(manager.process_message_batch().unwrap());
        });
    });
    
    group.finish();
}

/// 基準測試：批量處理
fn bench_batch_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_processing");
    
    for batch_size in [10, 50, 100, 500].iter() {
        group.bench_with_input(
            BenchmarkId::new("batch_size", batch_size),
            batch_size,
            |b, &batch_size| {
                let mut manager = HighPerfOrderBookManager::new(batch_size, 1000);
                let sender = manager.get_message_sender();
                
                b.iter(|| {
                    // 發送一批消息
                    for _ in 0..batch_size {
                        let message = create_bitget_message(10);
                        sender.send(message).unwrap();
                    }
                    
                    // 處理批量
                    black_box(manager.process_message_batch().unwrap());
                });
            },
        );
    }
    
    group.finish();
}

/// 基準測試：內存分配
fn bench_memory_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_allocation");
    
    // 標準版本 - 測試克隆開銷
    group.bench_function("standard_clone_overhead", |b| {
        let (bids, asks) = create_test_orderbook_data(20);
        
        b.iter(|| {
            let _bids_clone = black_box(bids.clone());
            let _asks_clone = black_box(asks.clone());
        });
    });
    
    // 高性能版本 - 測試克隆開銷
    group.bench_function("high_perf_clone_overhead", |b| {
        let (bids, asks) = create_high_perf_test_data(20);
        
        b.iter(|| {
            let _bids_clone = black_box(bids.clone());
            let _asks_clone = black_box(asks.clone());
        });
    });
    
    group.finish();
}

criterion_group!(
    benches,
    bench_standard_orderbook_update,
    bench_statistics_calculation,
    bench_vwap_calculation,
    bench_message_processing,
    bench_batch_processing,
    bench_memory_allocation
);

criterion_main!(benches);