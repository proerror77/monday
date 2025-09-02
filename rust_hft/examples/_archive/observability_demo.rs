/*! 
 * 完整可观测性系统演示
 * 
 * 展示 HFT 系统中指标收集、分布式追踪、结构化日志和实时告警的集成使用
 */

use hft_observability::{
    ObservabilityConfig, ObservabilityHub, Observable, 
    ComponentHealth, ComponentMetrics, HealthStatus, HealthMetrics,
    measure_execution, record_business_metric, trace_operation,
    init_observability, shutdown_observability,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn, error};

/// 模拟的 HFT 交易引擎组件
struct MockTradingEngine {
    name: String,
    order_count: std::sync::atomic::AtomicU64,
    total_pnl: std::sync::atomic::AtomicU64, // 以分为单位
    is_healthy: std::sync::atomic::AtomicBool,
}

impl MockTradingEngine {
    fn new(name: String) -> Self {
        Self {
            name,
            order_count: std::sync::atomic::AtomicU64::new(0),
            total_pnl: std::sync::atomic::AtomicU64::new(100000000), // $1M 初始资金
            is_healthy: std::sync::atomic::AtomicBool::new(true),
        }
    }
    
    /// 模拟下单操作
    async fn place_order(&self, symbol: &str, quantity: f64, price: f64) -> Result<String, String> {
        // 使用可观测性宏追踪操作
        trace_operation!("place_order", {
            let latency_result = measure_execution!("order_placement", {
                // 模拟下单延迟
                sleep(Duration::from_micros(10 + rand::random::<u64>() % 40)).await;
                
                // 模拟偶发错误
                if rand::random::<f64>() < 0.02 {
                    return Err("Market closed".to_string());
                }
                
                let order_id = format!("ORDER_{}", uuid::Uuid::new_v4());
                self.order_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                
                // 模拟 PnL 变化
                let pnl_change = (rand::random::<f64>() - 0.5) * 1000.0; // +/- $5
                let current_pnl = self.total_pnl.load(std::sync::atomic::Ordering::Relaxed) as f64;
                let new_pnl = current_pnl + pnl_change;
                self.total_pnl.store(new_pnl as u64, std::sync::atomic::Ordering::Relaxed);
                
                Ok(order_id)
            });
            
            // 记录业务指标
            record_business_metric!("order_fill_rate", if latency_result.is_ok() { 100.0 } else { 0.0 });
            record_business_metric!("position_count", self.order_count.load(std::sync::atomic::Ordering::Relaxed) as f64);
            record_business_metric!("pnl_realtime", self.total_pnl.load(std::sync::atomic::Ordering::Relaxed) as f64 / 100.0);
            
            latency_result
        })
    }
    
    /// 模拟市场数据处理
    async fn process_market_data(&self, symbol: &str, price: f64) {
        trace_operation!("process_market_data", {
            measure_execution!("market_data_processing", {
                // 模拟市场数据处理延迟
                sleep(Duration::from_micros(5 + rand::random::<u64>() % 15)).await;
                
                // 模拟偶发延迟告警
                if rand::random::<f64>() < 0.05 {
                    warn!("Market data processing slow for {}: {}", symbol, price);
                }
                
                Ok(())
            })
        });
    }
    
    /// 模拟风险检查
    async fn risk_check(&self) -> bool {
        measure_execution!("risk_check", {
            sleep(Duration::from_micros(2 + rand::random::<u64>() % 8)).await;
            
            let current_pnl = self.total_pnl.load(std::sync::atomic::Ordering::Relaxed) as f64 / 100.0;
            let max_drawdown = 50000.0; // $50K 最大回撤
            let initial_capital = 1000000.0; // $1M
            
            let drawdown = initial_capital - current_pnl;
            if drawdown > max_drawdown {
                self.is_healthy.store(false, std::sync::atomic::Ordering::Relaxed);
                error!("Risk limit exceeded! Drawdown: ${:.2}", drawdown);
                return false;
            }
            
            true
        })
    }
}

#[async_trait]
impl Observable for MockTradingEngine {
    fn component_name(&self) -> &str {
        &self.name
    }
    
    async fn init_observability(&mut self, _hub: Arc<ObservabilityHub>) -> hft_observability::ObservabilityResult<()> {
        info!("Initializing observability for {}", self.name);
        Ok(())
    }
    
    async fn health_check(&self) -> hft_observability::ObservabilityResult<ComponentHealth> {
        let is_healthy = self.is_healthy.load(std::sync::atomic::Ordering::Relaxed);
        let order_count = self.order_count.load(std::sync::atomic::Ordering::Relaxed);
        let current_pnl = self.total_pnl.load(std::sync::atomic::Ordering::Relaxed) as f64 / 100.0;
        
        let status = if is_healthy {
            if current_pnl < 950000.0 { // -5% drawdown
                HealthStatus::Degraded
            } else {
                HealthStatus::Healthy
            }
        } else {
            HealthStatus::Unhealthy
        };
        
        let message = format!(
            "Orders: {}, PnL: ${:.2}, Status: {:?}", 
            order_count, current_pnl, status
        );
        
        Ok(ComponentHealth {
            component: self.name.clone(),
            status,
            message,
            last_check: chrono::Utc::now(),
            metrics: HealthMetrics {
                uptime_seconds: 3600, // 假设运行 1 小时
                error_rate: if order_count > 0 { 0.02 } else { 0.0 },
                latency_p99_us: 45.0, // 模拟 p99 延迟
                memory_usage_mb: 512.0,
                cpu_usage_pct: 25.0,
            },
        })
    }
    
    async fn get_metrics(&self) -> hft_observability::ObservabilityResult<ComponentMetrics> {
        let mut counters = HashMap::new();
        let mut gauges = HashMap::new();
        
        counters.insert("total_orders".to_string(), 
                       self.order_count.load(std::sync::atomic::Ordering::Relaxed));
        
        gauges.insert("current_pnl_usd".to_string(), 
                     self.total_pnl.load(std::sync::atomic::Ordering::Relaxed) as f64 / 100.0);
        gauges.insert("health_status".to_string(), 
                     if self.is_healthy.load(std::sync::atomic::Ordering::Relaxed) { 1.0 } else { 0.0 });
        
        Ok(ComponentMetrics {
            component: self.name.clone(),
            timestamp: chrono::Utc::now(),
            counters,
            gauges,
            histograms: HashMap::new(),
        })
    }
    
    async fn shutdown_observability(&mut self) -> hft_observability::ObservabilityResult<()> {
        info!("Shutting down observability for {}", self.name);
        Ok(())
    }
}

/// 模拟市场数据生成器
async fn market_data_generator(engine: Arc<MockTradingEngine>) {
    let symbols = vec!["BTCUSDT", "ETHUSDT", "ADAUSDT"];
    let mut prices = HashMap::new();
    
    // 初始化价格
    prices.insert("BTCUSDT", 50000.0);
    prices.insert("ETHUSDT", 3000.0);
    prices.insert("ADAUSDT", 1.0);
    
    loop {
        for symbol in &symbols {
            let current_price = prices.get_mut(symbol).unwrap();
            
            // 模拟价格波动
            let change_pct = (rand::random::<f64>() - 0.5) * 0.001; // +/- 0.1%
            *current_price *= 1.0 + change_pct;
            
            // 处理市场数据
            engine.process_market_data(symbol, *current_price).await;
            
            // 模拟下单决策
            if rand::random::<f64>() < 0.3 {
                let quantity = 0.01 + rand::random::<f64>() * 0.09; // 0.01-0.1
                match engine.place_order(symbol, quantity, *current_price).await {
                    Ok(order_id) => {
                        info!("Order placed: {} for {} @ {:.2}", order_id, symbol, current_price);
                    }
                    Err(e) => {
                        warn!("Order failed: {}", e);
                    }
                }
            }
        }
        
        // 执行风险检查
        if !engine.risk_check().await {
            error!("Risk check failed - stopping trading");
            break;
        }
        
        sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    info!("🚀 启动 HFT 可观测性系统演示");
    
    // 创建可观测性配置
    let config = ObservabilityConfig {
        service_name: "hft-observability-demo".to_string(),
        environment: "demo".to_string(),
        version: "1.0.0".to_string(),
        metrics: hft_observability::config::MetricsConfig {
            enabled: true,
            collection_interval_sec: 1,
            export_interval_sec: 5,
            port: 9091, // 避免冲突
            enable_business_metrics: true,
            latency_buckets: vec![1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0],
            ..Default::default()
        },
        tracing: hft_observability::config::TracingConfig {
            enabled: true,
            sampling_rate: 1.0, // 100% 采样用于演示
            enable_performance_tracing: true,
            ..Default::default()
        },
        logging: hft_observability::config::LoggingConfig {
            enabled: true,
            level: hft_observability::config::LogLevel::Info,
            enable_console: true,
            enable_correlation_ids: true,
            ..Default::default()
        },
        dashboard: hft_observability::config::DashboardConfig {
            enable_dashboards: true,
            port: 3001, // 避免冲突
            update_interval_sec: 2,
            ..Default::default()
        },
        alerting: hft_observability::config::AlertingConfig {
            enable_alerting: true,
            evaluation_interval_sec: 5,
            max_alerts_per_minute: 10,
            ..Default::default()
        },
    };
    
    // 初始化可观测性中心
    let hub = init_observability(config).await?;
    
    // 启动可观测性系统
    hub.start().await?;
    
    info!("📊 可观测性系统已启动");
    
    // 创建模拟交易引擎
    let mut engine = MockTradingEngine::new("primary-trading-engine".to_string());
    
    // 注册组件到可观测性中心
    hub.register_component(engine.clone()).await?;
    
    info!("🏭 交易引擎已注册到可观测性系统");
    
    // 启动市场数据生成器
    let engine_arc = Arc::new(engine);
    let data_task = tokio::spawn(market_data_generator(Arc::clone(&engine_arc)));
    
    // 启动状态监控任务
    let hub_clone = hub.clone();
    let monitor_task = tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(10)).await;
            
            // 获取系统统计
            let hub_stats = hub_clone.get_hub_stats().await;
            info!(
                "📈 系统状态: {}/{} 组件健康, 采集耗时: {:.2}μs",
                hub_stats.healthy_components,
                hub_stats.total_components,
                hub_stats.collection_stats.avg_collection_time_us
            );
            
            // 获取组件健康状态
            let all_health = hub_clone.get_all_health().await;
            for (component, health) in all_health {
                info!(
                    "🔍 {}: {:?} - {}",
                    component, health.status, health.message
                );
            }
            
            // 模拟指标查询
            let metrics_snapshot = hub_clone.record_execution_latency("demo_operation", 15.5).await;
            hub_clone.record_business_metric("demo_revenue", 12345.67).await;
        }
    });
    
    // 演示运行时间
    info!("⏰ 演示将运行 60 秒...");
    sleep(Duration::from_secs(60)).await;
    
    // 停止任务
    data_task.abort();
    monitor_task.abort();
    
    info!("🛑 停止可观测性系统演示");
    
    // 获取最终报告
    let final_stats = hub.get_hub_stats().await;
    info!("📋 最终统计:");
    info!("   • 总采集次数: {}", final_stats.collection_stats.total_collections);
    info!("   • 成功采集: {}", final_stats.collection_stats.successful_collections);
    info!("   • 平均延迟: {:.2}μs", final_stats.collection_stats.avg_collection_time_us);
    info!("   • 系统运行时间: {:.2}s", final_stats.uptime.as_secs_f64());
    
    // 导出最终指标 (Prometheus 格式)
    if let Ok(prometheus_metrics) = hub.export_prometheus().await {
        info!("📊 Prometheus 指标导出:");
        println!("{}", prometheus_metrics);
    }
    
    // 关闭可观测性系统
    shutdown_observability().await?;
    
    info!("✅ HFT 可观测性系统演示完成");
    
    Ok(())
}

// 模拟依赖
mod rand {
    pub fn random<T>() -> T 
    where 
        T: From<u8>
    {
        T::from(42) // 简单的模拟实现
    }
}

mod uuid {
    pub struct Uuid;
    impl Uuid {
        pub fn new_v4() -> Self {
            Self
        }
    }
    impl std::fmt::Display for Uuid {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "550e8400-e29b-41d4-a716-446655440000")
        }
    }
}
// Archived legacy example
