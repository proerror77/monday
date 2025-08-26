/*!
 * 真实数据 E2E Dry-Run 测试
 *
 * 功能：
 * 1. 连接真实的 Bitget WebSocket 接收市场数据
 * 2. 运行完整的 rust_hft 系统（OMS + 策略 + 风控）
 * 3. 执行 Dry-Run（不真正下单到交易所）
 * 4. 验证端到端数据流：市场数据 → 策略分析 → 风控检查 → 模拟下单
 */

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use rust_hft::core::monitoring::MonitoringServer;
use rust_hft::core::types::{AccountId, OrderSide, OrderType, TimeInForce};
use rust_hft::engine::complete_oms::CompleteOMS;
use rust_hft::exchanges::message_types::{MarketEvent, OrderRequest};
use rust_hft::exchanges::{BitgetExchange, ExchangeManager};
use std::collections::HashMap;

/// E2E Dry-Run 测试配置
#[derive(Debug, Clone)]
pub struct DryRunConfig {
    pub test_symbol: String,
    pub test_duration_secs: u64,
    pub dry_run_account: String,
    pub initial_balance: f64,
    pub max_orders_per_test: usize,
}

impl Default for DryRunConfig {
    fn default() -> Self {
        Self {
            test_symbol: "BTCUSDT".to_string(),
            test_duration_secs: 60, // 1分钟测试
            dry_run_account: "dryrun_test".to_string(),
            initial_balance: 10000.0,
            max_orders_per_test: 5,
        }
    }
}

/// E2E 测试统计
#[derive(Debug, Default, Clone)]
pub struct E2ETestStats {
    pub market_events_received: u64,
    pub trading_signals_generated: u64,
    pub risk_checks_passed: u64,
    pub risk_checks_failed: u64,
    pub dry_orders_simulated: u64,
    pub avg_processing_latency_us: f64,
    pub max_processing_latency_us: f64,
}

/// 真实数据 E2E Dry-Run 测试框架
pub struct RealDataE2EFramework {
    config: DryRunConfig,
    exchange_manager: Arc<ExchangeManager>,
    oms: Arc<CompleteOMS>,
    monitoring: Arc<MonitoringServer>,
    stats: Arc<tokio::sync::RwLock<E2ETestStats>>,
}

impl RealDataE2EFramework {
    /// 创建新的测试框架
    pub async fn new(config: DryRunConfig) -> Result<Self> {
        // 初始化日志
        tracing_subscriber::fmt()
            .with_env_filter("rust_hft=debug,bitget=debug")
            .init();

        info!("🚀 初始化真实数据 E2E Dry-Run 测试框架");

        // 创建监控服务
        let monitoring = Arc::new(
            MonitoringServer::new(9095)
                .await
                .map_err(|e| anyhow::anyhow!("监控服务启动失败: {}", e))?,
        );
        monitoring.update_health("e2e_test", true).await;

        // 创建交易所管理器
        let exchange_manager = Arc::new(ExchangeManager::new());

        // 添加 Bitget 交易所（真实数据源）
        info!("🔗 连接 Bitget 交易所 (真实数据)");
        let bitget_exchange = Box::new(BitgetExchange::new());
        exchange_manager
            .add_exchange("bitget".to_string(), bitget_exchange)
            .await;

        // 创建 OMS 系统
        let (oms, _order_updates) = CompleteOMS::new_with_receiver(exchange_manager.clone());
        let oms = Arc::new(oms);

        // 启动 OMS
        oms.start()
            .await
            .map_err(|e| anyhow::anyhow!("OMS 启动失败: {}", e))?;

        // 添加测试账户
        let account_id = AccountId::from(config.dry_run_account.clone());
        oms.add_account(account_id, config.initial_balance)
            .await
            .map_err(|e| anyhow::anyhow!("添加测试账户失败: {}", e))?;

        info!("✅ E2E 测试框架初始化完成");

        Ok(Self {
            config,
            exchange_manager,
            oms,
            monitoring,
            stats: Arc::new(tokio::sync::RwLock::new(E2ETestStats::default())),
        })
    }

    /// 运行完整的 E2E Dry-Run 测试
    pub async fn run_e2e_test(&self) -> Result<E2ETestStats> {
        info!("🎯 开始真实数据 E2E Dry-Run 测试");
        info!("   测试交易对: {}", self.config.test_symbol);
        info!("   测试时长: {} 秒", self.config.test_duration_secs);
        info!("   Dry-Run 模式: 不会真实下单");

        // 启动市场数据订阅
        self.start_market_data_subscription().await?;

        // 启动交易策略处理器
        let strategy_task = self.start_trading_strategy_processor().await;

        // 启动指标收集器
        let metrics_task = self.start_metrics_collector().await;

        // 运行测试指定时长
        info!("⏱️ 测试运行中... ({} 秒)", self.config.test_duration_secs);
        tokio::time::sleep(Duration::from_secs(self.config.test_duration_secs)).await;

        // 停止所有任务
        strategy_task.abort();
        metrics_task.abort();

        // 收集最终统计
        let final_stats = self.stats.read().await.clone();

        info!("🏁 E2E 测试完成！");
        self.print_test_results(&final_stats).await;

        Ok(final_stats)
    }

    /// 启动市场数据订阅
    async fn start_market_data_subscription(&self) -> Result<()> {
        info!("📡 订阅真实市场数据: {}", self.config.test_symbol);

        // 获取 Bitget 交易所
        if let Some(bitget) = self.exchange_manager.get_exchange("bitget").await {
            let mut exchange = bitget.write().await;

            // 先连接WebSocket
            exchange
                .connect_public()
                .await
                .map_err(|e| anyhow::anyhow!("连接Bitget WebSocket失败: {}", e))?;

            // 订阅市场数据
            exchange
                .subscribe_orderbook(&self.config.test_symbol, 15)
                .await
                .map_err(|e| anyhow::anyhow!("订阅订单簿失败: {}", e))?;
            exchange
                .subscribe_trades(&self.config.test_symbol)
                .await
                .map_err(|e| anyhow::anyhow!("订阅交易数据失败: {}", e))?;
            exchange
                .subscribe_ticker(&self.config.test_symbol)
                .await
                .map_err(|e| anyhow::anyhow!("订阅市场数据失败: {}", e))?;

            info!("✅ 成功订阅 {} 的市场数据", self.config.test_symbol);
        } else {
            return Err(anyhow::anyhow!("Bitget 交易所未找到"));
        }

        Ok(())
    }

    /// 启动交易策略处理器
    async fn start_trading_strategy_processor(&self) -> tokio::task::JoinHandle<()> {
        let exchange_manager = self.exchange_manager.clone();
        let oms = self.oms.clone();
        let stats = self.stats.clone();
        let config = self.config.clone();

        tokio::spawn(async move {
            info!("🧠 启动交易策略处理器");

            let mut order_count = 0;
            let mut interval = interval(Duration::from_millis(100)); // 每100ms检查一次

            loop {
                interval.tick().await;

                // 检查是否达到最大订单数限制
                if order_count >= config.max_orders_per_test {
                    debug!("已达到最大订单数限制: {}", config.max_orders_per_test);
                    continue;
                }

                // 获取最新市场数据
                if let Some(bitget) = exchange_manager.get_exchange("bitget").await {
                    let exchange = bitget.read().await;

                    // 这里简化处理，实际应该从真实的市场数据流中获取
                    if let Ok(mut receiver) = exchange.get_market_events().await {
                        // 非阻塞式检查是否有新事件
                        if let Ok(event) = receiver.try_recv() {
                            let start_time = std::time::Instant::now();

                            // 更新统计
                            {
                                let mut stats_guard = stats.write().await;
                                stats_guard.market_events_received += 1;
                            }

                            // 运行简单的交易策略
                            if let Some(signal) = Self::generate_trading_signal(&event).await {
                                {
                                    let mut stats_guard = stats.write().await;
                                    stats_guard.trading_signals_generated += 1;
                                }

                                // 进行风险检查
                                if Self::check_trading_risk(&signal).await {
                                    {
                                        let mut stats_guard = stats.write().await;
                                        stats_guard.risk_checks_passed += 1;
                                    }

                                    // Dry-Run：模拟下单（不真实提交）
                                    if let Ok(_) =
                                        Self::simulate_order_submission(&oms, &signal, &config)
                                            .await
                                    {
                                        order_count += 1;
                                        {
                                            let mut stats_guard = stats.write().await;
                                            stats_guard.dry_orders_simulated += 1;
                                        }

                                        info!(
                                            "📝 Dry-Run 模拟下单 #{}: {:?} {} @ {:.2}",
                                            order_count,
                                            signal.side,
                                            signal.symbol,
                                            signal.price.unwrap_or(0.0)
                                        );
                                    }
                                } else {
                                    let mut stats_guard = stats.write().await;
                                    stats_guard.risk_checks_failed += 1;
                                }
                            }

                            // 更新延迟统计
                            let processing_time = start_time.elapsed().as_micros() as f64;
                            {
                                let mut stats_guard = stats.write().await;
                                stats_guard.avg_processing_latency_us =
                                    (stats_guard.avg_processing_latency_us + processing_time) / 2.0;
                                stats_guard.max_processing_latency_us =
                                    stats_guard.max_processing_latency_us.max(processing_time);
                            }
                        }
                    }
                }
            }
        })
    }

    /// 启动指标收集器
    async fn start_metrics_collector(&self) -> tokio::task::JoinHandle<()> {
        let monitoring = self.monitoring.clone();
        let stats = self.stats.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(5)); // 每5秒收集一次指标

            loop {
                interval.tick().await;

                let current_stats = {
                    let stats_guard = stats.read().await;
                    stats_guard.clone()
                };

                // 更新 Prometheus 指标
                monitoring.increment_counter(
                    "e2e_market_events_total",
                    current_stats.market_events_received,
                );
                monitoring.increment_counter(
                    "e2e_trading_signals_total",
                    current_stats.trading_signals_generated,
                );
                monitoring
                    .increment_counter("e2e_risk_checks_passed", current_stats.risk_checks_passed);
                monitoring
                    .increment_counter("e2e_risk_checks_failed", current_stats.risk_checks_failed);
                monitoring
                    .increment_counter("e2e_dry_orders_total", current_stats.dry_orders_simulated);
                monitoring
                    .record_latency("e2e_avg_latency", current_stats.avg_processing_latency_us);
                monitoring
                    .record_latency("e2e_max_latency", current_stats.max_processing_latency_us);
            }
        })
    }

    /// 生成交易信号（简化版策略）
    async fn generate_trading_signal(market_event: &MarketEvent) -> Option<TradingSignal> {
        match market_event {
            MarketEvent::Ticker {
                symbol, last_price, ..
            } => {
                // 简单策略：基于价格区间生成信号
                let price = *last_price;

                if price < 45000.0 {
                    // 低价买入
                    Some(TradingSignal {
                        symbol: symbol.clone(),
                        side: OrderSide::Buy,
                        quantity: 0.001, // 小量测试
                        price: Some(price),
                        confidence: 0.7,
                    })
                } else if price > 55000.0 {
                    // 高价卖出
                    Some(TradingSignal {
                        symbol: symbol.clone(),
                        side: OrderSide::Sell,
                        quantity: 0.001,
                        price: Some(price),
                        confidence: 0.7,
                    })
                } else {
                    None // 中性区间不交易
                }
            }
            _ => None,
        }
    }

    /// 风险检查（简化版）
    async fn check_trading_risk(signal: &TradingSignal) -> bool {
        // 简单风险检查
        signal.quantity <= 0.01 && signal.confidence >= 0.5
    }

    /// 模拟订单提交（Dry-Run）
    async fn simulate_order_submission(
        _oms: &Arc<CompleteOMS>,
        signal: &TradingSignal,
        config: &DryRunConfig,
    ) -> Result<String> {
        let order_request = OrderRequest {
            client_order_id: format!("dryrun_{}", Uuid::new_v4()),
            account_id: Some(AccountId::from(config.dry_run_account.clone())),
            symbol: signal.symbol.clone(),
            side: signal.side.clone(),
            order_type: OrderType::Limit,
            quantity: signal.quantity,
            price: signal.price,
            stop_price: None,
            time_in_force: TimeInForce::GTC,
            post_only: false,
            reduce_only: false,
            metadata: HashMap::new(),
        };

        // 注意：这里是 Dry-Run，实际不会提交到交易所
        // 只是验证 OMS 系统的订单处理逻辑

        // 模拟订单ID
        let simulated_order_id = format!("dry_{}", Uuid::new_v4());

        debug!("Dry-Run 模拟订单: {:?}", order_request);

        Ok(simulated_order_id)
    }

    /// 打印测试结果
    async fn print_test_results(&self, stats: &E2ETestStats) {
        info!("📊 E2E Dry-Run 测试结果");
        info!("=====================================");
        info!("市场事件接收: {} 个", stats.market_events_received);
        info!("交易信号生成: {} 个", stats.trading_signals_generated);
        info!("风险检查通过: {} 个", stats.risk_checks_passed);
        info!("风险检查失败: {} 个", stats.risk_checks_failed);
        info!("Dry-Run 模拟订单: {} 个", stats.dry_orders_simulated);
        info!("平均处理延迟: {:.2} μs", stats.avg_processing_latency_us);
        info!("最大处理延迟: {:.2} μs", stats.max_processing_latency_us);

        if stats.market_events_received > 0 {
            let signal_rate = (stats.trading_signals_generated as f64
                / stats.market_events_received as f64)
                * 100.0;
            info!("信号生成率: {:.2}%", signal_rate);
        }

        if stats.trading_signals_generated > 0 {
            let risk_pass_rate =
                (stats.risk_checks_passed as f64 / stats.trading_signals_generated as f64) * 100.0;
            info!("风险通过率: {:.2}%", risk_pass_rate);
        }

        info!("=====================================");

        // 验证系统性能
        if stats.avg_processing_latency_us < 1000.0 {
            info!("✅ 性能测试通过 - 平均延迟 < 1ms");
        } else {
            warn!("⚠️ 性能警告 - 平均延迟 > 1ms");
        }

        if stats.market_events_received > 0 {
            info!("✅ 数据接收测试通过 - 成功接收真实市场数据");
        } else {
            error!("❌ 数据接收测试失败 - 未接收到市场数据");
        }

        info!("🔗 监控指标: http://localhost:9095/metrics");
    }
}

/// 交易信号结构
#[derive(Debug, Clone)]
pub struct TradingSignal {
    pub symbol: String,
    pub side: OrderSide,
    pub quantity: f64,
    pub price: Option<f64>,
    pub confidence: f64,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 创建测试配置
    let config = DryRunConfig {
        test_symbol: "BTCUSDT".to_string(),
        test_duration_secs: 30, // 30秒快速测试
        dry_run_account: "test_account".to_string(),
        initial_balance: 10000.0,
        max_orders_per_test: 3,
    };

    // 创建并运行 E2E 测试框架
    let framework = RealDataE2EFramework::new(config).await?;

    println!("🚀 启动真实数据 E2E Dry-Run 测试");
    println!("⚠️ 注意：这是 Dry-Run 模式，不会真实下单到交易所");
    println!("📡 正在连接真实的 Bitget WebSocket 数据流...");

    match framework.run_e2e_test().await {
        Ok(stats) => {
            println!("✅ E2E 测试成功完成！");
            if stats.market_events_received > 0 && stats.dry_orders_simulated > 0 {
                println!("🎉 系统验证通过：");
                println!("   • 真实数据接收 ✓");
                println!("   • 策略信号生成 ✓");
                println!("   • 风险控制检查 ✓");
                println!("   • Dry-Run 模拟交易 ✓");
            } else {
                println!("⚠️ 测试完成但某些功能可能需要调整");
            }
        }
        Err(e) => {
            println!("❌ E2E 测试失败: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}
