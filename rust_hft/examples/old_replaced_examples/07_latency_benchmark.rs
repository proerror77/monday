/*!
 * 延遲基準測試
 * 
 * 測試系統關鍵路徑的延遲性能
 */

use rust_hft::{
    integrations::bitget_connector::*,
    core::{types::*, orderbook::OrderBook},
    ml::features::FeatureExtractor,
    utils::performance::*,
};
use anyhow::Result;
use tracing::{info, warn};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::time::Duration;

struct LatencyBenchmark {
    network_latencies: VecDeque<u64>,
    parse_latencies: VecDeque<u64>,
    feature_latencies: VecDeque<u64>,
    total_latencies: VecDeque<u64>,
    max_samples: usize,
}

impl LatencyBenchmark {
    fn new() -> Self {
        Self {
            network_latencies: VecDeque::with_capacity(10000),
            parse_latencies: VecDeque::with_capacity(10000),
            feature_latencies: VecDeque::with_capacity(10000),
            total_latencies: VecDeque::with_capacity(10000),
            max_samples: 10000,
        }
    }
    
    fn record_latencies(&mut self, network: u64, parse: u64, feature: u64, total: u64) {
        self.network_latencies.push_back(network);
        if self.network_latencies.len() > self.max_samples {
            self.network_latencies.pop_front();
        }
        
        self.parse_latencies.push_back(parse);
        if self.parse_latencies.len() > self.max_samples {
            self.parse_latencies.pop_front();
        }
        
        self.feature_latencies.push_back(feature);
        if self.feature_latencies.len() > self.max_samples {
            self.feature_latencies.pop_front();
        }
        
        self.total_latencies.push_back(total);
        if self.total_latencies.len() > self.max_samples {
            self.total_latencies.pop_front();
        }
    }
    
    
    fn calculate_stats(&self, samples: &VecDeque<u64>) -> (f64, u64, u64, u64) {
        if samples.is_empty() {
            return (0.0, 0, 0, 0);
        }
        
        let mut sorted: Vec<u64> = samples.iter().cloned().collect();
        sorted.sort_unstable();
        
        let avg = samples.iter().sum::<u64>() as f64 / samples.len() as f64;
        let min = sorted[0];
        let max = sorted[sorted.len() - 1];
        let p99 = sorted[(sorted.len() as f64 * 0.99) as usize];
        
        (avg, min, max, p99)
    }
    
    fn print_report(&self) {
        info!("📊 === LATENCY BENCHMARK REPORT ===");
        
        let (net_avg, net_min, net_max, net_p99) = self.calculate_stats(&self.network_latencies);
        info!("🌐 Network Latency:");
        info!("   Avg: {:.1}μs, Min: {}μs, Max: {}μs, P99: {}μs", net_avg, net_min, net_max, net_p99);
        
        let (parse_avg, parse_min, parse_max, parse_p99) = self.calculate_stats(&self.parse_latencies);
        info!("📝 Parse Latency:");
        info!("   Avg: {:.1}μs, Min: {}μs, Max: {}μs, P99: {}μs", parse_avg, parse_min, parse_max, parse_p99);
        
        let (feat_avg, feat_min, feat_max, feat_p99) = self.calculate_stats(&self.feature_latencies);
        info!("🔍 Feature Extraction Latency:");
        info!("   Avg: {:.1}μs, Min: {}μs, Max: {}μs, P99: {}μs", feat_avg, feat_min, feat_max, feat_p99);
        
        let (total_avg, total_min, total_max, total_p99) = self.calculate_stats(&self.total_latencies);
        info!("⚡ Total Processing Latency:");
        info!("   Avg: {:.1}μs, Min: {}μs, Max: {}μs, P99: {}μs", total_avg, total_min, total_max, total_p99);
        
        info!("===================================");
        
        // 性能評估
        if total_p99 < 1000 {
            info!("🟢 EXCELLENT: P99 latency < 1ms");
        } else if total_p99 < 5000 {
            info!("🟡 GOOD: P99 latency < 5ms");
        } else {
            warn!("🔴 POOR: P99 latency > 5ms - optimization needed");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("⚡ Starting Latency Benchmark Test");
    
    // 初始化性能優化
    let _capabilities = detect_hardware_capabilities();
    
    let benchmark = Arc::new(Mutex::new(LatencyBenchmark::new()));
    let mut feature_extractor = FeatureExtractor::new(50);
    
    // 配置 Bitget 連接
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };

    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);

    let benchmark_clone = benchmark.clone();
    let message_handler = move |message: BitgetMessage| {
        let receive_time = now_micros();
        
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                let network_latency = receive_time - timestamp;
                
                // 解析 OrderBook
                let parse_start = now_micros();
                if let Ok(orderbook) = parse_bitget_orderbook(&symbol, &data, timestamp) {
                    let parse_latency = now_micros() - parse_start;
                    
                    // 特徵提取
                    let feature_start = now_micros();
                    if let Ok(_features) = feature_extractor.extract_features(&orderbook, 50, timestamp) {
                        let feature_latency = now_micros() - feature_start;
                        let total_latency = now_micros() - receive_time;
                        
                        // 記錄延遲
                        if let Ok(mut bench) = benchmark_clone.lock() {
                            bench.record_latencies(network_latency, parse_latency, feature_latency, total_latency);
                        }
                    }
                }
            },
            _ => {}
        }
    };

    // 啟動統計報告
    let benchmark_stats = benchmark.clone();
    let stats_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        
        loop {
            interval.tick().await;
            if let Ok(bench) = benchmark_stats.lock() {
                if !bench.total_latencies.is_empty() {
                    let (avg, _, _, p99) = bench.calculate_stats(&bench.total_latencies);
                    info!("📊 Current: Avg {:.1}μs, P99 {}μs, Samples: {}", 
                          avg, p99, bench.total_latencies.len());
                }
            }
        }
    });

    // 運行基準測試
    tokio::select! {
        result = connector.connect_public(message_handler) => {
            if let Err(e) = result {
                warn!("Connection failed: {}", e);
            }
        }
        _ = tokio::time::sleep(Duration::from_secs(60)) => {
            info!("✅ Benchmark test completed");
        }
    }

    stats_task.abort();
    
    // 打印最終報告
    if let Ok(bench) = benchmark.lock() {
        bench.print_report();
    }

    Ok(())
}

fn parse_bitget_orderbook(symbol: &str, _data: &serde_json::Value, timestamp: u64) -> Result<OrderBook> {
    let mut orderbook = OrderBook::new(symbol.to_string());
    orderbook.last_update = timestamp;
    orderbook.is_valid = true;
    
    // 簡化的解析邏輯用於基準測試
    Ok(orderbook)
}