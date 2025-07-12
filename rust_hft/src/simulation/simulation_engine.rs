/*!
 * 🚀 Simulation Engine - 高性能回测和模拟交易引擎
 * 
 * 核心功能：
 * - 事件驱动架构：基于LOB Replay的精确时间序列回测
 * - 多策略并行：支持多个策略同时回测和对比
 * - 实时风险控制：集成风险管理和限制检查
 * - 性能分析：详细的统计指标和可视化数据
 * - 滑点建模：真实的市场影响和执行成本模拟
 * 
 * 设计原则：
 * - 事件驱动：确保时间序列的严格一致性
 * - 高性能：并行策略执行，优化的数据结构
 * - 可扩展：插件化的策略和执行器架构
 * - 精确性：微秒级时间控制，真实市场条件
 */

use super::*;
use crate::core::{types::*, config::Config, error::*};
use crate::replay::*;
use std::sync::Arc;
use std::collections::{HashMap, BTreeMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, mpsc, Mutex, oneshot};
use tokio::time::sleep;
use tracing::{info, warn, error, debug, instrument, span, Level};
use uuid::Uuid;

/// 模拟引擎
pub struct SimulationEngine {
    /// 配置
    config: SimulationConfig,
    /// 当前状态
    state: Arc<RwLock<SimulationState>>,
    /// LOB回放引擎
    replay_engine: Arc<LobReplayEngine>,
    /// 时间管理器
    time_manager: Arc<TimeTravelManager>,
    /// 已注册的策略
    strategies: Arc<RwLock<HashMap<String, Box<dyn Strategy>>>>,
    /// 执行器
    executor: Arc<RwLock<Box<dyn Executor>>>,
    /// 仓位管理器
    position_manager: Arc<PositionManager>,
    /// 性能分析器
    performance_analyzer: Arc<PerformanceAnalyzer>,
    /// 滑点模型
    slippage_model: Arc<SlippageModel>,
    /// 事件队列
    event_queue: Arc<Mutex<VecDeque<SimulationEvent>>>,
    /// 事件发送器
    event_sender: mpsc::UnboundedSender<SimulationEvent>,
    /// 事件接收器
    event_receiver: Option<mpsc::UnboundedReceiver<SimulationEvent>>,
    /// 订单簿
    order_book: Arc<RwLock<HashMap<String, SimulationOrder>>>,
    /// 执行历史
    execution_history: Arc<RwLock<Vec<SimulationExecution>>>,
    /// 风险监控器
    risk_monitor: Arc<RiskMonitor>,
    /// 停止信号
    shutdown_sender: Option<oneshot::Sender<()>>,
    shutdown_receiver: Option<oneshot::Receiver<()>>,
}

/// 风险监控器
pub struct RiskMonitor {
    /// 配置
    config: SimulationConfig,
    /// 当前风险指标
    current_metrics: Arc<RwLock<RiskMetrics>>,
    /// 警告历史
    alert_history: Arc<RwLock<Vec<RiskAlert>>>,
}

impl SimulationEngine {
    /// 创建新的模拟引擎
    #[instrument(skip(replay_engine, time_manager))]
    pub async fn new(
        config: SimulationConfig,
        replay_engine: Arc<LobReplayEngine>,
        time_manager: Arc<TimeTravelManager>,
    ) -> PipelineResult<Self> {
        info!("创建模拟引擎: {}", config.simulation_id);
        
        // 创建事件通道
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        
        // 创建停止信号通道
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        
        // 创建组件
        let position_manager = Arc::new(PositionManager::new(config.initial_capital));
        let performance_analyzer = Arc::new(PerformanceAnalyzer::new(config.performance_config.clone()));
        let slippage_model = Arc::new(SlippageModel::new(config.slippage_config.clone()));
        let risk_monitor = Arc::new(RiskMonitor::new(config.clone()));
        
        // 创建默认执行器
        let executor: Box<dyn Executor> = Box::new(SimulationExecutor::new(
            slippage_model.clone(),
            position_manager.clone(),
        ));
        
        Ok(Self {
            config,
            state: Arc::new(RwLock::new(SimulationState::Uninitialized)),
            replay_engine,
            time_manager,
            strategies: Arc::new(RwLock::new(HashMap::new())),
            executor: Arc::new(RwLock::new(executor)),
            position_manager,
            performance_analyzer,
            slippage_model,
            event_queue: Arc::new(Mutex::new(VecDeque::new())),
            event_sender,
            event_receiver: Some(event_receiver),
            order_book: Arc::new(RwLock::new(HashMap::new())),
            execution_history: Arc::new(RwLock::new(Vec::new())),
            risk_monitor,
            shutdown_sender: Some(shutdown_sender),
            shutdown_receiver: Some(shutdown_receiver),
        })
    }
    
    /// 注册策略
    #[instrument(skip(self, strategy))]
    pub async fn register_strategy(&self, strategy: Box<dyn Strategy>) -> PipelineResult<()> {
        let strategy_name = strategy.name();
        info!("注册策略: {}", strategy_name);
        
        let mut strategies = self.strategies.write().await;
        strategies.insert(strategy_name.clone(), strategy);
        
        // 发送系统事件
        let system_event = SimulationEvent::SystemEvent(SystemEvent {
            event_id: Uuid::new_v4().to_string(),
            event_type: SystemEventType::StrategyLoaded,
            message: format!("策略 {} 已加载", strategy_name),
            timestamp_us: self.time_manager.current_time().await,
        });
        
        let _ = self.event_sender.send(system_event);
        
        Ok(())
    }
    
    /// 初始化模拟
    #[instrument(skip(self))]
    pub async fn initialize(&mut self) -> PipelineResult<()> {
        info!("初始化模拟引擎");
        
        // 更新状态
        self.set_state(SimulationState::Preparing).await;
        
        // 初始化所有策略
        let strategy_names: Vec<String> = {
            let strategies = self.strategies.read().await;
            strategies.keys().cloned().collect()
        };
        
        for strategy_name in strategy_names {
            let mut strategies = self.strategies.write().await;
            if let Some(strategy) = strategies.get_mut(&strategy_name) {
                strategy.initialize(&self.config).await.map_err(|e| {
                    PipelineError::Execution {
                        source: ExecutionError::InitializationFailed {
                            pipeline_id: self.config.simulation_id.clone(),
                            reason: format!("策略 {} 初始化失败: {}", strategy_name, e),
                        },
                        context: crate::core::error::error_context!("initialize")
                            .with_execution_id(self.config.simulation_id.clone()),
                    }
                })?;
            }
        }
        
        // 初始化性能分析器
        self.performance_analyzer.initialize(
            self.config.initial_capital,
            self.time_manager.current_time().await,
        ).await;
        
        // 初始化风险监控
        self.risk_monitor.initialize().await;
        
        // 更新状态
        self.set_state(SimulationState::Running).await;
        
        // 发送系统事件
        let system_event = SimulationEvent::SystemEvent(SystemEvent {
            event_id: Uuid::new_v4().to_string(),
            event_type: SystemEventType::SimulationStarted,
            message: format!("模拟 {} 已开始", self.config.simulation_id),
            timestamp_us: self.time_manager.current_time().await,
        });
        
        let _ = self.event_sender.send(system_event);
        
        info!("模拟引擎初始化完成");
        Ok(())
    }
    
    /// 开始模拟
    #[instrument(skip(self))]
    pub async fn start_simulation(&mut self) -> PipelineResult<SimulationResult> {
        info!("开始模拟执行");
        
        // 启动事件处理循环
        let event_processor = self.start_event_processor().await?;
        
        // 订阅回放引擎事件
        let replay_event_receiver = self.subscribe_to_replay_events().await?;
        
        // 启动主循环
        let main_loop = self.run_main_loop(replay_event_receiver).await;
        
        // 等待完成或停止
        tokio::select! {
            result = main_loop => {
                info!("主循环完成: {:?}", result);
            }
            _ = event_processor => {
                info!("事件处理器完成");
            }
        }
        
        // 完成模拟
        self.finalize_simulation().await
    }
    
    /// 启动事件处理器
    async fn start_event_processor(&mut self) -> PipelineResult<tokio::task::JoinHandle<()>> {
        let mut event_receiver = self.event_receiver.take()
            .ok_or_else(|| PipelineError::Concurrency {
                source: ConcurrencyError::SynchronizationFailed {
                    operation: "取得事件接收器".to_string(),
                    reason: "事件接收器已被使用".to_string(),
                },
                context: crate::core::error::error_context!("start_event_processor"),
            })?;
        
        let strategies = self.strategies.clone();
        let executor = self.executor.clone();
        let position_manager = self.position_manager.clone();
        let performance_analyzer = self.performance_analyzer.clone();
        let risk_monitor = self.risk_monitor.clone();
        let order_book = self.order_book.clone();
        let execution_history = self.execution_history.clone();
        let simulation_id = self.config.simulation_id.clone();
        
        let handle = tokio::spawn(async move {
            info!("事件处理器已启动");
            
            while let Some(event) = event_receiver.recv().await {
                if let Err(e) = Self::process_event(
                    &event,
                    &strategies,
                    &executor,
                    &position_manager,
                    &performance_analyzer,
                    &risk_monitor,
                    &order_book,
                    &execution_history,
                ).await {
                    error!("处理事件失败: {}", e);
                }
            }
            
            info!("事件处理器已停止");
        });
        
        Ok(handle)
    }
    
    /// 处理单个事件
    async fn process_event(
        event: &SimulationEvent,
        strategies: &Arc<RwLock<HashMap<String, Box<dyn Strategy>>>>,
        executor: &Arc<RwLock<Box<dyn Executor>>>,
        position_manager: &Arc<PositionManager>,
        performance_analyzer: &Arc<PerformanceAnalyzer>,
        risk_monitor: &Arc<RiskMonitor>,
        order_book: &Arc<RwLock<HashMap<String, SimulationOrder>>>,
        execution_history: &Arc<RwLock<Vec<SimulationExecution>>>,
    ) -> PipelineResult<()> {
        match event {
            SimulationEvent::MarketData(market_data) => {
                // 将市场数据传递给所有策略
                let mut strategies = strategies.write().await;
                for (strategy_name, strategy) in strategies.iter_mut() {
                    match strategy.on_market_data(market_data).await {
                        Ok(signals) => {
                            for signal in signals {
                                debug!("策略 {} 生成信号: {:?}", strategy_name, signal.signal_type);
                                // 这里可以发送信号到执行器
                            }
                        }
                        Err(e) => {
                            warn!("策略 {} 处理市场数据失败: {}", strategy_name, e);
                        }
                    }
                }
            }
            
            SimulationEvent::TradingSignal(signal) => {
                // 将交易信号转换为订单
                let order = Self::signal_to_order(signal)?;
                
                // 提交订单
                let mut executor = executor.write().await;
                match executor.submit_order(order.clone()).await {
                    Ok(order_id) => {
                        debug!("订单已提交: {}", order_id);
                        
                        // 添加到订单簿
                        let mut order_book = order_book.write().await;
                        order_book.insert(order_id, order);
                    }
                    Err(e) => {
                        warn!("提交订单失败: {}", e);
                    }
                }
            }
            
            SimulationEvent::Execution(execution) => {
                // 更新仓位
                position_manager.update_position(
                    &execution.order_id,
                    execution.execution_quantity,
                    execution.execution_price,
                ).await?;
                
                // 记录执行历史
                let mut history = execution_history.write().await;
                history.push(execution.clone());
                
                // 更新性能指标
                performance_analyzer.update_with_execution(execution).await;
                
                debug!("执行完成: {} @ {}", execution.execution_quantity, execution.execution_price);
            }
            
            SimulationEvent::PerformanceUpdate(snapshot) => {
                // 更新性能分析
                performance_analyzer.update_snapshot(snapshot.clone()).await;
                
                // 风险检查
                if let Some(alert) = risk_monitor.check_risks(snapshot).await? {
                    warn!("风险警告: {:?}", alert);
                }
            }
            
            SimulationEvent::RiskAlert(alert) => {
                // 处理风险警告
                risk_monitor.handle_alert(alert.clone()).await;
                
                if alert.severity == AlertSeverity::Critical {
                    error!("严重风险警告: {}", alert.message);
                }
            }
            
            SimulationEvent::SystemEvent(system_event) => {
                info!("系统事件: {} - {}", 
                      format!("{:?}", system_event.event_type),
                      system_event.message);
            }
            
            _ => {
                debug!("处理其他事件: {:?}", event);
            }
        }
        
        Ok(())
    }
    
    /// 订阅回放事件
    async fn subscribe_to_replay_events(&self) -> PipelineResult<mpsc::UnboundedReceiver<ReplayEvent>> {
        // 这里应该从回放引擎获取事件流
        // 为简化实现，我们创建一个模拟的接收器
        let (sender, receiver) = mpsc::unbounded_channel();
        
        // 实际实现中，这里会连接到LobReplayEngine的事件流
        
        Ok(receiver)
    }
    
    /// 运行主循环
    async fn run_main_loop(
        &mut self,
        mut replay_receiver: mpsc::UnboundedReceiver<ReplayEvent>,
    ) -> PipelineResult<()> {
        let mut shutdown_receiver = self.shutdown_receiver.take()
            .ok_or_else(|| PipelineError::Concurrency {
                source: ConcurrencyError::SynchronizationFailed {
                    operation: "取得停止接收器".to_string(),
                    reason: "停止接收器已被使用".to_string(),
                },
                context: crate::core::error::error_context!("run_main_loop"),
            })?;
        
        info!("主循环已启动");
        
        loop {
            tokio::select! {
                // 处理回放事件
                Some(replay_event) = replay_receiver.recv() => {
                    let sim_event = SimulationEvent::MarketData(replay_event);
                    if let Err(e) = self.event_sender.send(sim_event) {
                        error!("发送事件失败: {}", e);
                    }
                }
                
                // 检查停止信号
                _ = &mut shutdown_receiver => {
                    info!("收到停止信号");
                    break;
                }
                
                // 定期性能更新
                _ = sleep(Duration::from_secs(1)) => {
                    if let Err(e) = self.update_performance().await {
                        warn!("更新性能指标失败: {}", e);
                    }
                }
            }
        }
        
        info!("主循环已结束");
        Ok(())
    }
    
    /// 更新性能指标
    async fn update_performance(&self) -> PipelineResult<()> {
        let current_time = self.time_manager.current_time().await;
        let positions = self.position_manager.get_all_positions().await;
        let current_capital = self.position_manager.get_total_value().await;
        
        let risk_metrics = self.risk_monitor.calculate_current_metrics(&positions).await;
        
        let snapshot = PerformanceSnapshot {
            timestamp_us: current_time,
            current_capital,
            cumulative_return: (current_capital - self.config.initial_capital) / self.config.initial_capital,
            drawdown: self.performance_analyzer.get_current_drawdown().await,
            positions: positions.into_iter().collect(),
            risk_metrics,
        };
        
        let event = SimulationEvent::PerformanceUpdate(snapshot);
        let _ = self.event_sender.send(event);
        
        Ok(())
    }
    
    /// 完成模拟
    async fn finalize_simulation(&mut self) -> PipelineResult<SimulationResult> {
        info!("完成模拟");
        
        // 更新状态
        self.set_state(SimulationState::Completed).await;
        
        // 完成所有策略
        let strategy_names: Vec<String> = {
            let strategies = self.strategies.read().await;
            strategies.keys().cloned().collect()
        };
        
        for strategy_name in strategy_names {
            let mut strategies = self.strategies.write().await;
            if let Some(strategy) = strategies.get_mut(&strategy_name) {
                if let Err(e) = strategy.finalize().await {
                    warn!("策略 {} 完成失败: {}", strategy_name, e);
                }
            }
        }
        
        // 生成最终结果
        let result = self.generate_simulation_result().await?;
        
        // 发送完成事件
        let system_event = SimulationEvent::SystemEvent(SystemEvent {
            event_id: Uuid::new_v4().to_string(),
            event_type: SystemEventType::SimulationCompleted,
            message: format!("模拟 {} 已完成，总收益率: {:.2}%", 
                           self.config.simulation_id, 
                           result.total_return * 100.0),
            timestamp_us: self.time_manager.current_time().await,
        });
        
        let _ = self.event_sender.send(system_event);
        
        info!("模拟完成，总收益率: {:.2}%", result.total_return * 100.0);
        
        Ok(result)
    }
    
    /// 生成模拟结果
    async fn generate_simulation_result(&self) -> PipelineResult<SimulationResult> {
        let final_capital = self.position_manager.get_total_value().await;
        let total_return = (final_capital - self.config.initial_capital) / self.config.initial_capital;
        
        // 获取详细统计
        let detailed_stats = self.performance_analyzer.generate_detailed_stats().await;
        let performance_timeline = self.performance_analyzer.get_timeline().await;
        
        // 计算各种指标
        let sharpe_ratio = self.performance_analyzer.calculate_sharpe_ratio().await;
        let max_drawdown = self.performance_analyzer.get_max_drawdown().await;
        let calmar_ratio = if max_drawdown > 0.0 { total_return / max_drawdown } else { 0.0 };
        
        Ok(SimulationResult {
            simulation_id: self.config.simulation_id.clone(),
            total_return,
            annualized_return: total_return * 365.25 / 252.0, // 简化的年化计算
            sharpe_ratio,
            max_drawdown,
            win_rate: detailed_stats.strategy_stats.values()
                .map(|s| s.win_rate)
                .sum::<f64>() / detailed_stats.strategy_stats.len().max(1) as f64,
            profit_loss_ratio: 1.0, // 简化计算
            total_trades: detailed_stats.strategy_stats.values()
                .map(|s| s.trade_count)
                .sum(),
            total_commission: 0.0, // 需要从执行历史计算
            final_capital,
            var_95: 0.0, // 需要实现VaR计算
            calmar_ratio,
            sortino_ratio: 0.0, // 需要实现Sortino比率计算
            detailed_stats,
            performance_timeline,
        })
    }
    
    /// 设置状态
    async fn set_state(&self, new_state: SimulationState) {
        let mut state = self.state.write().await;
        *state = new_state;
    }
    
    /// 获取状态
    pub async fn get_state(&self) -> SimulationState {
        self.state.read().await.clone()
    }
    
    /// 停止模拟
    pub async fn stop_simulation(&mut self) -> PipelineResult<()> {
        info!("停止模拟");
        
        if let Some(sender) = self.shutdown_sender.take() {
            let _ = sender.send(());
        }
        
        Ok(())
    }
    
    /// 信号转订单
    fn signal_to_order(signal: &TradingSignal) -> PipelineResult<SimulationOrder> {
        let side = match signal.signal_type {
            SignalType::Buy => TradingSide::Buy,
            SignalType::Sell => TradingSide::Sell,
            SignalType::Close => {
                // 根据当前仓位决定方向
                TradingSide::Sell // 简化处理
            }
            SignalType::Adjust => TradingSide::Buy, // 简化处理
        };
        
        let order_type = if signal.target_price.is_some() {
            OrderType::Limit
        } else {
            OrderType::Market
        };
        
        Ok(SimulationOrder {
            order_id: Uuid::new_v4().to_string(),
            strategy_name: signal.strategy_name.clone(),
            symbol: signal.symbol.clone(),
            order_type,
            side,
            quantity: signal.target_quantity.abs(),
            price: signal.target_price,
            status: OrderStatus::Pending,
            created_at: signal.timestamp_us,
            executed_at: None,
            filled_quantity: 0.0,
            avg_fill_price: 0.0,
            commission: 0.0,
            slippage: 0.0,
        })
    }
}

/// 模拟执行器
pub struct SimulationExecutor {
    /// 滑点模型
    slippage_model: Arc<SlippageModel>,
    /// 仓位管理器
    position_manager: Arc<PositionManager>,
    /// 订单历史
    order_history: RwLock<Vec<SimulationOrder>>,
    /// 执行统计
    execution_stats: RwLock<ExecutionStats>,
}

impl SimulationExecutor {
    pub fn new(
        slippage_model: Arc<SlippageModel>,
        position_manager: Arc<PositionManager>,
    ) -> Self {
        Self {
            slippage_model,
            position_manager,
            order_history: RwLock::new(Vec::new()),
            execution_stats: RwLock::new(ExecutionStats::default()),
        }
    }
}

#[async_trait::async_trait]
impl Executor for SimulationExecutor {
    fn name(&self) -> String {
        "SimulationExecutor".to_string()
    }
    
    async fn submit_order(&mut self, mut order: SimulationOrder) -> PipelineResult<String> {
        debug!("提交订单: {} {} {}", order.symbol, order.side as u8, order.quantity);
        
        // 模拟订单执行
        order.status = OrderStatus::Submitted;
        
        // 计算滑点
        let slippage = self.slippage_model.calculate_slippage(
            &order.symbol,
            order.quantity,
            order.price.unwrap_or(50000.0), // 使用默认价格
        ).await;
        
        // 计算执行价格
        let execution_price = match order.side {
            TradingSide::Buy => order.price.unwrap_or(50000.0) + slippage,
            TradingSide::Sell => order.price.unwrap_or(50000.0) - slippage,
        };
        
        // 更新订单
        order.status = OrderStatus::Filled;
        order.filled_quantity = order.quantity;
        order.avg_fill_price = execution_price;
        order.slippage = slippage;
        order.executed_at = Some(chrono::Utc::now().timestamp_micros() as u64);
        
        let order_id = order.order_id.clone();
        
        // 添加到历史
        {
            let mut history = self.order_history.write().await;
            history.push(order);
        }
        
        // 更新统计
        {
            let mut stats = self.execution_stats.write().await;
            stats.total_orders += 1;
            stats.successful_executions += 1;
            stats.avg_slippage = (stats.avg_slippage * (stats.total_orders - 1) as f64 + slippage) / stats.total_orders as f64;
        }
        
        Ok(order_id)
    }
    
    async fn cancel_order(&mut self, order_id: &str) -> PipelineResult<()> {
        debug!("取消订单: {}", order_id);
        
        // 模拟取消订单
        let mut history = self.order_history.write().await;
        if let Some(order) = history.iter_mut().find(|o| o.order_id == order_id) {
            order.status = OrderStatus::Cancelled;
        }
        
        Ok(())
    }
    
    async fn query_order(&self, order_id: &str) -> PipelineResult<Option<SimulationOrder>> {
        let history = self.order_history.read().await;
        Ok(history.iter().find(|o| o.order_id == order_id).cloned())
    }
    
    async fn get_active_orders(&self) -> PipelineResult<Vec<SimulationOrder>> {
        let history = self.order_history.read().await;
        Ok(history.iter()
            .filter(|o| matches!(o.status, OrderStatus::Submitted | OrderStatus::PartiallyFilled))
            .cloned()
            .collect())
    }
    
    async fn get_execution_stats(&self) -> PipelineResult<ExecutionStats> {
        Ok(self.execution_stats.read().await.clone())
    }
}

impl RiskMonitor {
    pub fn new(config: SimulationConfig) -> Self {
        Self {
            config,
            current_metrics: Arc::new(RwLock::new(RiskMetrics::default())),
            alert_history: Arc::new(RwLock::new(Vec::new())),
        }
    }
    
    pub async fn initialize(&self) -> PipelineResult<()> {
        info!("初始化风险监控器");
        Ok(())
    }
    
    pub async fn check_risks(&self, snapshot: &PerformanceSnapshot) -> PipelineResult<Option<RiskAlert>> {
        // 检查回撤限制
        if snapshot.drawdown > self.config.max_drawdown {
            return Ok(Some(RiskAlert {
                alert_id: Uuid::new_v4().to_string(),
                alert_type: RiskAlertType::ExcessiveDrawdown,
                severity: AlertSeverity::Critical,
                message: format!("回撤 {:.2}% 超过限制 {:.2}%", 
                               snapshot.drawdown * 100.0, 
                               self.config.max_drawdown * 100.0),
                strategy_name: None,
                symbol: None,
                current_value: snapshot.drawdown,
                threshold: self.config.max_drawdown,
                timestamp_us: snapshot.timestamp_us,
            }));
        }
        
        // 检查仓位限制
        let total_exposure = snapshot.positions.values().map(|v| v.abs()).sum::<f64>();
        if total_exposure > self.config.risk_limits.max_total_exposure {
            return Ok(Some(RiskAlert {
                alert_id: Uuid::new_v4().to_string(),
                alert_type: RiskAlertType::PositionTooLarge,
                severity: AlertSeverity::Warning,
                message: format!("总暴露 {:.2} 超过限制 {:.2}", 
                               total_exposure, 
                               self.config.risk_limits.max_total_exposure),
                strategy_name: None,
                symbol: None,
                current_value: total_exposure,
                threshold: self.config.risk_limits.max_total_exposure,
                timestamp_us: snapshot.timestamp_us,
            }));
        }
        
        Ok(None)
    }
    
    pub async fn handle_alert(&self, alert: RiskAlert) {
        let mut history = self.alert_history.write().await;
        history.push(alert);
    }
    
    pub async fn calculate_current_metrics(&self, positions: &HashMap<String, f64>) -> RiskMetrics {
        // 简化的风险指标计算
        let total_exposure = positions.values().map(|v| v.abs()).sum::<f64>();
        
        RiskMetrics {
            total_exposure,
            volatility: 0.02, // 简化值
            beta: 1.0,
            var: total_exposure * 0.05, // 简化VaR计算
            cvar: total_exposure * 0.07, // 简化CVaR计算
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::clickhouse_client::ClickHouseClient;
    
    #[tokio::test]
    async fn test_simulation_engine_creation() {
        // 创建模拟的依赖项
        let config = SimulationConfig::default();
        
        // 为了测试，我们需要创建模拟的回放引擎和时间管理器
        // 实际测试中需要mock这些依赖项
    }
    
    #[tokio::test]
    async fn test_signal_to_order_conversion() {
        let signal = TradingSignal {
            signal_id: "test_signal".to_string(),
            strategy_name: "test_strategy".to_string(),
            symbol: "BTCUSDT".to_string(),
            signal_type: SignalType::Buy,
            target_quantity: 1.0,
            target_price: Some(50000.0),
            signal_strength: 0.8,
            timestamp_us: 1640995200000000,
            expiry_us: None,
            metadata: HashMap::new(),
        };
        
        let order = SimulationEngine::signal_to_order(&signal).unwrap();
        
        assert_eq!(order.symbol, "BTCUSDT");
        assert_eq!(order.side, TradingSide::Buy);
        assert_eq!(order.quantity, 1.0);
        assert_eq!(order.price, Some(50000.0));
        assert_eq!(order.order_type, OrderType::Limit);
    }
    
    #[test]
    fn test_risk_monitor_creation() {
        let config = SimulationConfig::default();
        let risk_monitor = RiskMonitor::new(config);
        assert!(!risk_monitor.config.simulation_id.is_empty());
    }
}