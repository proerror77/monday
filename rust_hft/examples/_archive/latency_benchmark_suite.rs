//! # Latency Benchmark Suite
//!
//! 整合的延遲基準測試套件，包含以下測試類型：
//! - 核心訂單簿操作延遲測試
//! - 端到端交易流程延遲測試
//! - 數據處理管道延遲測試
//! - 網絡通訊延遲測試
//!
//! ## 使用方法
//! ```bash
//! cargo run --example latency_benchmark_suite -- --test-type core
//! cargo run --example latency_benchmark_suite -- --test-type e2e
//! cargo run --example latency_benchmark_suite -- --test-type pipeline
//! cargo run --example latency_benchmark_suite -- --test-type network
//! cargo run --example latency_benchmark_suite -- --test-type all
//! ```

use clap::{Arg, Command};
use rust_hft::core::{
    orderbook::OrderBook,
    types::{now_micros, ExchangeTimestamp, OrderSide, Price, Quantity, Symbol},
};
use rust_hft::engine::execution::ExecutionEngine;
use rust_hft::engine::strategies::basic::BasicStrategy;
use rust_hft::integrations::redis_bridge::RedisBridge;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{info, warn, error};

/// 延遲測試類型
#[derive(Debug, Clone, Copy)]
pub enum LatencyTestType {
    Core,       // 核心訂單簿操作
    E2E,        // 端到端交易流程
    Pipeline,   // 數據處理管道
    Network,    // 網絡通訊
    All,        // 全部測試
}

/// 延遲測量結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyMeasurement {
    pub test_type: String,
    pub operation: String,
    pub latency_us: u64,
    pub stage_latencies: HashMap<String, u64>,
    pub total_latency_us: u64,
    pub timestamp: u64,
}

/// 延遲統計信息
#[derive(Debug, Clone, Default)]
pub struct LatencyStatistics {
    pub samples: Vec<u64>,
    pub min: u64,
    pub max: u64,
    pub avg: f64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub count: usize,
}

impl LatencyStatistics {
    pub fn add_sample(&mut self, latency_us: u64) {
        self.samples.push(latency_us);
        self.count += 1;

        if self.count == 1 {
            self.min = latency_us;
            self.max = latency_us;
            self.avg = latency_us as f64;
        } else {
            self.min = self.min.min(latency_us);
            self.max = self.max.max(latency_us);
            self.avg = (self.avg * (self.count - 1) as f64 + latency_us as f64) / self.count as f64;
        }
    }

    pub fn calculate_percentiles(&mut self) {
        if self.samples.is_empty() {
            return;
        }

        self.samples.sort_unstable();
        let len = self.samples.len();

        self.p50 = self.samples[len * 50 / 100];
        self.p95 = self.samples[len * 95 / 100];
        self.p99 = self.samples[len * 99 / 100];
    }
}

/// 延遲基準測試器
pub struct LatencyBenchmarker {
    pub test_config: LatencyTestConfig,
    pub measurements: Vec<LatencyMeasurement>,
    pub statistics: HashMap<String, LatencyStatistics>,
}

#[derive(Debug, Clone)]
pub struct LatencyTestConfig {
    pub test_type: LatencyTestType,
    pub iterations: usize,
    pub warmup_iterations: usize,
    pub target_latency_us: u64,
    pub symbol: String,
    pub batch_size: usize,
}

impl Default for LatencyTestConfig {
    fn default() -> Self {
        Self {
            test_type: LatencyTestType::Core,
            iterations: 10000,
            warmup_iterations: 1000,
            target_latency_us: 25, // 25μs 目標延遲
            symbol: "BTCUSDT".to_string(),
            batch_size: 100,
        }
    }
}

impl LatencyBenchmarker {
    pub fn new(config: LatencyTestConfig) -> Self {
        Self {
            test_config: config,
            measurements: Vec::new(),
            statistics: HashMap::new(),
        }
    }

    /// 執行延遲基準測試
    pub async fn run_benchmark(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("🚀 啟動延遲基準測試套件");
        info!("   測試類型: {:?}", self.test_config.test_type);
        info!("   迭代次數: {}", self.test_config.iterations);
        info!("   目標延遲: {}μs", self.test_config.target_latency_us);

        match self.test_config.test_type {
            LatencyTestType::Core => self.run_core_latency_test().await?,
            LatencyTestType::E2E => self.run_e2e_latency_test().await?,
            LatencyTestType::Pipeline => self.run_pipeline_latency_test().await?,
            LatencyTestType::Network => self.run_network_latency_test().await?,
            LatencyTestType::All => {
                self.run_core_latency_test().await?;
                self.run_e2e_latency_test().await?;
                self.run_pipeline_latency_test().await?;
                self.run_network_latency_test().await?;
            }
        }

        self.calculate_statistics();
        self.print_results();

        Ok(())
    }

    /// 核心訂單簿操作延遲測試
    async fn run_core_latency_test(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("📊 執行核心訂單簿操作延遲測試");

        let mut orderbook = OrderBook::new();
        let symbol = Symbol::from(self.test_config.symbol.as_str());

        // 暖身運行
        for _ in 0..self.test_config.warmup_iterations {
            let _ = self.measure_orderbook_operation(&mut orderbook, &symbol);
        }

        // 正式測試
        for i in 0..self.test_config.iterations {
            let measurement = self.measure_orderbook_operation(&mut orderbook, &symbol);
            self.measurements.push(measurement);

            if i % 1000 == 0 && i > 0 {
                let avg_latency = self.measurements.iter()
                    .filter(|m| m.test_type == "core")
                    .map(|m| m.latency_us)
                    .sum::<u64>() as f64 / i as f64;

                info!("   進度: {}/{}, 平均延遲: {:.1}μs", i, self.test_config.iterations, avg_latency);
            }
        }

        Ok(())
    }

    /// 測量單次訂單簿操作延遲
    fn measure_orderbook_operation(&self, orderbook: &mut OrderBook, symbol: &Symbol) -> LatencyMeasurement {
        let mut stage_latencies = HashMap::new();
        let start_time = now_micros();

        // 階段1: 市場數據接收
        let t1 = now_micros();
        let price = Price::from(50000.0);
        let quantity = Quantity::from(1.0);
        stage_latencies.insert("market_data_received".to_string(), t1 - start_time);

        // 階段2: 訂單簿更新
        let t2 = now_micros();
        orderbook.update_level(symbol.clone(), OrderSide::Buy, price, quantity, ExchangeTimestamp::now());
        stage_latencies.insert("orderbook_updated".to_string(), t2 - t1);

        // 階段3: 策略計算
        let t3 = now_micros();
        let _ = self.simulate_strategy_calculation();
        stage_latencies.insert("strategy_calculation".to_string(), t3 - t2);

        // 階段4: 風險檢查
        let t4 = now_micros();
        let _ = self.simulate_risk_check();
        stage_latencies.insert("risk_check".to_string(), t4 - t3);

        // 階段5: 訂單生成
        let t5 = now_micros();
        let _ = self.simulate_order_generation();
        stage_latencies.insert("order_generation".to_string(), t5 - t4);

        let total_latency = t5 - start_time;

        LatencyMeasurement {
            test_type: "core".to_string(),
            operation: "orderbook_update_to_order".to_string(),
            latency_us: total_latency,
            stage_latencies,
            total_latency_us: total_latency,
            timestamp: start_time,
        }
    }

    /// 端到端交易流程延遲測試
    async fn run_e2e_latency_test(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("🔄 執行端到端交易流程延遲測試");

        // 模擬完整的交易流程
        for i in 0..self.test_config.iterations {
            let measurement = self.measure_e2e_trading_flow().await;
            self.measurements.push(measurement);

            if i % 500 == 0 && i > 0 {
                info!("   E2E測試進度: {}/{}", i, self.test_config.iterations);
            }
        }

        Ok(())
    }

    /// 測量端到端交易流程延遲
    async fn measure_e2e_trading_flow(&self) -> LatencyMeasurement {
        let mut stage_latencies = HashMap::new();
        let start_time = now_micros();

        // 階段1: WebSocket 數據接收
        let t1 = now_micros();
        let _ = self.simulate_websocket_data_reception().await;
        stage_latencies.insert("websocket_reception".to_string(), t1 - start_time);

        // 階段2: 數據解析和驗證
        let t2 = now_micros();
        let _ = self.simulate_data_parsing();
        stage_latencies.insert("data_parsing".to_string(), t2 - t1);

        // 階段3: 特徵提取
        let t3 = now_micros();
        let _ = self.simulate_feature_extraction();
        stage_latencies.insert("feature_extraction".to_string(), t3 - t2);

        // 階段4: ML 模型推理
        let t4 = now_micros();
        let _ = self.simulate_ml_inference();
        stage_latencies.insert("ml_inference".to_string(), t4 - t3);

        // 階段5: 交易決策
        let t5 = now_micros();
        let _ = self.simulate_trading_decision();
        stage_latencies.insert("trading_decision".to_string(), t5 - t4);

        // 階段6: 訂單提交
        let t6 = now_micros();
        let _ = self.simulate_order_submission().await;
        stage_latencies.insert("order_submission".to_string(), t6 - t5);

        let total_latency = t6 - start_time;

        LatencyMeasurement {
            test_type: "e2e".to_string(),
            operation: "complete_trading_flow".to_string(),
            latency_us: total_latency,
            stage_latencies,
            total_latency_us: total_latency,
            timestamp: start_time,
        }
    }

    /// 數據處理管道延遲測試
    async fn run_pipeline_latency_test(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("⚡ 執行數據處理管道延遲測試");

        for i in 0..self.test_config.iterations {
            let measurement = self.measure_pipeline_latency().await;
            self.measurements.push(measurement);

            if i % 500 == 0 && i > 0 {
                info!("   管道測試進度: {}/{}", i, self.test_config.iterations);
            }
        }

        Ok(())
    }

    /// 測量數據處理管道延遲
    async fn measure_pipeline_latency(&self) -> LatencyMeasurement {
        let mut stage_latencies = HashMap::new();
        let start_time = now_micros();

        // 階段1: 數據接收
        let t1 = now_micros();
        let _ = self.simulate_data_ingestion();
        stage_latencies.insert("data_ingestion".to_string(), t1 - start_time);

        // 階段2: 數據轉換
        let t2 = now_micros();
        let _ = self.simulate_data_transformation();
        stage_latencies.insert("data_transformation".to_string(), t2 - t1);

        // 階段3: 數據驗證
        let t3 = now_micros();
        let _ = self.simulate_data_validation();
        stage_latencies.insert("data_validation".to_string(), t3 - t2);

        // 階段4: 存儲寫入
        let t4 = now_micros();
        let _ = self.simulate_storage_write().await;
        stage_latencies.insert("storage_write".to_string(), t4 - t3);

        let total_latency = t4 - start_time;

        LatencyMeasurement {
            test_type: "pipeline".to_string(),
            operation: "data_processing_pipeline".to_string(),
            latency_us: total_latency,
            stage_latencies,
            total_latency_us: total_latency,
            timestamp: start_time,
        }
    }

    /// 網絡通訊延遲測試
    async fn run_network_latency_test(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("🌐 執行網絡通訊延遲測試");

        for i in 0..self.test_config.iterations {
            let measurement = self.measure_network_latency().await;
            self.measurements.push(measurement);

            if i % 500 == 0 && i > 0 {
                info!("   網絡測試進度: {}/{}", i, self.test_config.iterations);
            }
        }

        Ok(())
    }

    /// 測量網絡通訊延遲
    async fn measure_network_latency(&self) -> LatencyMeasurement {
        let mut stage_latencies = HashMap::new();
        let start_time = now_micros();

        // 階段1: TCP 連接
        let t1 = now_micros();
        let _ = self.simulate_tcp_connection().await;
        stage_latencies.insert("tcp_connection".to_string(), t1 - start_time);

        // 階段2: TLS 握手
        let t2 = now_micros();
        let _ = self.simulate_tls_handshake().await;
        stage_latencies.insert("tls_handshake".to_string(), t2 - t1);

        // 階段3: WebSocket 升級
        let t3 = now_micros();
        let _ = self.simulate_websocket_upgrade().await;
        stage_latencies.insert("websocket_upgrade".to_string(), t3 - t2);

        // 階段4: 消息發送
        let t4 = now_micros();
        let _ = self.simulate_message_send().await;
        stage_latencies.insert("message_send".to_string(), t4 - t3);

        // 階段5: 響應接收
        let t5 = now_micros();
        let _ = self.simulate_response_receive().await;
        stage_latencies.insert("response_receive".to_string(), t5 - t4);

        let total_latency = t5 - start_time;

        LatencyMeasurement {
            test_type: "network".to_string(),
            operation: "full_network_roundtrip".to_string(),
            latency_us: total_latency,
            stage_latencies,
            total_latency_us: total_latency,
            timestamp: start_time,
        }
    }

    /// 計算統計信息
    fn calculate_statistics(&mut self) {
        let test_types = ["core", "e2e", "pipeline", "network"];

        for test_type in &test_types {
            let mut stats = LatencyStatistics::default();

            for measurement in &self.measurements {
                if measurement.test_type == *test_type {
                    stats.add_sample(measurement.latency_us);
                }
            }

            if !stats.samples.is_empty() {
                stats.calculate_percentiles();
                self.statistics.insert(test_type.to_string(), stats);
            }
        }
    }

    /// 打印測試結果
    fn print_results(&self) {
        info!("📈 延遲基準測試結果");
        info!("================================");

        for (test_type, stats) in &self.statistics {
            info!("📊 {} 測試結果:", test_type.to_uppercase());
            info!("   樣本數量: {}", stats.count);
            info!("   平均延遲: {:.1}μs", stats.avg);
            info!("   最小延遲: {}μs", stats.min);
            info!("   最大延遲: {}μs", stats.max);
            info!("   P50延遲: {}μs", stats.p50);
            info!("   P95延遲: {}μs", stats.p95);
            info!("   P99延遲: {}μs", stats.p99);

            // 性能評級
            let grade = self.calculate_performance_grade(stats);
            info!("   性能評級: {}", grade);

            // 目標達成情況
            let target_met = stats.p99 <= self.test_config.target_latency_us;
            let status = if target_met { "✅ 達成" } else { "❌ 未達成" };
            info!("   目標({}μs): {}", self.test_config.target_latency_us, status);

            info!("--------------------------------");
        }

        // 整體評估
        self.print_overall_assessment();
    }

    /// 計算性能評級
    fn calculate_performance_grade(&self, stats: &LatencyStatistics) -> String {
        let avg_latency = stats.avg;

        if avg_latency <= 10.0 {
            "🏆 卓越 (≤10μs)".to_string()
        } else if avg_latency <= 25.0 {
            "🥇 優秀 (≤25μs)".to_string()
        } else if avg_latency <= 50.0 {
            "🥈 良好 (≤50μs)".to_string()
        } else if avg_latency <= 100.0 {
            "🥉 一般 (≤100μs)".to_string()
        } else {
            "⚠️ 需要優化 (>100μs)".to_string()
        }
    }

    /// 打印整體評估
    fn print_overall_assessment(&self) {
        info!("🎯 整體性能評估");
        info!("================================");

        let core_stats = self.statistics.get("core");
        let e2e_stats = self.statistics.get("e2e");

        if let (Some(core), Some(e2e)) = (core_stats, e2e_stats) {
            let avg_latency = (core.avg + e2e.avg) / 2.0;
            let theoretical_tps = 1_000_000.0 / avg_latency;

            info!("   系統平均延遲: {:.1}μs", avg_latency);
            info!("   理論吞吐量: {:.0} TPS", theoretical_tps);

            // HFT 系統評估標準
            if avg_latency <= 25.0 {
                info!("   🚀 符合 HFT 標準 - 延遲控制優秀");
            } else if avg_latency <= 100.0 {
                info!("   ⚡ 接近 HFT 標準 - 可接受範圍");
            } else {
                info!("   ⚠️ 未達 HFT 標準 - 需要優化");
            }

            // 推薦優化方向
            if core.avg > 50.0 {
                warn!("   💡 建議優化核心訂單簿操作");
            }

            if e2e.avg > 1000.0 {
                warn!("   💡 建議優化端到端流程");
            }
        }
    }

    // 模擬函數 - 實際實現中應該替換為真實的測試邏輯
    fn simulate_strategy_calculation(&self) -> u64 {
        let start = now_micros();
        // 模擬策略計算 - 2-5μs
        std::thread::sleep(Duration::from_nanos(3000)); // 3μs
        now_micros() - start
    }

    fn simulate_risk_check(&self) -> u64 {
        let start = now_micros();
        // 模擬風險檢查 - 1-3μs
        std::thread::sleep(Duration::from_nanos(2000)); // 2μs
        now_micros() - start
    }

    fn simulate_order_generation(&self) -> u64 {
        let start = now_micros();
        // 模擬訂單生成 - 1-2μs
        std::thread::sleep(Duration::from_nanos(1500)); // 1.5μs
        now_micros() - start
    }

    async fn simulate_websocket_data_reception(&self) -> u64 {
        let start = now_micros();
        sleep(Duration::from_nanos(5000)).await; // 5μs
        now_micros() - start
    }

    fn simulate_data_parsing(&self) -> u64 {
        let start = now_micros();
        std::thread::sleep(Duration::from_nanos(3000)); // 3μs
        now_micros() - start
    }

    fn simulate_feature_extraction(&self) -> u64 {
        let start = now_micros();
        std::thread::sleep(Duration::from_nanos(8000)); // 8μs
        now_micros() - start
    }

    fn simulate_ml_inference(&self) -> u64 {
        let start = now_micros();
        std::thread::sleep(Duration::from_nanos(15000)); // 15μs
        now_micros() - start
    }

    fn simulate_trading_decision(&self) -> u64 {
        let start = now_micros();
        std::thread::sleep(Duration::from_nanos(2000)); // 2μs
        now_micros() - start
    }

    async fn simulate_order_submission(&self) -> u64 {
        let start = now_micros();
        sleep(Duration::from_nanos(10000)).await; // 10μs
        now_micros() - start
    }

    fn simulate_data_ingestion(&self) -> u64 {
        let start = now_micros();
        std::thread::sleep(Duration::from_nanos(4000)); // 4μs
        now_micros() - start
    }

    fn simulate_data_transformation(&self) -> u64 {
        let start = now_micros();
        std::thread::sleep(Duration::from_nanos(6000)); // 6μs
        now_micros() - start
    }

    fn simulate_data_validation(&self) -> u64 {
        let start = now_micros();
        std::thread::sleep(Duration::from_nanos(3000)); // 3μs
        now_micros() - start
    }

    async fn simulate_storage_write(&self) -> u64 {
        let start = now_micros();
        sleep(Duration::from_nanos(20000)).await; // 20μs
        now_micros() - start
    }

    async fn simulate_tcp_connection(&self) -> u64 {
        let start = now_micros();
        sleep(Duration::from_millis(1)).await; // 1ms
        now_micros() - start
    }

    async fn simulate_tls_handshake(&self) -> u64 {
        let start = now_micros();
        sleep(Duration::from_millis(5)).await; // 5ms
        now_micros() - start
    }

    async fn simulate_websocket_upgrade(&self) -> u64 {
        let start = now_micros();
        sleep(Duration::from_millis(1)).await; // 1ms
        now_micros() - start
    }

    async fn simulate_message_send(&self) -> u64 {
        let start = now_micros();
        sleep(Duration::from_nanos(50000)).await; // 50μs
        now_micros() - start
    }

    async fn simulate_response_receive(&self) -> u64 {
        let start = now_micros();
        sleep(Duration::from_nanos(30000)).await; // 30μs
        now_micros() - start
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let matches = Command::new("Latency Benchmark Suite")
        .version("1.0")
        .about("HFT系統延遲基準測試套件")
        .arg(
            Arg::new("test-type")
                .long("test-type")
                .value_name("TYPE")
                .help("測試類型: core, e2e, pipeline, network, all")
                .default_value("all")
        )
        .arg(
            Arg::new("iterations")
                .long("iterations")
                .value_name("N")
                .help("測試迭代次數")
                .default_value("10000")
        )
        .arg(
            Arg::new("target-latency")
                .long("target-latency")
                .value_name("MICROSECONDS")
                .help("目標延遲(微秒)")
                .default_value("25")
        )
        .arg(
            Arg::new("symbol")
                .long("symbol")
                .value_name("SYMBOL")
                .help("測試交易對")
                .default_value("BTCUSDT")
        )
        .get_matches();

    let test_type = match matches.get_one::<String>("test-type").unwrap().as_str() {
        "core" => LatencyTestType::Core,
        "e2e" => LatencyTestType::E2E,
        "pipeline" => LatencyTestType::Pipeline,
        "network" => LatencyTestType::Network,
        "all" => LatencyTestType::All,
        _ => LatencyTestType::All,
    };

    let iterations = matches.get_one::<String>("iterations")
        .unwrap()
        .parse::<usize>()
        .unwrap_or(10000);

    let target_latency_us = matches.get_one::<String>("target-latency")
        .unwrap()
        .parse::<u64>()
        .unwrap_or(25);

    let symbol = matches.get_one::<String>("symbol")
        .unwrap()
        .clone();

    let config = LatencyTestConfig {
        test_type,
        iterations,
        warmup_iterations: iterations / 10,
        target_latency_us,
        symbol,
        batch_size: 100,
    };

    let mut benchmarker = LatencyBenchmarker::new(config);
    benchmarker.run_benchmark().await?;

    Ok(())
}
// Archived legacy example; see grouped examples
