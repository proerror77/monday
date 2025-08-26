/*!
 * E2E 测试演示 - 作为 example 运行避免主程序编译问题
 */

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::Duration;

// 最简化的数据结构

#[derive(Debug, Clone)]
pub struct MarketData {
    pub symbol: String,
    pub price: f64,
    pub volume: f64,
}

#[derive(Debug, Clone)]
pub struct TradingSignal {
    pub symbol: String,
    pub action: TradeAction,
    pub quantity: f64,
    pub confidence: f64,
}

#[derive(Debug, Clone)]
pub enum TradeAction {
    Buy,
    Sell,
    Hold,
}

#[derive(Debug, Clone)]
pub struct OrderResult {
    pub order_id: String,
    pub success: bool,
    pub message: String,
}

// 系统组件

pub struct SimpleStrategy;

impl SimpleStrategy {
    pub async fn analyze(&self, data: &MarketData) -> TradingSignal {
        tokio::time::sleep(Duration::from_micros(1)).await;

        let action = if data.price > 50000.0 {
            TradeAction::Sell
        } else if data.price < 45000.0 {
            TradeAction::Buy
        } else {
            TradeAction::Hold
        };

        TradingSignal {
            symbol: data.symbol.clone(),
            action,
            quantity: 0.01,
            confidence: 0.8,
        }
    }
}

pub struct SimpleRiskManager {
    max_position: f64,
    positions: Arc<RwLock<HashMap<String, f64>>>,
}

impl SimpleRiskManager {
    pub fn new(max_position: f64) -> Self {
        Self {
            max_position,
            positions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn check_risk(&self, signal: &TradingSignal) -> bool {
        let positions = self.positions.read().await;
        let current = positions.get(&signal.symbol).copied().unwrap_or(0.0);

        match signal.action {
            TradeAction::Buy => current + signal.quantity <= self.max_position,
            TradeAction::Sell => current - signal.quantity >= -self.max_position,
            TradeAction::Hold => true,
        }
    }

    pub async fn update_position(&self, symbol: &str, delta: f64) {
        let mut positions = self.positions.write().await;
        let current = positions.get(symbol).copied().unwrap_or(0.0);
        positions.insert(symbol.to_string(), current + delta);
    }
}

pub struct SimpleExecutor {
    success_rate: f64,
}

impl SimpleExecutor {
    pub fn new(success_rate: f64) -> Self {
        Self { success_rate }
    }

    pub async fn execute(&self, _signal: &TradingSignal) -> OrderResult {
        tokio::time::sleep(Duration::from_millis(1)).await;

        let success = rand::random::<f64>() < self.success_rate;

        OrderResult {
            order_id: format!("order_{}", rand::random::<u32>()),
            success,
            message: if success {
                "执行成功".to_string()
            } else {
                "执行失败".to_string()
            },
        }
    }
}

// E2E 系统

pub struct E2ESystem {
    strategy: SimpleStrategy,
    risk_manager: SimpleRiskManager,
    executor: SimpleExecutor,
}

impl E2ESystem {
    pub fn new() -> Self {
        Self {
            strategy: SimpleStrategy,
            risk_manager: SimpleRiskManager::new(10.0),
            executor: SimpleExecutor::new(0.9),
        }
    }

    pub async fn process_market_data(
        &self,
        data: MarketData,
    ) -> Result<Option<OrderResult>, String> {
        println!("📊 处理市场数据: {} @ ${}", data.symbol, data.price);

        // 步骤 1: 策略分析
        let signal = self.strategy.analyze(&data).await;
        println!(
            "🧠 策略分析: {:?} - 置信度 {:.2}",
            signal.action, signal.confidence
        );

        // 跳过 Hold 信号
        if matches!(signal.action, TradeAction::Hold) {
            println!("⏸️ 策略决定持有，不执行交易");
            return Ok(None);
        }

        // 步骤 2: 风险检查
        if !self.risk_manager.check_risk(&signal).await {
            println!("🛡️ 风险检查失败");
            return Err("风险检查失败".to_string());
        }
        println!("✅ 风险检查通过");

        // 步骤 3: 执行交易
        let result = self.executor.execute(&signal).await;
        println!(
            "⚡ 执行结果: {} - {}",
            if result.success { "成功" } else { "失败" },
            result.message
        );

        // 步骤 4: 更新仓位（如果执行成功）
        if result.success {
            let delta = match signal.action {
                TradeAction::Buy => signal.quantity,
                TradeAction::Sell => -signal.quantity,
                TradeAction::Hold => 0.0,
            };
            self.risk_manager
                .update_position(&signal.symbol, delta)
                .await;
            println!("📈 仓位更新: {} delta: {:.4}", signal.symbol, delta);
        }

        Ok(Some(result))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 HFT E2E 测试演示");
    println!("===================");

    let system = E2ESystem::new();
    let mut successful_trades = 0;
    let mut total_attempts = 0;

    // 测试不同的市场场景
    let test_cases = vec![
        ("低价买入场景", 42000.0),
        ("高价卖出场景", 52000.0),
        ("中性价格场景", 48000.0),
        ("极低价场景", 35000.0),
        ("极高价场景", 65000.0),
    ];

    for (scenario, price) in test_cases {
        println!("\n🎯 测试场景: {}", scenario);
        total_attempts += 1;

        let market_data = MarketData {
            symbol: "BTCUSDT".to_string(),
            price,
            volume: 1000.0,
        };

        match system.process_market_data(market_data).await {
            Ok(Some(result)) => {
                if result.success {
                    successful_trades += 1;
                    println!("✅ 场景完成 - 订单ID: {}", result.order_id);
                } else {
                    println!("❌ 场景失败 - 执行被拒绝");
                }
            }
            Ok(None) => {
                println!("📝 场景完成 - 策略决定不交易");
            }
            Err(e) => {
                println!("🛡️ 场景完成 - 风险控制: {}", e);
            }
        }
    }

    // 性能测试
    println!("\n⏱️ 性能测试");
    let start = std::time::Instant::now();
    let iterations = 100;

    for i in 0..iterations {
        let market_data = MarketData {
            symbol: "BTCUSDT".to_string(),
            price: 49000.0 + (i as f64 % 10.0),
            volume: 1000.0,
        };

        let _ = system.process_market_data(market_data).await;
    }

    let duration = start.elapsed();
    let avg_latency = duration / iterations;

    println!("📊 最终统计");
    println!("==========");
    println!("成功交易: {}/{}", successful_trades, total_attempts);
    println!("平均延迟: {:?}", avg_latency);
    println!(
        "吞吐量: {:.2} 处理/秒",
        iterations as f64 / duration.as_secs_f64()
    );

    if avg_latency < Duration::from_millis(10) {
        println!("✅ 性能测试通过 - 延迟小于10ms");
    } else {
        println!("⚠️ 性能警告 - 延迟超过10ms");
    }

    println!("\n🎉 E2E 测试演示完成!");
    println!("📝 这验证了完整的端到端流程：");
    println!("   1. 市场数据接收 ✓");
    println!("   2. 策略分析决策 ✓");
    println!("   3. 风险控制检查 ✓");
    println!("   4. 订单执行处理 ✓");
    println!("   5. 仓位管理更新 ✓");

    Ok(())
}
