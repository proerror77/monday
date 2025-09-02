//! 系統建構器 - 宣告式裝配與註冊
use std::sync::Arc;
use tracing::{info, warn};
use serde::{Serialize, Deserialize};
use crate::sharding::{ShardConfig, NoSharding};

use hft_core::*;
use ports::*;
use hft_core::HftError;
use engine::{Engine, EngineConfig, dataflow::{EventConsumer, FlipPolicy}, create_execution_queues, ExecutionQueueConfig, ExecutionWorkerConfig, spawn_execution_worker};
use tokio::sync::Mutex;
use integration;
use risk;
use serde_yaml::Value as YamlValue;
use std::collections::{BTreeSet, HashMap};
use rust_decimal::prelude::ToPrimitive;

#[cfg(feature = "redis")]
use serde_json;

#[cfg(feature = "redis")]
use engine::aggregation;

// ClickHouse 插入行結構（模組級，避免局部 struct 導致 Row derive 失效）
#[cfg(feature = "infra-clickhouse")]
#[derive(clickhouse::Row, serde::Serialize, serde::Deserialize)]
#[derive(Debug)]
struct LobDepthRow {
    timestamp: u64,
    symbol: String,
    venue: String,
    side: String,        // "bid" or "ask"
    level: u32,          // 0 = best, 1 = second, etc.
    price: f64,
    quantity: f64,
}

// ClickHouse 引擎統計行（每秒一次，用於計算填單率等）
#[cfg(feature = "infra-clickhouse")]
#[derive(clickhouse::Row, serde::Serialize, serde::Deserialize)]
#[derive(Debug)]
struct EngineStatsRow {
    timestamp: u64,
    orders_submitted: u64,
    orders_filled: u64,
    delta_submitted: u64,
    delta_filled: u64,
    fill_rate: f64,
    execution_events_processed: u64,
}

#[cfg(feature = "infra-clickhouse")]
#[derive(clickhouse::Row, serde::Serialize, serde::Deserialize)]
#[derive(Debug)]
struct FactorRow {
    timestamp: u64,
    symbol: String,
    venue: String,
    obi_l1: f64,
    obi_l5: f64,
    spread_bps: f64,
    microprice: f64,
    depth_ratio_l5: f64,
    ofi_l1: f64,
    ofi_l5: f64,
    mid_change_bps: f64,
}

/// 系統配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemConfig {
    pub engine: SystemEngineConfig,
    pub venues: Vec<VenueConfig>,
    pub strategies: Vec<StrategyConfig>,
    pub risk: RiskConfig,
    /// 🔥 Phase 1.5: 執行路由器配置（可選）
    pub router: Option<ports::RouterConfig>,
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
      #[serde(default)]
      pub use_incremental_books: bool,
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
    Imbalance,
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
    Imbalance {
        obi_threshold: f64,
        lot: rust_decimal::Decimal,
        top_levels: usize,
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
    /// Risk manager type: "Default" or "Enhanced"
    #[serde(default = "default_risk_type")]
    pub risk_type: String,
    
    /// Base risk settings (applied to all strategies)
    pub global_position_limit: rust_decimal::Decimal,
    pub global_notional_limit: rust_decimal::Decimal,
    pub max_daily_trades: u32,
    pub max_orders_per_second: u32,
    pub staleness_threshold_us: u64,
    
    /// Enhanced risk settings (only used if risk_type = "Enhanced")
    #[serde(default)]
    pub enhanced: Option<EnhancedRiskSettings>,
    
    /// Per-strategy risk overrides
    #[serde(default)]
    pub strategy_overrides: HashMap<String, StrategyRiskOverride>,
}

fn default_risk_type() -> String {
    "Default".to_string()
}

/// Enhanced risk manager specific settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedRiskSettings {
    // Position and notional limits
    pub max_position_per_symbol: rust_decimal::Decimal,
    pub max_order_notional: rust_decimal::Decimal,
    
    // Advanced rate limiting
    pub max_orders_per_minute: u32,
    pub max_orders_per_hour: u32,
    
    // Cooldown periods (milliseconds)
    pub global_order_cooldown_ms: u64,
    pub symbol_order_cooldown_ms: u64,
    pub failed_order_penalty_ms: u64,
    
    // Data staleness thresholds (microseconds)
    pub market_data_staleness_us: u64,
    pub inference_staleness_us: u64,
    pub execution_report_staleness_us: u64,
    
    // Loss control
    pub max_daily_loss: rust_decimal::Decimal,
    pub max_drawdown_pct: f64,
    pub max_consecutive_losses: u32,
    pub max_position_loss_pct: f64,
    
    // Circuit breaker
    pub circuit_breaker_enabled: bool,
    pub cb_daily_loss_threshold: rust_decimal::Decimal,
    pub cb_drawdown_threshold: f64,
    pub cb_consecutive_losses: u32,
    pub cb_recovery_time_minutes: u64,
    
    // Trading window (optional)
    pub trading_window: Option<TradingWindowConfig>,
    
    // System control
    pub aggressive_mode: bool,
    pub dry_run_mode: bool,
}

impl Default for EnhancedRiskSettings {
    fn default() -> Self {
        Self {
            max_position_per_symbol: rust_decimal::Decimal::from(100),
            max_order_notional: rust_decimal::Decimal::from(50000),
            max_orders_per_minute: 300,
            max_orders_per_hour: 3000,
            global_order_cooldown_ms: 50,
            symbol_order_cooldown_ms: 100,
            failed_order_penalty_ms: 1000,
            market_data_staleness_us: 3000,
            inference_staleness_us: 5000,
            execution_report_staleness_us: 10000,
            max_daily_loss: rust_decimal::Decimal::from(10000),
            max_drawdown_pct: 5.0,
            max_consecutive_losses: 5,
            max_position_loss_pct: 2.0,
            circuit_breaker_enabled: true,
            cb_daily_loss_threshold: rust_decimal::Decimal::from(8000),
            cb_drawdown_threshold: 4.0,
            cb_consecutive_losses: 4,
            cb_recovery_time_minutes: 30,
            trading_window: None,
            aggressive_mode: false,
            dry_run_mode: false,
        }
    }
}

/// Trading window configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingWindowConfig {
    pub start_hour_utc: u8,
    pub end_hour_utc: u8,
    pub allowed_weekdays: Vec<String>, // ["Monday", "Tuesday", etc.]
    #[serde(default)]
    pub market_holidays: Vec<String>, // ISO date strings
}

/// Per-strategy risk overrides
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRiskOverride {
    /// Override global position limit for this strategy
    pub max_position: Option<rust_decimal::Decimal>,
    
    /// Override max notional for this strategy
    pub max_notional: Option<rust_decimal::Decimal>,
    
    /// Override order rate limits
    pub max_orders_per_second: Option<u32>,
    
    /// Override cooldown period
    pub order_cooldown_ms: Option<u64>,
    
    /// Override staleness threshold
    pub staleness_threshold_us: Option<u64>,
    
    /// Override daily loss limit for this strategy
    pub max_daily_loss: Option<rust_decimal::Decimal>,
    
    /// Strategy-specific aggressive mode
    pub aggressive_mode: Option<bool>,
    
    /// Enhanced-specific overrides (only used with Enhanced risk manager)
    #[serde(default)]
    pub enhanced_overrides: Option<StrategyEnhancedRiskOverride>,
}

/// Enhanced risk manager specific per-strategy overrides
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyEnhancedRiskOverride {
    pub max_drawdown_pct: Option<f64>,
    pub max_consecutive_losses: Option<u32>,
    pub circuit_breaker_enabled: Option<bool>,
    pub dry_run_mode: Option<bool>,
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
    // 分片配置
    shard_config: Option<ShardConfig>,
    // 🔥 Phase 1: 跟蹤執行客戶端對應的交易所
    execution_client_venues: Vec<VenueId>,
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
            shard_config: None,
            execution_client_venues: Vec::new(),
        }
    }
    
    /// 設置分片配置
    pub fn with_sharding(mut self, shard_config: ShardConfig) -> Self {
        info!("設置分片配置: {}", shard_config.get_stats());
        self.shard_config = Some(shard_config);
        self
    }
    
    /// 從 YAML 文件加載配置
    pub fn from_yaml(yaml_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let yaml_content = std::fs::read_to_string(yaml_path)?;
        let expanded_content = expand_env_vars(&yaml_content)?;

        // 嘗試直接解析為強類型（無模板情況下更穩健）
        if let Ok(config) = serde_yaml::from_str::<SystemConfig>(&expanded_content) {
            return Ok(Self::new(config));
        }

        // 回退：解析為動態 YAML，支援模板展開
        let mut root: YamlValue = serde_yaml::from_str(&expanded_content)?;
        if let Some(expanded) = expand_templates_into_strategies(&root)? {
            if let YamlValue::Mapping(ref mut map) = root {
                map.insert(YamlValue::from("strategies"), serde_yaml::to_value(&expanded)?);
                map.remove(&YamlValue::from("instruments"));
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
    
    /// 註冊執行客戶端 (不推薦使用 - 無法構建真實的 venue 映射)
    pub fn register_execution_client<E: ExecutionClient + 'static>(mut self, client: E) -> Self {
        warn!("使用不推薦的 register_execution_client 方法: {}。建議使用 register_execution_client_with_venue", 
              std::any::type_name::<E>());
        self.execution_clients.push(Box::new(client));
        self
    }
    
    /// 註冊執行客戶端並指定對應的交易所
    pub fn register_execution_client_with_venue<E: ExecutionClient + 'static>(
        mut self, 
        client: E, 
        venue: VenueId
    ) -> Self {
        let client_idx = self.execution_clients.len();
        info!("註冊執行客戶端 {} 到交易所 {:?} (索引: {})", 
              std::any::type_name::<E>(), venue, client_idx);
        self.execution_clients.push(Box::new(client));
        self.execution_client_venues.push(venue);
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
        
        info!("收集到 {} 個符號需要訂閱: {:?}", symbols.len(), symbols);

        // 2) 為每個 venue 登記市場數據流規劃（Runtime 啟動時橋接）
        // 應用分片過濾：只註冊當前分片應該處理的符號
        for venue in self.config.venues.clone() {
            let venue_id = match venue.venue_type {
                VenueType::Binance => VenueId::BINANCE,
                VenueType::Bitget => VenueId::BITGET,
                VenueType::Bybit => VenueId::BYBIT,
                VenueType::Mock => VenueId::MOCK,
                VenueType::Okx => VenueId::MOCK,
            };
            
            let filtered_symbols = if let Some(ref shard_config) = self.shard_config {
                // 有分片配置：過濾符號
                let mut filtered = Vec::new();
                for symbol in &symbols {
                    let base_symbol = BaseSymbol::from(symbol.0.as_str());
                    if shard_config.should_handle(&base_symbol, &venue_id) {
                        filtered.push(symbol.clone());
                    }
                }
                info!("分片過濾後，交易所 {} 需要處理 {} 個符號: {:?}", venue.name, filtered.len(), filtered);
                filtered
            } else {
                // 無分片配置：處理所有符號
                info!("未配置分片，交易所 {} 處理所有 {} 個符號", venue.name, symbols.len());
                symbols.clone()
            };
            
            // 只有當有符號需要處理時才註冊
            if !filtered_symbols.is_empty() {
                self = self.register_market_stream_plan(venue.venue_type.clone(), filtered_symbols);
            } else if self.shard_config.is_some() {
                info!("分片過濾後，交易所 {} 無符號需要處理，跳過註冊", venue.name);
            }
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
                    self = self.register_execution_client_with_venue(execution_client, VenueId::BITGET);
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
            self = self.register_execution_client_with_venue(execution_client, VenueId::BINANCE);
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
                        // 生成穩定的實例 ID: 使用配置名稱而非默認的類型名稱
                        let instance_id = format!("{}:{}", strategy_config.name, sym.0);
                        let strat = strategy_trend::TrendStrategy::with_name(
                            sym.clone(), 
                            strategy_trend::TrendStrategyConfig::default(), 
                            instance_id
                        );
                        s = s.register_strategy(strat);
                    }
                    return s;
                }
                #[cfg(not(feature = "strategy-trend"))]
                {
                    warn!("趨勢策略未啟用 (缺少 feature flag)");
                }
            }
            StrategyType::Imbalance => {
                #[cfg(feature = "strategy-imbalance")]
                {
                    info!("註冊不平衡策略: {}", strategy_config.name);
                    let mut s = self;
                    for sym in &strategy_config.symbols {
                        let params = match &strategy_config.params {
                            StrategyParams::Imbalance { obi_threshold, lot, top_levels } => {
                                Some(strategy_imbalance::ImbalanceParams { 
                                    obi_threshold: *obi_threshold, 
                                    lot: (lot.clone()).to_f64().unwrap_or(0.01), 
                                    top_levels: *top_levels,
                                })
                            }
                            _ => None,
                        };
                        // 生成穩定的實例 ID: 使用配置名稱而非默認的類型名稱
                        let instance_id = format!("{}:{}", strategy_config.name, sym.0);
                        let strat = strategy_imbalance::ImbalanceStrategy::with_name(
                            sym.clone(), 
                            params, 
                            instance_id
                        );
                        s = s.register_strategy(strat);
                    }
                    return s;
                }
                #[cfg(not(feature = "strategy-imbalance"))]
                {
                    warn!("不平衡策略未啟用 (缺少 feature flag)");
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
            latency_monitor: engine::latency_monitor::LatencyMonitorConfig::default(),
        };
        
        // 創建引擎
        let mut engine = Engine::new(engine_config);
        
        // 提取策略到場所的映射（從 router 配置中）
        if let Some(ref router_config) = self.config.router {
            if let ports::RouterConfig::StrategyMap { strategy_venues, .. } = router_config {
                let mut strategy_venue_mapping = HashMap::new();
                for (strategy_name, venue_str) in strategy_venues {
                    if let Some(venue_id) = hft_core::VenueId::from_str(&venue_str.to_uppercase()) {
                        strategy_venue_mapping.insert(strategy_name.clone(), venue_id);
                    }
                }
                engine.set_strategy_venue_mapping(strategy_venue_mapping);
                info!("設置策略場所映射: {:?}", router_config);
            }
        }
        
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
        
        // Create configurable risk manager using factory
        let risk_manager = crate::RiskManagerFactory::create_strategy_aware_risk_manager(&self.config.risk);
        engine.register_risk_manager_boxed(risk_manager);
        info!("已注册风控管理器 (类型: {}, 策略覆盖数: {})", 
              self.config.risk.risk_type, 
              self.config.risk.strategy_overrides.len());
        
        SystemRuntime { 
            engine: Arc::new(Mutex::new(engine)), 
            config: self.config, 
            tasks: Vec::new(),
            execution_worker_tasks: Vec::new(),
            exec_control_txs: Vec::new(),
            market_plans: self.market_stream_plans,
            execution_client_venues: self.execution_client_venues
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
    // 執行控制通道（撤單等）
    exec_control_txs: Vec<tokio::sync::mpsc::UnboundedSender<engine::execution_worker::ControlCommand>>,
    // 登記的市場流規劃
    market_plans: Vec<(VenueType, Vec<Symbol>)>,
    // 🔥 Phase 1: 執行客戶端到交易所的映射
    execution_client_venues: Vec<VenueId>,
}

impl SystemRuntime {
    /// 啟動系統
    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("啟動系統運行時...");

        // 啟動恢復流程（若啟用 recovery 功能）：在執行 worker 和引擎主循環之前
        #[cfg(feature = "recovery")]
        {
            let recovery = crate::recovery_integration::create_default_recovery();
            if let Err(e) = recovery.init().await {
                tracing::warn!("Recovery init 失敗: {}", e);
            } else {
                match recovery.perform_startup_recovery(self).await {
                    Ok(stats) => {
                        tracing::info!("Recovery 完成: orders_restored={}, positions_restored={}", stats.orders_restored, stats.positions_restored);
                    }
                    Err(e) => {
                        tracing::warn!("Recovery 執行失敗（將繼續啟動）: {}", e);
                    }
                }
            }
        }
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
                        // 根據能力配置決定是否使用增量訂閱
                        let use_inc = self.config.venues.iter()
                            .find(|v| matches!(v.venue_type, VenueType::Bitget))
                            .map(|v| v.capabilities.use_incremental_books)
                            .unwrap_or(false);

                        let stream = if use_inc {
                            adapter_bitget_data::BitgetMarketStream::new_with_incremental(true)
                        } else {
                            adapter_bitget_data::BitgetMarketStream::new()
                        };
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Bitget 行情已橋接至引擎");
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
        
        // 🔥 Phase 1.5: 启动执行 worker - 支持路由器配置
        let worker_config = ExecutionWorkerConfig {
            name: "main_execution_worker".to_string(),
            ..Default::default()
        };
        
        // Capture client count before move
        let client_count = execution_clients.len();
        
        let (worker_handle, control_tx) = if let Some(router_config) = &self.config.router {
            // 有路由器配置，創建路由器並使用帶路由器的 worker
            let router = router_config.clone().build();
            
            // 🔥 Phase 1: 構建真實的 venue_to_client 映射 - 基於實際註冊的執行客戶端
            let mut venue_to_client = std::collections::HashMap::new();
            for (client_idx, venue_id) in self.execution_client_venues.iter().enumerate() {
                venue_to_client.insert(*venue_id, client_idx);
            }
            
            // 如果沒有註冊任何帶 venue 的客戶端，回退到配置順序
            if venue_to_client.is_empty() {
                warn!("沒有找到帶 venue 信息的執行客戶端，回退到配置順序映射");
                for (idx, venue_config) in self.config.venues.iter().enumerate() {
                    if let Some(venue_id) = hft_core::VenueId::from_str(&venue_config.name.to_uppercase()) {
                        venue_to_client.insert(venue_id, idx);
                    }
                }
            }
            
            // 驗證 venue_to_client 映射的有效性
            for (venue_id, client_idx) in &venue_to_client {
                if *client_idx >= execution_clients.len() {
                    warn!("無效的客戶端索引: venue={:?}, client_idx={}, 總客戶端數={}", 
                          venue_id, client_idx, execution_clients.len());
                }
            }
            
            info!("使用路由器: {:?}, venue映射: {:?}, 執行客戶端總數: {}", 
                  router.name(), venue_to_client, execution_clients.len());
            engine::execution_worker::spawn_execution_worker_with_control_and_router(
                worker_config, 
                worker_queues, 
                execution_clients, 
                router, 
                venue_to_client
            )
        } else {
            // 沒有路由器配置，使用預設邏輯
            info!("未配置路由器，使用預設執行客戶端選擇邏輯");
            engine::execution_worker::spawn_execution_worker_with_control(worker_config, worker_queues, execution_clients)
        };
        self.execution_worker_tasks.push(worker_handle);
        self.exec_control_txs.push(control_tx);
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

        // 4) 狀態欄：每秒輸出一次系統狀態摘要（現金/持倉/PNL/事件統計）
        {
            let engine_arc = self.engine.clone();
            let status_handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    interval.tick().await;
                    let (cash, pos, unr, rlz, stats) = {
                        let eng = engine_arc.lock().await;
                        let av = eng.get_account_view();
                        let st = eng.get_statistics();
                        (av.cash_balance, av.positions.len(), av.unrealized_pnl, av.realized_pnl, st)
                    };
                    // 當引擎停止時，狀態任務退出，避免 Ctrl-C 卡住
                    if !stats.is_running {
                        break;
                    }
                    tracing::info!(
                        cash = cash,
                        pos = pos,
                        unrealized = unr,
                        realized = rlz,
                        cycles = stats.cycle_count,
                        exec_events = stats.execution_events_processed,
                        ord_new = stats.orders_submitted,
                        ord_ack = stats.orders_ack,
                        ord_fill = stats.orders_filled,
                        ord_rej = stats.orders_rejected,
                        ord_can = stats.orders_canceled,
                        strategies = stats.strategies_count,
                        consumers = stats.consumers_count,
                        "STATUS | cash=${:.2} pos={} U={:.2} R={:.2} | cycles={} exec_evts={} | new={} ack={} fill={} rej={} can={} | strats={} cons={}",
                        cash, pos, unr, rlz,
                        stats.cycle_count, stats.execution_events_processed,
                        stats.orders_submitted, stats.orders_ack, stats.orders_filled, stats.orders_rejected, stats.orders_canceled,
                        stats.strategies_count, stats.consumers_count
                    );
                }
            });
            self.tasks.push(status_handle);
        }

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

        // Start IPC control server if enabled
        #[cfg(feature = "infra-ipc")]
        {
            // 使用 Arc::new(Mutex::new(self)) 來避免創建新實例
            // 但由於 self 的生命週期問題，我們需要重構為使用共享的 runtime_arc
            let runtime_arc = Arc::new(Mutex::new(self.clone_for_ipc()));
            
            let ipc_handle = crate::ipc_handler::start_ipc_server(runtime_arc, None);
            self.tasks.push(ipc_handle);
            info!("IPC control server started");
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
        // 中止執行 worker 任務，避免阻塞退出
        for w in self.execution_worker_tasks.drain(..) { w.abort(); }
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

    /// 熱更新風控配置：替換風控管理器並應用新的策略覆蓋
    pub async fn update_risk_config(
        &mut self,
        new_risk: RiskConfig,
    ) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!("更新風控配置: 風控類型={}, 覆蓋策略數={}", new_risk.risk_type, new_risk.strategy_overrides.len());
        // 更新運行時配置
        self.config.risk = new_risk;
        
        // 重新創建風控管理器（帶策略覆蓋）
        let new_manager = crate::RiskManagerFactory::create_strategy_aware_risk_manager(&self.config.risk);
        
        // 註冊到引擎（覆蓋舊的風控管理器）
        let mut eng = self.engine.lock().await;
        eng.register_risk_manager_boxed(new_manager);
        
        tracing::info!("風控配置已更新並生效");
        Ok(())
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
            target_venue: None,
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
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(100));
            let client = match Client::open(redis_url.as_str()) {
                Ok(client) => client,
                Err(e) => {
                    tracing::error!("Redis 客戶端創建失敗: {}", e);
                    return;
                }
            };
            
            loop {
                interval.tick().await;
                // 如果引擎已停止，退出任務
                {
                    let eng = engine_arc.lock().await;
                    if !eng.get_statistics().is_running { break; }
                }
                
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
        // 複製連線參數，避免在多個 async move 任務中移動原始結構
        let ch_url: String = clickhouse_config.url.clone();
        let ch_db: String = clickhouse_config
            .database
            .clone()
            .unwrap_or_else(|| "default".to_string());
        
        // 測試連接
        let client = Client::default()
            .with_url(&ch_url)
            .with_database(&ch_db);
        
        // 測試連接可用性
        let result: Result<Vec<u8>, clickhouse::error::Error> = client
            .query("SELECT 1")
            .fetch_all()
            .await;
        
        if let Err(e) = result {
            return Err(format!("ClickHouse 連接測試失敗: {}", e).into());
        }
        info!("ClickHouse 連接測試成功");

        // 確保 lob_depth 表存在（使用與 Writer 一致的逐行扁平結構）
        let create_sql = r#"
            CREATE TABLE IF NOT EXISTS lob_depth (
                timestamp UInt64,
                symbol String,
                venue String,
                side LowCardinality(String),
                level UInt32,
                price Float64,
                quantity Float64
            )
            ENGINE = MergeTree()
            ORDER BY (symbol, timestamp, side, level)
        "#;
        if let Err(e) = client.query(create_sql).execute().await {
            return Err(format!("ClickHouse 建表失敗: {}", e).into());
        }
        info!("ClickHouse 表檢查完成（lob_depth 存在）");

        // 建立 trade_data 表（市場成交逐筆/秒級，簡化欄位）
        let create_trades_sql = r#"
            CREATE TABLE IF NOT EXISTS trade_data (
                timestamp UInt64,
                symbol String,
                venue String,
                trade_id String,
                price Float64,
                quantity Float64,
                side LowCardinality(String)
            )
            ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
        "#;
        if let Err(e) = client.query(create_trades_sql).execute().await {
            return Err(format!("ClickHouse 建表失敗(trade_data): {}", e).into());
        }
        info!("ClickHouse 表檢查完成（trade_data 存在）");

        // 建立 per-symbol engine_stats_symbol 表（每秒按 symbol 的統計）
        let create_sym_engine_sql = r#"
            CREATE TABLE IF NOT EXISTS engine_stats_symbol (
                timestamp UInt64,
                symbol String,
                orders_submitted UInt64,
                orders_filled UInt64,
                delta_submitted UInt64,
                delta_filled UInt64,
                fill_rate Float64
            )
            ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
        "#;
        if let Err(e) = client.query(create_sym_engine_sql).execute().await {
            return Err(format!("ClickHouse 建表失敗(engine_stats_symbol): {}", e).into());
        }
        info!("ClickHouse 表檢查完成（engine_stats_symbol 存在）");

        // 確保 engine_stats 表存在（每秒一條，用於計算填單率）
        let create_engine_sql = r#"
            CREATE TABLE IF NOT EXISTS engine_stats (
                timestamp UInt64,
                orders_submitted UInt64,
                orders_filled UInt64,
                delta_submitted UInt64,
                delta_filled UInt64,
                fill_rate Float64,
                execution_events_processed UInt64
            )
            ENGINE = MergeTree()
            ORDER BY (timestamp)
        "#;
        if let Err(e) = client.query(create_engine_sql).execute().await {
            return Err(format!("ClickHouse 建表失敗(engine_stats): {}", e).into());
        }
        info!("ClickHouse 表檢查完成（engine_stats 存在）");

        // 建立因子表（每秒每 symbol 一行）
        let create_factors_sql = r#"
            CREATE TABLE IF NOT EXISTS factors (
                timestamp UInt64,
                symbol String,
                venue String,
                obi_l1 Float64,
                obi_l5 Float64,
                spread_bps Float64,
                microprice Float64,
                depth_ratio_l5 Float64,
                ofi_l1 Float64,
                ofi_l5 Float64,
                mid_change_bps Float64
            )
            ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
        "#;
        if let Err(e) = client.query(create_factors_sql).execute().await {
            return Err(format!("ClickHouse 建表失敗(factors): {}", e).into());
        }
        info!("ClickHouse 表檢查完成（factors 存在）");

        // 克隆引擎引用和 client 以供任務使用
        let engine_arc = self.engine.clone();
        
        let ch_url_main = ch_url.clone();
        let ch_db_main = ch_db.clone();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
            let mut batch = Vec::<LobDepthRow>::new();
            let mut prev_submitted: u64 = 0;
            let mut prev_filled: u64 = 0;
            let mut prev_exec_processed: u64 = 0;
            use std::collections::HashMap;
            let mut prev_map: HashMap<String, (f64,f64,f64,f64,f64,f64,f64)> = HashMap::new();
            // per symbol: (prev_bid_px, prev_ask_px, prev_bid_qty_l1, prev_ask_qty_l1, prev_bid_qty_l5, prev_ask_qty_l5, prev_mid)
            let factors_client = Client::default()
                .with_url(&ch_url_main)
                .with_database(&ch_db_main);
            
            loop {
                interval.tick().await;
                // 若引擎已停止，退出任務以支持優雅關閉
                {
                    let eng = engine_arc.lock().await;
                    if !eng.get_statistics().is_running {
                        tracing::info!("ClickHouse Writer: 檢測到引擎已停止，退出寫入任務");
                        break;
                    }
                }
                
                // 獲取當前市場視圖
                let market_view = {
                    let engine = engine_arc.lock().await;
                    engine.get_market_view()
                };
                
                batch.clear();
                
                // 調試日志：檢查 market_view 狀態
                tracing::debug!(
                    "ClickHouse Writer: timestamp={}, orderbooks_count={}", 
                    market_view.timestamp, 
                    market_view.orderbooks.len()
                );
                
                if market_view.orderbooks.is_empty() {
                    tracing::debug!("ClickHouse Writer: 跳過寫入 - orderbooks 為空");
                    continue;
                }
                
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
                    tracing::info!("ClickHouse Writer: 準備寫入 {} 條記錄", batch.len());
                    
                    // 創建新的 insert 對象，使用完整表名並指定行類型
                    let insert_result = client.insert::<LobDepthRow>("hft.lob_depth");
                    
                    match insert_result {
                        Ok(mut inserter) => {
                            // 循環寫入每一行數據
                            let mut write_error = None;
                            for row in &batch {
                                if let Err(e) = inserter.write(row).await {
                                    write_error = Some(e);
                                    break;
                                }
                            }
                            
                            // 結束插入
                            match write_error {
                                None => {
                                    match inserter.end().await {
                                        Ok(()) => {
                                            tracing::info!("ClickHouse Writer: 成功寫入 {} 條 lob_depth 記錄", batch.len());
                                        }
                                        Err(e) => {
                                            tracing::error!("ClickHouse Writer: 結束插入失敗: {}", e);
                                        }
                                    }
                                }
                                Some(e) => {
                                    tracing::error!("ClickHouse Writer: 寫入行失敗: {}", e);
                                    tracing::debug!("失敗的批次大小: {}, 第一條記錄: {:?}", 
                                                   batch.len(), batch.first());
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("ClickHouse Writer: 創建 insert 對象失敗: {}", e);
                        }
                    }
                } else {
                    tracing::debug!("ClickHouse Writer: 批次為空，跳過寫入");
                }

                // 生成因子（每秒每 symbol 一條）
                let mut f_rows: Vec<FactorRow> = Vec::new();
                for (symbol, orderbook) in &market_view.orderbooks {
                    let ts = market_view.timestamp;
                    // L1
                    let bid_px = orderbook.bid_prices.get(0).map(|x| x.to_f64()).unwrap_or(0.0);
                    let bid_qty = orderbook.bid_quantities.get(0).map(|x| x.to_f64()).unwrap_or(0.0);
                    let ask_px = orderbook.ask_prices.get(0).map(|x| x.to_f64()).unwrap_or(0.0);
                    let ask_qty = orderbook.ask_quantities.get(0).map(|x| x.to_f64()).unwrap_or(0.0);
                    let mid = if bid_px>0.0 && ask_px>0.0 { (bid_px+ask_px)/2.0 } else { 0.0 };
                    // L5 sums (FixedQuantity -> f64)
                    let sum_n_qty = |v: &Vec<FixedQuantity>, n: usize| -> f64 {
                        v.iter().take(n).map(|q| q.to_f64()).sum::<f64>()
                    };
                    let bid_qty_l5 = sum_n_qty(&orderbook.bid_quantities, 5);
                    let ask_qty_l5 = sum_n_qty(&orderbook.ask_quantities, 5);
                    // OBI
                    let obi = |b: f64,a: f64| if b+a>0.0 {(b-a)/(b+a)} else {0.0};
                    let obi_l1 = obi(bid_qty, ask_qty);
                    let obi_l5 = obi(bid_qty_l5, ask_qty_l5);
                    // spread bps
                    let spread_bps = if mid>0.0 {(ask_px - bid_px)/mid*10000.0} else {0.0};
                    // depth ratio l5
                    let depth_ratio_l5 = if ask_qty_l5>0.0 { bid_qty_l5/ask_qty_l5 } else { 0.0 };
                    // prevs
                    let key = symbol.0.clone();
                    let (mut ofi1, mut ofi5, mut mid_change_bps) = (0.0,0.0,0.0);
                    if let Some((pb, pa, pb1, pa1, pb5, pa5, pmid)) = prev_map.get(&key).cloned() {
                        ofi1 = (bid_qty - pb1) - (ask_qty - pa1);
                        ofi5 = (bid_qty_l5 - pb5) - (ask_qty_l5 - pa5);
                        if pmid>0.0 && mid>0.0 { mid_change_bps = (mid - pmid)/pmid*10000.0; }
                    }
                    prev_map.insert(key.clone(), (bid_px, ask_px, bid_qty, ask_qty, bid_qty_l5, ask_qty_l5, mid));
                    f_rows.push(FactorRow { timestamp: ts, symbol: key, venue: "COMBINED".into(), obi_l1, obi_l5, spread_bps, microprice: if (bid_qty+ask_qty)>0.0 { (ask_px*bid_qty + bid_px*ask_qty)/(bid_qty+ask_qty) } else { mid }, depth_ratio_l5, ofi_l1: ofi1, ofi_l5: ofi5, mid_change_bps });
                }
                if !f_rows.is_empty() {
                    if let Ok(mut inserter) = factors_client.insert::<FactorRow>("hft.factors") {
                        for r in &f_rows { let _ = inserter.write(r).await; }
                        let _ = inserter.end().await;
                    }
                }

                // 追加寫入 engine_stats（每秒一條）
                let stats = {
                    let eng = engine_arc.lock().await;
                    eng.get_statistics()
                };
                let ts = market_view.timestamp; // 使用市場視圖時間戳
                let delta_submitted = stats.orders_submitted.saturating_sub(prev_submitted);
                let delta_filled = stats.orders_filled.saturating_sub(prev_filled);
                let fill_rate = if delta_submitted > 0 {
                    (delta_filled as f64) / (delta_submitted as f64)
                } else { 0.0 };
                let row = EngineStatsRow {
                    timestamp: ts,
                    orders_submitted: stats.orders_submitted,
                    orders_filled: stats.orders_filled,
                    delta_submitted,
                    delta_filled,
                    fill_rate,
                    execution_events_processed: stats.execution_events_processed,
                };
                prev_submitted = stats.orders_submitted;
                prev_filled = stats.orders_filled;
                prev_exec_processed = stats.execution_events_processed;

                if let Ok(mut inserter) = client.insert::<EngineStatsRow>("hft.engine_stats") {
                    if let Err(e) = inserter.write(&row).await {
                        tracing::warn!("ClickHouse Writer: 寫入 engine_stats 失敗: {}", e);
                    } else if let Err(e) = inserter.end().await {
                        tracing::warn!("ClickHouse Writer: 結束 engine_stats 插入失敗: {}", e);
                    }
                } else {
                    tracing::warn!("ClickHouse Writer: 創建 engine_stats insert 失敗");
                }
            }
        });
        
        self.tasks.push(handle);
        info!("ClickHouse Writer 已啟動，每秒批量寫入 lob_depth 表");

        // 啟動 Trade Writer：訂閱引擎內部成交廣播，逐批寫入 trade_data
        let engine_for_trades = self.engine.clone();
        let trades_client = Client::default()
            .with_url(&ch_url)
            .with_database(&ch_db);
        let trade_handle = tokio::spawn(async move {
            // 訂閱引擎成交事件
            let mut rx = { let eng = engine_for_trades.lock().await; eng.subscribe_market_trades() };
            
            // 以小批次寫入（最多 1000 筆或每 1 秒）
            let mut batch: Vec<(u64, String, String, String, f64, f64, String)> = Vec::with_capacity(1024);
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if !batch.is_empty() {
                            // 插入 trade_data
                            // 使用行結構 tuple 對應欄位順序
                            if let Ok(mut inserter) = trades_client.insert::<(u64, String, String, String, f64, f64, String)>("hft.trade_data") {
                                let mut err = None;
                                for row in &batch { if let Err(e) = inserter.write(row).await { err = Some(e); break; } }
                                if err.is_none() { let _ = inserter.end().await; }
                            }
                            batch.clear();
                        }
                    }
                    recv = rx.recv() => {
                        match recv {
                            Ok(tr) => {
                                let venue = tr.source_venue.map(|v| format!("{:?}", v)).unwrap_or_else(|| "UNKNOWN".to_string());
                                let side = match tr.side { hft_core::Side::Buy => "buy".to_string(), hft_core::Side::Sell => "sell".to_string() };
                                batch.push((tr.timestamp, tr.symbol.0.clone(), venue, tr.trade_id.clone(), tr.price.0.to_f64().unwrap_or(0.0), tr.quantity.0.to_f64().unwrap_or(0.0), side));
                                if batch.len() >= 1000 {
                                    if let Ok(mut inserter) = trades_client.insert::<(u64, String, String, String, f64, f64, String)>("hft.trade_data") {
                                        let mut err = None;
                                        for row in &batch { if let Err(e) = inserter.write(row).await { err = Some(e); break; } }
                                        if err.is_none() { let _ = inserter.end().await; }
                                    }
                                    batch.clear();
                                }
                            }
                            Err(_) => {
                                // 發送端可能暫時沒有數據，忽略
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                            }
                        }
                    }
                }
            }
        });
        self.tasks.push(trade_handle);
        info!("ClickHouse Trade Writer 已啟動（hft.trade_data）");

        // 啟動 per-symbol Engine Stats Writer：訂閱執行事件，彙總每秒每個 symbol 的 submitted/filled
        let engine_for_sym = self.engine.clone();
        let sym_client = Client::default()
            .with_url(&ch_url)
            .with_database(&ch_db);
        let sym_handle = tokio::spawn(async move {
            use std::collections::HashMap;
            // order_id -> symbol 映射（由 OrderNew 建立，用於 Fill 映射）
            let mut order_symbol: HashMap<hft_core::OrderId, hft_core::Symbol> = HashMap::new();
            // 每秒累積
            let mut sub_cnt: HashMap<String, u64> = HashMap::new();
            let mut fill_cnt: HashMap<String, u64> = HashMap::new();
            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(1));
            // 訂閱執行事件
            let mut rx = { let eng = engine_for_sym.lock().await; eng.subscribe_execution_events() };

            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        if !sub_cnt.is_empty() || !fill_cnt.is_empty() {
                            // 組裝本秒資料
                            let now_ts = hft_core::now_micros();
                            let mut rows: Vec<(u64, String, u64, u64, u64, u64, f64)> = Vec::new();
                            // 拿到當前 engine 全域統計以便填充 orders_submitted/orders_filled（可選）
                            // 這裡 per-symbol 的 orders_submitted/orders_filled 我們用累積計數器（只統計在本 writer 期間的）
                            // 若需持久累計，可改為查詢上一條紀錄或從 OMS 拉取。
                            for (sym, d_sub) in sub_cnt.iter() {
                                let d_fill = *fill_cnt.get(sym).unwrap_or(&0);
                                let rate = if *d_sub > 0 { (d_fill as f64)/( *d_sub as f64) } else { 0.0 };
                                rows.push((now_ts, sym.clone(), *d_sub, d_fill, *d_sub, d_fill, rate));
                            }
                            for (sym, d_fill) in fill_cnt.iter() {
                                if !sub_cnt.contains_key(sym) {
                                    let d_sub = 0u64;
                                    let rate = 0.0;
                                    rows.push((now_ts, sym.clone(), d_sub, *d_fill, d_sub, *d_fill, rate));
                                }
                            }
                            // 寫入
                            if let Ok(mut inserter) = sym_client.insert::<(u64, String, u64, u64, u64, u64, f64)>("hft.engine_stats_symbol") {
                                let mut err = None;
                                for r in &rows { if let Err(e) = inserter.write(r).await { err = Some(e); break; } }
                                if err.is_none() { let _ = inserter.end().await; }
                            }
                            sub_cnt.clear();
                            fill_cnt.clear();
                        }
                    }
                    evt = rx.recv() => {
                        match evt {
                            Ok(ports::ExecutionEvent::OrderNew { order_id, symbol, .. }) => {
                                order_symbol.insert(order_id.clone(), symbol.clone());
                                *sub_cnt.entry(symbol.0.clone()).or_insert(0) += 1;
                            }
                            Ok(ports::ExecutionEvent::Fill { order_id, .. }) => {
                                if let Some(sym) = order_symbol.get(&order_id) {
                                    *fill_cnt.entry(sym.0.clone()).or_insert(0) += 1;
                                }
                            }
                            Ok(_) => {}
                            Err(_) => { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
                        }
                    }
                }
            }
        });
        self.tasks.push(sym_handle);
        info!("ClickHouse per-symbol Engine Stats Writer 已啟動（hft.engine_stats_symbol）");

        // 建立 per-venue L1 表（只寫 L1，減小資料量）
        let create_l1_sql = r#"
            CREATE TABLE IF NOT EXISTS l1_venue (
                timestamp UInt64,
                symbol String,
                venue String,
                best_bid Float64,
                best_bid_qty Float64,
                best_ask Float64,
                best_ask_qty Float64
            )
            ENGINE = MergeTree()
            ORDER BY (symbol, venue, timestamp)
        "#;
        let client_l1 = Client::default()
            .with_url(&ch_url)
            .with_database(&ch_db);
        if let Err(e) = client_l1.query(create_l1_sql).execute().await {
            return Err(format!("ClickHouse 建表失敗(l1_venue): {}", e).into());
        }
        info!("ClickHouse 表檢查完成（l1_venue 存在）");

        // 啟動 per-venue L1 Writer：訂閱市場事件，遇 Snapshot 寫 L1
        let engine_for_l1 = self.engine.clone();
        let l1_client = Client::default()
            .with_url(&ch_url)
            .with_database(&ch_db);
        let l1_handle = tokio::spawn(async move {
            let mut rx = { let eng = engine_for_l1.lock().await; eng.subscribe_market_events() };
            loop {
                match rx.recv().await {
                    Ok(ports::MarketEvent::Snapshot(s)) => {
                        let venue = s.source_venue.map(|v| format!("{:?}", v)).unwrap_or_else(|| "UNKNOWN".to_string());
                        let best_bid = s.bids.get(0).map(|b| b.price.0.to_f64().unwrap_or(0.0)).unwrap_or(0.0);
                        let best_bid_qty = s.bids.get(0).map(|b| b.quantity.0.to_f64().unwrap_or(0.0)).unwrap_or(0.0);
                        let best_ask = s.asks.get(0).map(|a| a.price.0.to_f64().unwrap_or(0.0)).unwrap_or(0.0);
                        let best_ask_qty = s.asks.get(0).map(|a| a.quantity.0.to_f64().unwrap_or(0.0)).unwrap_or(0.0);
                        if let Ok(mut inserter) = l1_client.insert::<(u64, String, String, f64, f64, f64, f64)>("hft.l1_venue") {
                            let _ = inserter.write(&(s.timestamp, s.symbol.0.clone(), venue, best_bid, best_bid_qty, best_ask, best_ask_qty)).await;
                            let _ = inserter.end().await;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
                }
            }
        });
        self.tasks.push(l1_handle);
        info!("ClickHouse L1 per-venue Writer 已啟動（hft.l1_venue）");
        Ok(())
    }

    /// 為 IPC 創建共享實例，避免雙實例問題
    /// 共享引擎和配置，但使用獨立的任務列表
    fn clone_for_ipc(&self) -> SystemRuntime {
        SystemRuntime {
            engine: self.engine.clone(),
            config: self.config.clone(),
            tasks: vec![], // IPC 專用空任務列表
            execution_worker_tasks: vec![], // IPC 專用空任務列表
            exec_control_txs: vec![],
            market_plans: self.market_plans.clone(),
            execution_client_venues: self.execution_client_venues.clone(),
        }
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
                risk_type: "Default".to_string(),
                global_position_limit: rust_decimal::Decimal::from(1000000),
                global_notional_limit: rust_decimal::Decimal::from(10000000),
                max_daily_trades: 10000,
                max_orders_per_second: 100,
                staleness_threshold_us: 5000,
                enhanced: None,
                strategy_overrides: std::collections::HashMap::new(),
            },
            router: None,
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
