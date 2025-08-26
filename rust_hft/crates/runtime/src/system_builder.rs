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
use serde_yaml::Value as YamlValue;
use std::collections::{BTreeSet, HashMap};

#[cfg(feature = "redis")]
use serde_json;

#[cfg(feature = "redis")]
use engine::aggregation;

/// 系統配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemConfig {
    pub engine: SystemEngineConfig,
    pub venues: Vec<VenueConfig>,
    pub strategies: Vec<StrategyConfig>,
    pub risk: RiskConfig,
    pub infra: Option<InfraConfig>,
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

/// 基礎設施配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfraConfig {
    pub redis: Option<RedisConfig>,
    pub clickhouse: Option<ClickHouseConfig>,
}

/// Redis 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    pub url: String,
}

/// ClickHouse 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickHouseConfig {
    pub url: String,
    pub database: Option<String>,
}

/// 模板化配置（可選）：商品分組與策略模板/綁定
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct InstrumentsSection {
    pub groups: Vec<InstrumentGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstrumentGroup {
    pub name: String,
    #[serde(default)]
    pub symbols: Vec<Symbol>,
    #[serde(default)]
    pub selector: Option<String>, // TODO: 後續支持正則匹配
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StrategiesSection {
    #[serde(default)]
    pub templates: Vec<StrategyTemplate>,
    #[serde(default)]
    pub bindings: Vec<StrategyBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StrategyTemplate {
    pub id: String,
    pub strategy_type: StrategyType,
    pub params: StrategyParams,
    pub risk: StrategyRiskLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StrategyOverrides {
    #[serde(default)]
    pub risk: Option<StrategyRiskLimits>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StrategyBinding {
    pub template: String,
    pub apply_to: Vec<String>, // e.g. ["group:layer1", "symbol:ETHUSDT"]
    #[serde(default)]
    pub overrides: HashMap<String, StrategyOverrides>, // symbol -> overrides
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

        // 先解析為動態 YAML
        let mut root: YamlValue = serde_yaml::from_str(&expanded_content)?;
        // 展開模板到最終 strategies 列表
        if let Some(expanded) = expand_templates_into_strategies(&root)? {
            if let YamlValue::Mapping(ref mut map) = root {
                map.insert(YamlValue::from("strategies"), serde_yaml::to_value(&expanded)?);
                // 解析後移除 instruments / templates/bindings 節點：
                map.remove(&YamlValue::from("instruments"));
                if let Some(YamlValue::Mapping(mut strat_map)) = map.remove(&YamlValue::from("strategies")) {
                    // 已將 strategies 設為列表，恢復插入
                    map.insert(YamlValue::from("strategies"), serde_yaml::to_value(&expanded)?);
                }
            }
        }
        let config: SystemConfig = serde_yaml::from_value(root)?;
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
        let mut symbol_set = BTreeSet::new();
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

        // 啟動 Redis 導出任務（如果配置了 Redis）
        #[cfg(feature = "redis")]
        if let Some(infra) = &self.config.infra {
            if let Some(redis_config) = &infra.redis {
                self.spawn_redis_exporter(redis_config.clone()).await?;
            }
        }

        // 啟動 ClickHouse Writer 任務（如果配置了 ClickHouse）
        #[cfg(feature = "infra-clickhouse")]
        if let Some(infra) = &self.config.infra {
            if let Some(clickhouse_config) = &infra.clickhouse {
                self.spawn_clickhouse_writer(clickhouse_config.clone()).await?;
            }
        }

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
        
        // 提交訂單意圖到執行隊列
        {
            let mut engine_lock = self.engine.lock().await;
            engine_lock.submit_order_intent(intent)?;
        }
        
        // 生成測試用的 OrderId，實際執行結果通過 ExecutionEvent 異步回報
        let test_order_id = hft_core::OrderId(format!("test_{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
        info!("測試訂單已成功提交到執行隊列: {} {}", symbol, test_order_id.0);
        Ok(test_order_id)
    }

    /// 啟動 Redis 導出任務
    #[cfg(feature = "redis")]
    async fn spawn_redis_exporter(&mut self, redis_config: RedisConfig) -> Result<(), Box<dyn std::error::Error>> {
        use engine::aggregation::TopNSnapshot;
        
        // 計算中間價格的輔助函數
        fn calculate_mid_price(orderbook: &TopNSnapshot) -> Option<f64> {
            if !orderbook.bid_prices.is_empty() && !orderbook.ask_prices.is_empty() {
                let best_bid = orderbook.bid_prices[0].to_f64();
                let best_ask = orderbook.ask_prices[0].to_f64();
                Some((best_bid + best_ask) / 2.0)
            } else {
                None
            }
        }
        
        // 計算價差的輔助函數
        fn calculate_spread(orderbook: &TopNSnapshot) -> Option<f64> {
            if !orderbook.bid_prices.is_empty() && !orderbook.ask_prices.is_empty() {
                let best_bid = orderbook.bid_prices[0].to_f64();
                let best_ask = orderbook.ask_prices[0].to_f64();
                Some(best_ask - best_bid)
            } else {
                None
            }
        }
        use redis::{AsyncCommands, Client};
        
        info!("啟動 Redis 導出器，連接到: {}", redis_config.url);
        
        // 測試連接
        let client = Client::open(redis_config.url.as_str())?;
        let mut conn = client.get_async_connection().await?;
        let _: String = redis::cmd("PING").query_async(&mut conn).await?;
        info!("Redis 連接測試成功");
        
        // 克隆引擎引用以供任務使用
        let engine_arc = self.engine.clone();
        let redis_url = redis_config.url.clone();
        
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
            let client = match Client::open(redis_url.as_str()) {
                Ok(client) => client,
                Err(e) => {
                    tracing::error!("Redis 客戶端創建失敗: {}", e);
                    return;
                }
            };
            
            loop {
                interval.tick().await;
                
                // 獲取當前市場視圖
                let market_view = {
                    let engine = engine_arc.lock().await;
                    engine.get_market_view()
                };
                
                // 連接 Redis 並導出數據
                match client.get_async_connection().await {
                    Ok(mut conn) => {
                        // 為每個訂單簿創建簡化的快照
                        for (symbol, orderbook) in &market_view.orderbooks {
                            let snapshot_data = serde_json::json!({
                                "symbol": symbol.0,
                                "mid_price": calculate_mid_price(orderbook),
                                "spread": calculate_spread(orderbook),
                                "timestamp": market_view.timestamp,
                                "bid_levels": orderbook.bid_prices.len(),
                                "ask_levels": orderbook.ask_prices.len(),
                                "version": market_view.version
                            });
                            
                            // 寫入 Redis Streams
                            let result: Result<String, redis::RedisError> = conn.xadd(
                                "market_snapshots",
                                "*",
                                &[
                                    ("symbol", symbol.0.as_str()),
                                    ("data", snapshot_data.to_string().as_str())
                                ]
                            ).await;
                            
                            if let Err(e) = result {
                                tracing::warn!("Redis 寫入失敗 {}: {}", symbol.0, e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Redis 連接失敗: {}", e);
                    }
                }
            }
        });
        
        self.tasks.push(handle);
        info!("Redis 導出任務已啟動");
        Ok(())
    }

    /// 啟動 ClickHouse Writer 任務
    #[cfg(feature = "infra-clickhouse")]
    async fn spawn_clickhouse_writer(&mut self, clickhouse_config: ClickHouseConfig) -> Result<(), Box<dyn std::error::Error>> {
        use clickhouse::{Client, Row};
        use serde::{Deserialize, Serialize};
        
        info!("啟動 ClickHouse Writer，連接到: {}", clickhouse_config.url);
        
        // 測試連接
        let client = Client::default()
            .with_url(&clickhouse_config.url)
            .with_database(clickhouse_config.database.as_ref().unwrap_or(&"default".to_string()));
        
        // 測試連接可用性
        let result: Result<Vec<u8>, clickhouse::error::Error> = client
            .query("SELECT 1")
            .fetch_all()
            .await;
        
        if let Err(e) = result {
            return Err(format!("ClickHouse 連接測試失敗: {}", e).into());
        }
        info!("ClickHouse 連接測試成功");

        // 定義 lob_depth 表的行格式
        #[derive(Row, Serialize, Deserialize)]
        struct LobDepthRow {
            timestamp: u64,
            symbol: String,
            venue: String,
            side: String,        // "bid" or "ask"
            level: u32,          // 0 = best, 1 = second, etc.
            price: f64,
            quantity: f64,
        }

        // 克隆引擎引用以供任務使用
        let engine_arc = self.engine.clone();
        
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
            let mut batch = Vec::<LobDepthRow>::new();
            
            loop {
                interval.tick().await;
                
                // 獲取當前市場視圖
                let market_view = {
                    let engine = engine_arc.lock().await;
                    engine.get_market_view()
                };
                
                batch.clear();
                
                // 轉換 MarketView 到 LobDepthRow 批量數據
                for (symbol, orderbook) in &market_view.orderbooks {
                    let timestamp = market_view.timestamp;
                    let venue = "COMBINED"; // 可以後續擴展為多交易所
                    
                    // 處理買盤深度
                    for (level, (&price, &quantity)) in orderbook.bid_prices
                        .iter()
                        .zip(orderbook.bid_quantities.iter())
                        .enumerate() 
                    {
                        if level < 10 { // 只保存前10檔
                            batch.push(LobDepthRow {
                                timestamp,
                                symbol: symbol.0.clone(),
                                venue: venue.to_string(),
                                side: "bid".to_string(),
                                level: level as u32,
                                price: price.to_f64(),
                                quantity: quantity.to_f64(),
                            });
                        }
                    }
                    
                    // 處理賣盤深度
                    for (level, (&price, &quantity)) in orderbook.ask_prices
                        .iter()
                        .zip(orderbook.ask_quantities.iter())
                        .enumerate()
                    {
                        if level < 10 { // 只保存前10檔
                            batch.push(LobDepthRow {
                                timestamp,
                                symbol: symbol.0.clone(),
                                venue: venue.to_string(),
                                side: "ask".to_string(),
                                level: level as u32,
                                price: price.to_f64(),
                                quantity: quantity.to_f64(),
                            });
                        }
                    }
                }
                
                // 批量寫入 ClickHouse
                if !batch.is_empty() {
                    match client
                        .insert("lob_depth")
                        .expect("Failed to create insert")
                        .write(&batch)
                        .await
                    {
                        Ok(()) => {
                            tracing::debug!("成功寫入 {} 條 lob_depth 記錄", batch.len());
                        }
                        Err(e) => {
                            tracing::warn!("ClickHouse 寫入失敗: {}", e);
                        }
                    }
                }
            }
        });
        
        self.tasks.push(handle);
        info!("ClickHouse Writer 已啟動，每秒批量寫入 lob_depth 表");
        Ok(())
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
            infra: None,
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

/// 將模板化配置展開為最終 strategies 列表（若存在模板節點）
fn expand_templates_into_strategies(root: &YamlValue) -> Result<Option<Vec<StrategyConfig>>, Box<dyn std::error::Error>> {
    // 讀取 instruments
    let mut group_map: HashMap<String, Vec<Symbol>> = HashMap::new();
    if let Some(instruments) = root.get("instruments") {
        if let Ok(sec) = serde_yaml::from_value::<InstrumentsSection>(instruments.clone()) {
            for g in sec.groups {
                // 目前僅支持顯式 symbols；selector TODO
                if g.selector.is_some() {
                    warn!("instrument group '{}' 使用 selector 暫未實作，請使用 symbols 顯式列出", g.name);
                }
                group_map.insert(g.name, g.symbols);
            }
        }
    }

    // strategies 有兩種情況：
    // 1) 直接是列表（舊格式）→ 不處理（返回 None）
    // 2) 是 mapping，包含 templates/bindings → 展開
    let strategies_node = match root.get("strategies") { Some(v) => v, None => return Ok(None) };
    if strategies_node.is_sequence() { return Ok(None); }

    let sec: StrategiesSection = match serde_yaml::from_value(strategies_node.clone()) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };

    if sec.templates.is_empty() || sec.bindings.is_empty() {
        return Ok(None);
    }

    let tpl_map: HashMap<String, StrategyTemplate> = sec.templates.into_iter().map(|t| (t.id.clone(), t)).collect();
    let mut out: Vec<StrategyConfig> = Vec::new();

    for b in sec.bindings {
        let Some(tpl) = tpl_map.get(&b.template) else {
            warn!("找不到策略模板: {}，跳過綁定", b.template);
            continue;
        };

        // 收集作用範圍內的 symbols
        let mut symbols: Vec<Symbol> = Vec::new();
        for target in &b.apply_to {
            if let Some(rest) = target.strip_prefix("group:") {
                if let Some(gs) = group_map.get(rest) {
                    symbols.extend(gs.clone());
                } else {
                    warn!("未定義的商品組: {}，跳過", rest);
                }
            } else if let Some(sym) = target.strip_prefix("symbol:") {
                symbols.push(Symbol(sym.to_string()));
            } else {
                warn!("未知的 apply_to 項: {}，應為 group:<name> 或 symbol:<SYM>", target);
            }
        }

        // 產生實例化策略
        for sym in symbols {
            let mut risk = tpl.risk.clone();
            if let Some(ov) = b.overrides.get(&sym.0) {
                if let Some(r) = &ov.risk { risk = r.clone(); }
            }

            let cfg = StrategyConfig {
                name: format!("{}:{}", tpl.id, sym.0),
                strategy_type: tpl.strategy_type.clone(),
                symbols: vec![sym],
                params: tpl.params.clone(),
                risk_limits: risk,
            };
            out.push(cfg);
        }
    }

    Ok(Some(out))
}
