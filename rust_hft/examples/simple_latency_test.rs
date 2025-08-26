/*! 
 * 簡化版延遲測試 - 純時間戳測量，無複雜依賴
 * 
 * 測量核心組件的微秒級延遲：
 * 🕐 時間戳捕獲
 * 📊 簡單數據結構操作
 * 🧮 基本計算邏輯
 * 📤 模擬訂單創建
 * 💰 基本邏輯判斷
 */

use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::time::{sleep, Duration};

/// 簡化的市場數據結構
#[derive(Debug, Clone)]
pub struct SimpleMarketData {
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
    pub last_price: f64,
    pub timestamp_us: u64,
}

/// 簡化的交易信號
#[derive(Debug, Clone)]
pub struct SimpleSignal {
    pub symbol: String,
    pub action: SignalAction,
    pub quantity: f64,
    pub price: f64,
    pub timestamp_us: u64,
}

#[derive(Debug, Clone)]
pub enum SignalAction {
    Buy,
    Sell,
    Hold,
}

/// 簡化的訂單結構
#[derive(Debug, Clone)]
pub struct SimpleOrder {
    pub id: String,
    pub symbol: String,
    pub side: String,
    pub quantity: f64,
    pub price: f64,
    pub timestamp_us: u64,
}

/// 簡化的執行回報
#[derive(Debug, Clone)]
pub struct SimpleExecution {
    pub order_id: String,
    pub symbol: String,
    pub executed_qty: f64,
    pub executed_price: f64,
    pub timestamp_us: u64,
}

/// 延遲測量階段
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LatencyStage {
    Start,
    MarketDataReceived,
    PriceCalculated,
    SignalGenerated,
    RiskChecked,
    OrderCreated,
    ExecutionCompleted,
    ResultProcessed,
}

/// 延遲測量結果
#[derive(Debug, Clone)]
pub struct LatencyMeasurement {
    pub test_id: String,
    pub timestamps: HashMap<LatencyStage, u64>,
    pub latencies: HashMap<LatencyStage, u64>,
    pub total_latency_us: u64,
    pub symbol: String,
}

/// 延遲統計
#[derive(Debug, Default)]
pub struct LatencyStats {
    pub samples: Vec<u64>,
    pub min: u64,
    pub max: u64,
    pub avg: f64,
    pub p95: u64,
    pub p99: u64,
}

impl LatencyStats {
    pub fn new() -> Self {
        Default::default()
    }
    
    pub fn add_sample(&mut self, latency_us: u64) {
        self.samples.push(latency_us);
    }
    
    pub fn calculate(&mut self) {
        if self.samples.is_empty() {
            return;
        }
        
        self.samples.sort();
        self.min = self.samples[0];
        self.max = self.samples[self.samples.len() - 1];
        self.avg = self.samples.iter().sum::<u64>() as f64 / self.samples.len() as f64;
        
        let len = self.samples.len();
        self.p95 = self.samples[len * 95 / 100];
        self.p99 = self.samples[len * 99 / 100];
    }
}

/// 簡化延遲測試器
pub struct SimpleLatencyTester {
    symbol: String,
    results: Vec<LatencyMeasurement>,
    test_count: u64,
}

impl SimpleLatencyTester {
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            results: Vec::new(),
            test_count: 0,
        }
    }
    
    /// 運行延遲測試
    pub async fn run_test(&mut self, num_rounds: usize) -> Result<(), Box<dyn std::error::Error>> {
        println!("🎯 開始簡化延遲測試 - {} 輪測試", num_rounds);
        
        let start = Instant::now();
        
        for round in 0..num_rounds {
            match self.execute_single_round(round).await {
                Ok(measurement) => {
                    self.results.push(measurement);
                    
                    if round % 10000 == 0 {
                        println!("✅ 完成 {} 輪測試", round);
                    }
                },
                Err(e) => {
                    eprintln!("❌ 第{}輪測試失敗: {}", round, e);
                }
            }
            
            // 偶爾讓出控制權
            if round % 50000 == 49999 {
                tokio::task::yield_now().await;
            }
        }
        
        let elapsed = start.elapsed();
        println!("⏱️ 測試完成 - 總時間: {:.2}秒", elapsed.as_secs_f64());
        println!("🔄 平均每輪: {:.1}μs", elapsed.as_micros() as f64 / num_rounds as f64);
        
        self.generate_report().await;
        
        Ok(())
    }
    
    /// 執行單輪測試
    async fn execute_single_round(&mut self, round: usize) -> Result<LatencyMeasurement, Box<dyn std::error::Error>> {
        let test_id = format!("simple_test_{}", round);
        let start_time = get_timestamp_us();
        
        let mut measurement = LatencyMeasurement {
            test_id,
            timestamps: HashMap::new(),
            latencies: HashMap::new(),
            total_latency_us: 0,
            symbol: self.symbol.clone(),
        };
        
        // T0: 開始
        measurement.timestamps.insert(LatencyStage::Start, start_time);
        
        // T1: 模擬市場數據接收
        let market_data = self.simulate_market_data().await;
        let t1 = get_timestamp_us();
        measurement.timestamps.insert(LatencyStage::MarketDataReceived, t1);
        
        // T2: 價格計算（計算中間價等）
        let mid_price = self.calculate_mid_price(&market_data).await;
        let t2 = get_timestamp_us();
        measurement.timestamps.insert(LatencyStage::PriceCalculated, t2);
        
        // T3: 生成交易信號
        let signal = self.generate_signal(&market_data, mid_price).await;
        let t3 = get_timestamp_us();
        measurement.timestamps.insert(LatencyStage::SignalGenerated, t3);
        
        // T4: 基本風險檢查
        let risk_ok = self.basic_risk_check(&signal).await;
        let t4 = get_timestamp_us();
        measurement.timestamps.insert(LatencyStage::RiskChecked, t4);
        
        if !risk_ok {
            return Err("Risk check failed".into());
        }
        
        // T5: 創建訂單
        let order = self.create_order(&signal).await;
        let t5 = get_timestamp_us();
        measurement.timestamps.insert(LatencyStage::OrderCreated, t5);
        
        // T6: 模擬執行
        let execution = self.simulate_execution(&order).await;
        let t6 = get_timestamp_us();
        measurement.timestamps.insert(LatencyStage::ExecutionCompleted, t6);
        
        // T7: 處理結果
        self.process_execution_result(&execution).await;
        let t7 = get_timestamp_us();
        measurement.timestamps.insert(LatencyStage::ResultProcessed, t7);
        
        // 計算延遲
        self.calculate_latencies(&mut measurement);
        measurement.total_latency_us = t7 - start_time;
        
        Ok(measurement)
    }
    
    /// 模擬市場數據
    async fn simulate_market_data(&self) -> SimpleMarketData {
        let base_price = 50000.0;
        let spread = 2.0;
        
        SimpleMarketData {
            symbol: self.symbol.clone(),
            bid: base_price - spread / 2.0,
            ask: base_price + spread / 2.0,
            last_price: base_price,
            timestamp_us: get_timestamp_us(),
        }
    }
    
    /// 計算中間價
    async fn calculate_mid_price(&self, data: &SimpleMarketData) -> f64 {
        (data.bid + data.ask) / 2.0
    }
    
    /// 生成交易信號
    async fn generate_signal(&self, data: &SimpleMarketData, mid_price: f64) -> SimpleSignal {
        // 簡單的策略邏輯：如果最新價格低於中間價，買入
        let action = if data.last_price < mid_price {
            SignalAction::Buy
        } else {
            SignalAction::Sell
        };
        
        SimpleSignal {
            symbol: data.symbol.clone(),
            action,
            quantity: 0.01, // 小額測試
            price: mid_price,
            timestamp_us: get_timestamp_us(),
        }
    }
    
    /// 基本風險檢查
    async fn basic_risk_check(&self, signal: &SimpleSignal) -> bool {
        // 簡單檢查：數量和價格合理性
        signal.quantity > 0.0 && signal.quantity < 1.0 && signal.price > 1000.0
    }
    
    /// 創建訂單
    async fn create_order(&self, signal: &SimpleSignal) -> SimpleOrder {
        SimpleOrder {
            id: format!("order_{}", self.test_count),
            symbol: signal.symbol.clone(),
            side: match signal.action {
                SignalAction::Buy => "Buy".to_string(),
                SignalAction::Sell => "Sell".to_string(),
                SignalAction::Hold => "Buy".to_string(), // 預設
            },
            quantity: signal.quantity,
            price: signal.price,
            timestamp_us: get_timestamp_us(),
        }
    }
    
    /// 模擬執行
    async fn simulate_execution(&self, order: &SimpleOrder) -> SimpleExecution {
        // 模擬很小的執行延遲
        tokio::task::yield_now().await;
        
        SimpleExecution {
            order_id: order.id.clone(),
            symbol: order.symbol.clone(),
            executed_qty: order.quantity,
            executed_price: order.price,
            timestamp_us: get_timestamp_us(),
        }
    }
    
    /// 處理執行結果
    async fn process_execution_result(&mut self, _execution: &SimpleExecution) {
        self.test_count += 1;
        // 簡單的結果處理
        tokio::task::yield_now().await;
    }
    
    /// 計算各階段延遲
    fn calculate_latencies(&self, measurement: &mut LatencyMeasurement) {
        let stages = [
            LatencyStage::Start,
            LatencyStage::MarketDataReceived,
            LatencyStage::PriceCalculated,
            LatencyStage::SignalGenerated,
            LatencyStage::RiskChecked,
            LatencyStage::OrderCreated,
            LatencyStage::ExecutionCompleted,
            LatencyStage::ResultProcessed,
        ];
        
        for window in stages.windows(2) {
            let current = window[1];
            let previous = window[0];
            
            if let (Some(&current_time), Some(&prev_time)) = (
                measurement.timestamps.get(&current),
                measurement.timestamps.get(&previous)
            ) {
                let latency = current_time - prev_time;
                measurement.latencies.insert(current, latency);
            }
        }
    }
    
    /// 生成測試報告
    async fn generate_report(&self) {
        println!("\n🏁 === 簡化延遲測試報告 ===");
        println!("📊 測試概要:");
        println!("   測試符號: {}", self.symbol);
        println!("   測試數量: {}", self.results.len());
        
        if self.results.is_empty() {
            println!("⚠️ 沒有測試結果");
            return;
        }
        
        // 總延遲統計
        let mut total_stats = LatencyStats::new();
        for result in &self.results {
            total_stats.add_sample(result.total_latency_us);
        }
        total_stats.calculate();
        
        println!("\n⚡ 總延遲統計:");
        println!("   最小延遲: {}μs", total_stats.min);
        println!("   最大延遲: {}μs", total_stats.max);
        println!("   平均延遲: {:.1}μs", total_stats.avg);
        println!("   P95延遲: {}μs", total_stats.p95);
        println!("   P99延遲: {}μs", total_stats.p99);
        
        // 各階段延遲統計
        println!("\n🎯 各階段延遲統計:");
        
        let stages_with_names = [
            (LatencyStage::MarketDataReceived, "市場數據接收"),
            (LatencyStage::PriceCalculated, "價格計算"),
            (LatencyStage::SignalGenerated, "信號生成"),
            (LatencyStage::RiskChecked, "風險檢查"),
            (LatencyStage::OrderCreated, "訂單創建"),
            (LatencyStage::ExecutionCompleted, "執行完成"),
            (LatencyStage::ResultProcessed, "結果處理"),
        ];
        
        for (stage, name) in stages_with_names {
            let mut stage_stats = LatencyStats::new();
            for result in &self.results {
                if let Some(&latency) = result.latencies.get(&stage) {
                    stage_stats.add_sample(latency);
                }
            }
            
            if !stage_stats.samples.is_empty() {
                stage_stats.calculate();
                println!("   {}: 平均 {:.1}μs, P95 {}μs, P99 {}μs", 
                      name, stage_stats.avg, stage_stats.p95, stage_stats.p99);
            }
        }
        
        // 性能評估
        println!("\n📈 性能評估:");
        if total_stats.p99 < 100 {
            println!("   🟢 優秀: P99延遲 < 100μs (微秒級)");
        } else if total_stats.p99 < 1000 {
            println!("   🟡 良好: P99延遲 < 1ms");
        } else {
            println!("   🔴 需要優化: P99延遲 >= 1ms");
        }
        
        // 吞吐量計算
        let avg_latency_ms = total_stats.avg / 1000.0;
        let theoretical_tps = 1000.0 / avg_latency_ms;
        println!("   📊 理論吞吐量: {:.0} TPS", theoretical_tps);
        
        println!("\n✅ 簡化延遲測試完成！");
    }
}

/// 獲取微秒級時間戳
fn get_timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 啟動簡化延遲測試程序");

    // 創建測試器
    let mut tester = SimpleLatencyTester::new("BTCUSDT".to_string());
    
    println!("📋 測試配置:");
    println!("   測試符號: BTCUSDT");
    println!("   測試輪數: 1,000,000");
    println!("   測試模式: 微秒級核心延遲測量");
    println!("   測試組件: 市場數據 -> 價格計算 -> 信號生成 -> 風險檢查 -> 訂單創建 -> 執行完成");
    
    let start_time = std::time::Instant::now();
    tester.run_test(1_000_000).await?;
    let total_time = start_time.elapsed();
    
    println!("⏱️ 總測試時間: {:.2}秒", total_time.as_secs_f64());
    println!("🔄 平均每輪測試時間: {:.1}μs", 
          total_time.as_micros() as f64 / 1_000_000.0);

    println!("🎉 簡化延遲測試程序結束");
    Ok(())
}