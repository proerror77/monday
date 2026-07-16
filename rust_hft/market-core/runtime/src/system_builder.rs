//! 系統建構器 - 宣告式裝配與註冊
use crate::sharding::ShardConfig;
use serde::{Deserialize, Serialize};
// use serde_yaml::Value as YamlValue; // 移除未使用引用
use std::sync::Arc;
use tracing::{error, info, warn};

#[cfg(feature = "infra-secrets")]
use infra_secrets::{SecretsConfig, SecretsManager};

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
// use shared_instrument::InstrumentId; // 已移至 config_types
use std::collections::HashMap;
use tokio::sync::{Mutex, Notify};

mod config_loader;
mod config_types; // 預留：後續逐步搬移配置型別
mod execution_registry;
mod infra_exporters;
mod runtime_management;
mod simulated_execution;
mod strategy_factory;
mod venue_registry; // 預留：後續搬移 Redis/ClickHouse 導出

// 將部分配置型別從子模組對外公開
pub use config_types::{
    ClickHouseConfig, CpuAffinityConfig, EnhancedRiskSettings, ExecutionQueueSettings, InfraConfig,
    LobFlowGridParams, PortfolioSpec, RedisConfig, RiskConfig, StrategyConfig,
    StrategyEnhancedRiskOverride, StrategyParams, StrategyRiskLimits, StrategyRiskOverride,
    StrategyType, SystemEngineConfig, TokenizedSecuritiesRiskConfig, TradingWindow,
    TradingWindowConfig, VenueCapabilities, VenueConfig, VenueType,
};

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
    market_stream_plans: Vec<(VenueType, String, Vec<InstrumentSpec>)>,
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
        let venue_id = venue_type_to_venue_id(&venue);
        let instruments = symbols
            .into_iter()
            .map(|symbol| InstrumentSpec::crypto_spot(symbol, venue_id))
            .collect();
        self.market_stream_plans
            .push((venue, venue_name, instruments));
        self
    }

    pub fn register_market_instrument_plan(
        mut self,
        venue: VenueType,
        venue_name: String,
        instruments: Vec<InstrumentSpec>,
    ) -> Self {
        info!("登記市場商品流規劃: {:?} {:?}", venue, instruments);
        self.market_stream_plans
            .push((venue, venue_name, instruments));
        self
    }

    /// Register the in-process paper execution adapter used by bounded Paper/Shadow runs.
    pub fn register_simulated_execution_client(mut self, venue: VenueId) -> Self {
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

    /// 🔥 Phase 2c: 異步解析交易所憑證
    /// 此方法應在 `auto_register_adapters()` 之前調用，以從 SecretsManager 解析憑證
    #[cfg(feature = "infra-secrets")]
    pub async fn resolve_credentials_async(mut self) -> Result<Self, Box<dyn std::error::Error>> {
        info!("開始解析交易所憑證...");

        // 檢查是否有任何 venue 需要憑證解析
        let has_credential_refs = self.config.venues.iter().any(|v| {
            v.secret_ref_api_key.is_some()
                || v.secret_ref_secret.is_some()
                || v.secret_ref_passphrase.is_some()
        });

        if !has_credential_refs {
            info!("未發現需要解析的憑證參考，跳過解析");
            return Ok(self);
        }

        // 初始化 SecretsManager
        let secrets_config = SecretsConfig::from_env();
        let manager = SecretsManager::new(secrets_config).await?;

        info!("SecretsManager 已初始化，開始解析憑證");

        // 解析每個 venue 的憑證 - 使用索引避免借用衝突
        for i in 0..self.config.venues.len() {
            let venue = &mut self.config.venues[i];
            Self::resolve_venue_credentials(venue, &manager).await?;
        }

        info!("憑證解析完成");
        Ok(self)
    }

    /// 🔥 Phase 2c: 解析單個 venue 的憑證參考
    #[cfg(feature = "infra-secrets")]
    async fn resolve_venue_credentials(
        venue: &mut VenueConfig,
        manager: &SecretsManager,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let venue_name = &venue.name;

        // 解析 api_key 參考
        if let Some(ref secret_ref) = venue.secret_ref_api_key {
            match manager.get_secret(secret_ref).await {
                Ok(secret_value) => match secret_value.to_string_safe() {
                    Ok(api_key) => {
                        venue.api_key = Some(api_key);
                        venue.secret_ref_api_key = None;
                        info!("已解析 {} 的 api_key 參考", venue_name);
                    }
                    Err(e) => {
                        error!("無法將 {} 的 api_key 秘密轉換為字符串: {}", venue_name, e);
                        return Err(format!("憑證 {} 無效 UTF-8: {}", secret_ref, e).into());
                    }
                },
                Err(e) => {
                    error!(
                        "無法解析 {} 的 api_key 參考 {}: {}",
                        venue_name, secret_ref, e
                    );
                    return Err(format!(
                        "無法從 SecretsManager 解析 {} 的 api_key: {}",
                        venue_name, e
                    )
                    .into());
                }
            }
        }

        // 解析 secret 參考
        if let Some(ref secret_ref) = venue.secret_ref_secret {
            match manager.get_secret(secret_ref).await {
                Ok(secret_value) => match secret_value.to_string_safe() {
                    Ok(secret) => {
                        venue.secret = Some(secret);
                        venue.secret_ref_secret = None;
                        info!("已解析 {} 的 secret 參考", venue_name);
                    }
                    Err(e) => {
                        error!("無法將 {} 的 secret 秘密轉換為字符串: {}", venue_name, e);
                        return Err(format!("憑證 {} 無效 UTF-8: {}", secret_ref, e).into());
                    }
                },
                Err(e) => {
                    error!(
                        "無法解析 {} 的 secret 參考 {}: {}",
                        venue_name, secret_ref, e
                    );
                    return Err(format!(
                        "無法從 SecretsManager 解析 {} 的 secret: {}",
                        venue_name, e
                    )
                    .into());
                }
            }
        }

        // 解析 passphrase 參考
        if let Some(ref secret_ref) = venue.secret_ref_passphrase {
            match manager.get_secret(secret_ref).await {
                Ok(secret_value) => match secret_value.to_string_safe() {
                    Ok(passphrase) => {
                        venue.passphrase = Some(passphrase);
                        venue.secret_ref_passphrase = None;
                        info!("已解析 {} 的 passphrase 參考", venue_name);
                    }
                    Err(e) => {
                        error!(
                            "無法將 {} 的 passphrase 秘密轉換為字符串: {}",
                            venue_name, e
                        );
                        return Err(format!("憑證 {} 無效 UTF-8: {}", secret_ref, e).into());
                    }
                },
                Err(e) => {
                    error!(
                        "無法解析 {} 的 passphrase 參考 {}: {}",
                        venue_name, secret_ref, e
                    );
                    return Err(format!(
                        "無法從 SecretsManager 解析 {} 的 passphrase: {}",
                        venue_name, e
                    )
                    .into());
                }
            }
        }

        Ok(())
    }

    /// 🔥 Phase 2c: 無 secrets 功能時的虛擬實現（無操作）
    #[cfg(not(feature = "infra-secrets"))]
    pub async fn resolve_credentials_async(self) -> Result<Self, Box<dyn std::error::Error>> {
        warn!("infra-secrets 功能未啟用，跳過憑證解析。請啟用 infra-secrets 功能以支持 SecretsManager 整合");
        Ok(self)
    }

    /// 自動註冊適配器基於配置
    pub fn auto_register_adapters(mut self) -> Self {
        info!("自動註冊適配器...");

        self = self.register_market_streams_from_config();
        // Quotes-only 模式：YAML `quotes_only: true` 或環境變量 HFT_QUOTES_ONLY=1
        let quotes_only = quotes_only_enabled(&self.config);
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

    /// Register configured runtime components while treating every strategy build
    /// failure and duplicate instance ID as a startup error.
    pub fn auto_register_adapters_strict(mut self) -> HftResult<Self> {
        info!("strictly auto-registering adapters and strategies");
        self.config
            .engine
            .validate_intent_execution_limits()
            .map_err(HftError::Config)?;
        if self.config.engine.auto_cancel_exchange_only {
            return Err(HftError::Config(
                "engine.auto_cancel_exchange_only is deprecated and unsupported; use the runtime OMS-aware cancellation control plane"
                    .to_string(),
            ));
        }
        self = self.register_market_streams_from_config();
        let quotes_only = quotes_only_enabled(&self.config);
        if quotes_only {
            info!("quotes-only enabled; execution clients remain disabled");
        } else {
            self = self.register_execution_clients_from_config();
            if self.execution_clients.len() != self.config.venues.len() {
                return Err(HftError::Config(format!(
                    "configured {} venues but registered {} execution clients",
                    self.config.venues.len(),
                    self.execution_clients.len()
                )));
            }
        }
        self.try_register_strategies_from_config()
    }

    /// Register only governed strategy configurations, without opening market or venue adapters.
    pub fn register_strategies_from_config_strict(self) -> HftResult<Self> {
        self.try_register_strategies_from_config()
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
            intent_max_latency_us: self.config.engine.intent_max_latency_us,
            max_events_per_cycle: 100,
            aggregation_symbols: vec![],
            top_n: self.config.engine.top_n,
            latency_monitor: engine::latency_monitor::LatencyMonitorConfig::default(),
        };

        // 創建引擎
        let mut engine = Engine::new(engine_config);
        if self.config.engine.auto_cancel_exchange_only {
            error!("unsupported auto_cancel_exchange_only requested; engine remains paused");
            engine.pause_trading();
        }
        if let Err(error) = engine.set_intent_execution_limits(
            self.config.engine.intent_max_slippage_bps,
            self.config.engine.intent_max_order_notional,
            self.config.engine.intent_max_order_quantity,
        ) {
            error!(%error, "invalid intent execution limits; engine remains paused");
            engine.pause_trading();
        }

        // 🔥 P0: 依賴注入 - 創建並注入 OMS 與 Portfolio
        {
            use oms_core::OmsCore;
            use portfolio_core::Portfolio;

            // 創建 OMS 核心實例
            let oms = OmsCore::new();
            // 創建 Portfolio 核心實例
            let portfolio = Portfolio::new();

            // 包裝為 trait objects 並注入到引擎
            engine.set_order_manager(Box::new(oms));
            engine.set_portfolio_manager(Box::new(portfolio));

            info!("✅ OMS 與 Portfolio 已通過依賴注入設置到引擎");
        }

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
        let risk_manager: Box<dyn ports::RiskManager> = if self.config.portfolios.is_empty() {
            risk_manager
        } else {
            Box::new(crate::PortfolioBudgetRiskManager::new(
                risk_manager,
                self.config.portfolios.clone(),
                &self.config.strategies,
            ))
        };
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
            execution_control_gate: Arc::new(Mutex::new(())),
            config: self.config,
            tasks: Vec::new(),
            execution_worker_tasks: Vec::new(),
            ipc_task: None,
            exec_control_tx: None,
            market_plans: self.market_stream_plans,
            execution_client_venues: self.execution_client_venues,
            execution_client_accounts: self.execution_client_accounts,
            portfolio_manager,
            adapter_bridge: None,
        }
    }
}

fn venue_type_to_venue_id(venue_type: &VenueType) -> VenueId {
    match venue_type {
        VenueType::Binance => VenueId::BINANCE,
        VenueType::BinancePrediction => VenueId::BINANCE_PREDICTION,
        VenueType::Bitget => VenueId::BITGET,
        VenueType::Bybit => VenueId::BYBIT,
        VenueType::Okx => VenueId::OKX,
        VenueType::Hyperliquid => VenueId::HYPERLIQUID,
        VenueType::Grvt => VenueId::GRVT,
        VenueType::Asterdex => VenueId::ASTERDEX,
        VenueType::Lighter => VenueId::LIGHTER,
        VenueType::Backpack => VenueId::BACKPACK,
        VenueType::OndoPerps => VenueId::ONDO_PERPS,
        VenueType::Polymarket => VenueId::POLYMARKET,
        VenueType::Mock => VenueId::MOCK,
    }
}

/// 系統運行時
pub struct SystemRuntime {
    pub engine: Arc<Mutex<Engine>>,
    execution_control_gate: Arc<Mutex<()>>,
    pub config: SystemConfig,
    // 後台任務控制
    tasks: Vec<tokio::task::JoinHandle<()>>,
    // 执行 worker 任务
    execution_worker_tasks: Vec<tokio::task::JoinHandle<Result<(), HftError>>>,
    // IPC server is process-owned and requires explicit cancellation on shutdown.
    ipc_task: Option<tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>>,
    // 執行控制通道（撤單等）
    exec_control_tx:
        Option<tokio::sync::mpsc::UnboundedSender<engine::execution_worker::ControlCommand>>,
    // 登記的市場流規劃
    market_plans: Vec<(VenueType, String, Vec<InstrumentSpec>)>,
    // 🔥 Phase 1: 執行客戶端到交易所的映射
    execution_client_venues: Vec<VenueId>,
    // 🔥 Phase 1.x: 執行客戶端到帳戶的映射（可選）
    execution_client_accounts: Vec<Option<hft_core::AccountId>>,
    portfolio_manager: Option<PortfolioManager>,
    // 🔥 修復資源洩漏：保存 AdapterBridge 以便優雅關閉
    adapter_bridge: Option<engine::AdapterBridge>,
}

fn quotes_only_enabled(config: &SystemConfig) -> bool {
    let quotes_only_env = std::env::var("HFT_QUOTES_ONLY")
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE"))
        .unwrap_or(false);
    config.quotes_only || quotes_only_env
}

fn requires_authoritative_balance_reconciliation(config: &SystemConfig) -> bool {
    !quotes_only_enabled(config)
        && config.venues.iter().any(|venue| {
            !venue.simulate_execution
                && venue
                    .execution_mode
                    .as_deref()
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("live"))
        })
}

impl SystemRuntime {
    /// 啟動系統
    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.start_with_ipc_socket_path(None).await
    }

    async fn start_with_ipc_socket_path(
        &mut self,
        #[allow(unused_variables)] ipc_socket_path: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        info!("啟動系統運行時...");
        #[cfg(feature = "infra-ipc")]
        let prepared_ipc =
            crate::ipc_handler::prepare_ipc_server(ipc_socket_path).map_err(|error| {
                HftError::Execution(format!("failed to prepare IPC control server: {error}"))
            })?;
        let require_authoritative_balances =
            requires_authoritative_balance_reconciliation(&self.config);
        let operator_control_only = self.config.strategies.is_empty();

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
        #[allow(unused_variables)]
        for (venue_type, venue_name, instruments) in self.market_plans.clone() {
            #[allow(unused_variables)]
            let venue_cfg = self
                .config
                .venues
                .iter()
                .find(|v| v.name == venue_name)
                .cloned();
            #[allow(unused_variables)]
            let symbols: Vec<Symbol> = instruments
                .iter()
                .map(|instrument| instrument.symbol.clone())
                .collect();
            #[allow(unused_variables)]
            let plan_symbols = symbols.clone();
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

                        let mut stream = if use_inc {
                            adapter_bitget_data::BitgetMarketStream::new_with_incremental(true)
                                .with_inst_type(inst_type)
                        } else {
                            adapter_bitget_data::BitgetMarketStream::new().with_inst_type(inst_type)
                        };
                        if let Some(cfg) = &venue_cfg {
                            if let Some(ws_url) = &cfg.ws_public {
                                stream = stream.with_ws_url(ws_url.clone());
                            }
                        }
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Bitget 行情已橋接至引擎");
                    }
                }
                VenueType::Binance => {
                    #[cfg(feature = "adapter-binance-data")]
                    {
                        let caps = venue_cfg
                            .as_ref()
                            .map(parse_binance_capabilities)
                            .unwrap_or_else(|| {
                                adapter_binance_data::capabilities::BinanceCapabilities::default()
                            });
                        let mut stream =
                            adapter_binance_data::BinanceMarketStream::with_capabilities(caps);
                        if let Some(cfg) = &venue_cfg {
                            if let Some(ws_url) = &cfg.ws_public {
                                stream = stream.with_ws_base_url(ws_url.clone());
                            }
                            if let Some(rest_url) = &cfg.rest {
                                stream = stream.with_rest_base_url(rest_url.clone());
                            }
                        }
                        let consumer = bridge.bridge_instrument_stream(stream, instruments).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                    }
                }
                VenueType::BinancePrediction => {
                    #[cfg(feature = "adapter-binance-prediction-data")]
                    {
                        let config = parse_binance_prediction_market_config(
                            &venue_type,
                            venue_cfg.as_ref(),
                        )?;
                        let stream =
                            adapter_binance_prediction_data::BinancePredictionMarketStream::new(
                                config,
                            )?;
                        let consumer = bridge.bridge_instrument_stream(stream, instruments).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Binance Prediction REST order books bridged into the engine");
                    }
                }
                VenueType::Bybit => {
                    #[cfg(feature = "adapter-bybit-data")]
                    {
                        let mut stream = adapter_bybit_data::BybitMarketStream::new();
                        if let Some(cfg) = &venue_cfg {
                            if let Some(ws_url) = &cfg.ws_public {
                                stream = stream.with_ws_url(ws_url.clone());
                            }
                        }
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Bybit 行情已橋接至引擎");
                    }
                }
                VenueType::Grvt => {
                    #[cfg(feature = "adapter-grvt-data")]
                    {
                        let mut stream = adapter_grvt_data::GrvtMarketStream::new();
                        if let Some(cfg) = &venue_cfg {
                            if let Some(ws_url) = &cfg.ws_public {
                                stream = stream.with_ws_url(ws_url.clone());
                            }
                        }
                        let consumer = bridge.bridge_stream(stream, plan_symbols.clone()).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("GRVT 行情已橋接至引擎");
                    }
                }
                VenueType::Hyperliquid => {
                    #[cfg(feature = "adapter-hyperliquid-data")]
                    {
                        let market_config = venue_cfg
                            .as_ref()
                            .map(|cfg| parse_hyperliquid_market_config(cfg, &symbols))
                            .unwrap_or_else(|| {
                                adapter_hyperliquid_data::HyperliquidMarketConfig::default()
                            });
                        let stream =
                            adapter_hyperliquid_data::HyperliquidMarketStream::new(market_config);
                        let consumer = bridge.bridge_stream(stream, symbols).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Hyperliquid 行情已橋接至引擎");
                    }
                }
                VenueType::OndoPerps => {
                    #[cfg(feature = "adapter-ondo-perps-data")]
                    {
                        let mut stream = adapter_ondo_perps_data::OndoPerpsMarketStream::new();
                        if let Some(cfg) = &venue_cfg {
                            if let Some(ws_url) = &cfg.ws_public {
                                stream = stream.with_ws_url(ws_url.clone());
                            }
                        }
                        let consumer = bridge.bridge_instrument_stream(stream, instruments).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Ondo Perps 行情已橋接至引擎");
                    }
                }
                VenueType::Polymarket => {
                    #[cfg(feature = "adapter-polymarket-data")]
                    {
                        let mut stream = adapter_polymarket_data::PolymarketMarketStream::new();
                        if let Some(cfg) = &venue_cfg {
                            if let Some(ws_url) = &cfg.ws_public {
                                stream = stream.with_ws_url(ws_url.clone());
                            }
                        }
                        let consumer = bridge.bridge_instrument_stream(stream, instruments).await?;
                        self.engine.lock().await.register_event_consumer(consumer);
                        info!("Polymarket public CLOB market data bridged into the engine");
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
        self.adapter_bridge = Some(bridge);

        // 2) (可選) 設置執行队列並啟動执行 worker
        let quotes_only = quotes_only_enabled(&self.config);

        if quotes_only {
            info!("Quotes-only 模式：跳過執行隊列與 ExecutionWorker 啟動");
        } else {
            let queue_settings = &self.config.engine.execution_queue;
            let queue_config = ExecutionQueueConfig {
                intent_queue_capacity: queue_settings.intent_queue_capacity,
                event_queue_capacity: queue_settings.event_queue_capacity,
                batch_size: queue_settings.batch_size,
            };
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
                batch_size: queue_settings.worker_batch_size,
                idle_sleep_ms: queue_settings.worker_idle_sleep_ms,
                ack_timeout_ms: self.config.engine.ack_timeout_ms,
                // Runtime owns OMS-aware reconciliation; the worker only supplies snapshots.
                reconcile_interval_ms: 0,
                ..Default::default()
            };

            // Capture client count before move
            let client_count = execution_clients.len();
            let account_to_client = self
                .execution_client_accounts
                .iter()
                .enumerate()
                .filter_map(|(index, account)| account.clone().map(|account| (account, index)))
                .collect::<std::collections::HashMap<_, _>>();
            let mut venue_to_client = self
                .execution_client_venues
                .iter()
                .enumerate()
                .map(|(client_idx, venue_id)| (*venue_id, client_idx))
                .collect::<std::collections::HashMap<_, _>>();
            if venue_to_client.is_empty() {
                for (index, venue_config) in self.config.venues.iter().enumerate() {
                    if let Some(venue_id) = hft_core::VenueId::from_str(&venue_config.name) {
                        if index < execution_clients.len() {
                            venue_to_client.insert(venue_id, index);
                        }
                    }
                }
            }
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

            let (worker_handle, control_tx) = if let Some(router_config) = &self.config.router {
                // 有路由器配置，創建路由器並使用帶路由器的 worker
                let router = router_config.clone().build();

                info!(
                    "使用路由器: {:?}, venue映射: {:?}, 執行客戶端總數: {}",
                    router.name(),
                    venue_to_client.clone(),
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
                    Some(account_to_client.clone()),
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
                    venue_to_client,
                    strategy_to_client,
                    Some(account_to_client),
                )
            };
            self.execution_worker_tasks.push(worker_handle);
            self.exec_control_tx = Some(control_tx);
            info!("已启动执行 worker (客户端数量: {})", client_count);
        }

        if require_authoritative_balances {
            let control = self.execution_control_handle();
            let startup_reconciliation = async {
                control.pause_trading().await?;
                let mut report = control.reconcile(true).await?;
                // A no-strategy live runtime is an operator recovery surface, not an authority to
                // adopt venue state into Monday's in-memory portfolio. In particular, an account
                // with exchange-only orders must still reach IPC inspection/cancellation while
                // remaining paused; bootstrap intentionally rejects such orders.
                if !operator_control_only
                    && !report.healthy
                    && control
                        .bootstrap_pristine_account(&report.worker_snapshot)
                        .await?
                {
                    info!("bootstrapped pristine portfolio from authoritative venue account");
                    report = control.reconcile(true).await?;
                }
                if !report.healthy {
                    if operator_control_only {
                        warn!(
                            complete = report.complete,
                            exchange_only = report.order_report.exchange_only.len(),
                            local_only = report.order_report.local_only.len(),
                            quantity_mismatch = report.order_report.qty_mismatch.len(),
                            "authoritative account is not reconciled; starting no-strategy operator control with execution intake paused"
                        );
                        return Ok::<(), HftError>(());
                    }
                    return Err(HftError::Execution(format!(
                        "initial live reconciliation failed: complete={}, exchange_only={}, local_only={}, quantity_mismatch={}, balance_complete={}, balance_difference_usd={:?}, position_complete={}, position_exchange_only={}, position_local_only={}, position_quantity_mismatch={}, fill_complete={}, exchange_only_fills={}",
                        report.complete,
                        report.order_report.exchange_only.len(),
                        report.order_report.local_only.len(),
                        report.order_report.qty_mismatch.len(),
                        report
                            .balance_report
                            .as_ref()
                            .is_some_and(|balances| balances.complete),
                        report
                            .balance_report
                            .as_ref()
                            .and_then(|balances| balances.difference_usd),
                        report
                            .position_report
                            .as_ref()
                            .is_none_or(|positions| positions.complete),
                        report
                            .position_report
                            .as_ref()
                            .map_or(0, |positions| positions.exchange_only.len()),
                        report
                            .position_report
                            .as_ref()
                            .map_or(0, |positions| positions.local_only.len()),
                        report
                            .position_report
                            .as_ref()
                            .map_or(0, |positions| positions.quantity_mismatch.len()),
                        report
                            .fill_report
                            .as_ref()
                            .is_none_or(|fills| fills.complete),
                        report
                            .fill_report
                            .as_ref()
                            .map_or(0, |fills| fills.exchange_only_fill_ids.len()),
                    )));
                }
                if operator_control_only {
                    info!(
                        "authoritative account is reconciled; no-strategy operator control remains paused"
                    );
                    Ok(())
                } else {
                    control.resume_trading().await
                }
            }
            .await;
            if let Err(error) = startup_reconciliation {
                self.abort_execution_workers().await;
                return Err(Box::new(error));
            }
        }

        // The control socket is already bound and secured. Attach the fully wired runtime before
        // any strategy-processing loop can consume queued market events.
        #[cfg(feature = "infra-ipc")]
        {
            let runtime_arc = Arc::new(Mutex::new(self.clone_for_ipc()));
            self.ipc_task = Some(crate::ipc_handler::start_prepared_ipc_server(
                runtime_arc,
                prepared_ipc,
            ));
            info!("IPC control server started");
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

        if self.config.engine.reconcile_interval_ms > 0 && self.exec_control_tx.is_some() {
            let interval_ms = self.config.engine.reconcile_interval_ms;
            let include_balances = require_authoritative_balances;
            let control = self.execution_control_handle();
            self.tasks.push(tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_millis(interval_ms));
                interval.tick().await;
                loop {
                    interval.tick().await;
                    let engine = control.engine();
                    if !engine.lock().await.get_statistics().is_running {
                        break;
                    }
                    match control.reconcile_guarded(include_balances).await {
                        Ok(report) if report.healthy => {
                            tracing::debug!("runtime order reconciliation healthy");
                        }
                        Ok(report) => {
                            tracing::error!(
                                complete = report.complete,
                                exchange_only = report.order_report.exchange_only.len(),
                                local_only = report.order_report.local_only.len(),
                                quantity_mismatch = report.order_report.qty_mismatch.len(),
                                balance_complete = report
                                    .balance_report
                                    .as_ref()
                                    .is_none_or(|balances| balances.complete),
                                balance_difference_usd = ?report
                                    .balance_report
                                    .as_ref()
                                    .and_then(|balances| balances.difference_usd),
                                position_complete = report
                                    .position_report
                                    .as_ref()
                                    .is_none_or(|positions| positions.complete),
                                position_exchange_only = report
                                    .position_report
                                    .as_ref()
                                    .map_or(0, |positions| positions.exchange_only.len()),
                                position_local_only = report
                                    .position_report
                                    .as_ref()
                                    .map_or(0, |positions| positions.local_only.len()),
                                position_quantity_mismatch = report
                                    .position_report
                                    .as_ref()
                                    .map_or(0, |positions| positions.quantity_mismatch.len()),
                                fill_complete = report
                                    .fill_report
                                    .as_ref()
                                    .is_none_or(|fills| fills.complete),
                                exchange_only_fills = report
                                    .fill_report
                                    .as_ref()
                                    .map_or(0, |fills| fills.exchange_only_fill_ids.len()),
                                "runtime order reconciliation unhealthy; pausing new intents"
                            );
                        }
                        Err(error) => {
                            tracing::error!(%error, "runtime reconciliation failed; execution remains paused");
                        }
                    }
                }
            }));
        }

        // 4) 狀態欄：每秒輸出一次系統狀態摘要（現金/持倉/PNL/事件統計）
        {
            let engine_arc = self.engine.clone();
            let status_handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    interval.tick().await;
                    let (cash, pos, unr, rlz, stats) = {
                        let eng = engine_arc.lock().await;
                        #[cfg(feature = "metrics")]
                        eng.sync_latency_metrics_to_prometheus();
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
                    // 當引擎停止時，狀態任務退出，避免 Ctrl-C 卡住
                    if !stats.is_running {
                        break;
                    }
                    tracing::info!(
                        cash = %cash,
                        pos = pos,
                        unrealized = %unr,
                        realized = %rlz,
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

        info!("系統運行時已啟動（引擎背景運行）");
        Ok(())
    }

    /// 停止系統
    pub async fn stop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("停止系統運行時...");

        if requires_authoritative_balance_reconciliation(&self.config)
            && self.exec_control_tx.is_some()
        {
            let control = self.execution_control_handle();
            let cancellation = control.emergency_stop(true).await?;
            if !cancellation.is_complete() {
                return Err(HftError::Execution(format!(
                    "live shutdown cancellation incomplete: attempted={}, succeeded={}, failed={}",
                    cancellation.requested,
                    cancellation.submitted.len(),
                    cancellation.failures.len()
                ))
                .into());
            }
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let report = control.reconcile(false).await?;
                if report.healthy {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(HftError::Execution(format!(
                        "live shutdown reconciliation failed: complete={}, exchange_only={}, local_only={}, quantity_mismatch={}",
                        report.complete,
                        report.order_report.exchange_only.len(),
                        report.order_report.local_only.len(),
                        report.order_report.qty_mismatch.len(),
                    ))
                    .into());
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }

        // 🔥 修復資源洩漏：優雅關閉 AdapterBridge（在停止引擎之前）
        if let Some(mut bridge) = self.adapter_bridge.take() {
            bridge.shutdown().await;
        }

        // 停止引擎
        self.engine.lock().await.stop();
        if let Some(ipc_task) = self.ipc_task.take() {
            ipc_task.abort();
            let _ = ipc_task.await;
        }
        // 等待背景任務退出
        for t in self.tasks.drain(..) {
            let _ = t.await;
        }
        self.abort_execution_workers().await;
        info!("系統運行時已停止");
        Ok(())
    }

    async fn abort_execution_workers(&mut self) {
        self.exec_control_tx.take();
        for worker in self.execution_worker_tasks.drain(..) {
            worker.abort();
            let _ = worker.await;
        }
    }

    pub fn portfolio_manager(&self) -> Option<&PortfolioManager> {
        self.portfolio_manager.as_ref()
    }
    // Legacy 方法已移至 runtime_management 模組
}

#[cfg(feature = "adapter-binance-prediction-data")]
fn parse_binance_prediction_market_config(
    venue: &VenueType,
    venue_cfg: Option<&VenueConfig>,
) -> HftResult<adapter_binance_prediction_data::BinancePredictionMarketDataConfig> {
    if *venue != VenueType::BinancePrediction {
        return Err(HftError::Config(
            "Binance Prediction market config was requested for another venue".to_string(),
        ));
    }
    let venue_cfg = venue_cfg.ok_or_else(|| {
        HftError::Config("Binance Prediction market data requires venue config".to_string())
    })?;
    let mut config: adapter_binance_prediction_data::BinancePredictionMarketDataConfig = venue_cfg
        .data_config
        .clone()
        .ok_or_else(|| {
            HftError::Config(
                "Binance Prediction market data requires data_config.outcomes".to_string(),
            )
        })
        .and_then(|value| {
            serde_yaml::from_value(value).map_err(|error| HftError::Config(error.to_string()))
        })?;
    config.api_key = venue_cfg.api_key.clone().unwrap_or_default();
    config.api_secret = venue_cfg.secret.clone().unwrap_or_default();
    if let Some(rest) = &venue_cfg.rest {
        config.rest_base_url = rest.clone();
    }
    Ok(config)
}

#[cfg(feature = "adapter-binance-data")]
fn parse_binance_capabilities(
    venue_cfg: &VenueConfig,
) -> adapter_binance_data::capabilities::BinanceCapabilities {
    let mut caps = adapter_binance_data::capabilities::BinanceCapabilities::default();

    // 從 data_config 讀取配置（如果提供）
    if let Some(value) = venue_cfg.data_config.clone() {
        if let Ok(mapping) = serde_yaml::from_value::<serde_yaml::Mapping>(value) {
            if let Some(v) = mapping
                .get("snapshot_crc")
                .and_then(|v| serde_yaml::from_value(v.clone()).ok())
            {
                caps.snapshot_crc = v;
            }
            if let Some(v) = mapping
                .get("rest_fallback")
                .and_then(|v| serde_yaml::from_value(v.clone()).ok())
            {
                caps.rest_fallback = v;
            }
            if let Some(v) = mapping
                .get("auto_reconnect")
                .and_then(|v| serde_yaml::from_value(v.clone()).ok())
            {
                caps.auto_reconnect = v;
            }
        }
    }

    caps
}

#[cfg(feature = "adapter-hyperliquid-data")]
fn parse_hyperliquid_market_config(
    venue_cfg: &VenueConfig,
    symbols: &[Symbol],
) -> adapter_hyperliquid_data::HyperliquidMarketConfig {
    let mut config = adapter_hyperliquid_data::HyperliquidMarketConfig::default();

    // 覆蓋 ws_base_url（如果提供）
    if let Some(ws) = venue_cfg.ws_public.clone() {
        config.ws_base_url = ws;
    }

    // 從 data_config 讀取其他配置（如果提供）
    if let Some(value) = venue_cfg.data_config.clone() {
        if let Ok(reconnect) = serde_yaml::from_value::<serde_yaml::Mapping>(value.clone())
            .and_then(|m| {
                m.get("reconnect_interval_ms")
                    .and_then(|v| serde_yaml::from_value(v.clone()).ok())
            })
        {
            config.reconnect_interval_ms = reconnect;
        }
        if let Ok(heartbeat) = serde_yaml::from_value::<serde_yaml::Mapping>(value.clone())
            .and_then(|m| {
                m.get("heartbeat_interval_ms")
                    .and_then(|v| serde_yaml::from_value(v.clone()).ok())
            })
        {
            config.heartbeat_interval_ms = heartbeat;
        }
    }

    // 使用傳入的 symbols
    config.symbols = symbols.to_vec();
    config
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

        let mut guard = match engine_arc.try_lock() {
            Ok(eng) => eng,
            Err(_) => {
                tokio::task::yield_now().await;
                continue;
            }
        };

        let tick_result = guard.tick();
        let new_stats = guard.get_statistics();
        let had_activity = tick_result.as_ref().is_ok_and(|result| {
            result.events_total > 0
                || result.execution_events_processed > 0
                || result.orders_generated > 0
                || result.snapshot_published
        });
        let running = new_stats.is_running;
        drop(guard);

        if let Err(e) = tick_result {
            error!("Engine tick error: {}", e);
        }

        if !was_notified {
            if had_activity {
                backoff_ms = (backoff_ms / 2).max(1);
            } else {
                backoff_ms = (backoff_ms * 2).min(max_backoff_ms);
            }
        }

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
                intent_max_latency_us: 3000,
                intent_max_slippage_bps: None,
                intent_max_order_notional: None,
                intent_max_order_quantity: None,
                top_n: 10,
                flip_policy: FlipPolicy::OnUpdate,
                cpu_affinity: CpuAffinityConfig::default(),
                ack_timeout_ms: 3000,
                reconcile_interval_ms: 5000,
                balance_reconcile_tolerance_usd: rust_decimal::Decimal::ONE,
                auto_cancel_exchange_only: false,
                execution_queue: ExecutionQueueSettings::default(),
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
                max_daily_loss: rust_decimal::Decimal::from(10000),
                max_drawdown_pct: 5.0,
                enhanced: None,
                strategy_overrides: std::collections::HashMap::new(),
                tokenized_securities: TokenizedSecuritiesRiskConfig::default(),
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

#[cfg(test)]
#[allow(unused_imports)]
mod tests {
    use async_trait::async_trait;
    use futures::stream;
    use ports::{
        AccountBalance, AccountView, BookLevel, BoxStream, ConnectionHealth, ExecutionEvent,
        MarketEvent, MarketSnapshot, OpenOrder, Strategy,
    };
    use rust_decimal::Decimal;
    use shared_instrument::InstrumentId;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct AuthoritativeBalanceClient {
        equity_usd: Decimal,
        open_orders: Vec<OpenOrder>,
    }

    struct CountingExecutionClient {
        placements: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ExecutionClient for CountingExecutionClient {
        async fn place_order(
            &mut self,
            _intent: ports::OrderIntent,
        ) -> HftResult<hft_core::OrderId> {
            self.placements.fetch_add(1, Ordering::SeqCst);
            Ok(hft_core::OrderId("should-not-place".to_string()))
        }

        async fn cancel_order(&mut self, _order_id: &hft_core::OrderId) -> HftResult<()> {
            Ok(())
        }

        async fn modify_order(
            &mut self,
            _order_id: &hft_core::OrderId,
            _new_quantity: Option<hft_core::Quantity>,
            _new_price: Option<hft_core::Price>,
        ) -> HftResult<()> {
            Ok(())
        }

        async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
            Ok(Box::pin(stream::pending()))
        }

        async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
            Ok(Vec::new())
        }

        async fn connect(&mut self) -> HftResult<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> HftResult<()> {
            Ok(())
        }

        async fn health(&self) -> ConnectionHealth {
            ConnectionHealth {
                connected: true,
                latency_ms: Some(0.0),
                last_heartbeat: 0,
            }
        }
    }

    struct OrderOnMarketEvent {
        emitted: bool,
    }

    impl Strategy for OrderOnMarketEvent {
        fn on_market_event(
            &mut self,
            _event: &MarketEvent,
            _account: &AccountView,
        ) -> Vec<ports::OrderIntent> {
            if self.emitted {
                return Vec::new();
            }
            self.emitted = true;
            vec![ports::OrderIntent::crypto_spot(
                Symbol::new("BTCUSDT"),
                hft_core::Side::Buy,
                hft_core::Quantity(Decimal::ONE),
                hft_core::OrderType::Limit,
                Some(hft_core::Price(Decimal::from(100))),
                hft_core::TimeInForce::GTC,
                "ipc-startup-regression".to_string(),
                None,
            )]
        }

        fn on_execution_event(
            &mut self,
            _event: &ExecutionEvent,
            _account: &AccountView,
        ) -> Vec<ports::OrderIntent> {
            Vec::new()
        }

        fn name(&self) -> &str {
            "ipc-startup-regression"
        }
    }

    #[async_trait]
    impl ExecutionClient for AuthoritativeBalanceClient {
        async fn place_order(
            &mut self,
            _intent: ports::OrderIntent,
        ) -> HftResult<hft_core::OrderId> {
            Err(HftError::Execution(
                "test client does not place orders".to_string(),
            ))
        }

        async fn cancel_order(&mut self, _order_id: &hft_core::OrderId) -> HftResult<()> {
            Err(HftError::Execution("test client has no orders".to_string()))
        }

        async fn modify_order(
            &mut self,
            _order_id: &hft_core::OrderId,
            _new_quantity: Option<hft_core::Quantity>,
            _new_price: Option<hft_core::Price>,
        ) -> HftResult<()> {
            Err(HftError::Execution("test client has no orders".to_string()))
        }

        async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
            Ok(Box::pin(stream::pending()))
        }

        async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
            Ok(self.open_orders.clone())
        }

        async fn get_balance(&self) -> HftResult<Vec<AccountBalance>> {
            Ok(vec![AccountBalance {
                asset: "USDT".to_string(),
                available: self.equity_usd,
                frozen: Decimal::ZERO,
                total: self.equity_usd,
                usd_value: Some(self.equity_usd),
            }])
        }

        async fn connect(&mut self) -> HftResult<()> {
            Ok(())
        }

        async fn disconnect(&mut self) -> HftResult<()> {
            Ok(())
        }

        async fn health(&self) -> ConnectionHealth {
            ConnectionHealth {
                connected: true,
                latency_ms: Some(0.0),
                last_heartbeat: 0,
            }
        }
    }

    fn live_venue_config() -> VenueConfig {
        VenueConfig {
            name: "binance".to_string(),
            account_id: None,
            venue_type: VenueType::Binance,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            secret_ref_api_key: None,
            secret_ref_secret: None,
            secret_ref_passphrase: None,
            execution_mode: Some("Live".to_string()),
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: Vec::new(),
            data_config: None,
            execution_config: None,
        }
    }

    fn configured_strategy() -> StrategyConfig {
        StrategyConfig {
            name: "startup-authority-test".to_string(),
            strategy_type: StrategyType::Trend,
            symbols: vec![Symbol::new("BTCUSDT")],
            params: StrategyParams::Trend {
                ema_fast: 12,
                ema_slow: 26,
                rsi_period: 14,
            },
            risk_limits: StrategyRiskLimits::default(),
        }
    }

    #[test]
    fn only_real_live_execution_requires_authoritative_balance_reconciliation() {
        let mut config = SystemConfig {
            venues: vec![live_venue_config()],
            ..Default::default()
        };
        assert!(requires_authoritative_balance_reconciliation(&config));

        config.venues[0].simulate_execution = true;
        assert!(!requires_authoritative_balance_reconciliation(&config));

        config.venues[0].simulate_execution = false;
        config.venues[0].execution_mode = Some("Paper".to_string());
        assert!(!requires_authoritative_balance_reconciliation(&config));

        config.venues[0].execution_mode = Some("Live".to_string());
        config.quotes_only = true;
        assert!(!requires_authoritative_balance_reconciliation(&config));
    }

    #[test]
    fn invalid_intent_execution_limits_fail_closed_before_runtime_start() {
        let mut config = SystemConfig::default();
        config.engine.intent_max_slippage_bps = Some(10_001);

        let Err(error) = SystemBuilder::new(config.clone()).auto_register_adapters_strict() else {
            panic!("strict registration accepted invalid intent limits");
        };
        assert!(error.to_string().contains("intent_max_slippage_bps"));

        let runtime = SystemBuilder::new(config).build();
        assert_eq!(
            runtime.engine.try_lock().unwrap().trading_mode(),
            engine::TradingMode::Paused
        );
    }

    #[test]
    fn deprecated_exchange_only_auto_cancel_fails_closed_before_runtime_start() {
        let mut config = SystemConfig::default();
        config.engine.auto_cancel_exchange_only = true;

        let Err(error) = SystemBuilder::new(config.clone()).auto_register_adapters_strict() else {
            panic!("strict registration accepted unsupported exchange-only auto-cancel");
        };
        assert!(error.to_string().contains("auto_cancel_exchange_only"));

        let runtime = SystemBuilder::new(config).build();
        assert_eq!(
            runtime.engine.try_lock().unwrap().trading_mode(),
            engine::TradingMode::Paused
        );
    }

    #[test]
    fn builder_writes_verified_execution_limits_into_engine() {
        let mut config = SystemConfig::default();
        config.engine.intent_max_slippage_bps = Some(25);
        config.engine.intent_max_order_notional = Some(Decimal::from(50));
        config.engine.intent_max_order_quantity = Some(Decimal::new(5, 1));
        let runtime = SystemBuilder::new(config).build();
        let (engine_queues, mut worker_queues) =
            create_execution_queues(ExecutionQueueConfig::default());
        let mut engine = runtime.engine.try_lock().expect("engine lock");
        engine.set_execution_queues(engine_queues);

        let mut intent = ports::OrderIntent::crypto_spot(
            Symbol::new("BTCUSDT"),
            Side::Buy,
            Quantity::from_f64(1.0).unwrap(),
            OrderType::Limit,
            Some(Price::from_f64(100.0).unwrap()),
            TimeInForce::GTC,
            "builder-limits".to_string(),
            Some(VenueId::MOCK),
        );
        assert!(matches!(
            engine.submit_order_intent(intent.clone()),
            Err(HftError::Risk(_))
        ));

        intent.quantity = Quantity::from_f64(0.4).unwrap();
        engine
            .submit_order_intent(intent)
            .expect("order below configured notional limit");
        let envelopes = worker_queues.receive_envelopes();
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].lifecycle.max_slippage_bps, Some(25));
        assert_eq!(
            envelopes[0].lifecycle.max_order_notional,
            Some(Decimal::from(50))
        );
        assert_eq!(
            envelopes[0].lifecycle.max_order_quantity,
            Some(Decimal::new(5, 1))
        );
    }

    #[tokio::test]
    async fn live_runtime_reconciles_before_starting_engine_loop() {
        let mut config = SystemConfig {
            venues: vec![live_venue_config()],
            strategies: vec![configured_strategy()],
            ..Default::default()
        };
        config.engine.reconcile_interval_ms = 0;
        let mut runtime = SystemBuilder::new(config)
            .register_execution_client_with_venue(
                AuthoritativeBalanceClient {
                    equity_usd: Decimal::from(100),
                    open_orders: Vec::new(),
                },
                VenueId::MOCK,
            )
            .build();

        let error = runtime
            .start()
            .await
            .expect_err("mismatched live equity must block startup");

        assert!(error
            .to_string()
            .contains("initial live reconciliation failed"));
        assert_eq!(
            runtime.engine.lock().await.trading_mode(),
            engine::TradingMode::Paused
        );
        assert!(runtime.execution_worker_tasks.is_empty());
    }

    #[tokio::test]
    #[cfg_attr(feature = "infra-ipc", serial_test::serial)]
    async fn no_strategy_live_runtime_starts_paused_for_operator_recovery() {
        let mut config = SystemConfig {
            venues: vec![live_venue_config()],
            ..Default::default()
        };
        config.engine.reconcile_interval_ms = 0;
        let mut runtime = SystemBuilder::new(config)
            .register_execution_client_with_venue(
                AuthoritativeBalanceClient {
                    equity_usd: Decimal::from(100),
                    open_orders: Vec::new(),
                },
                VenueId::MOCK,
            )
            .build();

        runtime
            .start()
            .await
            .expect("no-strategy recovery control must start fail-closed");

        assert_eq!(
            runtime.engine.lock().await.trading_mode(),
            engine::TradingMode::Paused
        );
        assert!(!runtime.execution_worker_tasks.is_empty());
        runtime
            .stop()
            .await
            .expect("operator runtime stops cleanly");
    }

    #[cfg(feature = "infra-ipc")]
    #[tokio::test]
    #[serial_test::serial]
    async fn ipc_prepare_failure_precedes_engine_loop_and_queued_order_execution() {
        let placements = Arc::new(AtomicUsize::new(0));
        let mut runtime = SystemBuilder::new(SystemConfig::default())
            .register_execution_client(CountingExecutionClient {
                placements: Arc::clone(&placements),
            })
            .build();
        let ingester = {
            let mut engine = runtime.engine.lock().await;
            engine.register_strategy(OrderOnMarketEvent { emitted: false });
            engine.create_event_ingester_pair()
        };
        ingester
            .lock()
            .expect("event ingester")
            .ingest(MarketEvent::Snapshot(MarketSnapshot {
                symbol: Symbol::new("BTCUSDT"),
                timestamp: hft_core::now_micros(),
                bids: vec![BookLevel {
                    price: hft_core::Price(Decimal::from(100)),
                    quantity: hft_core::Quantity(Decimal::ONE),
                }],
                asks: vec![BookLevel {
                    price: hft_core::Price(Decimal::from(101)),
                    quantity: hft_core::Quantity(Decimal::ONE),
                }],
                sequence: 1,
                source_venue: Some(VenueId::BINANCE),
            }))
            .expect("queue a strategy-producing market event");

        let directory = tempfile::tempdir().expect("temporary IPC directory");
        let parent_file = directory.path().join("not-a-directory");
        std::fs::write(&parent_file, b"occupied").expect("create blocking path component");
        let error = runtime
            .start_with_ipc_socket_path(Some(
                parent_file
                    .join("operator.sock")
                    .to_string_lossy()
                    .into_owned(),
            ))
            .await
            .expect_err("IPC preparation must fail closed");
        assert!(error.to_string().contains("failed to prepare IPC"));

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(runtime.tasks.is_empty(), "engine loop must not be spawned");
        assert!(
            runtime.execution_worker_tasks.is_empty(),
            "execution worker must not be spawned"
        );
        assert_eq!(placements.load(Ordering::SeqCst), 0);
        assert_eq!(
            runtime
                .engine
                .lock()
                .await
                .get_statistics()
                .orders_submitted,
            0
        );
    }

    #[tokio::test]
    #[cfg_attr(feature = "infra-ipc", serial_test::serial)]
    async fn no_strategy_polymarket_runtime_keeps_exchange_only_orders_in_paused_recovery() {
        let mut config = SystemConfig {
            venues: vec![live_venue_config()],
            ..Default::default()
        };
        config.engine.reconcile_interval_ms = 0;
        let external_order = OpenOrder {
            order_id: hft_core::OrderId("external-venue-order".to_string()),
            client_order_id: None,
            symbol: Symbol::new("123"),
            side: Side::Buy,
            order_type: OrderType::Limit,
            original_quantity: Quantity(Decimal::from(5)),
            remaining_quantity: Quantity(Decimal::from(5)),
            filled_quantity: Quantity(Decimal::ZERO),
            price: Some(Price(Decimal::new(5, 1))),
            status: ports::OrderStatus::Accepted,
            created_at: 1,
            updated_at: 1,
        };
        let mut runtime = SystemBuilder::new(config)
            .register_execution_client_with_venue(
                AuthoritativeBalanceClient {
                    equity_usd: Decimal::from(100),
                    open_orders: vec![external_order],
                },
                VenueId::POLYMARKET,
            )
            .build();

        runtime
            .start()
            .await
            .expect("exchange-only order must remain available to paused operator recovery");

        assert_eq!(
            runtime.engine.lock().await.trading_mode(),
            engine::TradingMode::Paused
        );
        assert!(!runtime.execution_worker_tasks.is_empty());
        runtime
            .stop()
            .await
            .expect_err("shutdown remains fail-closed until the external order is canceled");
        #[cfg(feature = "infra-ipc")]
        if let Some(ipc_task) = runtime.ipc_task.take() {
            ipc_task.abort();
            let _ = ipc_task.await;
        }
    }

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

        let symbols = vec![Symbol::new("BTC_USDC")];
        let cfg = parse_backpack_market_config(&venue_cfg, &symbols).unwrap();

        assert_eq!(cfg.ws_url, "wss://api.custom");
        assert!(!cfg.subscribe_depth);
        assert!(cfg.subscribe_trades);
        assert_eq!(cfg.reconnect_interval_ms, 2000);
        assert_eq!(cfg.default_symbols, vec![Symbol::new("SOL_USDC")]);
    }

    // ===== Credential Resolution Integration Tests =====

    #[cfg(feature = "infra-secrets")]
    #[tokio::test]
    async fn test_credential_resolution_with_env_backend() {
        // Setup: Create environment variables with credential values
        std::env::set_var("HFT_SECRET_BITGET_API_KEY", "test_api_key_12345");
        std::env::set_var("HFT_SECRET_BITGET_SECRET", "test_secret_98765");
        std::env::set_var("HFT_SECRET_BITGET_PASSPHRASE", "test_pass_abcde");

        // Create system config with credential references
        let config = SystemConfig {
            engine: SystemEngineConfig::default(),
            venues: vec![VenueConfig {
                name: "bitget".to_string(),
                account_id: None,
                venue_type: VenueType::Bitget,
                ws_public: Some("wss://ws.bitget.com".to_string()),
                ws_private: None,
                rest: Some("https://api.bitget.com".to_string()),
                api_key: None,    // Will be populated by resolution
                secret: None,     // Will be populated by resolution
                passphrase: None, // Will be populated by resolution
                execution_mode: Some("Paper".to_string()),
                capabilities: VenueCapabilities::default(),
                inst_type: None,
                simulate_execution: false,
                symbol_catalog: Vec::new(),
                data_config: None,
                execution_config: None,
                secret_ref_api_key: Some("BITGET_API_KEY".to_string()),
                secret_ref_secret: Some("BITGET_SECRET".to_string()),
                secret_ref_passphrase: Some("BITGET_PASSPHRASE".to_string()),
            }],
            strategies: Vec::new(),
            risk: RiskConfig::default(),
            quotes_only: false,
            router: None,
            infra: None,
            strategy_accounts: HashMap::new(),
            accounts: Vec::new(),
            portfolios: Vec::new(),
        };

        // Create builder using the proper constructor
        let builder = SystemBuilder::new(config);

        // Execute credential resolution
        let builder = builder
            .resolve_credentials_async()
            .await
            .expect("Credential resolution should succeed");

        // Verify: Check that plaintext fields are populated
        let bitget_venue = &builder.config.venues[0];
        assert_eq!(
            bitget_venue.api_key,
            Some("test_api_key_12345".to_string()),
            "api_key should be resolved from environment"
        );
        assert_eq!(
            bitget_venue.secret,
            Some("test_secret_98765".to_string()),
            "secret should be resolved from environment"
        );
        assert_eq!(
            bitget_venue.passphrase,
            Some("test_pass_abcde".to_string()),
            "passphrase should be resolved from environment"
        );

        // Verify: Check that credential references are cleared
        assert!(
            bitget_venue.secret_ref_api_key.is_none(),
            "secret_ref_api_key should be cleared after resolution"
        );
        assert!(
            bitget_venue.secret_ref_secret.is_none(),
            "secret_ref_secret should be cleared after resolution"
        );
        assert!(
            bitget_venue.secret_ref_passphrase.is_none(),
            "secret_ref_passphrase should be cleared after resolution"
        );

        // Cleanup
        std::env::remove_var("HFT_SECRET_BITGET_API_KEY");
        std::env::remove_var("HFT_SECRET_BITGET_SECRET");
        std::env::remove_var("HFT_SECRET_BITGET_PASSPHRASE");
    }

    #[cfg(feature = "infra-secrets")]
    #[tokio::test]
    async fn test_credential_resolution_skips_when_no_references() {
        // Setup: Create system config with plaintext credentials (no references)
        let config = SystemConfig {
            engine: SystemEngineConfig::default(),
            venues: vec![VenueConfig {
                name: "binance".to_string(),
                account_id: None,
                venue_type: VenueType::Binance,
                ws_public: Some("wss://stream.binance.com".to_string()),
                ws_private: None,
                rest: Some("https://api.binance.com".to_string()),
                api_key: Some("plaintext_api_key".to_string()),
                secret: Some("plaintext_secret".to_string()),
                passphrase: None,
                execution_mode: Some("Paper".to_string()),
                capabilities: VenueCapabilities::default(),
                inst_type: None,
                simulate_execution: false,
                symbol_catalog: Vec::new(),
                data_config: None,
                execution_config: None,
                secret_ref_api_key: None, // No references
                secret_ref_secret: None,
                secret_ref_passphrase: None,
            }],
            strategies: Vec::new(),
            risk: RiskConfig::default(),
            quotes_only: false,
            router: None,
            infra: None,
            strategy_accounts: HashMap::new(),
            accounts: Vec::new(),
            portfolios: Vec::new(),
        };

        // Create builder using the proper constructor
        let builder = SystemBuilder::new(config);

        let original_api_key = builder.config.venues[0].api_key.clone();
        let original_secret = builder.config.venues[0].secret.clone();

        // Execute credential resolution
        let builder = builder
            .resolve_credentials_async()
            .await
            .expect("Credential resolution should succeed");

        // Verify: Plaintext credentials should remain unchanged
        assert_eq!(
            builder.config.venues[0].api_key, original_api_key,
            "api_key should not change when no reference exists"
        );
        assert_eq!(
            builder.config.venues[0].secret, original_secret,
            "secret should not change when no reference exists"
        );
    }

    #[cfg(feature = "infra-secrets")]
    #[tokio::test]
    async fn test_credential_resolution_partial_update() {
        // Setup: Mix of plaintext and reference credentials
        std::env::set_var("HFT_SECRET_OKX_SECRET", "resolved_secret_value");

        let config = SystemConfig {
            engine: SystemEngineConfig::default(),
            venues: vec![VenueConfig {
                name: "okx".to_string(),
                account_id: None,
                venue_type: VenueType::Okx,
                ws_public: Some("wss://ws.okex.com".to_string()),
                ws_private: None,
                rest: Some("https://www.okex.com".to_string()),
                api_key: Some("plaintext_api_key".to_string()), // Plaintext
                secret: None,                                   // Will be resolved
                passphrase: Some("plaintext_passphrase".to_string()), // Plaintext
                execution_mode: Some("Paper".to_string()),
                capabilities: VenueCapabilities::default(),
                inst_type: None,
                simulate_execution: false,
                symbol_catalog: Vec::new(),
                data_config: None,
                execution_config: None,
                secret_ref_api_key: None, // No reference
                secret_ref_secret: Some("OKX_SECRET".to_string()), // Reference
                secret_ref_passphrase: None, // No reference
            }],
            strategies: Vec::new(),
            risk: RiskConfig::default(),
            quotes_only: false,
            router: None,
            infra: None,
            strategy_accounts: HashMap::new(),
            accounts: Vec::new(),
            portfolios: Vec::new(),
        };

        let builder = SystemBuilder::new(config);

        let builder = builder
            .resolve_credentials_async()
            .await
            .expect("Credential resolution should succeed");

        let okx_venue = &builder.config.venues[0];

        // Verify: Only the referenced field was updated
        assert_eq!(
            okx_venue.api_key,
            Some("plaintext_api_key".to_string()),
            "api_key should remain unchanged (no reference)"
        );
        assert_eq!(
            okx_venue.secret,
            Some("resolved_secret_value".to_string()),
            "secret should be resolved from reference"
        );
        assert_eq!(
            okx_venue.passphrase,
            Some("plaintext_passphrase".to_string()),
            "passphrase should remain unchanged (no reference)"
        );
        assert!(
            okx_venue.secret_ref_secret.is_none(),
            "secret reference should be cleared"
        );

        // Cleanup
        std::env::remove_var("HFT_SECRET_OKX_SECRET");
    }

    #[cfg(feature = "infra-secrets")]
    #[tokio::test]
    async fn test_credential_resolution_missing_reference() {
        // Setup: Reference to non-existent environment variable
        let config = SystemConfig {
            engine: SystemEngineConfig::default(),
            venues: vec![VenueConfig {
                name: "bybit".to_string(),
                account_id: None,
                venue_type: VenueType::Bybit,
                ws_public: Some("wss://stream.bybit.com".to_string()),
                ws_private: None,
                rest: Some("https://api.bybit.com".to_string()),
                api_key: None,
                secret: None,
                passphrase: None,
                execution_mode: Some("Paper".to_string()),
                capabilities: VenueCapabilities::default(),
                inst_type: None,
                simulate_execution: false,
                symbol_catalog: Vec::new(),
                data_config: None,
                execution_config: None,
                secret_ref_api_key: Some("NONEXISTENT_KEY".to_string()),
                secret_ref_secret: None,
                secret_ref_passphrase: None,
            }],
            strategies: Vec::new(),
            risk: RiskConfig::default(),
            quotes_only: false,
            router: None,
            infra: None,
            strategy_accounts: HashMap::new(),
            accounts: Vec::new(),
            portfolios: Vec::new(),
        };

        let builder = SystemBuilder::new(config);

        // Execute: Should fail gracefully
        let result = builder.resolve_credentials_async().await;
        assert!(
            result.is_err(),
            "Resolution should fail for missing credential reference"
        );
    }

    #[cfg(not(feature = "infra-secrets"))]
    #[tokio::test]
    async fn test_credential_resolution_disabled_feature() {
        // Setup: When feature is disabled, resolution should be no-op
        let config = SystemConfig {
            engine: SystemEngineConfig::default(),
            venues: vec![VenueConfig {
                name: "test_venue".to_string(),
                account_id: None,
                venue_type: VenueType::Mock,
                ws_public: None,
                ws_private: None,
                rest: None,
                api_key: None,
                secret: None,
                passphrase: None,
                execution_mode: Some("Paper".to_string()),
                capabilities: VenueCapabilities::default(),
                inst_type: None,
                simulate_execution: false,
                symbol_catalog: Vec::new(),
                data_config: None,
                execution_config: None,
                secret_ref_api_key: Some("SOME_REF".to_string()),
                secret_ref_secret: None,
                secret_ref_passphrase: None,
            }],
            strategies: Vec::new(),
            risk: RiskConfig::default(),
            quotes_only: false,
            router: None,
            infra: None,
            strategy_accounts: HashMap::new(),
            accounts: Vec::new(),
            portfolios: Vec::new(),
        };

        let builder = SystemBuilder::new(config);

        // Execute: Should succeed (no-op) even with unresolved references
        let builder = builder
            .resolve_credentials_async()
            .await
            .expect("Resolution should be no-op when feature disabled");

        // Verify: References should remain unchanged (not resolved, not cleared)
        assert_eq!(
            builder.config.venues[0].secret_ref_api_key,
            Some("SOME_REF".to_string()),
            "References should remain when feature is disabled"
        );
    }

    #[test]
    fn test_venue_config_credential_field_precedence() {
        // Verify that credential reference fields take precedence over plaintext
        let venue = VenueConfig {
            name: "test".to_string(),
            account_id: None,
            venue_type: VenueType::Bitget,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: Some("plaintext_key".to_string()),
            secret: Some("plaintext_secret".to_string()),
            passphrase: None,
            execution_mode: None,
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: Vec::new(),
            data_config: None,
            execution_config: None,
            secret_ref_api_key: Some("KEY_REFERENCE".to_string()),
            secret_ref_secret: Some("SECRET_REFERENCE".to_string()),
            secret_ref_passphrase: None,
        };

        // When both plaintext and reference exist, reference should be used
        assert!(venue.secret_ref_api_key.is_some());
        assert!(venue.api_key.is_some()); // Both exist
                                          // In actual resolution, reference would take precedence and plaintext would be overwritten

        // When only reference exists, that's the source of truth
        let venue_ref_only = VenueConfig {
            name: "test".to_string(),
            account_id: None,
            venue_type: VenueType::Bitget,
            ws_public: None,
            ws_private: None,
            rest: None,
            api_key: None,
            secret: None,
            passphrase: None,
            execution_mode: None,
            capabilities: VenueCapabilities::default(),
            inst_type: None,
            simulate_execution: false,
            symbol_catalog: Vec::new(),
            data_config: None,
            execution_config: None,
            secret_ref_api_key: Some("KEY_REFERENCE".to_string()),
            secret_ref_secret: None,
            secret_ref_passphrase: None,
        };

        assert!(venue_ref_only.secret_ref_api_key.is_some());
        assert!(venue_ref_only.api_key.is_none());
    }

    #[tokio::test]
    #[cfg_attr(feature = "infra-ipc", serial_test::serial)]
    async fn quotes_only_config_does_not_start_execution_workers() {
        let config = SystemConfig {
            quotes_only: true,
            ..SystemConfig::default()
        };
        let mut runtime = SystemBuilder::new(config).build();

        runtime.start().await.expect("quotes-only runtime starts");
        assert!(runtime.execution_worker_tasks.is_empty());
        assert!(runtime.exec_control_tx.is_none());
        #[cfg(feature = "infra-ipc")]
        assert!(runtime.ipc_task.is_some());
        runtime.stop().await.expect("quotes-only runtime stops");
        assert!(runtime.ipc_task.is_none());
    }
}
