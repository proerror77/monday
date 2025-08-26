/*!
 * E2E延遲測試 - 端到端性能測量
 * 
 * 測量完整交易流程的延遲：
 * 🕐 本地時間戳捕獲
 * 📊 市場數據接收和處理  
 * 🧠 策略計算時間
 * 📤 訂單生成和提交
 * 📨 訂單執行和回報接收
 * 
 * 目標：識別每個階段的延遲瓶頸，確保微秒級性能
 */

use rust_hft::core::types::*;
use rust_hft::core::orderbook::OrderBook;
use rust_hft::exchanges::message_types::{ExecutionReport, OrderRequest, MarketEvent};
use rust_hft::exchanges::{bitget::BitgetExchange, binance::BinanceExchange, ExchangeTrait};
use rust_hft::engine::{
    UnifiedTradingEngine, UnifiedEngineConfig, EngineMode, RiskConfig, 
    ExecutionConfig, PerformanceConfig, TradingStrategy, Signal, SignalAction,
    StrategyState, EnhancedRiskManager, EnhancedRiskConfig,
};
use rust_hft::security::rate_limiting::EnhancedRateLimiter;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, RwLock, Mutex};
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error, debug};
use serde::{Serialize, Deserialize};

/// E2E延遲測量結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2ELatencyMeasurement {
    /// 測試ID
    pub test_id: String,
    /// 測試開始時間戳（微秒）
    pub start_timestamp_us: u64,
    /// 各階段時間戳
    pub stage_timestamps: HashMap<LatencyStage, u64>,
    /// 各階段延遲（微秒）
    pub stage_latencies: HashMap<LatencyStage, u64>,
    /// 總延遲（微秒）
    pub total_latency_us: u64,
    /// 測試配置
    pub config: TestConfig,
    /// 額外元數據
    pub metadata: HashMap<String, String>,
}

/// 延遲測量階段
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LatencyStage {
    /// T0: 本地時間戳捕獲
    LocalTimestamp,
    /// T1: 市場數據接收
    MarketDataReceived,
    /// T2: 訂單簿更新完成
    OrderBookUpdated,
    /// T3: 策略計算開始
    StrategyCalculationStart,
    /// T4: 策略計算完成
    StrategyCalculationEnd,
    /// T5: 風險檢查開始
    RiskCheckStart,
    /// T6: 風險檢查完成
    RiskCheckEnd,
    /// T7: 訂單生成完成
    OrderGenerated,
    /// T8: 訂單提交至交易所
    OrderSubmitted,
    /// T9: 交易所確認接收
    ExchangeAcknowledged,
    /// T10: 執行回報接收
    ExecutionReportReceived,
    /// T11: 持倉更新完成
    PositionUpdated,
}

/// 測試配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestConfig {
    /// 測試符號
    pub symbol: String,
    /// 測試交易所
    pub exchange: String,
    /// 測試輪數
    pub test_rounds: usize,
    /// 測試間隔（毫秒）
    pub test_interval_ms: u64,
    /// 是否啟用詳細日誌
    pub verbose_logging: bool,
    /// 延遲目標（微秒）
    pub latency_targets: HashMap<LatencyStage, u64>,
}

impl Default for TestConfig {
    fn default() -> Self {
        let mut targets = HashMap::new();
        targets.insert(LatencyStage::MarketDataReceived, 50);      // 50μs
        targets.insert(LatencyStage::OrderBookUpdated, 100);       // 100μs
        targets.insert(LatencyStage::StrategyCalculationEnd, 200); // 200μs  
        targets.insert(LatencyStage::RiskCheckEnd, 50);            // 50μs
        targets.insert(LatencyStage::OrderGenerated, 25);          // 25μs
        targets.insert(LatencyStage::OrderSubmitted, 500);         // 500μs
        targets.insert(LatencyStage::ExecutionReportReceived, 2000); // 2ms
        targets.insert(LatencyStage::PositionUpdated, 100);        // 100μs
        
        Self {
            symbol: "BTCUSDT".to_string(),
            exchange: "bitget".to_string(),
            test_rounds: 100,
            test_interval_ms: 100,
            verbose_logging: false,
            latency_targets: targets,
        }
    }
}

/// E2E延遲測試器
pub struct E2ELatencyTester {
    /// 測試配置
    config: TestConfig,
    /// 統一交易引擎
    trading_engine: Arc<Mutex<UnifiedTradingEngine>>,
    /// 風險管理器
    risk_manager: Arc<EnhancedRiskManager>,
    /// 訂單簿
    orderbook: Arc<RwLock<OrderBook>>,
    /// 延遲統計
    latency_stats: Arc<RwLock<LatencyStatistics>>,
    /// 測試結果
    test_results: Arc<RwLock<Vec<E2ELatencyMeasurement>>>,
    /// 原子計數器
    test_counter: AtomicU64,
}

/// 延遲統計
#[derive(Debug, Default)]
pub struct LatencyStatistics {
    /// 各階段延遲統計
    pub stage_stats: HashMap<LatencyStage, StageStatistics>,
    /// 總延遲統計
    pub total_latency_stats: StageStatistics,
    /// 測試總數
    pub total_tests: u64,
    /// 成功測試數
    pub successful_tests: u64,
    /// 失敗測試數
    pub failed_tests: u64,
}

/// 階段統計
#[derive(Debug, Default, Clone)]
pub struct StageStatistics {
    /// 最小延遲（微秒）
    pub min_latency_us: u64,
    /// 最大延遲（微秒）
    pub max_latency_us: u64,
    /// 平均延遲（微秒）
    pub avg_latency_us: f64,
    /// P50延遲（微秒）
    pub p50_latency_us: u64,
    /// P95延遲（微秒）
    pub p95_latency_us: u64,
    /// P99延遲（微秒）
    pub p99_latency_us: u64,
    /// 樣本數
    pub sample_count: u64,
    /// 超標次數
    pub exceeded_target: u64,
}

/// 測試策略 - 專門用於延遲測試
pub struct LatencyTestStrategy {
    /// 策略ID
    strategy_id: String,
    /// 延遲測量器
    latency_tracker: Arc<RwLock<HashMap<String, E2ELatencyMeasurement>>>,
    /// 信號生成計數器
    signal_counter: AtomicU64,
}

impl LatencyTestStrategy {
    pub fn new() -> Self {
        Self {
            strategy_id: "latency_test_strategy".to_string(),
            latency_tracker: Arc::new(RwLock::new(HashMap::new())),
            signal_counter: AtomicU64::new(0),
        }
    }
    
    pub async fn get_latest_measurement(&self, test_id: &str) -> Option<E2ELatencyMeasurement> {
        let tracker = self.latency_tracker.read().await;
        tracker.get(test_id).cloned()
    }
    
    pub async fn update_stage_timestamp(&self, test_id: &str, stage: LatencyStage, timestamp_us: u64) {
        let mut tracker = self.latency_tracker.write().await;
        if let Some(measurement) = tracker.get_mut(test_id) {
            measurement.stage_timestamps.insert(stage, timestamp_us);
        }
    }
}

#[async_trait::async_trait]
impl TradingStrategy for LatencyTestStrategy {
    async fn generate_signals(
        &self,
        market_data: &rust_hft::engine::MarketData,
        _portfolio: &rust_hft::engine::Portfolio,
        _strategy_state: &mut StrategyState,
    ) -> Result<Vec<Signal>, anyhow::Error> {
        let timestamp_us = get_timestamp_us();
        
        // 生成測試信號
        let signal_id = self.signal_counter.fetch_add(1, Ordering::SeqCst);
        let test_id = format!("test_{}", signal_id);
        
        // 創建新的延遲測量
        let mut measurement = E2ELatencyMeasurement {
            test_id: test_id.clone(),
            start_timestamp_us: timestamp_us,
            stage_timestamps: HashMap::new(),
            stage_latencies: HashMap::new(),
            total_latency_us: 0,
            config: TestConfig::default(),
            metadata: HashMap::new(),
        };
        
        // 記錄策略計算開始時間
        measurement.stage_timestamps.insert(LatencyStage::StrategyCalculationStart, timestamp_us);
        
        // 模擬策略計算（應該非常快）
        let calculation_start = Instant::now();
        
        // 基於市場數據生成信號
        let signal = Signal {
            symbol: market_data.symbol.clone(),
            action: SignalAction::Buy,
            quantity: 0.01, // 小額測試
            price: market_data.price,
            confidence: 0.8,
            metadata: {
                let mut meta = HashMap::new();
                meta.insert("test_id".to_string(), test_id.clone());
                meta.insert("signal_type".to_string(), "latency_test".to_string());
                meta
            },
        };
        
        let calculation_end_us = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::StrategyCalculationEnd, calculation_end_us);
        
        // 計算策略延遲
        let strategy_latency = calculation_end_us - timestamp_us;
        measurement.stage_latencies.insert(LatencyStage::StrategyCalculationEnd, strategy_latency);
        
        // 存儲測量結果
        {
            let mut tracker = self.latency_tracker.write().await;
            tracker.insert(test_id, measurement);
        }
        
        Ok(vec![signal])
    }

    fn get_strategy_name(&self) -> String {
        self.strategy_id.clone()
    }
}

impl E2ELatencyTester {
    /// 創建新的E2E延遲測試器
    pub async fn new(config: TestConfig) -> Result<Self, anyhow::Error> {
        info!("🚀 初始化E2E延遲測試器...");
        
        // 創建測試策略
        let strategy = Box::new(LatencyTestStrategy::new());
        
        // 創建風險管理器配置
        let risk_config = EnhancedRiskConfig {
            max_portfolio_value: 100000.0,
            max_daily_loss: 1000.0,
            default_stop_loss_pct: 1.0,
            default_take_profit_pct: 2.0,
            trailing_stop_enabled: false,
            enable_dynamic_sizing: false,
            risk_check_interval_ms: 10, // 快速風險檢查
            ..Default::default()
        };
        
        // 創建風險管理器
        let (risk_manager, _risk_events) = EnhancedRiskManager::new(risk_config).await?;
        let risk_manager = Arc::new(risk_manager);
        
        // 創建統一引擎配置
        let engine_config = UnifiedEngineConfig {
            mode: EngineMode::Live {
                dry_run: true,  // 測試模式
                enable_paper_trading: true,
            },
            symbols: vec![config.symbol.clone()],
            risk_config: RiskConfig {
                max_position_ratio: 0.01,
                max_loss_per_trade: 0.005,
                max_daily_loss: 0.02,
                max_leverage: 1.0,
                enable_dynamic_adjustment: false,
                kelly_fraction: None,
            },
            execution_config: ExecutionConfig {
                preferred_order_type: OrderType::Market,
                slippage_tolerance_bps: 5.0,
                order_timeout_ms: 1000,
                enable_smart_routing: false,
                enable_iceberg: false,
            },
            performance_config: PerformanceConfig {
                enable_metrics: true,
                metrics_update_interval_secs: 1,
                enable_latency_tracking: true,
                enable_memory_optimization: true,
            },
        };
        
        // 創建統一交易引擎
        let trading_engine = rust_hft::engine::create_live_engine(
            vec![config.symbol.clone()],
            strategy,
            true, // dry_run
        );
        
        // 創建訂單簿
        let orderbook = Arc::new(RwLock::new(OrderBook::new(config.symbol.clone())));
        
        // 初始化統計
        let latency_stats = Arc::new(RwLock::new(LatencyStatistics::default()));
        
        Ok(Self {
            config,
            trading_engine: Arc::new(Mutex::new(trading_engine)),
            risk_manager,
            orderbook,
            latency_stats,
            test_results: Arc::new(RwLock::new(Vec::new())),
            test_counter: AtomicU64::new(0),
        })
    }
    
    /// 執行完整的E2E延遲測試
    pub async fn run_e2e_test(&self) -> Result<(), anyhow::Error> {
        info!("🎯 開始E2E延遲測試 - {} 輪測試", self.config.test_rounds);
        
        for round in 0..self.config.test_rounds {
            if self.config.verbose_logging {
                info!("📊 執行第 {}/{} 輪測試", round + 1, self.config.test_rounds);
            }
            
            // 執行單輪測試
            match self.execute_single_test_round().await {
                Ok(measurement) => {
                    // 存儲測試結果
                    {
                        let mut results = self.test_results.write().await;
                        results.push(measurement.clone());
                    }
                    
                    // 更新統計
                    self.update_statistics(&measurement).await;
                    
                    if self.config.verbose_logging {
                        info!("✅ 第{}輪測試完成 - 總延遲: {}μs", 
                              round + 1, measurement.total_latency_us);
                    }
                },
                Err(e) => {
                    error!("❌ 第{}輪測試失敗: {}", round + 1, e);
                    
                    // 更新失敗統計
                    {
                        let mut stats = self.latency_stats.write().await;
                        stats.failed_tests += 1;
                    }
                }
            }
            
            // 測試間隔
            if round < self.config.test_rounds - 1 {
                sleep(Duration::from_millis(self.config.test_interval_ms)).await;
            }
        }
        
        // 生成最終報告
        self.generate_final_report().await?;
        
        Ok(())
    }
    
    /// 執行單輪測試
    async fn execute_single_test_round(&self) -> Result<E2ELatencyMeasurement, anyhow::Error> {
        let test_id = format!("test_{}", self.test_counter.fetch_add(1, Ordering::SeqCst));
        let start_time = get_timestamp_us();
        
        let mut measurement = E2ELatencyMeasurement {
            test_id: test_id.clone(),
            start_timestamp_us: start_time,
            stage_timestamps: HashMap::new(),
            stage_latencies: HashMap::new(),
            total_latency_us: 0,
            config: self.config.clone(),
            metadata: HashMap::new(),
        };
        
        // T0: 本地時間戳捕獲
        measurement.stage_timestamps.insert(LatencyStage::LocalTimestamp, start_time);
        
        // T1: 模擬市場數據接收
        let market_data_timestamp = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::MarketDataReceived, market_data_timestamp);
        
        // 創建模擬市場數據
        let market_data = self.create_mock_market_data().await;
        
        // T2: 訂單簿更新
        let ob_update_start = get_timestamp_us();
        self.update_orderbook(&market_data).await?;
        let ob_update_end = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::OrderBookUpdated, ob_update_end);
        
        // T3-T4: 策略計算（通過交易引擎）
        let strategy_start = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::StrategyCalculationStart, strategy_start);
        
        // 模擬策略計算和信號生成
        let signals = self.generate_test_signals(&market_data).await?;
        
        let strategy_end = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::StrategyCalculationEnd, strategy_end);
        
        // T5-T6: 風險檢查
        let risk_start = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::RiskCheckStart, risk_start);
        
        let risk_result = self.perform_risk_check(&signals).await?;
        
        let risk_end = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::RiskCheckEnd, risk_end);
        
        // T7: 訂單生成
        let order_gen_start = get_timestamp_us();
        let order = self.generate_order(&signals[0]).await?;
        let order_gen_end = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::OrderGenerated, order_gen_end);
        
        // T8: 訂單提交（模擬）
        let order_submit_start = get_timestamp_us();
        let execution_report = self.simulate_order_submission(&order).await?;
        let order_submit_end = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::OrderSubmitted, order_submit_end);
        
        // T9: 交易所確認（模擬網絡延遲）
        sleep(Duration::from_micros(100)).await; // 模擬100μs網絡延遲
        let exchange_ack = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::ExchangeAcknowledged, exchange_ack);
        
        // T10: 執行回報接收
        let exec_report_time = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::ExecutionReportReceived, exec_report_time);
        
        // T11: 持倉更新
        let position_update_start = get_timestamp_us();
        self.update_position(&execution_report).await?;
        let position_update_end = get_timestamp_us();
        measurement.stage_timestamps.insert(LatencyStage::PositionUpdated, position_update_end);
        
        // 計算各階段延遲
        self.calculate_stage_latencies(&mut measurement);
        
        // 計算總延遲
        measurement.total_latency_us = position_update_end - start_time;
        
        // 添加元數據
        measurement.metadata.insert("risk_result".to_string(), format!("{:?}", risk_result));
        measurement.metadata.insert("order_type".to_string(), format!("{:?}", order.order_type));
        measurement.metadata.insert("quantity".to_string(), order.quantity.to_string());
        
        Ok(measurement)
    }
    
    /// 計算各階段延遲
    fn calculate_stage_latencies(&self, measurement: &mut E2ELatencyMeasurement) {
        let stages = [
            LatencyStage::LocalTimestamp,
            LatencyStage::MarketDataReceived,
            LatencyStage::OrderBookUpdated,
            LatencyStage::StrategyCalculationStart,
            LatencyStage::StrategyCalculationEnd,
            LatencyStage::RiskCheckStart,
            LatencyStage::RiskCheckEnd,
            LatencyStage::OrderGenerated,
            LatencyStage::OrderSubmitted,
            LatencyStage::ExchangeAcknowledged,
            LatencyStage::ExecutionReportReceived,
            LatencyStage::PositionUpdated,
        ];
        
        for window in stages.windows(2) {
            let current_stage = window[1];
            let prev_stage = window[0];
            
            if let (Some(&current_time), Some(&prev_time)) = (
                measurement.stage_timestamps.get(&current_stage),
                measurement.stage_timestamps.get(&prev_stage)
            ) {
                let latency = current_time - prev_time;
                measurement.stage_latencies.insert(current_stage, latency);
            }
        }
    }
    
    /// 創建模擬市場數據
    async fn create_mock_market_data(&self) -> rust_hft::engine::MarketData {
        rust_hft::engine::MarketData {
            symbol: self.config.symbol.clone(),
            price: 50000.0 + (rand::random::<f64>() - 0.5) * 100.0, // 隨機價格
            volume: 1.0,
            timestamp: get_timestamp_us(),
            bid: 49999.0,
            ask: 50001.0,
            bid_size: 10.0,
            ask_size: 10.0,
        }
    }
    
    /// 更新訂單簿
    async fn update_orderbook(&self, market_data: &rust_hft::engine::MarketData) -> Result<(), anyhow::Error> {
        let mut ob = self.orderbook.write().await;
        
        // 模擬訂單簿更新
        ob.update_bid(market_data.bid, market_data.bid_size);
        ob.update_ask(market_data.ask, market_data.ask_size);
        
        Ok(())
    }
    
    /// 生成測試信號
    async fn generate_test_signals(&self, market_data: &rust_hft::engine::MarketData) -> Result<Vec<Signal>, anyhow::Error> {
        Ok(vec![Signal {
            symbol: market_data.symbol.clone(),
            action: SignalAction::Buy,
            quantity: 0.01,
            price: Some(market_data.price),
            confidence: 0.8,
            metadata: HashMap::new(),
        }])
    }
    
    /// 執行風險檢查
    async fn perform_risk_check(&self, _signals: &[Signal]) -> Result<bool, anyhow::Error> {
        // 模擬快速風險檢查
        tokio::task::yield_now().await; // 模擬異步處理
        Ok(true)
    }
    
    /// 生成訂單
    async fn generate_order(&self, signal: &Signal) -> Result<OrderRequest, anyhow::Error> {
        Ok(OrderRequest {
            client_order_id: uuid::Uuid::new_v4().to_string(),
            account_id: Some(AccountId::from("test:latency")),
            symbol: signal.symbol.clone(),
            side: match signal.action {
                SignalAction::Buy => OrderSide::Buy,
                SignalAction::Sell => OrderSide::Sell,
                _ => OrderSide::Buy,
            },
            order_type: OrderType::Market,
            quantity: signal.quantity,
            price: signal.price,
            stop_price: None,
            time_in_force: TimeInForce::IOC,
            post_only: false,
            reduce_only: false,
            metadata: signal.metadata.clone(),
        })
    }
    
    /// 模擬訂單提交
    async fn simulate_order_submission(&self, order: &OrderRequest) -> Result<ExecutionReport, anyhow::Error> {
        // 模擬網絡延遲
        sleep(Duration::from_micros(50)).await;
        
        Ok(ExecutionReport {
            order_id: uuid::Uuid::new_v4().to_string(),
            client_order_id: Some(order.client_order_id.clone()),
            exchange: self.config.exchange.clone(),
            symbol: order.symbol.clone(),
            side: order.side,
            order_type: order.order_type,
            status: OrderStatus::Filled,
            original_quantity: order.quantity,
            executed_quantity: order.quantity,
            remaining_quantity: 0.0,
            price: order.price.unwrap_or(50000.0),
            avg_price: order.price.unwrap_or(50000.0),
            last_executed_price: order.price.unwrap_or(50000.0),
            last_executed_quantity: order.quantity,
            commission: order.quantity * order.price.unwrap_or(50000.0) * 0.001,
            commission_asset: "USDT".to_string(),
            create_time: get_timestamp_us(),
            update_time: get_timestamp_us(),
            transaction_time: get_timestamp_us(),
            reject_reason: None,
        })
    }
    
    /// 更新持倉
    async fn update_position(&self, execution_report: &ExecutionReport) -> Result<(), anyhow::Error> {
        // 模擬持倉更新
        self.risk_manager.update_position(execution_report).await?;
        Ok(())
    }
    
    /// 更新統計信息
    async fn update_statistics(&self, measurement: &E2ELatencyMeasurement) {
        let mut stats = self.latency_stats.write().await;
        stats.total_tests += 1;
        stats.successful_tests += 1;
        
        // 更新總延遲統計
        self.update_stage_statistics(&mut stats.total_latency_stats, measurement.total_latency_us);
        
        // 更新各階段統計
        for (&stage, &latency) in &measurement.stage_latencies {
            let stage_stats = stats.stage_stats.entry(stage).or_insert_with(StageStatistics::default);
            self.update_stage_statistics(stage_stats, latency);
            
            // 檢查是否超標
            if let Some(&target) = self.config.latency_targets.get(&stage) {
                if latency > target {
                    stage_stats.exceeded_target += 1;
                }
            }
        }
    }
    
    /// 更新階段統計
    fn update_stage_statistics(&self, stats: &mut StageStatistics, latency_us: u64) {
        stats.sample_count += 1;
        
        if stats.sample_count == 1 {
            stats.min_latency_us = latency_us;
            stats.max_latency_us = latency_us;
            stats.avg_latency_us = latency_us as f64;
        } else {
            stats.min_latency_us = stats.min_latency_us.min(latency_us);
            stats.max_latency_us = stats.max_latency_us.max(latency_us);
            
            // 更新移動平均
            let alpha = 1.0 / stats.sample_count as f64;
            stats.avg_latency_us = stats.avg_latency_us * (1.0 - alpha) + latency_us as f64 * alpha;
        }
    }
    
    /// 生成最終報告
    async fn generate_final_report(&self) -> Result<(), anyhow::Error> {
        info!("\n🏁 === E2E延遲測試最終報告 ===");
        
        let stats = self.latency_stats.read().await;
        let results = self.test_results.read().await;
        
        // 測試概要
        info!("📊 測試概要:");
        info!("   總測試數: {}", stats.total_tests);
        info!("   成功測試: {}", stats.successful_tests);
        info!("   失敗測試: {}", stats.failed_tests);
        info!("   成功率: {:.2}%", (stats.successful_tests as f64 / stats.total_tests as f64) * 100.0);
        
        // 總延遲統計
        info!("\n⚡ 總延遲統計:");
        self.print_stage_stats("總延遲", &stats.total_latency_stats, None);
        
        // 各階段延遲統計
        info!("\n🎯 各階段延遲統計:");
        
        let stages_with_names = [
            (LatencyStage::MarketDataReceived, "市場數據接收"),
            (LatencyStage::OrderBookUpdated, "訂單簿更新"),
            (LatencyStage::StrategyCalculationEnd, "策略計算"),
            (LatencyStage::RiskCheckEnd, "風險檢查"),
            (LatencyStage::OrderGenerated, "訂單生成"),
            (LatencyStage::OrderSubmitted, "訂單提交"),
            (LatencyStage::ExchangeAcknowledged, "交易所確認"),
            (LatencyStage::ExecutionReportReceived, "執行回報"),
            (LatencyStage::PositionUpdated, "持倉更新"),
        ];
        
        for (stage, name) in stages_with_names {
            if let Some(stage_stats) = stats.stage_stats.get(&stage) {
                let target = self.config.latency_targets.get(&stage).copied();
                self.print_stage_stats(name, stage_stats, target);
            }
        }
        
        // 性能分析
        info!("\n📈 性能分析:");
        
        // 計算百分位數
        let mut total_latencies: Vec<u64> = results.iter().map(|r| r.total_latency_us).collect();
        total_latencies.sort();
        
        if !total_latencies.is_empty() {
            let p50 = percentile(&total_latencies, 50.0);
            let p95 = percentile(&total_latencies, 95.0);
            let p99 = percentile(&total_latencies, 99.0);
            
            info!("   P50延遲: {}μs", p50);
            info!("   P95延遲: {}μs", p95);
            info!("   P99延遲: {}μs", p99);
        }
        
        // 瓶頸分析
        info!("\n🔍 瓶頸分析:");
        let mut stage_avg_latencies: Vec<(LatencyStage, f64)> = stats.stage_stats
            .iter()
            .map(|(&stage, stats)| (stage, stats.avg_latency_us))
            .collect();
        stage_avg_latencies.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        
        for (i, (stage, avg_latency)) in stage_avg_latencies.iter().take(3).enumerate() {
            info!("   {}. {:?}: {:.1}μs", i + 1, stage, avg_latency);
        }
        
        // 建議
        info!("\n💡 性能建議:");
        for (stage, stage_stats) in &stats.stage_stats {
            if let Some(&target) = self.config.latency_targets.get(stage) {
                if stage_stats.avg_latency_us > target as f64 {
                    let exceed_rate = (stage_stats.exceeded_target as f64 / stage_stats.sample_count as f64) * 100.0;
                    info!("   ⚠️ {:?} 平均延遲 {:.1}μs 超過目標 {}μs ({:.1}% 超標)", 
                          stage, stage_stats.avg_latency_us, target, exceed_rate);
                }
            }
        }
        
        info!("\n✅ E2E延遲測試完成！");
        Ok(())
    }
    
    /// 打印階段統計
    fn print_stage_stats(&self, name: &str, stats: &StageStatistics, target: Option<u64>) {
        let target_info = if let Some(target) = target {
            let exceed_rate = (stats.exceeded_target as f64 / stats.sample_count as f64) * 100.0;
            format!(" (目標: {}μs, 超標: {:.1}%)", target, exceed_rate)
        } else {
            String::new()
        };
        
        info!("   {}: 平均 {:.1}μs, 最小 {}μs, 最大 {}μs, 樣本 {}{}",
              name, stats.avg_latency_us, stats.min_latency_us, stats.max_latency_us, 
              stats.sample_count, target_info);
    }
}

/// 獲取微秒級時間戳
fn get_timestamp_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

/// 計算百分位數
fn percentile(sorted_data: &[u64], percentile: f64) -> u64 {
    if sorted_data.is_empty() {
        return 0;
    }
    
    let index = (percentile / 100.0 * (sorted_data.len() - 1) as f64).round() as usize;
    sorted_data[index.min(sorted_data.len() - 1)]
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日誌系統
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("🚀 啟動E2E延遲測試程序");

    // 創建測試配置
    let mut config = TestConfig::default();
    config.test_rounds = 1000;  // 1000輪測試
    config.test_interval_ms = 10; // 10ms間隔
    config.verbose_logging = false; // 關閉詳細日誌以提高性能
    
    // 設置更嚴格的延遲目標
    config.latency_targets.insert(LatencyStage::MarketDataReceived, 20);    // 20μs
    config.latency_targets.insert(LatencyStage::OrderBookUpdated, 50);      // 50μs
    config.latency_targets.insert(LatencyStage::StrategyCalculationEnd, 100); // 100μs
    config.latency_targets.insert(LatencyStage::RiskCheckEnd, 30);          // 30μs
    config.latency_targets.insert(LatencyStage::OrderGenerated, 15);        // 15μs
    config.latency_targets.insert(LatencyStage::OrderSubmitted, 200);       // 200μs
    config.latency_targets.insert(LatencyStage::ExecutionReportReceived, 1000); // 1ms
    config.latency_targets.insert(LatencyStage::PositionUpdated, 50);       // 50μs

    info!("📋 測試配置:");
    info!("   測試符號: {}", config.symbol);
    info!("   測試交易所: {}", config.exchange);
    info!("   測試輪數: {}", config.test_rounds);
    info!("   測試間隔: {}ms", config.test_interval_ms);

    // 創建並運行測試器
    let tester = E2ELatencyTester::new(config).await?;
    tester.run_e2e_test().await?;

    info!("🎉 E2E延遲測試程序結束");
    Ok(())
}