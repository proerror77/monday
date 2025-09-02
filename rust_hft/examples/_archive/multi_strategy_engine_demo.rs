//! 多策略引擎 - 支持多策略并行运行和账户隔离
//!
//! 本模块实现了基于事件驱动的多策略交易引擎，特点：
//! - 基于 ExchangeEventHub 的多消费者机制
//! - 每策略独立 tokio task + 事件队列
//! - 策略隔离和 QoS 控制
//! - 热插拔策略注册与生命周期管理

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tokio::time::Duration;
use tracing::{debug, error, info, warn};
// use futures::future::join_all; // 未使用
use serde::{Deserialize, Serialize};

use crate::core::orderbook::OrderBook;
use crate::core::types::*;
use crate::engine::strategies::traits::*;
// 避免與 core::types::TradingSignal 衝突

use crate::engine::strategies::traits::TradingSignal as StratSignal;
use crate::exchanges::message_types::*;
use crate::exchanges::{EventFilter, ExchangeEventHub};

// MarketEvent 的助手方法已在 message_types.rs 中实现

/// 策略运行时 ID
pub type StrategyId = String;

/// 策略运行实例 - 每个策略的运行时环境
pub struct StrategyInstance {
    /// 策略ID
    pub id: StrategyId,

    /// 策略实现
    pub strategy: Box<dyn TradingStrategy>,

    /// 策略上下文
    pub context: StrategyContext,

    /// 策略状态
    pub state: Arc<RwLock<StrategyInstanceState>>,

    /// 事件发送端（用于向策略发送市场事件）
    pub event_sender: mpsc::UnboundedSender<StrategyEvent>,

    /// 信号发送端（策略产生的交易信号）
    pub signal_sender: mpsc::UnboundedSender<StrategySignal>,

    /// 性能统计
    pub stats: Arc<RwLock<StrategyStats>>,

    /// QoS 控制器
    pub qos_controller: QoSController,
}

impl std::fmt::Debug for StrategyInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StrategyInstance")
            .field("id", &self.id)
            .field("strategy", &"<Strategy>") // 不能 Debug strategy trait object
            .field("context", &self.context)
            .field("state", &self.state)
            .field("stats", &self.stats)
            .finish()
    }
}

/// 策略实例状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyInstanceState {
    pub id: StrategyId,
    pub status: StrategyStatus,
    pub account_id: Option<AccountId>,
    pub symbol_universe: Vec<String>,
    pub started_at: Option<u64>, // 使用时间戳而非 Instant
    pub last_signal_at: Option<u64>,
    pub last_error: Option<String>,
    pub restart_count: u32,
}

/// 策略事件 - 输入到策略的事件
#[derive(Debug, Clone)]
pub enum StrategyEvent {
    /// 市场数据更新
    MarketUpdate {
        symbol: String,
        orderbook: OrderBook,
        timestamp: Timestamp,
    },
    /// 交易执行回报
    ExecutionReport {
        order_id: String,
        symbol: String,
        executed_quantity: f64,
        price: f64,
        timestamp: Timestamp,
    },
    /// 控制指令
    Control(StrategyControlCommand),
}

/// 策略信号 - 策略产生的输出
#[derive(Debug, Clone)]
pub struct StrategySignal {
    pub strategy_id: StrategyId,
    pub account_id: Option<AccountId>,
    pub symbol: String,
    pub signal: StratSignal,
    pub timestamp: Timestamp,
    pub metadata: HashMap<String, String>,
}

/// 策略控制指令
#[derive(Debug, Clone)]
pub enum StrategyControlCommand {
    Start,
    Pause,
    Resume,
    Stop,
    Reset,
    UpdateParams(StrategyParameters),
}

/// 策略统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyStats {
    pub strategy_id: StrategyId,
    pub events_processed: u64,
    pub signals_generated: u64,
    pub orders_placed: u64,
    pub avg_processing_time_us: f64,
    pub error_count: u64,
    pub last_activity: Option<u64>,
    pub cpu_usage_percent: f64,
    pub memory_usage_mb: u64,
    pub throughput_events_per_sec: f64,
}

/// QoS 控制器 - 控制策略资源使用
#[derive(Debug)]
pub struct QoSController {
    pub config: QoSConfig,
    pub rate_limiter: Option<RateLimiter>,
    pub resource_monitor: ResourceMonitor,
}

/// 速率限制器
#[derive(Debug)]
pub struct RateLimiter {
    max_events_per_second: u64,
    events_in_current_second: Arc<Mutex<u64>>,
    last_reset: Arc<Mutex<u64>>,
}

/// 资源监控器
#[derive(Debug)]
pub struct ResourceMonitor {
    start_time: u64,
    cpu_usage: Arc<Mutex<f64>>,
    memory_usage: Arc<Mutex<u64>>,
}

/// 策略注册表 - 支持策略热插拔
pub struct StrategyRegistry {
    /// 活跃策略实例
    strategies: Arc<RwLock<HashMap<StrategyId, StrategyInstance>>>,

    /// 策略工厂函数注册
    strategy_factories: Arc<RwLock<HashMap<String, StrategyFactory>>>,

    /// 全局配置
    global_config: MultiStrategyConfig,
}

/// 策略工厂函数类型
pub type StrategyFactory = Box<dyn Fn(&StrategyContext) -> Box<dyn TradingStrategy> + Send + Sync>;

/// 多策略引擎配置
#[derive(Debug, Clone)]
pub struct MultiStrategyConfig {
    pub max_strategies: usize,
    pub default_qos_config: QoSConfig,
    pub strategy_restart_policy: RestartPolicy,
    pub event_buffer_size: usize,
    pub signal_buffer_size: usize,
    pub stats_update_interval: Duration,
    pub enable_auto_restart: bool,
}

/// 重启策略
#[derive(Debug, Clone)]
pub enum RestartPolicy {
    Never,
    OnError { max_retries: u32, backoff: Duration },
    Always { interval: Duration },
}

/// 多策略引擎主体
pub struct MultiStrategyEngine {
    /// 策略注册表
    registry: StrategyRegistry,

    /// OMS 系统（用于执行策略信号）
    oms: Arc<CompleteOMS>,

    /// 事件分发中心
    event_hub: Arc<ExchangeEventHub>,

    /// 信号聚合器（收集所有策略信号）
    signal_aggregator: Arc<Mutex<mpsc::UnboundedReceiver<StrategySignal>>>,
    signal_sender: mpsc::UnboundedSender<StrategySignal>,

    /// 引擎状态
    state: Arc<RwLock<EngineState>>,

    /// 全局统计
    global_stats: Arc<RwLock<GlobalStats>>,

    /// 配置
    config: MultiStrategyConfig,
}

/// 引擎状态
#[derive(Debug, Clone)]
pub enum EngineState {
    Initializing,
    Running,
    Paused,
    Stopping,
    Stopped,
    Error(String),
}

/// 全局统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalStats {
    pub total_strategies: usize,
    pub active_strategies: usize,
    pub paused_strategies: usize,
    pub total_events_processed: u64,
    pub total_signals_generated: u64,
    pub total_orders_placed: u64,
    pub avg_engine_latency_us: f64,
    pub uptime_seconds: u64,
    pub start_time: u64,
}

impl MultiStrategyEngine {
    /// 创建新的多策略引擎
    pub fn new(
        oms: Arc<CompleteOMS>,
        event_hub: Arc<ExchangeEventHub>,
        config: MultiStrategyConfig,
    ) -> Self {
        let (signal_sender, signal_receiver) = mpsc::unbounded_channel();

        let registry = StrategyRegistry {
            strategies: Arc::new(RwLock::new(HashMap::new())),
            strategy_factories: Arc::new(RwLock::new(HashMap::new())),
            global_config: config.clone(),
        };

        Self {
            registry,
            oms,
            event_hub,
            signal_aggregator: Arc::new(Mutex::new(signal_receiver)),
            signal_sender,
            state: Arc::new(RwLock::new(EngineState::Initializing)),
            global_stats: Arc::new(RwLock::new(GlobalStats {
                total_strategies: 0,
                active_strategies: 0,
                paused_strategies: 0,
                total_events_processed: 0,
                total_signals_generated: 0,
                total_orders_placed: 0,
                avg_engine_latency_us: 0.0,
                uptime_seconds: 0,
                start_time: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            })),
            config,
        }
    }

    /// 启动多策略引擎
    pub async fn start(&self) -> anyhow::Result<()> {
        info!("Starting Multi-Strategy Engine");

        {
            let mut state = self.state.write().await;
            *state = EngineState::Running;
        }

        // 启动信号处理器
        self.start_signal_processor().await;

        // 启动统计更新器
        self.start_stats_updater().await;

        // 启动QoS监控器
        self.start_qos_monitor().await;

        info!("Multi-Strategy Engine started successfully");
        Ok(())
    }

    /// 注册策略工厂
    pub async fn register_strategy_factory<F>(&self, name: String, factory: F) -> anyhow::Result<()>
    where
        F: Fn(&StrategyContext) -> Box<dyn TradingStrategy> + Send + Sync + 'static,
    {
        let mut factories = self.registry.strategy_factories.write().await;
        factories.insert(name.clone(), Box::new(factory));
        info!("Strategy factory registered: {}", name);
        Ok(())
    }

    /// 创建并启动策略实例
    pub async fn create_strategy(
        &self,
        strategy_id: StrategyId,
        strategy_type: String,
        context: StrategyContext,
    ) -> anyhow::Result<()> {
        // 检查策略数量限制
        {
            let strategies = self.registry.strategies.read().await;
            if strategies.len() >= self.config.max_strategies {
                return Err(anyhow::anyhow!(
                    "Maximum strategy limit reached: {}",
                    self.config.max_strategies
                ));
            }

            if strategies.contains_key(&strategy_id) {
                return Err(anyhow::anyhow!(
                    "Strategy ID already exists: {}",
                    strategy_id
                ));
            }
        }

        // 获取策略工厂
        let strategy = {
            let factories = self.registry.strategy_factories.read().await;
            let factory = factories
                .get(&strategy_type)
                .ok_or_else(|| anyhow::anyhow!("Strategy type not found: {}", strategy_type))?;
            factory(&context)
        };

        // 创建策略实例
        let (event_sender, event_receiver) = mpsc::unbounded_channel();

        let strategy_instance = StrategyInstance {
            id: strategy_id.clone(),
            strategy,
            context: context.clone(),
            state: Arc::new(RwLock::new(StrategyInstanceState {
                id: strategy_id.clone(),
                status: StrategyStatus::Initializing,
                account_id: context.account_id.clone().map(AccountId::from),
                symbol_universe: context.symbol_universe.enabled_symbols.clone(),
                started_at: None,
                last_signal_at: None,
                last_error: None,
                restart_count: 0,
            })),
            event_sender,
            signal_sender: self.signal_sender.clone(),
            stats: Arc::new(RwLock::new(StrategyStats {
                strategy_id: strategy_id.clone(),
                events_processed: 0,
                signals_generated: 0,
                orders_placed: 0,
                avg_processing_time_us: 0.0,
                error_count: 0,
                last_activity: None,
                cpu_usage_percent: 0.0,
                memory_usage_mb: 0,
                throughput_events_per_sec: 0.0,
            })),
            qos_controller: QoSController::new(context.qos_config.clone()),
        };

        // 启动策略任务
        self.start_strategy_task(strategy_instance, event_receiver)
            .await?;

        info!(
            "Strategy created and started: {} (type: {})",
            strategy_id, strategy_type
        );
        Ok(())
    }

    /// 启动策略运行任务
    async fn start_strategy_task(
        &self,
        mut strategy_instance: StrategyInstance,
        mut event_receiver: mpsc::UnboundedReceiver<StrategyEvent>,
    ) -> anyhow::Result<()> {
        let strategy_id = strategy_instance.id.clone();
        let state = strategy_instance.state.clone();
        let stats = strategy_instance.stats.clone();
        let signal_sender = strategy_instance.signal_sender.clone();
        let qos_controller = strategy_instance.qos_controller.clone();

        // 订阅市场事件
        let subscriber_id = format!("strategy_{}", strategy_id);
        let symbol_filter = if strategy_instance
            .context
            .symbol_universe
            .enabled_symbols
            .len()
            == 1
        {
            EventFilter::Symbol(
                strategy_instance.context.symbol_universe.enabled_symbols[0].clone(),
            )
        } else {
            EventFilter::All // 多符号策略接收所有事件
        };

        let mut market_receiver = self
            .event_hub
            .subscribe_market_events(
                subscriber_id.clone(),
                self.config.event_buffer_size,
                symbol_filter,
            )
            .await;

        // 将策略实例加入注册表
        {
            let mut strategies = self.registry.strategies.write().await;
            strategies.insert(strategy_id.clone(), strategy_instance);
        }

        // 启动策略事件处理循环
        let strategies_ref = self.registry.strategies.clone();
        tokio::spawn(async move {
            info!("Strategy task started: {}", strategy_id);

            // 更新状态为运行中
            {
                let mut state_guard = state.write().await;
                state_guard.status = StrategyStatus::Active;
                state_guard.started_at = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                );
            }

            loop {
                tokio::select! {
                    // 处理内部事件
                    event = event_receiver.recv() => {
                        match event {
                            Some(event) => {
                                if let Err(e) = Self::handle_strategy_event(
                                    &strategy_id,
                                    event,
                                    &strategies_ref,
                                    &signal_sender,
                                    &stats,
                                    &qos_controller,
                                ).await {
                                    error!("Error handling strategy event for {}: {}", strategy_id, e);
                                }
                            }
                            None => {
                                warn!("Strategy event channel closed for: {}", strategy_id);
                                break;
                            }
                        }
                    }

                    // 处理市场数据事件
                    market_event = market_receiver.recv() => {
                        match market_event {
                            Ok(market_event) => {
                                if let Err(e) = Self::handle_market_event(
                                    &strategy_id,
                                    &*market_event,
                                    &strategies_ref,
                                    &signal_sender,
                                    &stats,
                                    &qos_controller,
                                ).await {
                                    error!("Error handling market event for {}: {}", strategy_id, e);
                                }
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                warn!("Market event channel closed for strategy: {}", strategy_id);
                                break;
                            }
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                warn!("Strategy {} lagged, skipped {} events", strategy_id, skipped);
                                // 策略跟不上，可能需要降级处理
                            }
                        }
                    }
                }

                // QoS 控制：检查是否需要节流
                if qos_controller.should_throttle().await {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }

            // 策略任务结束，清理状态
            {
                let mut state_guard = state.write().await;
                state_guard.status = StrategyStatus::Stopped;
            }

            info!("Strategy task ended: {}", strategy_id);
        });

        Ok(())
    }

    /// 处理策略事件
    async fn handle_strategy_event(
        strategy_id: &StrategyId,
        event: StrategyEvent,
        strategies: &Arc<RwLock<HashMap<StrategyId, StrategyInstance>>>,
        signal_sender: &mpsc::UnboundedSender<StrategySignal>,
        stats: &Arc<RwLock<StrategyStats>>,
        qos_controller: &QoSController,
    ) -> anyhow::Result<()> {
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // QoS 检查
        if !qos_controller.can_process_event().await {
            return Ok(()); // 跳过此事件
        }

        match event {
            StrategyEvent::MarketUpdate {
                symbol,
                orderbook,
                timestamp,
            } => {
                // 处理市场数据更新
                let strategies_read = strategies.read().await;
                if let Some(strategy_instance) = strategies_read.get(strategy_id) {
                    // 由于我们需要 &mut strategy，但在多线程环境中这很复杂
                    // 这里我们通过 strategy trait 的设计来调用推理逻辑

                    // 注意：在真实的实现中，strategy 应该封装在 Arc<Mutex<>> 中以支持并发访问
                    // 但目前的架构中，我们通过事件驱动的方式来处理

                    // 直接生成模拟的推理信号（实际中应该调用策略的推理方法）
                    // 这里可以集成 InferenceStrategy 或其他具体策略

                    // 模拟推理逻辑：基于订单簿生成交易信号
                    if orderbook.is_valid {
                        let best_bid = orderbook.best_bid();
                        let best_ask = orderbook.best_ask();

                        if let (Some(bid_price), Some(ask_price)) = (best_bid, best_ask) {
                            let spread = ask_price.0 - bid_price.0;
                            let mid_price = (bid_price.0 + ask_price.0) / 2.0;

                            // 简单的推理逻辑：基于价差决定交易方向
                            if spread < mid_price * 0.001 {
                                // 价差小于0.1%时考虑交易
                                let signal = if bid_price.0 > mid_price * 0.999 {
                                    // 价格接近中间价时买入
                                    StratSignal::Buy {
                                        quantity: 0.01, // 固定数量
                                        price: Some(bid_price.0),
                                        confidence: 0.7,
                                    }
                                } else if ask_price.0 < mid_price * 1.001 {
                                    // 价格接近中间价时卖出
                                    StratSignal::Sell {
                                        quantity: 0.01, // 固定数量
                                        price: Some(ask_price.0),
                                        confidence: 0.7,
                                    }
                                } else {
                                    StratSignal::Hold
                                };

                                // 只有非Hold信号才发送
                                if !matches!(signal, StratSignal::Hold) {
                                    let strategy_signal = StrategySignal {
                                        strategy_id: strategy_id.clone(),
                                        account_id: strategy_instance
                                            .context
                                            .account_id
                                            .clone()
                                            .map(AccountId::from),
                                        symbol: symbol.clone(),
                                        signal,
                                        timestamp,
                                        metadata: {
                                            let mut metadata = HashMap::new();
                                            metadata.insert(
                                                "source".to_string(),
                                                "market_update".to_string(),
                                            );
                                            metadata
                                                .insert("spread".to_string(), spread.to_string());
                                            metadata.insert(
                                                "mid_price".to_string(),
                                                mid_price.to_string(),
                                            );
                                            metadata
                                        },
                                    };

                                    // 发送信号
                                    if let Err(e) = signal_sender.send(strategy_signal) {
                                        error!(
                                            "Failed to send strategy signal from {}: {}",
                                            strategy_id, e
                                        );
                                    } else {
                                        debug!(
                                            "Strategy signal sent from {}: {}",
                                            strategy_id, symbol
                                        );

                                        // 更新信号统计
                                        let mut stats_guard = stats.write().await;
                                        stats_guard.signals_generated += 1;
                                    }
                                }
                            }
                        }
                    }

                    debug!(
                        "Processing market update for strategy {}: {}",
                        strategy_id, symbol
                    );
                }
            }
            StrategyEvent::ExecutionReport {
                order_id,
                symbol,
                executed_quantity,
                price,
                timestamp,
            } => {
                // 处理执行回报
                debug!(
                    "Processing execution report for strategy {}: order_id={}",
                    strategy_id, order_id
                );
            }
            StrategyEvent::Control(command) => {
                // 处理控制命令
                Self::handle_control_command(strategy_id, command, strategies).await?;
            }
        }

        // 更新统计
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let processing_time = (now - start_time) as f64;
        let mut stats_guard = stats.write().await;
        stats_guard.events_processed += 1;
        stats_guard.avg_processing_time_us =
            (stats_guard.avg_processing_time_us * 0.9) + (processing_time * 0.1);
        stats_guard.last_activity = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );

        Ok(())
    }

    /// 处理市场事件
    async fn handle_market_event(
        strategy_id: &StrategyId,
        market_event: &MarketEvent,
        strategies: &Arc<RwLock<HashMap<StrategyId, StrategyInstance>>>,
        signal_sender: &mpsc::UnboundedSender<StrategySignal>,
        stats: &Arc<RwLock<StrategyStats>>,
        qos_controller: &QoSController,
    ) -> anyhow::Result<()> {
        // QoS 检查
        if !qos_controller.can_process_event().await {
            return Ok(()); // 跳过此事件
        }

        // 根据市场事件类型处理推理信号
        match market_event {
            MarketEvent::OrderBookUpdate {
                symbol,
                bids,
                asks,
                timestamp,
                ..
            } => {
                let start_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;

                // 构建 OrderBook 对象
                let mut orderbook = OrderBook::new(symbol.clone());

                // 添加买盘 - 直接插入到 BTreeMap
                for bid in bids {
                    if let Ok(quantity) = Quantity::from_str(&bid.quantity.to_string()) {
                        orderbook.bids.insert(Price::from(bid.price), quantity);
                    }
                }

                // 添加卖盘 - 直接插入到 BTreeMap
                for ask in asks {
                    if let Ok(quantity) = Quantity::from_str(&ask.quantity.to_string()) {
                        orderbook.asks.insert(Price::from(ask.price), quantity);
                    }
                }

                // 标记为有效
                orderbook.is_valid = true;
                orderbook.last_update = *timestamp;

                // 获取策略实例并调用其推理逻辑
                let strategies_read = strategies.read().await;
                if let Some(strategy_instance) = strategies_read.get(strategy_id) {
                    // 这里需要调用策略的 on_orderbook_update 方法
                    // 但由于策略是 trait object，我们需要通过事件机制

                    // 创建策略事件
                    let strategy_event = StrategyEvent::MarketUpdate {
                        symbol: symbol.clone(),
                        orderbook,
                        timestamp: *timestamp,
                    };

                    // 发送事件到策略的事件队列
                    if let Err(e) = strategy_instance.event_sender.send(strategy_event) {
                        error!(
                            "Failed to send market event to strategy {}: {}",
                            strategy_id, e
                        );
                        return Err(anyhow::anyhow!("Failed to send market event: {}", e));
                    }

                    debug!(
                        "Market orderbook update sent to strategy {}: {}",
                        strategy_id, symbol
                    );
                }

                // 更新统计
                let processing_time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64
                    - start_time;
                let mut stats_guard = stats.write().await;
                stats_guard.events_processed += 1;
                stats_guard.avg_processing_time_us =
                    (stats_guard.avg_processing_time_us * 0.9) + (processing_time as f64 * 0.1);
                stats_guard.last_activity = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                );
            }
            MarketEvent::Trade {
                symbol,
                price,
                quantity,
                side,
                timestamp,
                ..
            } => {
                debug!(
                    "Trade event for strategy {}: {} {} @ {}",
                    strategy_id, quantity, symbol, price
                );
                // Trade 事件暂时不处理，但可以在这里添加基于成交的推理逻辑
            }
            MarketEvent::Ticker {
                symbol,
                last_price,
                timestamp,
                ..
            } => {
                debug!(
                    "Ticker event for strategy {}: {} @ {}",
                    strategy_id, symbol, last_price
                );
                // Ticker 事件暂时不处理，但可以在这里添加基于价格变化的推理逻辑
            }
            _ => {
                // 其他类型的市场事件暂时不处理
            }
        }

        Ok(())
    }

    /// 处理控制命令
    async fn handle_control_command(
        strategy_id: &StrategyId,
        command: StrategyControlCommand,
        strategies: &Arc<RwLock<HashMap<StrategyId, StrategyInstance>>>,
    ) -> anyhow::Result<()> {
        let mut strategies_write = strategies.write().await;
        if let Some(strategy_instance) = strategies_write.get_mut(strategy_id) {
            let mut state = strategy_instance.state.write().await;

            match command {
                StrategyControlCommand::Start => {
                    if state.status == StrategyStatus::Ready
                        || state.status == StrategyStatus::Stopped
                    {
                        state.status = StrategyStatus::Active;
                        state.started_at = Some(
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs(),
                        );
                        info!("Strategy started: {}", strategy_id);
                    }
                }
                StrategyControlCommand::Pause => {
                    if state.status == StrategyStatus::Active {
                        state.status = StrategyStatus::Paused;
                        info!("Strategy paused: {}", strategy_id);
                    }
                }
                StrategyControlCommand::Resume => {
                    if state.status == StrategyStatus::Paused {
                        state.status = StrategyStatus::Active;
                        info!("Strategy resumed: {}", strategy_id);
                    }
                }
                StrategyControlCommand::Stop => {
                    state.status = StrategyStatus::Stopped;
                    info!("Strategy stopped: {}", strategy_id);
                }
                StrategyControlCommand::Reset => {
                    state.status = StrategyStatus::Ready;
                    state.last_error = None;
                    state.restart_count = 0;
                    info!("Strategy reset: {}", strategy_id);
                }
                StrategyControlCommand::UpdateParams(params) => {
                    // 更新策略参数 - 这需要策略实现支持参数热更新
                    info!("Strategy parameters updated: {}", strategy_id);
                }
            }
        }

        Ok(())
    }

    /// 启动信号处理器
    async fn start_signal_processor(&self) {
        let oms = self.oms.clone();
        let signal_aggregator = self.signal_aggregator.clone();
        let global_stats = self.global_stats.clone();

        tokio::spawn(async move {
            info!("Strategy signal processor started");

            let mut signal_receiver = signal_aggregator.lock().await;

            while let Some(strategy_signal) = signal_receiver.recv().await {
                debug!(
                    "Processing strategy signal: {} from {}",
                    strategy_signal.signal.confidence(),
                    strategy_signal.strategy_id
                );

                // 将策略信号转换为订单请求
                if let Err(e) =
                    Self::process_strategy_signal(&oms, strategy_signal, &global_stats).await
                {
                    error!("Failed to process strategy signal: {}", e);
                }
            }

            warn!("Strategy signal processor ended");
        });
    }

    /// 处理策略信号，转换为OMS订单
    async fn process_strategy_signal(
        oms: &Arc<CompleteOMS>,
        signal: StrategySignal,
        global_stats: &Arc<RwLock<GlobalStats>>,
    ) -> anyhow::Result<()> {
        use crate::exchanges::OrderRequest;

        let order_request = match signal.signal {
            StratSignal::Buy {
                quantity, price, ..
            } => OrderRequest {
                account_id: signal.account_id.clone(),
                symbol: signal.symbol,
                side: OrderSide::Buy.into(),
                order_type: if price.is_some() {
                    OrderType::Limit.into()
                } else {
                    OrderType::Market.into()
                },
                quantity,
                price,
                stop_price: None,
                time_in_force: TimeInForce::GTC.into(),
                post_only: false,
                client_order_id: format!("{}_{}", signal.strategy_id, signal.timestamp),
                reduce_only: false,
                metadata: std::collections::HashMap::new(),
            },
            StratSignal::Sell {
                quantity, price, ..
            } => OrderRequest {
                account_id: signal.account_id.clone(),
                symbol: signal.symbol,
                side: OrderSide::Sell.into(),
                order_type: if price.is_some() {
                    OrderType::Limit.into()
                } else {
                    OrderType::Market.into()
                },
                quantity,
                price,
                stop_price: None,
                time_in_force: TimeInForce::GTC.into(),
                post_only: false,
                client_order_id: format!("{}_{}", signal.strategy_id, signal.timestamp),
                reduce_only: false,
                metadata: std::collections::HashMap::new(),
            },
            StratSignal::Hold => {
                // Hold 信号不产生订单
                return Ok(());
            }
            StratSignal::Close { ratio } => {
                // 平仓信号需要先查询当前持仓，然后生成平仓订单
                // 这里简化实现，实际需要与 portfolio 集成
                info!(
                    "Close signal received from strategy {}, ratio: {}",
                    signal.strategy_id, ratio
                );
                return Ok(());
            }
            StratSignal::CloseAll => {
                // 全平仓信号
                info!(
                    "Close all signal received from strategy {}",
                    signal.strategy_id
                );
                return Ok(());
            }
        };

        // 提交订单到 OMS
        match oms.submit_order(order_request).await {
            Ok(order_id) => {
                info!(
                    "Order submitted successfully: {} from strategy {}",
                    order_id, signal.strategy_id
                );

                // 更新全局统计
                let mut stats = global_stats.write().await;
                stats.total_orders_placed += 1;
            }
            Err(e) => {
                error!(
                    "Failed to submit order from strategy {}: {}",
                    signal.strategy_id, e
                );
                return Err(anyhow::anyhow!("Order submission failed: {}", e));
            }
        }

        Ok(())
    }

    /// 启动统计更新器
    async fn start_stats_updater(&self) {
        let global_stats = self.global_stats.clone();
        let strategies = self.registry.strategies.clone();
        let update_interval = self.config.stats_update_interval;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(update_interval);

            loop {
                interval.tick().await;

                // 更新全局统计
                let strategy_count = strategies.read().await.len();
                let mut active_count = 0;
                let mut paused_count = 0;

                for strategy_instance in strategies.read().await.values() {
                    let state = strategy_instance.state.read().await;
                    match state.status {
                        StrategyStatus::Active => active_count += 1,
                        StrategyStatus::Paused => paused_count += 1,
                        _ => {}
                    }
                }

                let mut global_stats_guard = global_stats.write().await;
                global_stats_guard.total_strategies = strategy_count;
                global_stats_guard.active_strategies = active_count;
                global_stats_guard.paused_strategies = paused_count;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                global_stats_guard.uptime_seconds = now - global_stats_guard.start_time;

                debug!(
                    "Global stats updated: {} total, {} active, {} paused strategies",
                    strategy_count, active_count, paused_count
                );
            }
        });
    }

    /// 启动QoS监控器
    async fn start_qos_monitor(&self) {
        let strategies = self.registry.strategies.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));

            loop {
                interval.tick().await;

                // 监控每个策略的资源使用情况
                for strategy_instance in strategies.read().await.values() {
                    strategy_instance
                        .qos_controller
                        .update_resource_usage()
                        .await;

                    let stats = strategy_instance.stats.read().await;
                    if stats.cpu_usage_percent > 80.0 {
                        warn!(
                            "High CPU usage detected for strategy {}: {:.1}%",
                            strategy_instance.id, stats.cpu_usage_percent
                        );
                    }

                    if stats.memory_usage_mb > 512 {
                        warn!(
                            "High memory usage detected for strategy {}: {} MB",
                            strategy_instance.id, stats.memory_usage_mb
                        );
                    }
                }
            }
        });
    }

    /// 获取策略状态
    pub async fn get_strategy_state(
        &self,
        strategy_id: &StrategyId,
    ) -> Option<StrategyInstanceState> {
        let strategies = self.registry.strategies.read().await;
        if let Some(strategy) = strategies.get(strategy_id) {
            Some(strategy.state.read().await.clone())
        } else {
            None
        }
    }

    /// 获取所有策略状态
    pub async fn get_all_strategy_states(&self) -> Vec<StrategyInstanceState> {
        let strategies = self.registry.strategies.read().await;
        let mut states = Vec::new();

        for strategy in strategies.values() {
            states.push(strategy.state.read().await.clone());
        }

        states
    }

    /// 获取策略统计
    pub async fn get_strategy_stats(&self, strategy_id: &StrategyId) -> Option<StrategyStats> {
        let strategies = self.registry.strategies.read().await;
        if let Some(strategy) = strategies.get(strategy_id) {
            Some(strategy.stats.read().await.clone())
        } else {
            None
        }
    }

    /// 获取全局统计
    pub async fn get_global_stats(&self) -> GlobalStats {
        self.global_stats.read().await.clone()
    }

    /// 发送控制命令到策略
    pub async fn send_control_command(
        &self,
        strategy_id: &StrategyId,
        command: StrategyControlCommand,
    ) -> anyhow::Result<()> {
        let strategies = self.registry.strategies.read().await;
        if let Some(strategy) = strategies.get(strategy_id) {
            strategy
                .event_sender
                .send(StrategyEvent::Control(command))
                .map_err(|e| anyhow::anyhow!("Failed to send control command: {}", e))?;
            Ok(())
        } else {
            Err(anyhow::anyhow!("Strategy not found: {}", strategy_id))
        }
    }

    /// 停止并移除策略
    pub async fn remove_strategy(&self, strategy_id: &StrategyId) -> anyhow::Result<()> {
        // 先发送停止命令
        self.send_control_command(strategy_id, StrategyControlCommand::Stop)
            .await?;

        // 等待一段时间让策略优雅关闭
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 从注册表中移除
        let mut strategies = self.registry.strategies.write().await;
        if strategies.remove(strategy_id).is_some() {
            info!("Strategy removed: {}", strategy_id);
            Ok(())
        } else {
            Err(anyhow::anyhow!("Strategy not found: {}", strategy_id))
        }
    }

    /// 停止引擎
    pub async fn stop(&self) -> anyhow::Result<()> {
        info!("Stopping Multi-Strategy Engine");

        {
            let mut state = self.state.write().await;
            *state = EngineState::Stopping;
        }

        // 停止所有策略
        let strategy_ids: Vec<_> = {
            let strategies = self.registry.strategies.read().await;
            strategies.keys().cloned().collect()
        };

        for strategy_id in strategy_ids {
            if let Err(e) = self.remove_strategy(&strategy_id).await {
                error!("Failed to stop strategy {}: {}", strategy_id, e);
            }
        }

        {
            let mut state = self.state.write().await;
            *state = EngineState::Stopped;
        }

        info!("Multi-Strategy Engine stopped");
        Ok(())
    }
}

impl QoSController {
    /// 创建新的QoS控制器
    pub fn new(config: QoSConfig) -> Self {
        let rate_limiter = if let Some(max_events) = config.max_events_per_second {
            Some(RateLimiter::new(max_events))
        } else {
            None
        };

        Self {
            config,
            rate_limiter,
            resource_monitor: ResourceMonitor::new(),
        }
    }

    /// 检查是否可以处理事件
    pub async fn can_process_event(&self) -> bool {
        if let Some(rate_limiter) = &self.rate_limiter {
            rate_limiter.check_rate().await
        } else {
            true
        }
    }

    /// 检查是否需要节流
    pub async fn should_throttle(&self) -> bool {
        if !self.config.enable_throttling {
            return false;
        }

        // 检查CPU使用率
        let cpu_usage = self.resource_monitor.cpu_usage.lock().await;
        if let Some(cpu_limit) = self.config.cpu_quota_percent {
            if *cpu_usage > cpu_limit {
                return true;
            }
        }

        // 检查内存使用
        let memory_usage = self.resource_monitor.memory_usage.lock().await;
        if let Some(memory_limit) = self.config.memory_limit_mb {
            if *memory_usage > memory_limit {
                return true;
            }
        }

        false
    }

    /// 更新资源使用情况
    pub async fn update_resource_usage(&self) {
        // 简化的资源监控实现
        // 实际实现中应该使用系统调用获取真实的CPU和内存使用情况
        use std::sync::atomic::{AtomicU64, Ordering};

        static CPU_COUNTER: AtomicU64 = AtomicU64::new(0);
        static MEM_COUNTER: AtomicU64 = AtomicU64::new(0);

        let cpu_val = CPU_COUNTER.fetch_add(1, Ordering::SeqCst) % 100;
        let mem_val = MEM_COUNTER.fetch_add(7, Ordering::SeqCst) % 1024;

        let mut cpu_usage = self.resource_monitor.cpu_usage.lock().await;
        *cpu_usage = cpu_val as f64; // 模拟CPU使用率

        let mut memory_usage = self.resource_monitor.memory_usage.lock().await;
        *memory_usage = mem_val + 128; // 模拟内存使用 128-1152 MB
    }
}

impl RateLimiter {
    pub fn new(max_events_per_second: u64) -> Self {
        Self {
            max_events_per_second,
            events_in_current_second: Arc::new(Mutex::new(0)),
            last_reset: Arc::new(Mutex::new(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            )),
        }
    }

    pub async fn check_rate(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut last_reset = self.last_reset.lock().await;
        let mut events_count = self.events_in_current_second.lock().await;

        // 如果过了一秒，重置计数器
        if now - *last_reset >= 1 {
            *last_reset = now;
            *events_count = 0;
        }

        // 检查是否超过限制
        if *events_count >= self.max_events_per_second {
            false
        } else {
            *events_count += 1;
            true
        }
    }
}

impl ResourceMonitor {
    pub fn new() -> Self {
        Self {
            start_time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            cpu_usage: Arc::new(Mutex::new(0.0)),
            memory_usage: Arc::new(Mutex::new(0)),
        }
    }
}

impl Default for MultiStrategyConfig {
    fn default() -> Self {
        Self {
            max_strategies: 32,
            default_qos_config: QoSConfig::default(),
            strategy_restart_policy: RestartPolicy::OnError {
                max_retries: 3,
                backoff: Duration::from_secs(5),
            },
            event_buffer_size: 1000,
            signal_buffer_size: 100,
            stats_update_interval: Duration::from_secs(5),
            enable_auto_restart: true,
        }
    }
}

impl Clone for QoSController {
    fn clone(&self) -> Self {
        Self::new(self.config.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_multi_strategy_engine_creation() {
        use crate::exchanges::ExchangeManager;

        let exchange_manager = Arc::new(ExchangeManager::new());
        let oms = Arc::new(CompleteOMS::new(exchange_manager));
        let event_hub = Arc::new(ExchangeEventHub::new());
        let config = MultiStrategyConfig::default();

        let engine = MultiStrategyEngine::new(oms, event_hub, config);

        let stats = engine.get_global_stats().await;
        assert_eq!(stats.total_strategies, 0);
        assert_eq!(stats.active_strategies, 0);
    }

    #[tokio::test]
    async fn test_qos_controller() {
        let config = QoSConfig::default();
        let qos = QoSController::new(config);

        // 测试事件处理检查
        assert!(qos.can_process_event().await);

        // 测试节流检查
        assert!(!qos.should_throttle().await); // 初始状态不应该节流
    }

    #[tokio::test]
    async fn test_rate_limiter() {
        let rate_limiter = RateLimiter::new(10); // 每秒10个事件

        // 前10个事件应该通过
        for _ in 0..10 {
            assert!(rate_limiter.check_rate().await);
        }

        // 第11个事件应该被限制
        assert!(!rate_limiter.check_rate().await);
    }
}
// Archived legacy example; see 02_strategy/
