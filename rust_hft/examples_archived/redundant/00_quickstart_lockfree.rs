/*!
 * Lock-Free OrderBook 性能測試和比較
 * 
 * 比較傳統 BTreeMap OrderBook 與 Lock-Free OrderBook 的性能差異
 * 目標：驗證 < 1μs 延遲的實現
 */

use rust_hft::core::{
    LockFreeOrderBook,
    OrderBook,
    types::*,
};
use std::time::Instant;
use tracing::info;
use clap::Parser;
use ordered_float::OrderedFloat;
use rust_decimal::Decimal;

#[derive(Parser, Debug)]
#[command(about = "Lock-Free OrderBook 性能基準測試")]
struct Args {
    /// 測試更新次數
    #[arg(short, long, default_value_t = 100000)]
    updates: usize,
    
    /// 價格級別數量
    #[arg(short, long, default_value_t = 100)]
    levels: usize,
    
    /// 詳細輸出
    #[arg(short, long)]
    verbose: bool,
}

fn generate_test_data(levels: usize) -> Vec<(Side, Price, Quantity)> {
    let mut data = Vec::with_capacity(levels * 2);
    
    // 生成買盤數據
    for i in 0..levels {
        let price = Price::from(100.0 - i as f64 * 0.01);
        let quantity = Quantity::try_from(10.0 + fastrand::f64() * 90.0).unwrap();
        data.push((Side::Bid, price, quantity));
    }
    
    // 生成賣盤數據
    for i in 0..levels {
        let price = Price::from(100.01 + i as f64 * 0.01);
        let quantity = Quantity::try_from(10.0 + fastrand::f64() * 90.0).unwrap();
        data.push((Side::Ask, price, quantity));
    }
    
    data
}

fn benchmark_traditional_orderbook(
    updates: usize, 
    test_data: &[(Side, Price, Quantity)]
) -> (u128, f64) {
    let mut ob = OrderBook::new("BTCUSDT".to_string());
    
    let start = Instant::now();
    
    for i in 0..updates {
        let (side, price, quantity) = &test_data[i % test_data.len()];
        
        // 模擬 OrderBook 更新
        match side {
            Side::Bid => {
                ob.bids.insert(*price, *quantity);
            }
            Side::Ask => {
                ob.asks.insert(*price, *quantity);
            }
        }
        
        // 執行一些常見操作
        if i % 100 == 0 {
            let _ = ob.best_bid();
            let _ = ob.best_ask();
            let _ = ob.mid_price();
        }
    }
    
    let elapsed = start.elapsed();
    let avg_latency_ns = elapsed.as_nanos() as f64 / updates as f64;
    
    (elapsed.as_nanos(), avg_latency_ns)
}

fn benchmark_lockfree_orderbook(
    updates: usize, 
    test_data: &[(Side, Price, Quantity)]
) -> (u128, f64) {
    let ob = LockFreeOrderBook::new("BTCUSDT".to_string());
    
    let start = Instant::now();
    
    for i in 0..updates {
        let (side, price, quantity) = &test_data[i % test_data.len()];
        
        // Lock-Free OrderBook 更新
        let _ = ob.update_level(*side, *price, *quantity);
        
        // 執行一些常見操作
        if i % 100 == 0 {
            let _ = ob.best_bid();
            let _ = ob.best_ask();
            let _ = ob.mid_price();
        }
    }
    
    let elapsed = start.elapsed();
    let avg_latency_ns = elapsed.as_nanos() as f64 / updates as f64;
    
    (elapsed.as_nanos(), avg_latency_ns)
}

fn run_concurrent_benchmark(
    updates_per_thread: usize,
    test_data: &[(Side, Price, Quantity)],
    num_threads: usize,
) -> (u128, f64) {
    use std::sync::Arc;
    use std::thread;
    
    let ob = Arc::new(LockFreeOrderBook::new("BTCUSDT".to_string()));
    let test_data = Arc::new(test_data.to_vec());
    
    let start = Instant::now();
    
    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let ob_clone = ob.clone();
            let data_clone = test_data.clone();
            
            thread::spawn(move || {
                for i in 0..updates_per_thread {
                    let idx = (thread_id * updates_per_thread + i) % data_clone.len();
                    let (side, price, quantity) = &data_clone[idx];
                    
                    let _ = ob_clone.update_level(*side, *price, *quantity);
                    
                    if i % 100 == 0 {
                        let _ = ob_clone.best_bid();
                        let _ = ob_clone.best_ask();
                    }
                }
            })
        })
        .collect();
    
    for handle in handles {
        handle.join().unwrap();
    }
    
    let elapsed = start.elapsed();
    let total_updates = updates_per_thread * num_threads;
    let avg_latency_ns = elapsed.as_nanos() as f64 / total_updates as f64;
    
    (elapsed.as_nanos(), avg_latency_ns)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    
    let args = Args::parse();
    
    info!("🚀 Lock-Free OrderBook 性能基準測試");
    info!("測試參數: {} 次更新, {} 個價格級別", args.updates, args.levels);
    
    // 生成測試數據
    let test_data = generate_test_data(args.levels);
    info!("✅ 生成了 {} 條測試數據", test_data.len());
    
    // 傳統 OrderBook 基準測試
    info!("📊 測試傳統 BTreeMap OrderBook...");
    let (traditional_total_ns, traditional_avg_ns) = 
        benchmark_traditional_orderbook(args.updates, &test_data);
    
    // Lock-Free OrderBook 基準測試
    info!("📊 測試 Lock-Free OrderBook...");
    let (lockfree_total_ns, lockfree_avg_ns) = 
        benchmark_lockfree_orderbook(args.updates, &test_data);
    
    // 並發測試 (4 個線程)
    info!("📊 測試並發 Lock-Free OrderBook (4 線程)...");
    let (concurrent_total_ns, concurrent_avg_ns) = 
        run_concurrent_benchmark(args.updates / 4, &test_data, 4);
    
    // 結果分析
    info!("\n🎯 ===== 性能測試結果 =====");
    
    info!("🔸 傳統 BTreeMap OrderBook:");
    info!("   總時間: {:.2}ms", traditional_total_ns as f64 / 1_000_000.0);
    info!("   平均延遲: {:.2}ns ({:.3}μs)", traditional_avg_ns, traditional_avg_ns / 1000.0);
    info!("   吞吐量: {:.0} ops/s", 1_000_000_000.0 / traditional_avg_ns);
    
    info!("🔸 Lock-Free OrderBook:");
    info!("   總時間: {:.2}ms", lockfree_total_ns as f64 / 1_000_000.0);
    info!("   平均延遲: {:.2}ns ({:.3}μs)", lockfree_avg_ns, lockfree_avg_ns / 1000.0);
    info!("   吞吐量: {:.0} ops/s", 1_000_000_000.0 / lockfree_avg_ns);
    
    info!("🔸 並發 Lock-Free OrderBook (4線程):");
    info!("   總時間: {:.2}ms", concurrent_total_ns as f64 / 1_000_000.0);
    info!("   平均延遲: {:.2}ns ({:.3}μs)", concurrent_avg_ns, concurrent_avg_ns / 1000.0);
    info!("   吞吐量: {:.0} ops/s", 1_000_000_000.0 / concurrent_avg_ns);
    
    // 性能提升計算
    let speedup = traditional_avg_ns / lockfree_avg_ns;
    let concurrent_speedup = traditional_avg_ns / concurrent_avg_ns;
    
    info!("\n🏆 ===== 性能提升分析 =====");
    info!("🚀 Lock-Free vs 傳統: {:.2}x 提升", speedup);
    info!("🚀 並發 Lock-Free vs 傳統: {:.2}x 提升", concurrent_speedup);
    
    // 延遲等級評估
    let lockfree_latency_us = lockfree_avg_ns / 1000.0;
    let target_met = lockfree_latency_us < 1.0;
    
    info!("\n🎯 ===== 目標達成評估 =====");
    info!("目標延遲: < 1μs");
    info!("實際延遲: {:.3}μs", lockfree_latency_us);
    info!("目標達成: {}", if target_met { "✅ 是" } else { "❌ 否" });
    
    if target_met {
        info!("🎉 恭喜！Lock-Free OrderBook 已達到 < 1μs 的目標延遲！");
    } else {
        info!("⚠️  需要進一步優化以達到 < 1μs 目標");
        info!("💡 建議：");
        info!("   • 使用更高效的無鎖數據結構");
        info!("   • 實施 SIMD 優化");
        info!("   • 優化內存分配策略");
        info!("   • 添加 CPU 親和性綁定");
    }
    
    // 詳細統計
    if args.verbose {
        info!("\n📈 ===== 詳細統計 =====");
        
        // 創建一個測試實例來獲取統計信息
        let test_ob = LockFreeOrderBook::new("TEST".to_string());
        for (side, price, quantity) in test_data.iter().take(100) {
            let _ = test_ob.update_level(*side, *price, *quantity);
        }
        
        let stats = test_ob.stats();
        info!("Lock-Free OrderBook 統計:");
        info!("   買盤級別: {}", stats.bid_levels);
        info!("   賣盤級別: {}", stats.ask_levels);
        info!("   更新次數: {}", stats.update_count);
        info!("   序列號: {}", stats.sequence);
        info!("   有效性: {}", stats.is_valid);
        
        let (bid_vol, ask_vol) = test_ob.depth_stats(5);
        info!("   前5級深度: 買盤 {:.2}, 賣盤 {:.2}", bid_vol, ask_vol);
        
        if let Some(spread) = test_ob.spread() {
            info!("   當前價差: {:.4}", spread);
        }
    }
    
    Ok(())
}