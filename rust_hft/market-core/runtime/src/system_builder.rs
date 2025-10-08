//! 系統建構器 - 宣告式裝配與註冊
use crate::sharding::ShardConfig;
use serde::{Deserialize, Serialize};
// use serde_yaml::Value as YamlValue; // 移除未使用引用
use std::sync::Arc;
use tracing::{info, warn};

// strategy_factory 僅在 runtime_management 使用
use crate::portfolio_manager::PortfolioManager;
use engine::{
    create_execution_queues,
    dataflow::{EventConsumer, FlipPolicy},
    Engine, EngineConfig, ExecutionQueueConfig, ExecutionWorkerConfig,
};
use hft_core::HftError;
use hft_core::*;
use ports::*;
use shared_config::{StrategyParams as SharedStrategyParams, StrategyType as SharedStrategyType};
// use shared_instrument::InstrumentId; // 已移至 config_types
use std::collections::HashMap;
use tokio::sync::{Mutex, Notify};

mod config_loader;
mod execution_registry;
mod simulated_execution;
mod strategy_factory;
mod venue_registry;
mod runtime_management;
mod config_types; // 預留：後續逐步搬移配置型別
mod infra_exporters; // 預留：後續搬移 Redis/ClickHouse 導出

// 將部分配置型別從子模組對外公開
pub use config_types::{
    ClickHouseConfig, CpuAffinityConfig, InfraConfig, LobFlowGridParams, PortfolioSpec,
    RedisConfig, RiskConfig, StrategyConfig, StrategyParams, StrategyRiskLimits,
    StrategyRiskOverride, StrategyType, SystemEngineConfig, TradingWindowConfig, VenueCapabilities,
    VenueConfig, VenueType,
};

#[cfg(feature = "redis")]
use serde_json;

#[cfg(feature = "redis")]
use engine::aggregation;

// ClickHouse 行結構已移至 system_builder::infra_exporters（feature = "clickhouse"）


/// 系統配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemConfig {
    pub engine: SystemEngineConfig,
    pub venues: Vec<VenueConfig>,
    pub strategies: Vec<StrategyConfig>,
    pub risk: RiskConfig,
    /// 僅行情模式（不啟動執行端、不下單）
    #[serde(default)]
    pub quotes_only: bool,
    /// 🔥 Phase 1.5: 執行路由器配置（可選）
    pub router: Option<ports::RouterConfig>,
    pub infra: Option<InfraConfig>,
    /// Phase 1 多帳戶：策略 → 帳戶映射（帳戶 ID 字串）
    #[serde(default)]
    pub strategy_accounts: HashMap<String, String>,
    /// 可選：以帳戶清單方式管理憑證與交易所（便於權限分離）
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
    /// 策略組合配置（投資組合層級）
    #[serde(default)]
    pub portfolios: Vec<PortfolioSpec>,
}

/// 帳戶憑證（集中管理）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountCredentials {
    pub api_key: Option<String>,
    pub secret: Option<String>,
    pub passphrase: Option<String>,
}

/// 帳戶配置（可映射為單個 venue 實例）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    pub id: String,
    pub venue_type: VenueType,
    #[serde(default)]
    pub ws_public: Option<String>,
    #[serde(default)]
    pub ws_private: Option<String>,
    #[serde(default)]
    pub rest: Option<String>,
    #[serde(default)]
    pub execution_mode: Option<String>,
    #[serde(default)]
    pub capabilities: Option<VenueCapabilities>,
    #[serde(default)]
    pub inst_type: Option<String>,
    #[serde(default)]
    pub credentials: Option<AccountCredentials>,
}

// SystemEngineConfig 與 CpuAffinityConfig 已移至 config_types 模組

// FlipPolicy 現在從 engine::dataflow 導入

// VenueConfig 已移至 config_types

// VenueType 已移至 config_types

// VenueCapabilities 已移至 config_types

// StrategyConfig 已移至 config_types

/// 投資組合設定，用於聚合多策略並套用整體限制
// PortfolioSpec 已移至 config_types

// StrategyType 已移至 config_types

// StrategyParams 已移至 config_types

// LobFlowGridParams 已移至 config_types

#[cfg(feature = "strategy-lob-flow-grid")]
fn apply_lob_flow_overrides(
    cfg: &mut strategy_lob_flow_grid::LobFlowGridConfig,
    params: &LobFlowGridParams,
) {
    if let Some(venue_str) = &params.venue {
        if let Some(venue) = VenueId::from_str(venue_str) {
            cfg.venue = venue;
        }
    }
    if let Some(v) = params.base_order_size {
        cfg.base_order_size = v;
    }
    if let Some(v) = params.max_long_position {
        cfg.max_long_position = v;
    }
    if let Some(v) = params.max_short_position {
        cfg.max_short_position = v;
    }
    if let Some(v) = params.maker_fee_bps {
        cfg.maker_fee_bps = v;
    }
    if let Some(v) = params.slippage_bps {
        cfg.slippage_bps = v;
    }
    if let Some(v) = params.safety_bps {
        cfg.safety_bps = v;
    }
    if let Some(v) = params.min_margin_bps {
        cfg.min_margin_bps = v;
    }
    if let Some(v) = params.volatility_alpha {
        cfg.volatility_alpha = v;
    }
    if let Some(v) = params.asr_gamma {
        cfg.asr_gamma = v;
    }
    if let Some(v) = params.asr_micro_coeff {
        cfg.asr_micro_coeff = v;
    }
    if let Some(v) = params.asr_ai_coeff {
        cfg.asr_ai_coeff = v;
    }
    if let Some(v) = params.asr_ofi_coeff {
        cfg.asr_ofi_coeff = v;
    }
    if let Some(v) = params.top_levels {
        cfg.top_levels = v;
    }
    if let Some(v) = params.ai_window_secs {
        cfg.ai_window_secs = v;
    }
    if let Some(v) = params.ofi_halflife_secs {
        cfg.ofi_halflife_secs = v;
    }
    if let Some(v) = params.mid_return_halflife_secs {
        cfg.mid_return_halflife_secs = v;
    }
    if let Some(v) = params.refresh_interval_secs {
        cfg.refresh_interval_secs = v;
    }
    if let Some(v) = params.tick_size {
        cfg.tick_size = v;
    }
    if let Some(v) = params.lot_size {
        cfg.lot_size = v;
    }
    if let Some(v) = params.core_levels {
        cfg.core_levels = v;
    }
    if let Some(v) = params.buffer_levels {
        cfg.buffer_levels = v;
    }
    if let Some(v) = params.tail_levels {
        cfg.tail_levels = v;
    }
    if let Some(v) = params.core_weight {
        cfg.core_weight = v;
    }
    if let Some(v) = params.buffer_weight {
        cfg.buffer_weight = v;
    }
    if let Some(v) = params.tail_weight {
        cfg.tail_weight = v;
    }
    if let Some(v) = params.core_spacing_multiplier {
        cfg.core_spacing_multiplier = v;
    }
    if let Some(v) = params.buffer_spacing_multiplier {
        cfg.buffer_spacing_multiplier = v;
    }
    if let Some(v) = params.tail_spacing_multiplier {
        cfg.tail_spacing_multiplier = v;
    }
    if let Some(v) = params.target_depth_usd {
        cfg.target_depth_usd = v;
    }
    if let Some(v) = params.max_depth_steps {
        cfg.max_depth_steps = v;
    }
    if let Some(v) = params.min_top_depth_usd {
        cfg.min_top_depth_usd = v;
    }
    if let Some(v) = params.min_spread_bps {
        cfg.min_spread_bps = v;
    }
    if let Some(v) = params.max_spread_bps {
        cfg.max_spread_bps = v;
    }
    if let Some(v) = params.bias_micro_threshold_bps {
        cfg.bias_micro_threshold_bps = v;
    }
    if let Some(v) = params.bias_ai_threshold {
        cfg.bias_ai_threshold = v;
    }
    if let Some(v) = params.bias_extra_ticks {
        cfg.bias_extra_ticks = v;
    }
}

// RiskConfig 與相關型別已移至 config_types 模組


/// 系統建構器 - 使用構建者模式
pub struct SystemBuilder {
    config: SystemConfig,
    event_consumers: Vec<EventConsumer>,
    execution_clients: Vec<Box<dyn ExecutionClient>>,
    strategies: Vec<Box<dyn Strategy>>,
    risk_managers: Vec<Box<dyn RiskManager>>,
    // 僅登記市場流規劃，實際橋接在 Runtime::start() 內進行
    market_stream_plans: Vec<(VenueType, String, Vec<Symbol>)>,
    // 分片配置
    shard_config: Option<ShardConfig>,
    // 🔥 Phase 1: 跟蹤執行客戶端對應的交易所
    execution_client_venues: Vec<VenueId>,
    // 🔥 Phase 1.x: 跟蹤執行客戶端對應的帳戶（可選）
    execution_client_accounts: Vec<Option<hft_core::AccountId>>,
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
            execution_client_accounts: Vec::new(),
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
        let config = config_loader::load_config_from_yaml(yaml_path)?;
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
    pub fn register_market_stream_plan(
        mut self,
        venue: VenueType,
        venue_name: String,
        symbols: Vec<Symbol>,
    ) -> Self {
        info!("登記市場數據流規劃: {:?} {:?}", venue, symbols);
        self.market_stream_plans.push((venue, venue_name, symbols));
        self
    }

    #[allow(dead_code)]
    pub(crate) fn register_simulated_execution_client(mut self, venue: VenueId) -> Self {
        let client = simulated_execution::SimulatedExecutionClient::new(venue);
        self = self.register_execution_client_with_venue(client, venue);
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
        self,
        client: E,
        venue: VenueId,
    ) -> Self {
        self.register_execution_client_with_key(client, venue, None)
    }

    /// 註冊執行客戶端並指定對應的交易所與可選帳戶
    pub fn register_execution_client_with_key<E: ExecutionClient + 'static>(
        mut self,
        client: E,
        venue: VenueId,
        account: Option<hft_core::AccountId>,
    ) -> Self {
        let client_idx = self.execution_clients.len();
        info!(
            "註冊執行客戶端 {} 到交易所 {:?} (索引: {})",
            std::any::type_name::<E>(),
            venue,
            client_idx
        );
        self.execution_clients.push(Box::new(client));
        self.execution_client_venues.push(venue);
        self.execution_client_accounts.push(account);
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

        self = self.register_market_streams_from_config();
        // Quotes-only 模式：YAML `quotes_only: true` 或環境變量 HFT_QUOTES_ONLY=1
        let quotes_only_env = match std::env::var("HFT_QUOTES_ONLY") {
            Ok(val) => matches!(val.as_str(), "1" | "true" | "TRUE"),
            Err(_) => false,
        };
        let quotes_only = self.config.quotes_only || quotes_only_env;
        if quotes_only {
            info!("已啟用 quotes-only（僅行情，不註冊執行客戶端）");
        } else {
            self = self.register_execution_clients_from_config();
        }

        let strategies = self.config.strategies.clone();
        for strategy_config in strategies {
            self = self.register_strategy_from_config(&strategy_config);
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
            if let ports::RouterConfig::StrategyMap {
                strategy_venues, ..
            } = router_config
            {
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
        let risk_manager =
            crate::RiskManagerFactory::create_strategy_aware_risk_manager(&self.config.risk);
        engine.register_risk_manager_boxed(risk_manager);
        info!(
            "已注册风控管理器 (类型: {}, 策略覆盖数: {})",
            self.config.risk.risk_type,
            self.config.risk.strategy_overrides.len()
        );

        // Phase 1 多帳戶：設置策略到帳戶映射
        if !self.config.strategy_accounts.is_empty() {
            let mut map: HashMap<String, hft_core::AccountId> = HashMap::new();
            for (k, v) in &self.config.strategy_accounts {
                map.insert(k.clone(), hft_core::AccountId(v.clone()));
            }
            engine.set_strategy_account_mapping(map);
            info!(
                "設置策略到帳戶映射: {} 項",
                self.config.strategy_accounts.len()
            );
        }

        let portfolio_manager = if self.config.portfolios.is_empty() {
            None
        } else {
            let manager =
                PortfolioManager::new(self.config.portfolios.clone(), &self.config.strategies);
            info!(
                portfolio_count = manager.portfolio_specs().len(),
                "已載入投資組合配置"
            );
            Some(manager)
        };

        SystemRuntime {
            engine: Arc::new(Mutex::new(engine)),
            config: self.config,
            tasks: Vec::new(),
            execution_worker_tasks: Vec::new(),
            exec_control_txs: Vec::new(),
            market_plans: self.market_stream_plans,
            execution_client_venues: self.execution_client_venues,
            execution_client_accounts: self.execution_client_accounts,
            portfolio_manager,
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
    exec_control_txs:
        Vec<tokio::sync::mpsc::UnboundedSender<engine::execution_worker::ControlCommand>>,
    // 登記的市場流規劃
    market_plans: Vec<(VenueType, String, Vec<Symbol>)>,
    // 🔥 Phase 1: 執行客戶端到交易所的映射
    execution_client_venues: Vec<VenueId>,
    // 🔥 Phase 1.x: 執行客戶端到帳戶的映射（可選）
    execution_client_accounts: Vec<Option<hft_core::AccountId>>,
    portfolio_manager: Option<PortfolioManager>,
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
                        tracing::info!(
                            "Recovery 完成: orders_restored={}, positions_restored={}",
                            stats.orders_restored,
                            stats.positions_restored
                        );
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

        let mut bridge = engine::AdapterBridge::new(bridge_cfg);
        // 在橋接前取得引擎的 Notify，讓攝取成功入隊即可喚醒引擎
        let engine_notify = {
            let eng = self.engine.lock().await;
            eng.get_wakeup_notify()
        };
        bridge.set_engine_notify(engine_notify);
        // 橋接每個登記的市場數據流
        for (venue_type, venue_name, symbols) in self.market_plans.clone() {
            #[allow(unused_variables)]
            let symbols = symbols;
            #[allow(unused_variables)]
            let venue_cfg = self
                .config
                .venues
                .iter()
                .find(|v| v.name == venue_name)
                .cloned();
            match venue_type {
                VenueType::Bitget => {
                    #[cfg(feature = "adapter-bitget-data")]
                    {
                        let use_inc = venue_cfg
                            .as_ref()
                            .map(|v| v.capabilities.use_incremental_books)
                            .unwrap_or(false);

                        let inst_type = venue_cfg
                            .as_ref()
                            .and_then(|v| v.inst_type.clone())
                            .unwrap_or_else(|| "SPOT".to_string());

                        let stream = if use_inc {
                            adapter_bitget_data::BitgetMarketStream::new_with_incremental(true)
                                .with_inst_type(inst_type)
                        } else {
                            adapter_bitget_data::BitgetMarketStream::new().with_inst_type(inst_type)
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
                VenueType::Grvt => {
                    #[cfg(feature = "adapter-grvt-data")]
                    {
                        let stream = adapter_grvt_data::GrvtMarketStream::new();
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("GRVT 行情已橋接至引擎");
                    }
                }
                VenueType::Hyperliquid => {
                    #[cfg(feature = "adapter-hyperliquid-data")]
                    {
                        let stream = adapter_hyperliquid_data::HyperliquidMarketStream::new();
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Hyperliquid 行情已橋接至引擎");
                    }
                }
                VenueType::Backpack => {
                    #[cfg(feature = "adapter-backpack-data")]
                    {
                        let market_config = venue_cfg
                            .as_ref()
                            .and_then(|cfg| parse_backpack_market_config(cfg, &symbols))
                            .unwrap_or_else(adapter_backpack_data::BackpackMarketConfig::default);
                        let stream =
                            adapter_backpack_data::BackpackMarketStream::new(market_config);
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Backpack 行情已橋接至引擎");
                    }
                }
                VenueType::Mock => {
                    #[cfg(feature = "adapter-mock-data")]
                    {
                        let stream = adapter_mock_data::MockMarketStream::new();
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                    }
                }
                VenueType::Asterdex => {
                    #[cfg(feature = "adapter-asterdex-data")]
                    {
                        let stream = adapter_asterdex_data::AsterdexMarketStream::new();
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Aster DEX 行情已橋接至引擎");
                    }
                }
                VenueType::Lighter => {
                    #[cfg(feature = "adapter-lighter-data")]
                    {
                        let stream = adapter_lighter_data::LighterMarketStream::new();
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Lighter 行情已橋接至引擎");
                    }
                }
                _ => {}
            }
        }

        // 2) (可選) 設置執行队列並啟動执行 worker
        let quotes_only = match std::env::var("HFT_QUOTES_ONLY") {
            Ok(val) => matches!(val.as_str(), "1" | "true" | "TRUE"),
            Err(_) => false,
        };

        if quotes_only {
            info!("Quotes-only 模式：跳過執行隊列與 ExecutionWorker 啟動");
        } else {
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
                ack_timeout_ms: self.config.engine.ack_timeout_ms,
                reconcile_interval_ms: self.config.engine.reconcile_interval_ms,
                auto_cancel_exchange_only: self.config.engine.auto_cancel_exchange_only,
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
                        if let Some(venue_id) =
                            hft_core::VenueId::from_str(&venue_config.name.to_uppercase())
                        {
                            venue_to_client.insert(venue_id, idx);
                        }
                    }
                }

                // 驗證 venue_to_client 映射的有效性
                for (venue_id, client_idx) in &venue_to_client {
                    if *client_idx >= execution_clients.len() {
                        warn!(
                            "無效的客戶端索引: venue={:?}, client_idx={}, 總客戶端數={}",
                            venue_id,
                            client_idx,
                            execution_clients.len()
                        );
                    }
                }

                info!(
                    "使用路由器: {:?}, venue映射: {:?}, 執行客戶端總數: {}",
                    router.name(),
                    venue_to_client,
                    execution_clients.len()
                );
                // 構建策略→客戶端映射（根據 strategy_accounts 與註冊的帳戶）
                let strategy_to_client = if !self.config.strategy_accounts.is_empty() {
                    let mut map = std::collections::HashMap::new();
                    for (strategy_key, account_str) in &self.config.strategy_accounts {
                        let account = hft_core::AccountId(account_str.clone());
                        if let Some((idx, _)) = self
                            .execution_client_accounts
                            .iter()
                            .enumerate()
                            .find(|(_, a)| a.as_ref() == Some(&account))
                        {
                            map.insert(strategy_key.clone(), idx);
                        }
                    }
                    if map.is_empty() {
                        None
                    } else {
                        Some(map)
                    }
                } else {
                    None
                };

                engine::execution_worker::spawn_execution_worker_with_control_and_router(
                    worker_config,
                    worker_queues,
                    execution_clients,
                    router,
                    venue_to_client,
                    strategy_to_client,
                )
            } else {
                // 沒有路由器配置，使用預設邏輯
                info!("未配置路由器，使用預設執行客戶端選擇邏輯");
                let strategy_to_client = if !self.config.strategy_accounts.is_empty() {
                    let mut map = std::collections::HashMap::new();
                    for (strategy_key, account_str) in &self.config.strategy_accounts {
                        let account = hft_core::AccountId(account_str.clone());
                        if let Some((idx, _)) = self
                            .execution_client_accounts
                            .iter()
                            .enumerate()
                            .find(|(_, a)| a.as_ref() == Some(&account))
                        {
                            map.insert(strategy_key.clone(), idx);
                        }
                    }
                    if map.is_empty() {
                        None
                    } else {
                        Some(map)
                    }
                } else {
                    None
                };

                engine::execution_worker::spawn_execution_worker_with_control(
                    worker_config,
                    worker_queues,
                    execution_clients,
                    strategy_to_client,
                )
            };
            self.execution_worker_tasks.push(worker_handle);
            self.exec_control_txs.push(control_tx);
            info!("已启动执行 worker (客户端数量: {})", client_count);
        }

        // 3) 啟動引擎主循環（後台，事件驅動）
        let engine_arc = self.engine.clone();

        // 獲取引擎唤醒通知器
        let notify = {
            let eng = engine_arc.lock().await;
            eng.get_wakeup_notify()
        };

        let affinity_core = self.config.engine.cpu_affinity.engine_core;
        let engine_handle = spawn_engine_loop(engine_arc, notify, affinity_core);
        self.tasks.push(engine_handle);

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
                        (
                            av.cash_balance,
                            av.positions.len(),
                            av.unrealized_pnl,
                            av.realized_pnl,
                            st,
                        )
                    };
                    // 導出引擎統計到 Prometheus（僅在 metrics feature 啟用時）
                    #[cfg(feature = "metrics")]
                    {
                        infra_metrics::MetricsRegistry::global()
                            .update_engine_statistics(&infra_metrics::EngineStatisticsExport {
                                cycle_count: stats.cycle_count,
                                execution_events_processed: stats.execution_events_processed,
                                orders_submitted: stats.orders_submitted,
                                orders_ack: stats.orders_ack,
                                orders_filled: stats.orders_filled,
                                orders_rejected: stats.orders_rejected,
                                orders_canceled: stats.orders_canceled,
                            });
                    }
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
        #[cfg(feature = "clickhouse")]
        if let Some(infra) = &self.config.infra {
            if let Some(clickhouse_config) = &infra.clickhouse {
                self.spawn_clickhouse_writer(clickhouse_config.clone())
                    .await?;
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
        for t in self.tasks.drain(..) {
            let _ = t.await;
        }
        // 中止執行 worker 任務，避免阻塞退出
        for w in self.execution_worker_tasks.drain(..) {
            w.abort();
        }
        info!("系統運行時已停止");
        Ok(())
    }

    /// 獲取市場視圖快照（已遷移至 runtime_management）
    #[allow(dead_code)]
    pub async fn get_market_view_legacy(&self) -> Arc<engine::aggregation::MarketView> {
        self.engine.lock().await.get_market_view()
    }

    /// 獲取賬戶視圖快照（已遷移至 runtime_management）
    #[allow(dead_code)]
    pub async fn get_account_view_legacy(&self) -> Arc<ports::AccountView> {
        self.engine.lock().await.get_account_view()
    }

    pub fn portfolio_manager(&self) -> Option<&PortfolioManager> {
        self.portfolio_manager.as_ref()
    }

    /// 取消指定策略的所有未結訂單（非阻塞）：
    /// 返回已下發取消請求的訂單數量
    #[allow(dead_code)]
    pub async fn cancel_orders_for_strategy_legacy(&self, strategy_id: &str) -> usize {
        // 收集該策略未結訂單 (order_id, symbol)
        let pairs = {
            let eng = self.engine.lock().await;
            eng.open_order_pairs_by_strategy(strategy_id)
        };
        if pairs.is_empty() {
            return 0;
        }
        // 發送取消請求到所有執行 worker
        for tx in &self.exec_control_txs {
            let _ = tx.send(engine::execution_worker::ControlCommand::CancelOrders(
                pairs.clone(),
            ));
        }
        pairs.len()
    }

    /// 熱更新風控配置：替換風控管理器並應用新的策略覆蓋
    #[allow(dead_code)]
    pub async fn update_risk_config_legacy(
        &mut self,
        new_risk: RiskConfig,
    ) -> Result<(), Box<dyn std::error::Error>> {
        tracing::info!(
            "更新風控配置: 風控類型={}, 覆蓋策略數={}",
            new_risk.risk_type,
            new_risk.strategy_overrides.len()
        );
        // 更新運行時配置
        self.config.risk = new_risk;

        // 重新創建風控管理器（帶策略覆蓋）
        let new_manager =
            crate::RiskManagerFactory::create_strategy_aware_risk_manager(&self.config.risk);

        // 註冊到引擎（覆蓋舊的風控管理器）
        let mut eng = self.engine.lock().await;
        eng.register_risk_manager_boxed(new_manager);

        tracing::info!("風控配置已更新並生效");
        Ok(())
    }

    /// 測試：通过执行队列发送订单意图 (异步处理)
    #[allow(dead_code)]
    pub async fn place_test_order_legacy(
        &self,
        symbol: &str,
    ) -> Result<hft_core::OrderId, Box<dyn std::error::Error>> {
        use hft_core::{OrderType, Quantity, Side, Symbol, TimeInForce};
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
        let test_order_id = hft_core::OrderId(format!(
            "test_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        info!(
            "測試訂單已成功提交到執行隊列: {} {}",
            symbol, test_order_id.0
        );
        Ok(test_order_id)
    }

    // legacy Redis exporter 已移除（請使用 infra_exporters 版本）

    // legacy ClickHouse exporter 已移除（請使用 infra_exporters 版本）
    // clone_for_ipc 已移至 runtime_management 模組
}

#[cfg(feature = "adapter-backpack-data")]
fn parse_backpack_market_config(
    venue_cfg: &VenueConfig,
    symbols: &[Symbol],
) -> Option<adapter_backpack_data::BackpackMarketConfig> {
    let mut config = adapter_backpack_data::BackpackMarketConfig::default();

    if let Some(value) = venue_cfg.data_config.clone() {
        if let Some(parsed) = deserialize_backpack_market_value(value) {
            config = parsed;
        } else {
            warn!(
                "Backpack data_config 解析失敗 (venue: {}), 將使用預設設定",
                venue_cfg.name
            );
        }
    }

    if let Some(ws) = venue_cfg.ws_public.clone() {
        if config.ws_url == adapter_backpack_data::DEFAULT_WS_URL {
            config.ws_url = ws;
        }
    }

    if config.default_symbols.is_empty() && !symbols.is_empty() {
        config.default_symbols = symbols.to_vec();
    }

    Some(config)
}

#[cfg(feature = "adapter-backpack-data")]
fn deserialize_backpack_market_value(
    value: YamlValue,
) -> Option<adapter_backpack_data::BackpackMarketConfig> {
    match value {
        YamlValue::Mapping(mut map) => {
            if let Some(adapter) = map
                .get(&YamlValue::from("adapter_type"))
                .and_then(|v| v.as_str())
            {
                if !adapter.eq_ignore_ascii_case("backpack") {
                    warn!(
                        "data_config.adapter_type = {} 非 backpack，忽略自訂配置",
                        adapter
                    );
                    return None;
                }
            }
            map.remove(&YamlValue::from("adapter_type"));
            serde_yaml::from_value(YamlValue::Mapping(map)).ok()
        }
        other => serde_yaml::from_value(other).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_instrument::InstrumentId;

    #[cfg(feature = "adapter-backpack-data")]
    #[test]
    fn backpack_market_config_overrides_defaults() {
        let data_yaml = serde_yaml::from_str::<YamlValue>(
            r#"
adapter_type: backpack
ws_url: wss://api.custom
subscribe_depth: false
reconnect_interval_ms: 2000
default_symbols:
  - SOL_USDC
"#,
        )
        .unwrap();

        let venue_cfg = VenueConfig {
            name: "backpack".to_string(),
            account_id: None,
            venue_type: VenueType::Backpack,
            ws_public: Some("wss://custom".to_string()),
            ws_private: None,
            rest: Some("https://overridden".to_string()),
            api_key: None,
            secret: None,
            passphrase: None,
            execution_mode: Some("Paper".to_string()),
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: Vec::<InstrumentId>::new(),
            data_config: Some(data_yaml),
            execution_config: None,
        };

        let symbols = vec![Symbol("BTC_USDC".to_string())];
        let cfg = parse_backpack_market_config(&venue_cfg, &symbols).unwrap();

        assert_eq!(cfg.ws_url, "wss://api.custom");
        assert!(!cfg.subscribe_depth);
        assert!(cfg.subscribe_trades);
        assert_eq!(cfg.reconnect_interval_ms, 2000);
        assert_eq!(cfg.default_symbols, vec![Symbol("SOL_USDC".to_string())]);
    }
}

fn to_shared_strategy_type(rt: &StrategyType) -> SharedStrategyType {
    match rt {
        StrategyType::Trend => SharedStrategyType::Trend,
        StrategyType::Arbitrage => SharedStrategyType::Arbitrage,
        StrategyType::MarketMaking => SharedStrategyType::MarketMaking,
        StrategyType::Dl => SharedStrategyType::Dl,
        StrategyType::Imbalance => SharedStrategyType::Imbalance,
        StrategyType::LobFlowGrid => SharedStrategyType::LobFlowGrid,
    }
}

fn spawn_engine_loop(
    engine_arc: Arc<Mutex<Engine>>,
    notify: Arc<Notify>,
    cpu_core: Option<usize>,
) -> tokio::task::JoinHandle<()> {
    if let Some(core_index) = cpu_core {
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            set_thread_affinity(core_index);
            handle.block_on(engine_event_loop(engine_arc, notify));
        })
    } else {
        tokio::spawn(engine_event_loop(engine_arc, notify))
    }
}

async fn engine_event_loop(engine_arc: Arc<Mutex<Engine>>, notify: Arc<Notify>) {
    let mut backoff_ms = 1u64; // 从 1ms 开始
    let max_backoff_ms = 100u64; // 最大 100ms

    loop {
        // 事件驱动：等待唤醒通知或自适应超时
        let was_notified = tokio::select! {
            _ = notify.notified() => {
                backoff_ms = 1;
                true
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {
                false
            }
        };

        let (tick_result, had_activity) = {
            let mut eng = engine_arc.lock().await;
            let prev_stats = eng.get_statistics();
            let tick_result = eng.tick();
            let new_stats = eng.get_statistics();
            let had_activity = new_stats.cycle_count > prev_stats.cycle_count;
            (tick_result, had_activity)
        };

        if let Err(e) = tick_result {
            tracing::error!("Engine tick error: {}", e);
        }

        if !was_notified {
            if had_activity {
                backoff_ms = (backoff_ms / 2).max(1);
            } else {
                backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
            }
        }

        let running = {
            let eng = engine_arc.lock().await;
            eng.get_statistics().is_running
        };
        if !running {
            break;
        }
    }
}

fn set_thread_affinity(core_index: usize) {
    if let Some(cores) = core_affinity::get_core_ids() {
        if let Some(core) = cores.iter().find(|c| c.id == core_index).cloned() {
            if core_affinity::set_for_current(core) {
                info!("Engine loop pinned to CPU core {}", core_index);
            } else {
                warn!("Failed to set CPU affinity for engine core {}", core_index);
            }
        } else {
            warn!(
                "Requested engine core {} not available (available cores: {})",
                core_index,
                cores.len()
            );
        }
    } else {
        warn!("Unable to retrieve CPU core ids; engine affinity not applied");
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
                cpu_affinity: CpuAffinityConfig::default(),
                ack_timeout_ms: 3000,
                reconcile_interval_ms: 5000,
                auto_cancel_exchange_only: false,
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
            quotes_only: false,
            router: None,
            infra: None,
            strategy_accounts: std::collections::HashMap::new(),
            accounts: Vec::new(),
            portfolios: Vec::new(),
        }
    }
}
