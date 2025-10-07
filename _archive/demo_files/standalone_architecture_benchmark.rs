//! 獨立的架構效果驗證測試
//! 
//! 驗證 ArcSwap + simd-json 的性能表現

use std::sync::Arc;
use std::collections::HashMap;
use std::time::Instant;
use arc_swap::ArcSwap;
use std::sync::atomic::{AtomicU64, Ordering};
use dashmap::DashMap;

/// 簡化的實例結構
#[derive(Clone)]
struct TestInstance {
    name: String,
    enabled: bool,
}

/// 使用 ArcSwap 的快照結構
struct TestSnapshot {
    symbols: ArcSwap<HashMap<String, Arc<Vec<TestInstance>>>>,
}

impl TestSnapshot {
    fn new(map: HashMap<String, Arc<Vec<TestInstance>>>) -> Self {
        Self {
            symbols: ArcSwap::new(Arc::new(map))
        }
    }
    
    #[inline]
    fn load(&self) -> Arc<HashMap<String, Arc<Vec<TestInstance>>>> {
        self.symbols.load_full()
    }
    
    fn store(&self, new_map: HashMap<String, Arc<Vec<TestInstance>>>) {
        self.symbols.store(Arc::new(new_map));
    }
}

/// 測試管理器
struct TestManager {
    exchanges: ArcSwap<HashMap<String, Arc<TestSnapshot>>>,
    rr_counters: Arc<DashMap<(String, String), Arc<AtomicU64>>>,
}

impl TestManager {
    fn new() -> Self {
        Self {
            exchanges: ArcSwap::new(Arc::new(HashMap::new())),
            rr_counters: Arc::new(DashMap::new()),
        }
    }
    
    fn publish_exchanges(&self, map: HashMap<String, Arc<TestSnapshot>>) {
        self.exchanges.store(Arc::new(map));
    }
    
    #[inline]
    fn select_instance(&self, exchange: &str, symbol: &str) -> Option<TestInstance> {
        let exchanges = self.exchanges.load_full();
        let snap = exchanges.get(exchange)?;
        let sym_map = snap.load();
        let list = sym_map.get(symbol)?;
        if list.is_empty() { return None; }
        
        let enabled: Vec<_> = list.iter()
            .filter(|i| i.enabled)
            .cloned()
            .collect();
        if enabled.is_empty() { return None; }
        
        let key = (exchange.to_string(), symbol.to_string());
        let arc = self.rr_counters
            .entry(key)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone();
        let n = arc.fetch_add(1, Ordering::Relaxed);
        let idx = (n as usize) % enabled.len();
        Some(enabled[idx].clone())
    }
}

fn main() {
    println!("🚀 HFT 架構效果驗證開始");
    
    // 測試 1: ArcSwap 兩層快照性能
    test_arcswap_performance();
    
    // 測試 2: JSON 解析性能對比
    test_json_parsing_performance();
    
    // 測試 3: 併發實例選擇性能
    test_concurrent_performance();
    
    // 測試 4: 端到端延遲測試
    test_end_to_end_latency();
    
    println!("✅ 所有測試完成");
}

fn test_arcswap_performance() {
    println!("\n📊 測試 1: ArcSwap 兩層快照性能");
    
    let manager = create_test_manager();
    let exchanges = ["bitget", "binance", "bybit"];
    let symbols = ["BTCUSDT", "ETHUSDT", "ADAUSDT", "BNBUSDT", "SOLUSDT"];
    
    // 測試實例選擇性能
    let start = Instant::now();
    let mut selections = 0;
    for _ in 0..10000 {
        for exchange in &exchanges {
            for symbol in &symbols {
                if manager.select_instance(exchange, symbol).is_some() {
                    selections += 1;
                }
            }
        }
    }
    let duration = start.elapsed();
    
    println!("   無鎖實例選擇:");
    println!("   - 總次數: {} 次", selections);
    println!("   - 總耗時: {:.2?}", duration);
    println!("   - 平均耗時: {:.0} ns/次", duration.as_nanos() as f64 / selections as f64);
    println!("   - 吞吐量: {:.2} M ops/sec", selections as f64 / duration.as_secs_f64() / 1_000_000.0);
    
    // 測試快照發布性能
    let start = Instant::now();
    for _ in 0..1000 {
        let new_snapshot = create_test_snapshots();
        manager.publish_exchanges(new_snapshot);
    }
    let publish_duration = start.elapsed();
    
    println!("   快照發布:");
    println!("   - 1000次發布耗時: {:.2?}", publish_duration);
    println!("   - 平均每次發布: {:.2?}", publish_duration / 1000);
}

fn test_json_parsing_performance() {
    println!("\n📊 測試 2: JSON 解析性能對比");
    
    let orderbook_json = r#"{"action":"snapshot","arg":{"instId":"BTCUSDT","channel":"books15"},"data":[{"asks":[["67189.5","0.25",1],["67190.0","0.15",2],["67191.0","0.30",1]],"bids":[["67188.1","0.12",1],["67187.5","0.25",3],["67186.0","0.18",2]],"ts":"1721810005123"}]}"#;
    
    let trade_json = r#"{"arg":{"instId":"BTCUSDT","channel":"trade"},"data":[{"px":"67188.4","sz":"0.02","side":"buy","ts":"1721810005345","tradeId":"1234567890"}]}"#;
    
    // 測試 serde_json 性能
    let start = Instant::now();
    for _ in 0..10000 {
        let _: serde_json::Value = serde_json::from_str(orderbook_json).unwrap();
    }
    let serde_duration = start.elapsed();
    
    // 測試 simd-json 性能
    let start = Instant::now();
    for _ in 0..10000 {
        let mut data = orderbook_json.to_string();
        let _: serde_json::Value = simd_json::from_str(&mut data).unwrap();
    }
    let simd_duration = start.elapsed();
    
    println!("   訂單簿 JSON 解析 (10,000 次):");
    println!("   - serde_json: {:.2?}", serde_duration);
    println!("   - simd_json:  {:.2?}", simd_duration);
    println!("   - 性能提升:   {:.2}x", serde_duration.as_nanos() as f64 / simd_duration.as_nanos() as f64);
    
    // 測試交易 JSON 解析
    let start = Instant::now();
    for _ in 0..10000 {
        let _: serde_json::Value = serde_json::from_str(trade_json).unwrap();
    }
    let serde_trade_duration = start.elapsed();
    
    let start = Instant::now();
    for _ in 0..10000 {
        let mut data = trade_json.to_string();
        let _: serde_json::Value = simd_json::from_str(&mut data).unwrap();
    }
    let simd_trade_duration = start.elapsed();
    
    println!("   交易 JSON 解析 (10,000 次):");
    println!("   - serde_json: {:.2?}", serde_trade_duration);
    println!("   - simd_json:  {:.2?}", simd_trade_duration);
    println!("   - 性能提升:   {:.2}x", serde_trade_duration.as_nanos() as f64 / simd_trade_duration.as_nanos() as f64);
}

fn test_concurrent_performance() {
    println!("\n📊 測試 3: 併發實例選擇性能");
    
    let manager = Arc::new(create_test_manager());
    let rt = tokio::runtime::Runtime::new().unwrap();
    
    let start = Instant::now();
    rt.block_on(async {
        let tasks: Vec<_> = (0..8).map(|thread_id| {
            let manager = manager.clone();
            tokio::spawn(async move {
                let mut selections = 0;
                for _ in 0..1000 {
                    for exchange in ["bitget", "binance", "bybit"] {
                        for symbol in ["BTCUSDT", "ETHUSDT", "ADAUSDT"] {
                            if manager.select_instance(exchange, symbol).is_some() {
                                selections += 1;
                            }
                        }
                    }
                }
                (thread_id, selections)
            })
        }).collect();
        
        let results: Vec<_> = futures::future::join_all(tasks).await;
        let total_selections: usize = results.iter().map(|r| r.as_ref().unwrap().1).sum();
        
        println!("   8線程併發測試:");
        println!("   - 總選擇次數: {}", total_selections);
        for (thread_id, selections) in results.iter().map(|r| r.as_ref().unwrap()) {
            println!("   - 線程 {}: {} 次", thread_id, selections);
        }
    });
    let concurrent_duration = start.elapsed();
    
    println!("   - 總耗時: {:.2?}", concurrent_duration);
    println!("   - 併發TPS: {:.2} K ops/sec", (8 * 1000 * 3 * 3) as f64 / concurrent_duration.as_secs_f64() / 1000.0);
}

fn test_end_to_end_latency() {
    println!("\n📊 測試 4: 端到端延遲測試");
    
    let manager = create_test_manager();
    let json_message = r#"{"action":"snapshot","arg":{"instId":"BTCUSDT","channel":"books15"},"data":[{"asks":[["67189.5","0.25",1]],"bids":[["67188.1","0.12",1]],"ts":"1721810005123"}]}"#;
    
    // 測試完整處理流程
    let mut latencies = Vec::new();
    for _ in 0..10000 {
        let start = Instant::now();
        
        // 1. JSON 解析 (simd-json)
        let mut data = json_message.to_string();
        let _parsed = simd_json::from_str::<serde_json::Value>(&mut data);
        
        // 2. 實例選擇 (ArcSwap)
        let _instance = manager.select_instance("bitget", "BTCUSDT");
        
        let duration = start.elapsed();
        latencies.push(duration.as_nanos() as f64);
    }
    
    // 計算統計數據
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let len = latencies.len();
    let mean = latencies.iter().sum::<f64>() / len as f64;
    let p50 = latencies[len / 2];
    let p99 = latencies[len * 99 / 100];
    let max = latencies[len - 1];
    
    println!("   完整處理流程延遲 (JSON解析 + 實例選擇) - 10,000次:");
    println!("   - 平均延遲: {:.0} ns", mean);
    println!("   - p50 延遲: {:.0} ns", p50);
    println!("   - p99 延遲: {:.0} ns", p99);
    println!("   - 最大延遲: {:.0} ns", max);
    println!("   - 目標達成: {} (目標: p99 < 25μs = 25,000ns)", 
             if p99 < 25000.0 { "✅ 達標" } else { "❌ 未達標" });
}

fn create_test_manager() -> TestManager {
    let manager = TestManager::new();
    let snapshots = create_test_snapshots();
    manager.publish_exchanges(snapshots);
    manager
}

fn create_test_snapshots() -> HashMap<String, Arc<TestSnapshot>> {
    let mut snapshots = HashMap::new();
    
    let exchanges = ["bitget", "binance", "bybit"];
    let symbols = ["BTCUSDT", "ETHUSDT", "ADAUSDT", "BNBUSDT", "SOLUSDT"];
    
    for exchange in &exchanges {
        let mut symbol_map = HashMap::new();
        
        for symbol in &symbols {
            let instances = vec![
                TestInstance {
                    name: format!("{}-account-1", exchange),
                    enabled: true,
                },
                TestInstance {
                    name: format!("{}-account-2", exchange),
                    enabled: true,
                },
                TestInstance {
                    name: format!("{}-account-3", exchange),
                    enabled: true,
                },
            ];
            symbol_map.insert(symbol.to_string(), Arc::new(instances));
        }
        
        let snapshot = TestSnapshot::new(symbol_map);
        snapshots.insert(exchange.to_string(), Arc::new(snapshot));
    }
    
    snapshots
}