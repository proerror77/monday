//! SIMD-JSON + ArcSwap High-Performance Demo
//! 
//! 展示完整的高性能數據流：WebSocket -> SIMD-JSON 解析 -> ArcSwap 快照存儲

use hft_data::{
    MessageParser, 
    create_optimal_snapshot_store,
    types::{OrderBookSnapshot, OrderBookLevel, InstrumentId}
};
use std::time::Instant;
use tokio::time::{sleep, Duration};

// 模擬 Bitget OrderBook 數據
const SAMPLE_ORDERBOOK_JSON: &str = r#"
{
    "action": "snapshot",
    "arg": {"instId": "BTCUSDT", "channel": "books15"},
    "data": [{
        "asks": [
            ["67189.5", "0.25", "1"],
            ["67190.0", "0.15", "1"],
            ["67191.0", "0.30", "2"]
        ],
        "bids": [
            ["67188.1", "0.12", "1"],
            ["67187.5", "0.20", "1"],
            ["67186.0", "0.35", "2"]
        ],
        "ts": "1721810005123"
    }]
}
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 SIMD-JSON + ArcSwap 高性能演示");
    println!("================================");
    
    // 1. 創建高性能解析器 (feature-gated simd-json)
    println!("\n📊 初始化組件...");
    let mut parser = MessageParser::new();
    let snapshot_store = create_optimal_snapshot_store();
    
    // 2. 性能基準測試
    println!("\n⚡ 執行性能基準測試...");
    run_performance_benchmark(&mut parser, &snapshot_store).await?;
    
    // 3. 展示零拷貝讀取性能
    println!("\n🔍 展示零拷貝讀取...");
    demonstrate_zero_copy_reads(&snapshot_store).await;
    
    // 4. 展示實時更新性能
    println!("\n🔄 展示實時更新性能...");
    demonstrate_realtime_updates(&mut parser, &snapshot_store).await?;
    
    println!("\n✅ 演示完成！");
    Ok(())
}

async fn run_performance_benchmark(
    parser: &mut MessageParser, 
    store: &hft_data::SnapshotStore
) -> Result<(), Box<dyn std::error::Error>> {
    const ITERATIONS: usize = 10_000;
    
    println!("   測試 {} 次解析與存儲操作...", ITERATIONS);
    
    let start_time = Instant::now();
    
    for i in 0..ITERATIONS {
        // 解析 JSON (使用 SIMD-JSON feature)
        let message = parser.parse_message(SAMPLE_ORDERBOOK_JSON.as_bytes())?;
        
        // 創建快照並存儲 (使用 ArcSwap)
        let snapshot = create_test_snapshot(i as u64);
        store.store_snapshot(format!("BTCUSDT_{}", i), snapshot)?;
        
        // 每1000次操作讀取一次以測試讀性能
        if i % 1000 == 0 {
            let _ = store.get_snapshot(&format!("BTCUSDT_{}", i));
        }
    }
    
    let elapsed = start_time.elapsed();
    let ops_per_sec = ITERATIONS as f64 / elapsed.as_secs_f64();
    
    println!("   📈 結果:");
    println!("   - 總耗時: {:?}", elapsed);
    println!("   - 每秒操作數: {:.0} ops/sec", ops_per_sec);
    println!("   - 平均延遲: {:.1} μs/op", elapsed.as_micros() as f64 / ITERATIONS as f64);
    println!("   - 解析平均延遲: {:.1} μs", parser.avg_parse_latency_us());
    println!("   - 存儲平均延遲: {:.1} μs", store.avg_store_latency_us());
    
    Ok(())
}

async fn demonstrate_zero_copy_reads(store: &hft_data::SnapshotStore) {
    println!("   測試零拷貝讀取性能...");
    
    // 存儲一個測試快照
    let snapshot = create_test_snapshot(12345);
    store.store_snapshot("DEMO".to_string(), snapshot).unwrap();
    
    const READ_ITERATIONS: usize = 100_000;
    let start_time = Instant::now();
    
    for _ in 0..READ_ITERATIONS {
        // ArcSwap 零拷貝讀取
        let _snapshot = store.get_snapshot(&"DEMO".to_string());
        
        // 快速訪問最佳價位
        let _best_bid = store.best_bid(&"DEMO".to_string());
        let _best_ask = store.best_ask(&"DEMO".to_string());
        let _spread = store.spread(&"DEMO".to_string());
    }
    
    let elapsed = start_time.elapsed();
    let reads_per_sec = READ_ITERATIONS as f64 / elapsed.as_secs_f64();
    
    println!("   📊 零拷貝讀取結果:");
    println!("   - {} 次讀取耗時: {:?}", READ_ITERATIONS, elapsed);
    println!("   - 每秒讀取數: {:.0} reads/sec", reads_per_sec);
    println!("   - 平均讀取延遲: {:.1} ns/read", elapsed.as_nanos() as f64 / READ_ITERATIONS as f64);
}

async fn demonstrate_realtime_updates(
    parser: &mut MessageParser,
    store: &hft_data::SnapshotStore
) -> Result<(), Box<dyn std::error::Error>> {
    println!("   模擬實時市場數據更新...");
    
    // 模擬不同價位的市場數據
    let market_updates = vec![
        (67189.0, 0.25),
        (67188.5, 0.30),
        (67190.0, 0.20),
        (67187.0, 0.35),
        (67191.5, 0.15),
    ];
    
    for (i, (price, qty)) in market_updates.iter().enumerate() {
        let start_time = Instant::now();
        
        // 創建更新的快照
        let mut snapshot = create_test_snapshot(i as u64);
        snapshot.bids[0] = OrderBookLevel::new(*price, *qty);
        
        // 原子更新 (ArcSwap)
        store.store_snapshot("REALTIME".to_string(), snapshot)?;
        
        // 立即讀取驗證
        let retrieved = store.get_snapshot(&"REALTIME".to_string()).unwrap();
        let update_latency = start_time.elapsed();
        
        println!("   📊 更新 {}: 最佳買價 {:.1} @ {:.3}, 延遲 {:?}", 
            i + 1, retrieved.bids[0].price, retrieved.bids[0].quantity, update_latency);
        
        // 模擬實時間隔
        sleep(Duration::from_millis(10)).await;
    }
    
    Ok(())
}

fn create_test_snapshot(sequence: u64) -> OrderBookSnapshot {
    OrderBookSnapshot {
        asks: vec![
            OrderBookLevel::new(67189.5, 0.25),
            OrderBookLevel::new(67190.0, 0.15),
            OrderBookLevel::new(67191.0, 0.30),
        ],
        bids: vec![
            OrderBookLevel::new(67188.1, 0.12),
            OrderBookLevel::new(67187.5, 0.20),
            OrderBookLevel::new(67186.0, 0.35),
        ],
        sequence: Some(sequence),
        checksum: None,
    }
}