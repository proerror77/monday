//! 系統建構器 - 宣告式裝配與註冊
use std::sync::Arc;
use tracing::{info, warn};
use serde::{Serialize, Deserialize};

use hft_core::*;
use ports::*;
use hft_core::HftError;
use engine::{Engine, EngineConfig, dataflow::{EventConsumer, FlipPolicy}, create_execution_queues, ExecutionQueueConfig, ExecutionWorkerConfig, spawn_execution_worker};
use tokio::sync::Mutex;
use integration;
use risk;

/// 系統配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemConfig {
    pub engine: SystemEngineConfig,
    pub venues: Vec<VenueConfig>,
    pub strategies: Vec<StrategyConfig>,
    pub risk: RiskConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemEngineConfig {
    pub queue_capacity: usize,
    pub stale_us: u64,
    pub top_n: usize,
    pub flip_policy: FlipPolicy,
}

// FlipPolicy 現在從 engine::dataflow 導入

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueConfig {
    pub name: String,
    pub venue_type: VenueType,
    pub ws_public: Option<String>,
    pub ws_private: Option<String>,
    pub rest: Option<String>,
    pub api_key: Option<String>,
    pub secret: Option<String>,
    pub passphrase: Option<String>,  // 新增：用於 Bitget 等需要 passphrase 的交易所
    pub execution_mode: Option<String>,  // 新增："Paper" | "Live"，預設為 Paper
    pub capabilities: VenueCapabilities,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VenueType {
    Bitget,
    Binance,
    Bybit,
    Okx,
    Mock,
}

  #[derive(Debug, Clone, Default, Serialize, Deserialize)]
  pub struct VenueCapabilities {
      pub ws_order_placement: bool,
      pub snapshot_crc: bool,
      pub all_in_one_topics: bool,
      pub private_ws_heartbeat: bool,
  }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub name: String,
    pub strategy_type: StrategyType,
    pub symbols: Vec<Symbol>,
    pub params: StrategyParams,
    pub risk_limits: StrategyRiskLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StrategyType {
    Trend,
    Arbitrage,
    MarketMaking,
    Dl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StrategyParams {
    Trend {
        ema_fast: u32,
        ema_slow: u32,
        rsi_period: u32,
    },
    Arbitrage {
        min_spread_bps: rust_decimal::Decimal,
        max_position: rust_decimal::Decimal,
        execution_timeout_ms: u64,
    },
    MarketMaking {
        spread_bps: rust_decimal::Decimal,
        max_inventory: rust_decimal::Decimal,
        skew_factor: rust_decimal::Decimal,
    },
    Dl {
        model_path: String,
        device: String,
        top_n: usize,
        window_size: Option<usize>,
        trigger_threshold: f64,
        output_threshold: f64,
        queue_capacity: usize,
        timeout_ms: u64,
        max_error_rate: f64,
        degradation_mode: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRiskLimits {
    pub max_notional: rust_decimal::Decimal,
    pub max_position: rust_decimal::Decimal,
    pub daily_loss_limit: rust_decimal::Decimal,
    pub cooldown_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub global_position_limit: rust_decimal::Decimal,
    pub global_notional_limit: rust_decimal::Decimal,
    pub max_daily_trades: u32,
    pub max_orders_per_second: u32,
    pub staleness_threshold_us: u64,
}

/// 系統建構器 - 使用構建者模式
pub struct SystemBuilder {
    config: SystemConfig,
    event_consumers: Vec<EventConsumer>,
    execution_clients: Vec<Box<dyn ExecutionClient>>,
    strategies: Vec<Box<dyn Strategy>>,
    risk_managers: Vec<Box<dyn RiskManager>>,
    // 僅登記市場流規劃，實際橋接在 Runtime::start() 內進行
    market_stream_plans: Vec<(VenueType, Vec<Symbol>)>,
}


impl SystemBuilder {
    pub fn new(config: SystemConfig) -> Self {
        Self {
            config,
            event_consumers: Vec::new(),
            execution_clients: Vec::new(),
            strategies: Vec::new(),
            risk_managers: Vec::new(),
            market_stream_plans: Vec::new(),
        }
    }
    
    /// 從 YAML 文件加載配置
    pub fn from_yaml(yaml_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let yaml_content = std::fs::read_to_string(yaml_path)?;
        let expanded_content = expand_env_vars(&yaml_content)?;
        let config: SystemConfig = serde_yaml::from_str(&expanded_content)?;
        Ok(Self::new(config))
    }
    
    /// 獲取系統配置（用於測試）
    pub fn config(&self) -> &SystemConfig {
        &self.config
    }
    
    /// 註冊事件消費者
    pub fn register_event_consumer(mut self, consumer: EventConsumer) -> Self {
        info!("註冊事件消費者");
        self.event_consumers.push(consumer);
        self
    }
    
    /// 註冊市場數據流（登記規劃，實際橋接在 Runtime::start() 進行）
    pub fn register_market_stream_plan(mut self, venue: VenueType, symbols: Vec<Symbol>) -> Self {
        info!("登記市場數據流規劃: {:?} {:?}", venue, symbols);
        self.market_stream_plans.push((venue, symbols));
        self
    }
    
    /// 註冊執行客戶端
    pub fn register_execution_client<E: ExecutionClient + 'static>(mut self, client: E) -> Self {
        info!("註冊執行客戶端: {}", std::any::type_name::<E>());
        self.execution_clients.push(Box::new(client));
        self
    }
    
    /// 註冊策略
    pub fn register_strategy<S: Strategy + 'static>(mut self, strategy: S) -> Self {
        info!("註冊策略: {}", std::any::type_name::<S>());
        self.strategies.push(Box::new(strategy));
        self
    }
    
    /// 註冊風控管理器
    pub fn register_risk_manager<R: RiskManager + 'static>(mut self, risk_manager: R) -> Self {
        info!("註冊風控管理器: {}", std::any::type_name::<R>());
        self.risk_managers.push(Box::new(risk_manager));
        self
    }
    
    /// 自動註冊適配器基於配置
    pub fn auto_register_adapters(mut self) -> Self {
        info!("自動註冊適配器...");

        // 1) 根據策略聚合需要訂閱的 symbols（去重）
        let mut symbol_set = std::collections::BTreeSet::new();
        for strat in &self.config.strategies {
            for s in &strat.symbols {
                symbol_set.insert(s.0.clone());
            }
        }
        // 如果策略未指定 symbols，預設訂閱 BTCUSDT 作為 demo (V2 API 移除業務線標記)
        if symbol_set.is_empty() { symbol_set.insert("BTCUSDT".to_string()); }
        let symbols: Vec<Symbol> = symbol_set.into_iter().map(Symbol).collect();

        // 2) 為每個 venue 登記市場數據流規劃（Runtime 啟動時橋接）
        for venue in self.config.venues.clone() {
            self = self.register_market_stream_plan(venue.venue_type.clone(), symbols.clone());
        }

        // 3) 註冊執行適配器
        let venues = self.config.venues.clone();
        for venue in venues {
            match venue.venue_type {
                VenueType::Bitget => { self = self.register_bitget_adapters(&venue); }
                VenueType::Binance => { self = self.register_binance_adapters(&venue); }
                VenueType::Bybit => { self = self.register_bybit_adapters(&venue); }
                VenueType::Okx => { warn!("OKX 適配器尚未實現"); }
                VenueType::Mock => { info!("Mock 適配器不需要執行客戶端配置"); }
            }
        }
        
        // 註冊策略
        let strategies = self.config.strategies.clone();
        for strategy_config in strategies {
            self = self.register_strategy_from_config(&strategy_config);
        }
        
        self
    }
    
    #[cfg(feature = "adapter-bitget-data")]
    fn register_bitget_adapters(mut self, venue: &VenueConfig) -> Self {
        info!("註冊 Bitget 適配器");
        // 行情規劃已由 auto_register_adapters 登記
        #[cfg(feature = "adapter-bitget-execution")]
        {
            // 解析執行模式：從 YAML 配置或預設為 Paper
            let execution_mode = match venue.execution_mode.as_deref().unwrap_or("Paper") {
                "Live" => adapter_bitget_execution::ExecutionMode::Live,
                _ => adapter_bitget_execution::ExecutionMode::Paper, // 預設和未知值都用 Paper
            };
            
            info!("配置 Bitget 執行模式: {:?}", execution_mode);
            
            // 從配置創建執行客戶端配置
            let execution_config = adapter_bitget_execution::BitgetExecutionConfig {
                credentials: integration::signing::BitgetCredentials {
                    api_key: venue.api_key.clone().unwrap_or_default(),
                    secret_key: venue.secret.clone().unwrap_or_default(),
                    passphrase: venue.passphrase.clone().unwrap_or_default(), // 從配置讀取 passphrase
                },
                mode: execution_mode,
                rest_base_url: venue.rest.clone().unwrap_or_default(),
                ws_private_url: venue.ws_private.clone().unwrap_or_default(),
                timeout_ms: 5000, // 5 秒超時
            };
            
            // 註冊執行適配器
            match adapter_bitget_execution::BitgetExecutionClient::new(execution_config) {
                Ok(execution_client) => {
                    self = self.register_execution_client(execution_client);
                }
                Err(e) => {
                    warn!("無法創建 Bitget 執行客戶端: {}", e);
                }
            }
        }
        
        self
    }
    
    #[cfg(not(feature = "adapter-bitget-data"))]
    fn register_bitget_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Bitget 適配器未啟用 (缺少 feature flag)");
        self
    }
    
    #[cfg(feature = "adapter-binance-data")]
    fn register_binance_adapters(mut self, _venue: &VenueConfig) -> Self {
        info!("註冊 Binance 適配器");
        // 行情規劃已由 auto_register_adapters 登記
        #[cfg(feature = "adapter-binance-execution")]
        {
            // 註冊執行適配器
            let execution_client = adapter_binance_execution::BinanceExecutionClient::new();
            self = self.register_execution_client(execution_client);
        }

        self
    }
    
    #[cfg(not(feature = "adapter-binance-data"))]
    fn register_binance_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Binance 適配器未啟用 (缺少 feature flag)");
        self
    }
    
    fn register_bybit_adapters(self, _venue: &VenueConfig) -> Self {
        warn!("Bybit 適配器為占位符實現，跳過註冊");
        self
    }
    
    fn register_strategy_from_config(self, strategy_config: &StrategyConfig) -> Self {
        match strategy_config.strategy_type {
            StrategyType::Trend => {
                #[cfg(feature = "strategy-trend")]
                {
                    info!("註冊趨勢策略: {}", strategy_config.name);
                    let mut s = self;
                    for sym in &strategy_config.symbols {
                        let strat = strategy_trend::create_trend_strategy(sym.clone(), None);
                        s = s.register_strategy(strat);
                    }
                    return s;
                }
                #[cfg(not(feature = "strategy-trend"))]
                {
                    warn!("趨勢策略未啟用 (缺少 feature flag)");
                }
            }
            StrategyType::Arbitrage => {
                #[cfg(feature = "strategy-arbitrage")]
                {
                    info!("註冊套利策略: {}", strategy_config.name);
                    // TODO: 從配置創建策略實例
                    // let strategy = strategy_arbitrage::ArbitrageStrategy::from_config(strategy_config);
                    // self = self.register_strategy(strategy);
                }
                
                #[cfg(not(feature = "strategy-arbitrage"))]
                {
                    warn!("套利策略未啟用 (缺少 feature flag)");
                }
            }
            StrategyType::MarketMaking => {
                warn!("做市策略尚未實現");
            }
            StrategyType::Dl => {
                // TODO: DL strategy support will be added in a future task
                warn!("DL 策略尚未實現 - 將在後續任務中添加支持");
            }
        }
        
        self
    }
    
    /// 建構並啟動系統
    pub fn build(self) -> SystemRuntime {
        info!("建構系統運行時...");
        
        // 從系統配置創建引擎配置
        let engine_config = EngineConfig {
            ingestion: engine::dataflow::IngestionConfig {
                queue_capacity: self.config.engine.queue_capacity,
                stale_threshold_us: self.config.engine.stale_us,
                flip_policy: self.config.engine.flip_policy.clone(),
                backpressure_policy: engine::dataflow::BackpressurePolicy::DropNew, // 默认丢弃新事件，保持稳定性
            },
            max_events_per_cycle: 100,
            aggregation_symbols: vec![], // top_n 暫時不用，留待聚合層實現
        };
        
        // 創建引擎
        let mut engine = Engine::new(engine_config);
        
        // 註冊組件到引擎
        for consumer in self.event_consumers {
            engine.register_event_consumer(consumer);
        }
        
        for client in self.execution_clients {
            engine.register_execution_client_boxed(client);
        }
        
        for strategy in self.strategies {
            engine.register_strategy_boxed(strategy);
        }
        
        // 从配置注册风控管理器
        let risk_config = risk::RiskConfig {
            max_position_per_symbol: Quantity::from_f64(100.0).unwrap(),
            max_global_notional: self.config.risk.global_notional_limit,
            max_orders_per_second: self.config.risk.max_orders_per_second,
            order_cooldown_ms: 100, // 默认100ms冷却期
            staleness_threshold_us: self.config.risk.staleness_threshold_us,
            max_daily_loss: rust_decimal::Decimal::from(10000), // 默认10000损失限额
            aggressive_mode: false,
        };
        let risk_manager = risk::DefaultRiskManager::new(risk_config);
        engine.register_risk_manager(risk_manager);
        info!("已注册默认风控管理器");
        
        SystemRuntime { 
            engine: Arc::new(Mutex::new(engine)), 
            config: self.config, 
            tasks: Vec::new(),
            execution_worker_tasks: Vec::new(),
            market_plans: self.market_stream_plans 
        }
    }
}

/// 系統運行時
pub struct SystemRuntime {
    pub engine: Arc<Mutex<Engine>>,
    pub config: SystemConfig,
    // 後台任務控制
    tasks: Vec<tokio::task::JoinHandle<()>>,
    // 执行 worker 任务
    execution_worker_tasks: Vec<tokio::task::JoinHandle<Result<(), HftError>>>,
    // 登記的市場流規劃
    market_plans: Vec<(VenueType, Vec<Symbol>)>,
}

impl SystemRuntime {
    /// 啟動系統
    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("啟動系統運行時...");
        // 1) 橋接市場流（依登記規劃）
        let bridge_cfg = engine::AdapterBridgeConfig {
            ingestion: engine::dataflow::IngestionConfig {
                queue_capacity: self.config.engine.queue_capacity,
                stale_threshold_us: self.config.engine.stale_us,
                flip_policy: self.config.engine.flip_policy.clone(),
                backpressure_policy: engine::dataflow::BackpressurePolicy::DropNew,
            },
            max_concurrent_adapters: 8,
        };

        let bridge = engine::AdapterBridge::new(bridge_cfg);
        // 橋接每個登記的市場數據流
        for (venue_type, symbols) in self.market_plans.clone() {
            match venue_type {
                VenueType::Bitget => {
                    #[cfg(feature = "adapter-bitget-data")]
                    {
                        // 使用零拷貝版本 - 直接寫入 SPSC，移除 tokio::mpsc 中間層
                        let mut stream = adapter_bitget_data::ZeroCopyBitgetStream::new();
                        
                        // 創建配對的 EventIngester，引擎自動註冊對應的 EventConsumer
                        let ingester = {
                            let mut engine_lock = self.engine.lock().await;
                            engine_lock.create_event_ingester_pair()
                        };
                        
                        // 設置零拷貝適配器的攝取器
                        stream.set_ingester(ingester);
                        
                        // 訂閱符號（返回狀態流，實際數據已直接寫入 SPSC）
                        let _status_stream = stream.subscribe(symbols).await?;
                        
                        info!("Bitget 零拷貝適配器已啟用，直接寫入 SPSC");
                    }
                }
                VenueType::Binance => {
                    #[cfg(feature = "adapter-binance-data")]
                    {
                        let stream = adapter_binance_data::BinanceMarketStream::new();
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                    }
                }
                VenueType::Bybit => {
                    warn!("Bybit 適配器為占位符實現，跳過註冊");
                }
                VenueType::Mock => {
                    #[cfg(feature = "adapter-mock-data")]
                    {
                        let stream = adapter_mock_data::MockMarketStream::new();
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                    }
                }
                _ => {}
            }
        }

        // 2) 設置執行队列系统並啟動执行 worker
        let queue_config = ExecutionQueueConfig::default();
        let (engine_queues, mut worker_queues) = create_execution_queues(queue_config);
        
        // 获取执行客户端从引擎移出，设置队列
        let (execution_clients, engine_notify) = {
            let mut eng = self.engine.lock().await;
            let notify = eng.get_wakeup_notify();
            eng.set_execution_queues(engine_queues);
            let clients = eng.take_execution_clients();
            (clients, notify)
        };
        
        // 为执行队列设置引擎唤醒通知器
        worker_queues.set_engine_notify(engine_notify);
        
        // 启动执行 worker (即使没有执行客户端也启动，用于队列系统)
        let worker_config = ExecutionWorkerConfig {
            name: "main_execution_worker".to_string(),
            ..Default::default()
        };
        
        // Capture client count before move
        let client_count = execution_clients.len();
        let worker_handle = spawn_execution_worker(worker_config, worker_queues, execution_clients);
        self.execution_worker_tasks.push(worker_handle);
        info!("已启动执行 worker (客户端数量: {})", client_count);

        // 3) 啟動引擎主循環（後台，事件驅動）
        let engine_arc = self.engine.clone();
        
        // 獲取引擎唤醒通知器
        let notify = {
            let eng = engine_arc.lock().await;
            eng.get_wakeup_notify()
        };
        
        let handle = tokio::spawn(async move {
            let mut backoff_ms = 1u64; // 从 1ms 开始
            let max_backoff_ms = 100u64; // 最大 100ms
            
            loop {
                // 事件驱动：等待唤醒通知或自适应超时
                let was_notified = tokio::select! {
                    _ = notify.notified() => {
                        // 收到唤醒通知，重置 backoff
                        backoff_ms = 1;
                        true
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {
                        // 超时，增加 backoff（指数退避）
                        false
                    }
                };
                
                let (tick_result, had_activity) = { 
                    let mut eng = engine_arc.lock().await; 
                    let prev_stats = eng.get_statistics();
                    let tick_result = eng.tick();
                    let new_stats = eng.get_statistics();
                    
                    // 检查是否有新的事件处理活动（基于周期数变化来判断）
                    let had_activity = new_stats.cycle_count > prev_stats.cycle_count;
                    
                    (tick_result, had_activity)
                };
                
                if let Err(e) = tick_result { 
                    tracing::error!("Engine tick error: {}", e); 
                }
                
                // 自适应退避：如果有活动或被通知唤醒，保持低延迟；否则增加退避
                if !was_notified {
                    if had_activity {
                        // 有活动但未被通知，可能还有更多事件，减少退避
                        backoff_ms = (backoff_ms / 2).max(1);
                    } else {
                        // 无活动，增加退避（指数增长）
                        backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
                    }
                }
                
                let running = { let eng = engine_arc.lock().await; eng.get_statistics().is_running };
                if !running { break; }
            }
        });
        self.tasks.push(handle);

        info!("系統運行時已啟動（引擎背景運行）");
        Ok(())
    }
    
    /// 停止系統
    pub async fn stop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("停止系統運行時...");
        // 停止引擎
        self.engine.lock().await.stop();
        // 等待背景任務退出
        for t in self.tasks.drain(..) { let _ = t.await; }
        info!("系統運行時已停止");
        Ok(())
    }
    
    /// 獲取市場視圖快照
    pub async fn get_market_view(&self) -> Arc<engine::aggregation::MarketView> {
        self.engine.lock().await.get_market_view()
    }
    
    /// 獲取賬戶視圖快照
    pub async fn get_account_view(&self) -> Arc<ports::AccountView> {
        self.engine.lock().await.get_account_view()
    }

    /// 測試：通过执行队列发送订单意图 (异步处理)
    pub async fn place_test_order(&self, symbol: &str) -> Result<hft_core::OrderId, Box<dyn std::error::Error>> {
        use hft_core::{Symbol, Quantity, Side, OrderType, TimeInForce};
        let intent = ports::OrderIntent {
            symbol: Symbol(symbol.to_string()),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.001)?,
            order_type: OrderType::Market,
            price: None,
            time_in_force: TimeInForce::GTC,
            strategy_id: "test_order".to_string(),
        };
        
        // 注意：现在订单通过队列系统异步处理，无法直接返回 OrderId
        // 返回一个测试用的 OrderId，实际的执行结果通过 ExecutionEvent 异步回报
        let test_order_id = hft_core::OrderId(format!("test_{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        info!("测试订单已发送到执行队列: {} {}", symbol, test_order_id.0);
        Ok(test_order_id)
    }
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            engine: SystemEngineConfig {
                queue_capacity: 32768,
                stale_us: 3000,
                top_n: 10,
                flip_policy: FlipPolicy::OnUpdate,
            },
            venues: Vec::new(),
            strategies: Vec::new(),
            risk: RiskConfig {
                global_position_limit: rust_decimal::Decimal::from(1000000),
                global_notional_limit: rust_decimal::Decimal::from(10000000),
                max_daily_trades: 10000,
                max_orders_per_second: 100,
                staleness_threshold_us: 5000,
            },
        }
    }
}

/// 環境變量展開：將 ${VAR} 模式替換為環境變量值
fn expand_env_vars(content: &str) -> Result<String, Box<dyn std::error::Error>> {
    use std::env;
    use std::collections::HashMap;
    
    let mut result = content.to_string();
    
    // 預編譯正則表達式來匹配 ${VAR} 模式
    let re = regex::Regex::new(r"\$\{([^}]+)\}")?;
    
    // 收集所有需要替換的變量
    let mut replacements = HashMap::new();
    
    for cap in re.captures_iter(content) {
        let full_match = &cap[0]; // ${VAR}
        let var_name = &cap[1];   // VAR
        
        if !replacements.contains_key(full_match) {
            match env::var(var_name) {
                Ok(value) => {
                    replacements.insert(full_match.to_string(), value);
                }
                Err(_) => {
                    // 環境變量未設置時，保持原始格式不變並警告
                    warn!("環境變量 {} 未設置，保留原始格式", var_name);
                    replacements.insert(full_match.to_string(), full_match.to_string());
                }
            }
        }
    }
    
    // 執行替換
    for (pattern, value) in replacements {
        result = result.replace(&pattern, &value);
    }
    
    Ok(result)
}