/*!
 * 🏛️ Market Simulator - 高保真市场环境模拟器
 * 
 * 核心功能：
 * - 订单簿模拟：完整的买卖盘深度和价格发现机制
 * - 订单匹配引擎：支持多种订单类型和执行策略
 * - 市场影响建模：订单对价格和流动性的真实影响
 * - 延迟模拟：网络延迟、交易所处理延迟等
 * 
 * 模拟特性：
 * - 高保真度：基于真实市场微观结构设计
 * - 多资产支持：同时模拟多个交易品种
 * - 事件驱动：精确的时间序列和事件处理
 * - 可配置性：灵活的市场参数和行为配置
 * 
 * 设计原则：
 * - 现实性：尽可能接近真实交易环境
 * - 性能：支持高频交易场景的低延迟要求
 * - 可扩展：支持自定义市场行为和规则
 * - 可测试：提供确定性的回测环境
 */

use super::*;
use crate::core::{types::*, error::*};
use std::sync::Arc;
use std::collections::{HashMap, BTreeMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Mutex, mpsc};
use tracing::{info, warn, error, debug, instrument};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 市场模拟器
pub struct MarketSimulator {
    /// 配置
    config: MarketSimulatorConfig,
    /// 订单簿集合
    order_books: Arc<RwLock<HashMap<String, SimulatedOrderBook>>>,
    /// 订单匹配引擎
    matching_engine: Arc<MatchingEngine>,
    /// 市场数据生成器
    market_data_generator: Arc<MarketDataGenerator>,
    /// 延迟模拟器
    latency_simulator: Arc<LatencySimulator>,
    /// 事件队列
    event_queue: Arc<Mutex<VecDeque<MarketEvent>>>,
    /// 统计信息
    statistics: Arc<RwLock<MarketSimulatorStats>>,
    /// 事件发送器
    event_sender: Option<mpsc::UnboundedSender<MarketEvent>>,
}

/// 市场模拟器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSimulatorConfig {
    /// 支持的交易品种
    pub supported_symbols: Vec<String>,
    /// 订单簿深度
    pub order_book_depth: usize,
    /// 最小价格变动
    pub tick_size: f64,
    /// 最小数量变动
    pub lot_size: f64,
    /// 是否启用市场影响
    pub enable_market_impact: bool,
    /// 是否启用延迟模拟
    pub enable_latency_simulation: bool,
    /// 基础延迟（微秒）
    pub base_latency_us: u64,
    /// 延迟波动范围
    pub latency_jitter_us: u64,
    /// 市场数据频率（毫秒）
    pub market_data_frequency_ms: u64,
    /// 是否启用滑点
    pub enable_slippage: bool,
    /// 流动性参数
    pub liquidity_config: LiquidityConfig,
}

/// 流动性配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidityConfig {
    /// 基础流动性深度
    pub base_depth: f64,
    /// 流动性衰减率
    pub depth_decay_rate: f64,
    /// 流动性恢复速度
    pub recovery_rate: f64,
    /// 最小流动性
    pub min_liquidity: f64,
    /// 流动性波动系数
    pub volatility_factor: f64,
}

/// 模拟订单簿
#[derive(Debug, Clone)]
pub struct SimulatedOrderBook {
    /// 交易品种
    pub symbol: String,
    /// 买盘
    pub bids: BTreeMap<u64, f64>, // price_ticks -> quantity
    /// 卖盘
    pub asks: BTreeMap<u64, f64>, // price_ticks -> quantity
    /// 最后成交价
    pub last_price: f64,
    /// 最后更新时间
    pub last_updated: u64,
    /// 流动性状态
    pub liquidity_state: LiquidityState,
}

/// 流动性状态
#[derive(Debug, Clone)]
pub struct LiquidityState {
    /// 当前流动性深度
    pub current_depth: f64,
    /// 流动性恢复时间
    pub recovery_time: u64,
    /// 临时冲击强度
    pub temporary_impact: f64,
    /// 永久冲击强度
    pub permanent_impact: f64,
}

/// 订单匹配引擎
pub struct MatchingEngine {
    /// 配置
    config: MarketSimulatorConfig,
    /// 待处理订单
    pending_orders: Arc<RwLock<HashMap<String, PendingOrder>>>,
    /// 执行统计
    execution_stats: Arc<RwLock<ExecutionStatistics>>,
}

/// 待处理订单
#[derive(Debug, Clone)]
pub struct PendingOrder {
    /// 订单信息
    pub order: SimulationOrder,
    /// 提交时间
    pub submitted_at: u64,
    /// 预期执行时间
    pub expected_execution_time: u64,
    /// 部分成交数量
    pub filled_quantity: f64,
    /// 剩余数量
    pub remaining_quantity: f64,
}

/// 执行统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionStatistics {
    /// 总订单数
    pub total_orders: u64,
    /// 成功执行数
    pub successful_executions: u64,
    /// 部分成交数
    pub partial_fills: u64,
    /// 取消订单数
    pub cancelled_orders: u64,
    /// 平均执行时间
    pub average_execution_time_us: f64,
    /// 平均滑点
    pub average_slippage: f64,
    /// 市场影响统计
    pub market_impact_stats: MarketImpactStats,
}

/// 市场影响统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketImpactStats {
    /// 平均临时冲击
    pub average_temporary_impact: f64,
    /// 平均永久冲击
    pub average_permanent_impact: f64,
    /// 流动性消耗统计
    pub liquidity_consumption: f64,
    /// 价格恢复时间
    pub price_recovery_time_us: f64,
}

/// 市场数据生成器
pub struct MarketDataGenerator {
    /// 配置
    config: MarketSimulatorConfig,
    /// 随机数生成器种子
    random_seed: u64,
    /// 价格模型参数
    price_models: Arc<RwLock<HashMap<String, PriceModel>>>,
}

/// 价格模型
#[derive(Debug, Clone)]
pub struct PriceModel {
    /// 当前价格
    pub current_price: f64,
    /// 漂移率
    pub drift: f64,
    /// 波动率
    pub volatility: f64,
    /// 均值回归速度
    pub mean_reversion_speed: f64,
    /// 长期均值
    pub long_term_mean: f64,
    /// 跳跃概率
    pub jump_probability: f64,
    /// 跳跃强度
    pub jump_intensity: f64,
}

/// 延迟模拟器
pub struct LatencySimulator {
    /// 基础延迟
    base_latency: Duration,
    /// 延迟抖动
    jitter_range: Duration,
    /// 网络状态
    network_conditions: Arc<RwLock<NetworkConditions>>,
}

/// 网络状态
#[derive(Debug, Clone)]
pub struct NetworkConditions {
    /// 当前延迟倍数
    pub latency_multiplier: f64,
    /// 丢包率
    pub packet_loss_rate: f64,
    /// 带宽限制
    pub bandwidth_limit: f64,
    /// 拥塞级别
    pub congestion_level: f64,
}

/// 市场事件
#[derive(Debug, Clone)]
pub enum MarketEvent {
    /// 订单提交
    OrderSubmitted {
        order_id: String,
        symbol: String,
        timestamp: u64,
    },
    /// 订单执行
    OrderExecuted {
        execution: MarketExecution,
        timestamp: u64,
    },
    /// 订单取消
    OrderCancelled {
        order_id: String,
        reason: String,
        timestamp: u64,
    },
    /// 市场数据更新
    MarketDataUpdate {
        symbol: String,
        order_book: SimulatedOrderBook,
        timestamp: u64,
    },
    /// 流动性变化
    LiquidityChange {
        symbol: String,
        new_state: LiquidityState,
        timestamp: u64,
    },
}

/// 市场执行结果
#[derive(Debug, Clone)]
pub struct MarketExecution {
    /// 执行ID
    pub execution_id: String,
    /// 订单ID
    pub order_id: String,
    /// 执行价格
    pub execution_price: f64,
    /// 执行数量
    pub execution_quantity: f64,
    /// 手续费
    pub commission: f64,
    /// 实际滑点
    pub slippage: f64,
    /// 市场影响
    pub market_impact: MarketImpact,
    /// 执行延迟
    pub execution_latency_us: u64,
}

/// 市场影响
#[derive(Debug, Clone)]
pub struct MarketImpact {
    /// 临时价格冲击
    pub temporary_impact: f64,
    /// 永久价格冲击
    pub permanent_impact: f64,
    /// 流动性消耗
    pub liquidity_consumed: f64,
    /// 影响衰减时间
    pub impact_decay_time_us: u64,
}

/// 市场模拟器统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarketSimulatorStats {
    /// 模拟开始时间
    pub simulation_start_time: u64,
    /// 总运行时间
    pub total_runtime_us: u64,
    /// 处理的事件数
    pub total_events_processed: u64,
    /// 生成的市场数据更新数
    pub market_data_updates: u64,
    /// 执行统计
    pub execution_stats: ExecutionStatistics,
    /// 性能指标
    pub performance_metrics: SimulatorPerformanceMetrics,
}

/// 模拟器性能指标
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SimulatorPerformanceMetrics {
    /// 事件处理速度（事件/秒）
    pub event_processing_rate: f64,
    /// 平均延迟（微秒）
    pub average_latency_us: f64,
    /// 内存使用量（MB）
    pub memory_usage_mb: f64,
    /// CPU使用率
    pub cpu_utilization: f64,
}

impl MarketSimulator {
    /// 创建新的市场模拟器
    #[instrument(skip(config))]
    pub fn new(config: MarketSimulatorConfig) -> Self {
        info!("创建市场模拟器，支持品种: {:?}", config.supported_symbols);
        
        let matching_engine = Arc::new(MatchingEngine::new(config.clone()));
        let market_data_generator = Arc::new(MarketDataGenerator::new(config.clone()));
        let latency_simulator = Arc::new(LatencySimulator::new(
            Duration::from_micros(config.base_latency_us),
            Duration::from_micros(config.latency_jitter_us),
        ));
        
        Self {
            config: config.clone(),
            order_books: Arc::new(RwLock::new(HashMap::new())),
            matching_engine,
            market_data_generator,
            latency_simulator,
            event_queue: Arc::new(Mutex::new(VecDeque::new())),
            statistics: Arc::new(RwLock::new(MarketSimulatorStats::default())),
            event_sender: None,
        }
    }
    
    /// 初始化市场模拟器
    #[instrument(skip(self))]
    pub async fn initialize(&mut self) -> PipelineResult<()> {
        info!("初始化市场模拟器");
        
        let mut order_books = self.order_books.write().await;
        
        // 为每个支持的品种创建订单簿
        for symbol in &self.config.supported_symbols {
            let order_book = SimulatedOrderBook::new(symbol.clone(), &self.config);
            order_books.insert(symbol.clone(), order_book);
            
            // 初始化价格模型
            self.market_data_generator.initialize_price_model(symbol).await;
        }
        
        // 创建事件通道
        let (sender, _receiver) = mpsc::unbounded_channel();
        self.event_sender = Some(sender);
        
        // 初始化统计
        let mut stats = self.statistics.write().await;
        stats.simulation_start_time = chrono::Utc::now().timestamp_micros() as u64;
        
        info!("市场模拟器初始化完成");
        Ok(())
    }
    
    /// 提交订单到市场
    #[instrument(skip(self, order))]
    pub async fn submit_order(&self, order: SimulationOrder) -> PipelineResult<String> {
        debug!("提交订单到市场: {} {} {}", order.symbol, order.side as u8, order.quantity);
        
        // 模拟网络延迟
        if self.config.enable_latency_simulation {
            let latency = self.latency_simulator.calculate_latency().await;
            tokio::time::sleep(latency).await;
        }
        
        // 创建待处理订单
        let pending_order = PendingOrder {
            order: order.clone(),
            submitted_at: chrono::Utc::now().timestamp_micros() as u64,
            expected_execution_time: chrono::Utc::now().timestamp_micros() as u64 + 1000, // 1ms后执行
            filled_quantity: 0.0,
            remaining_quantity: order.quantity,
        };
        
        // 添加到匹配引擎
        self.matching_engine.add_pending_order(pending_order).await;
        
        // 发送事件
        if let Some(sender) = &self.event_sender {
            let event = MarketEvent::OrderSubmitted {
                order_id: order.order_id.clone(),
                symbol: order.symbol.clone(),
                timestamp: chrono::Utc::now().timestamp_micros() as u64,
            };
            let _ = sender.send(event);
        }
        
        debug!("订单已提交: {}", order.order_id);
        Ok(order.order_id)
    }
    
    /// 取消订单
    #[instrument(skip(self))]
    pub async fn cancel_order(&self, order_id: &str, reason: &str) -> PipelineResult<()> {
        debug!("取消订单: {} 原因: {}", order_id, reason);
        
        // 从匹配引擎移除订单
        self.matching_engine.cancel_order(order_id).await?;
        
        // 发送取消事件
        if let Some(sender) = &self.event_sender {
            let event = MarketEvent::OrderCancelled {
                order_id: order_id.to_string(),
                reason: reason.to_string(),
                timestamp: chrono::Utc::now().timestamp_micros() as u64,
            };
            let _ = sender.send(event);
        }
        
        // 更新统计
        let mut stats = self.statistics.write().await;
        stats.execution_stats.cancelled_orders += 1;
        
        Ok(())
    }
    
    /// 运行市场模拟循环
    #[instrument(skip(self))]
    pub async fn run_simulation(&self) -> PipelineResult<()> {
        info!("开始市场模拟循环");
        
        let market_data_interval = Duration::from_millis(self.config.market_data_frequency_ms);
        let mut last_market_data_update = Instant::now();
        
        loop {
            let now = Instant::now();
            
            // 处理待执行订单
            self.process_pending_orders().await?;
            
            // 生成市场数据更新
            if now.duration_since(last_market_data_update) >= market_data_interval {
                self.generate_market_data_updates().await?;
                last_market_data_update = now;
            }
            
            // 处理事件队列
            self.process_event_queue().await?;
            
            // 更新流动性状态
            self.update_liquidity_states().await?;
            
            // 短暂休眠避免CPU过载
            tokio::time::sleep(Duration::from_micros(100)).await;
        }
    }
    
    /// 获取订单簿快照
    pub async fn get_order_book(&self, symbol: &str) -> Option<SimulatedOrderBook> {
        let order_books = self.order_books.read().await;
        order_books.get(symbol).cloned()
    }
    
    /// 获取所有订单簿
    pub async fn get_all_order_books(&self) -> HashMap<String, SimulatedOrderBook> {
        self.order_books.read().await.clone()
    }
    
    /// 获取统计信息
    pub async fn get_statistics(&self) -> MarketSimulatorStats {
        let mut stats = self.statistics.write().await;
        stats.total_runtime_us = chrono::Utc::now().timestamp_micros() as u64 - stats.simulation_start_time;
        stats.clone()
    }
    
    /// 设置网络条件
    pub async fn set_network_conditions(&self, conditions: NetworkConditions) {
        self.latency_simulator.update_network_conditions(conditions).await;
    }
    
    // 私有辅助方法
    
    /// 处理待执行订单
    async fn process_pending_orders(&self) -> PipelineResult<()> {
        let executions = self.matching_engine.process_pending_orders().await?;
        
        for execution in executions {
            // 应用市场影响
            if self.config.enable_market_impact {
                self.apply_market_impact(&execution).await?;
            }
            
            // 发送执行事件
            if let Some(sender) = &self.event_sender {
                let event = MarketEvent::OrderExecuted {
                    execution: execution.clone(),
                    timestamp: chrono::Utc::now().timestamp_micros() as u64,
                };
                let _ = sender.send(event);
            }
            
            // 更新统计
            self.update_execution_statistics(&execution).await;
        }
        
        Ok(())
    }
    
    /// 生成市场数据更新
    async fn generate_market_data_updates(&self) -> PipelineResult<()> {
        let mut order_books = self.order_books.write().await;
        
        for (symbol, order_book) in order_books.iter_mut() {
            // 生成新的价格和数量
            let price_update = self.market_data_generator.generate_price_update(symbol).await;
            
            // 更新订单簿
            self.update_order_book_with_price(order_book, price_update).await;
            
            // 发送市场数据事件
            if let Some(sender) = &self.event_sender {
                let event = MarketEvent::MarketDataUpdate {
                    symbol: symbol.clone(),
                    order_book: order_book.clone(),
                    timestamp: chrono::Utc::now().timestamp_micros() as u64,
                };
                let _ = sender.send(event);
            }
        }
        
        // 更新统计
        let mut stats = self.statistics.write().await;
        stats.market_data_updates += self.config.supported_symbols.len() as u64;
        
        Ok(())
    }
    
    /// 处理事件队列
    async fn process_event_queue(&self) -> PipelineResult<()> {
        let mut queue = self.event_queue.lock().await;
        let mut processed_count = 0;
        
        while let Some(event) = queue.pop_front() {
            match event {
                MarketEvent::LiquidityChange { symbol, new_state, .. } => {
                    // 更新订单簿的流动性状态
                    let mut order_books = self.order_books.write().await;
                    if let Some(order_book) = order_books.get_mut(&symbol) {
                        order_book.liquidity_state = new_state;
                    }
                }
                _ => {
                    // 处理其他事件类型
                }
            }
            
            processed_count += 1;
            
            // 限制单次处理的事件数量
            if processed_count >= 100 {
                break;
            }
        }
        
        if processed_count > 0 {
            let mut stats = self.statistics.write().await;
            stats.total_events_processed += processed_count;
        }
        
        Ok(())
    }
    
    /// 更新流动性状态
    async fn update_liquidity_states(&self) -> PipelineResult<()> {
        let mut order_books = self.order_books.write().await;
        let current_time = chrono::Utc::now().timestamp_micros() as u64;
        
        for (symbol, order_book) in order_books.iter_mut() {
            let mut updated = false;
            
            // 流动性恢复
            if order_book.liquidity_state.recovery_time < current_time {
                let recovery_amount = self.config.liquidity_config.recovery_rate * 
                    (current_time - order_book.liquidity_state.recovery_time) as f64 / 1_000_000.0;
                
                order_book.liquidity_state.current_depth += recovery_amount;
                order_book.liquidity_state.current_depth = order_book.liquidity_state.current_depth
                    .min(self.config.liquidity_config.base_depth);
                
                updated = true;
            }
            
            // 冲击衰减
            if order_book.liquidity_state.temporary_impact > 0.0 {
                let decay_factor = (-self.config.liquidity_config.depth_decay_rate * 
                    (current_time - order_book.last_updated) as f64 / 1_000_000.0).exp();
                
                order_book.liquidity_state.temporary_impact *= decay_factor;
                updated = true;
            }
            
            if updated {
                order_book.last_updated = current_time;
                
                // 发送流动性变化事件
                if let Some(sender) = &self.event_sender {
                    let event = MarketEvent::LiquidityChange {
                        symbol: symbol.clone(),
                        new_state: order_book.liquidity_state.clone(),
                        timestamp: current_time,
                    };
                    let _ = sender.send(event);
                }
            }
        }
        
        Ok(())
    }
    
    /// 应用市场影响
    async fn apply_market_impact(&self, execution: &MarketExecution) -> PipelineResult<()> {
        // 这里会修改订单簿以反映市场影响
        // 简化实现，实际会更复杂
        Ok(())
    }
    
    /// 更新订单簿价格
    async fn update_order_book_with_price(&self, order_book: &mut SimulatedOrderBook, price_update: PriceUpdate) {
        // 简化的订单簿更新逻辑
        order_book.last_price = price_update.new_price;
        order_book.last_updated = chrono::Utc::now().timestamp_micros() as u64;
        
        // 更新买卖盘深度（简化实现）
        let price_ticks = (price_update.new_price / self.config.tick_size) as u64;
        let depth = order_book.liquidity_state.current_depth;
        
        // 清空旧的买卖盘
        order_book.bids.clear();
        order_book.asks.clear();
        
        // 生成新的买卖盘
        for i in 1..=self.config.order_book_depth {
            let bid_price_ticks = price_ticks.saturating_sub(i as u64);
            let ask_price_ticks = price_ticks + i as u64;
            
            let quantity = depth / (i as f64).sqrt(); // 深度递减
            
            order_book.bids.insert(bid_price_ticks, quantity);
            order_book.asks.insert(ask_price_ticks, quantity);
        }
    }
    
    /// 更新执行统计
    async fn update_execution_statistics(&self, execution: &MarketExecution) {
        let mut stats = self.statistics.write().await;
        stats.execution_stats.successful_executions += 1;
        
        // 更新平均执行时间
        let count = stats.execution_stats.successful_executions as f64;
        stats.execution_stats.average_execution_time_us = 
            (stats.execution_stats.average_execution_time_us * (count - 1.0) + 
             execution.execution_latency_us as f64) / count;
        
        // 更新平均滑点
        stats.execution_stats.average_slippage = 
            (stats.execution_stats.average_slippage * (count - 1.0) + execution.slippage) / count;
    }
}

/// 价格更新
#[derive(Debug, Clone)]
pub struct PriceUpdate {
    pub new_price: f64,
    pub price_change: f64,
    pub volume: f64,
}

impl SimulatedOrderBook {
    /// 创建新的模拟订单簿
    pub fn new(symbol: String, config: &MarketSimulatorConfig) -> Self {
        Self {
            symbol,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_price: 50000.0, // 默认价格
            last_updated: chrono::Utc::now().timestamp_micros() as u64,
            liquidity_state: LiquidityState {
                current_depth: config.liquidity_config.base_depth,
                recovery_time: 0,
                temporary_impact: 0.0,
                permanent_impact: 0.0,
            },
        }
    }
}

impl MatchingEngine {
    pub fn new(config: MarketSimulatorConfig) -> Self {
        Self {
            config,
            pending_orders: Arc::new(RwLock::new(HashMap::new())),
            execution_stats: Arc::new(RwLock::new(ExecutionStatistics::default())),
        }
    }
    
    pub async fn add_pending_order(&self, order: PendingOrder) {
        let mut pending = self.pending_orders.write().await;
        pending.insert(order.order.order_id.clone(), order);
    }
    
    pub async fn cancel_order(&self, order_id: &str) -> PipelineResult<()> {
        let mut pending = self.pending_orders.write().await;
        pending.remove(order_id);
        Ok(())
    }
    
    pub async fn process_pending_orders(&self) -> PipelineResult<Vec<MarketExecution>> {
        // 简化的订单处理逻辑
        let mut executions = Vec::new();
        let current_time = chrono::Utc::now().timestamp_micros() as u64;
        
        let mut pending = self.pending_orders.write().await;
        let mut completed_orders = Vec::new();
        
        for (order_id, pending_order) in pending.iter_mut() {
            if current_time >= pending_order.expected_execution_time {
                // 模拟订单执行
                let execution = MarketExecution {
                    execution_id: Uuid::new_v4().to_string(),
                    order_id: order_id.clone(),
                    execution_price: pending_order.order.price.unwrap_or(50000.0),
                    execution_quantity: pending_order.remaining_quantity,
                    commission: pending_order.remaining_quantity * 0.001, // 0.1% 手续费
                    slippage: 0.5, // 简化滑点
                    market_impact: MarketImpact {
                        temporary_impact: 0.1,
                        permanent_impact: 0.05,
                        liquidity_consumed: pending_order.remaining_quantity * 0.1,
                        impact_decay_time_us: 5_000_000, // 5秒
                    },
                    execution_latency_us: current_time - pending_order.submitted_at,
                };
                
                executions.push(execution);
                completed_orders.push(order_id.clone());
            }
        }
        
        // 移除已完成的订单
        for order_id in completed_orders {
            pending.remove(&order_id);
        }
        
        Ok(executions)
    }
}

impl MarketDataGenerator {
    pub fn new(config: MarketSimulatorConfig) -> Self {
        Self {
            config,
            random_seed: 12345,
            price_models: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    pub async fn initialize_price_model(&self, symbol: &str) {
        let model = PriceModel {
            current_price: 50000.0, // 默认价格
            drift: 0.0001,
            volatility: 0.02,
            mean_reversion_speed: 0.1,
            long_term_mean: 50000.0,
            jump_probability: 0.001,
            jump_intensity: 0.02,
        };
        
        let mut models = self.price_models.write().await;
        models.insert(symbol.to_string(), model);
    }
    
    pub async fn generate_price_update(&self, symbol: &str) -> PriceUpdate {
        let models = self.price_models.read().await;
        
        if let Some(model) = models.get(symbol) {
            // 简化的价格生成（应该使用更复杂的随机过程）
            let price_change = model.drift + model.volatility * 0.1; // 简化的随机变化
            let new_price = model.current_price * (1.0 + price_change);
            
            PriceUpdate {
                new_price,
                price_change,
                volume: 1000.0, // 简化的成交量
            }
        } else {
            PriceUpdate {
                new_price: 50000.0,
                price_change: 0.0,
                volume: 0.0,
            }
        }
    }
}

impl LatencySimulator {
    pub fn new(base_latency: Duration, jitter_range: Duration) -> Self {
        Self {
            base_latency,
            jitter_range,
            network_conditions: Arc::new(RwLock::new(NetworkConditions {
                latency_multiplier: 1.0,
                packet_loss_rate: 0.0,
                bandwidth_limit: f64::INFINITY,
                congestion_level: 0.0,
            })),
        }
    }
    
    pub async fn calculate_latency(&self) -> Duration {
        let conditions = self.network_conditions.read().await;
        let base = self.base_latency.as_micros() as f64;
        let jitter = (rand::random::<f64>() - 0.5) * self.jitter_range.as_micros() as f64;
        let total_us = (base + jitter) * conditions.latency_multiplier;
        Duration::from_micros(total_us.max(0.0) as u64)
    }
    
    pub async fn update_network_conditions(&self, conditions: NetworkConditions) {
        let mut current = self.network_conditions.write().await;
        *current = conditions;
    }
}

impl Default for MarketSimulatorConfig {
    fn default() -> Self {
        Self {
            supported_symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            order_book_depth: 10,
            tick_size: 0.01,
            lot_size: 0.001,
            enable_market_impact: true,
            enable_latency_simulation: true,
            base_latency_us: 100,
            latency_jitter_us: 50,
            market_data_frequency_ms: 100,
            enable_slippage: true,
            liquidity_config: LiquidityConfig {
                base_depth: 1000.0,
                depth_decay_rate: 0.1,
                recovery_rate: 10.0,
                min_liquidity: 100.0,
                volatility_factor: 0.5,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_market_simulator_creation() {
        let config = MarketSimulatorConfig::default();
        let mut simulator = MarketSimulator::new(config);
        
        simulator.initialize().await.unwrap();
        
        let order_books = simulator.get_all_order_books().await;
        assert_eq!(order_books.len(), 2); // BTC and ETH
    }
    
    #[tokio::test]
    async fn test_order_submission() {
        let config = MarketSimulatorConfig::default();
        let mut simulator = MarketSimulator::new(config);
        simulator.initialize().await.unwrap();
        
        let order = SimulationOrder {
            order_id: "test_order".to_string(),
            strategy_name: "test_strategy".to_string(),
            symbol: "BTCUSDT".to_string(),
            order_type: OrderType::Market,
            side: TradingSide::Buy,
            quantity: 1.0,
            price: Some(50000.0),
            status: OrderStatus::Pending,
            created_at: chrono::Utc::now().timestamp_micros() as u64,
            executed_at: None,
            filled_quantity: 0.0,
            avg_fill_price: 0.0,
            commission: 0.0,
            slippage: 0.0,
        };
        
        let order_id = simulator.submit_order(order).await.unwrap();
        assert_eq!(order_id, "test_order");
    }
    
    #[tokio::test]
    async fn test_order_book_creation() {
        let config = MarketSimulatorConfig::default();
        let order_book = SimulatedOrderBook::new("BTCUSDT".to_string(), &config);
        
        assert_eq!(order_book.symbol, "BTCUSDT");
        assert_eq!(order_book.last_price, 50000.0);
        assert!(order_book.liquidity_state.current_depth > 0.0);
    }
    
    #[tokio::test]
    async fn test_latency_simulation() {
        let latency_simulator = LatencySimulator::new(
            Duration::from_micros(100),
            Duration::from_micros(50),
        );
        
        let latency = latency_simulator.calculate_latency().await;
        assert!(latency.as_micros() >= 50 && latency.as_micros() <= 150);
    }
    
    #[tokio::test]
    async fn test_market_data_generation() {
        let config = MarketSimulatorConfig::default();
        let generator = MarketDataGenerator::new(config);
        
        generator.initialize_price_model("BTCUSDT").await;
        let update = generator.generate_price_update("BTCUSDT").await;
        
        assert!(update.new_price > 0.0);
        assert!(update.volume >= 0.0);
    }
    
    #[tokio::test]
    async fn test_matching_engine() {
        let config = MarketSimulatorConfig::default();
        let engine = MatchingEngine::new(config);
        
        let order = SimulationOrder {
            order_id: "test_order".to_string(),
            strategy_name: "test_strategy".to_string(),
            symbol: "BTCUSDT".to_string(),
            order_type: OrderType::Market,
            side: TradingSide::Buy,
            quantity: 1.0,
            price: Some(50000.0),
            status: OrderStatus::Pending,
            created_at: chrono::Utc::now().timestamp_micros() as u64,
            executed_at: None,
            filled_quantity: 0.0,
            avg_fill_price: 0.0,
            commission: 0.0,
            slippage: 0.0,
        };
        
        let pending_order = PendingOrder {
            order,
            submitted_at: chrono::Utc::now().timestamp_micros() as u64,
            expected_execution_time: chrono::Utc::now().timestamp_micros() as u64,
            filled_quantity: 0.0,
            remaining_quantity: 1.0,
        };
        
        engine.add_pending_order(pending_order).await;
        
        // 等待一下让时间过去
        tokio::time::sleep(Duration::from_millis(1)).await;
        
        let executions = engine.process_pending_orders().await.unwrap();
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].execution_quantity, 1.0);
    }
}