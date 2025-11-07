//! 配置型別模組：逐步將 system_builder.rs 中的配置結構抽離於此
//! 注意：優先重用 shared_config 既有型別，僅保留 runtime 專屬欄位。

use engine::dataflow::FlipPolicy;
use hft_core::Symbol;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
// use crate::system_builder::VenueId; // 未使用，保留註解

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

/// 引擎層配置（從 system_builder.rs 抽離）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemEngineConfig {
    #[serde(default = "default_queue_capacity")]
    pub queue_capacity: usize,
    #[serde(default = "default_stale_us")]
    pub stale_us: u64,
    #[serde(default = "default_top_n")]
    pub top_n: usize,
    #[serde(default)]
    pub flip_policy: FlipPolicy,
    #[serde(default)]
    pub cpu_affinity: CpuAffinityConfig,
    /// Ack timeout for execution worker (ms)
    #[serde(default = "default_ack_timeout_ms")]
    pub ack_timeout_ms: u64,
    /// Reconciliation interval (ms)
    #[serde(default = "default_reconcile_interval_ms")]
    pub reconcile_interval_ms: u64,
    /// Auto-cancel exchange-only orders discovered in reconciliation
    #[serde(default)]
    pub auto_cancel_exchange_only: bool,
    /// 執行隊列與 worker 設定
    #[serde(default)]
    pub execution_queue: ExecutionQueueSettings,
}

fn default_queue_capacity() -> usize {
    32768
}

fn default_stale_us() -> u64 {
    3000
}

fn default_top_n() -> usize {
    10
}

fn default_ack_timeout_ms() -> u64 {
    3000
}

fn default_reconcile_interval_ms() -> u64 {
    5000
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CpuAffinityConfig {
    /// 將引擎主循環綁定到指定 CPU 核心（0-based）
    pub engine_core: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionQueueSettings {
    #[serde(default = "default_intent_queue_capacity")]
    pub intent_queue_capacity: usize,
    #[serde(default = "default_event_queue_capacity")]
    pub event_queue_capacity: usize,
    #[serde(default = "default_execution_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_worker_batch_size")]
    pub worker_batch_size: usize,
    #[serde(default = "default_worker_idle_sleep_ms")]
    pub worker_idle_sleep_ms: u64,
}

impl Default for ExecutionQueueSettings {
    fn default() -> Self {
        Self {
            intent_queue_capacity: default_intent_queue_capacity(),
            event_queue_capacity: default_event_queue_capacity(),
            batch_size: default_execution_batch_size(),
            worker_batch_size: default_worker_batch_size(),
            worker_idle_sleep_ms: default_worker_idle_sleep_ms(),
        }
    }
}

const fn default_intent_queue_capacity() -> usize {
    4096
}

const fn default_event_queue_capacity() -> usize {
    8192
}

const fn default_execution_batch_size() -> usize {
    32
}

const fn default_worker_batch_size() -> usize {
    16
}

const fn default_worker_idle_sleep_ms() -> u64 {
    1
}

/// 策略級風控限制
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StrategyRiskLimits {
    pub max_notional: Decimal,
    pub max_position: Decimal,
    pub daily_loss_limit: Decimal,
    pub cooldown_ms: u64,
}

/// 風控配置（含增強設置與每策略覆蓋）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RiskConfig {
    /// Risk manager type: "Default" or "Enhanced"
    #[serde(default = "default_risk_type")]
    pub risk_type: String,

    /// Base risk settings (applied to all strategies)
    #[serde(default)]
    pub global_position_limit: Decimal,
    #[serde(default)]
    pub global_notional_limit: Decimal,
    #[serde(default)]
    pub max_daily_trades: u32,
    #[serde(default)]
    pub max_orders_per_second: u32,
    #[serde(default)]
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

/// Enhanced risk manager specific settings（與現有工廠相容的完整欄位）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnhancedRiskSettings {
    // Position and notional limits
    pub max_position_per_symbol: Decimal,
    pub max_order_notional: Decimal,

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
    pub max_daily_loss: Decimal,
    pub max_drawdown_pct: f64,
    pub max_consecutive_losses: u32,
    pub max_position_loss_pct: f64,

    // Circuit breaker
    pub circuit_breaker_enabled: bool,
    pub cb_daily_loss_threshold: Decimal,
    pub cb_drawdown_threshold: f64,
    pub cb_consecutive_losses: u32,
    pub cb_recovery_time_minutes: u64,

    // Trading window (optional)
    pub trading_window: Option<TradingWindowConfig>,

    // System control
    pub aggressive_mode: bool,
    pub dry_run_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct TradingWindow {
    pub start_hhmm: String,
    pub end_hhmm: String,
}

/// Trading window configuration（舊版 runtime 使用的時段結構）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingWindowConfig {
    pub start_hour_utc: u8,
    pub end_hour_utc: u8,
    pub allowed_weekdays: Vec<String>, // ["Monday", "Tuesday", ...]
    #[serde(default)]
    pub market_holidays: Vec<String>, // ISO date strings
}

/// Per-strategy override entry（與現有工廠/loader 相容）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StrategyRiskOverride {
    /// Override global position limit for this strategy
    pub max_position: Option<Decimal>,
    /// Override max notional for this strategy
    pub max_notional: Option<Decimal>,
    /// Override order rate limits
    pub max_orders_per_second: Option<u32>,
    /// Override cooldown period
    pub order_cooldown_ms: Option<u64>,
    /// Override staleness threshold
    pub staleness_threshold_us: Option<u64>,
    /// Override daily loss limit for this strategy
    pub max_daily_loss: Option<Decimal>,
    /// Strategy-specific aggressive mode
    pub aggressive_mode: Option<bool>,
    /// Enhanced-specific overrides (only used with Enhanced risk manager)
    #[serde(default)]
    pub enhanced_overrides: Option<StrategyEnhancedRiskOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StrategyEnhancedRiskOverride {
    pub max_drawdown_pct: Option<f64>,
    pub max_consecutive_losses: Option<u32>,
    pub circuit_breaker_enabled: Option<bool>,
    pub dry_run_mode: Option<bool>,
}

/// 投資組合設定，用於聚合多策略並套用整體限制
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PortfolioSpec {
    pub name: String,
    #[serde(default)]
    pub strategies: Vec<String>,
    #[serde(default)]
    pub max_notional: Option<Decimal>,
    #[serde(default)]
    pub max_position: Option<Decimal>,
    #[serde(default)]
    pub max_drawdown_pct: Option<f64>,
    #[serde(default)]
    pub notes: Option<String>,
}

// ===== Venue definitions =====

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueConfig {
    pub name: String,
    /// 具名帳戶/實例 ID（同交易所多帳戶時用於路由）
    #[serde(default)]
    pub account_id: Option<String>,
    pub venue_type: VenueType,
    pub ws_public: Option<String>,
    pub ws_private: Option<String>,
    pub rest: Option<String>,
    /// API 密鑰（支援 ${VAR_NAME} 展開，或直接值，向後相容）
    pub api_key: Option<String>,
    /// API 秘密（支援 ${VAR_NAME} 展開，或直接值，向後相容）
    pub secret: Option<String>,
    /// Passphrase（支援 ${VAR_NAME} 展開，或直接值，向後相容）
    pub passphrase: Option<String>, // Bitget 等需要 passphrase 的交易所
    /// 秘密管理器參考鍵（取代 api_key，優先於明文值）
    /// 格式：「venue::field」，例：「bitget::api_key」
    /// 優先級：secret_ref > api_key
    #[serde(default)]
    pub secret_ref_api_key: Option<String>,
    /// 秘密管理器參考鍵（取代 secret，優先於明文值）
    /// 格式：「venue::field」，例：「bitget::secret」
    /// 優先級：secret_ref > secret
    #[serde(default)]
    pub secret_ref_secret: Option<String>,
    /// 秘密管理器參考鍵（取代 passphrase，優先於明文值）
    /// 格式：「venue::field」，例：「bitget::passphrase」
    /// 優先級：secret_ref > passphrase
    #[serde(default)]
    pub secret_ref_passphrase: Option<String>,
    pub execution_mode: Option<String>, // "Paper" | "Live"，預設為 Paper
    pub capabilities: VenueCapabilities,
    /// 產品型別（Bitget：SPOT/USDT-FUTURES/COIN-FUTURES/USDC-FUTURES）
    #[serde(default)]
    pub inst_type: Option<String>,
    #[serde(default)]
    pub simulate_execution: bool,
    #[serde(default)]
    pub symbol_catalog: Vec<shared_instrument::InstrumentId>,
    #[serde(default)]
    pub data_config: Option<serde_yaml::Value>,
    #[serde(default)]
    pub execution_config: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VenueType {
    Bitget,
    Binance,
    Bybit,
    Okx,
    Hyperliquid,
    Grvt,
    Asterdex,
    Lighter,
    Backpack,
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

// ===== Strategy definitions =====

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
    LobFlowGrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StrategyParams {
    Trend {
        ema_fast: u32,
        ema_slow: u32,
        rsi_period: u32,
    },
    Arbitrage {
        min_spread_bps: Decimal,
        max_position: Decimal,
        execution_timeout_ms: u64,
    },
    Imbalance {
        obi_threshold: f64,
        lot: Decimal,
        top_levels: usize,
    },
    MarketMaking {
        spread_bps: Decimal,
        max_inventory: Decimal,
        skew_factor: Decimal,
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
    LobFlowGrid {
        #[serde(flatten)]
        config: Box<LobFlowGridParams>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LobFlowGridParams {
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub base_order_size: Option<f64>,
    #[serde(default)]
    pub max_long_position: Option<f64>,
    #[serde(default)]
    pub max_short_position: Option<f64>,
    #[serde(default)]
    pub maker_fee_bps: Option<f64>,
    #[serde(default)]
    pub slippage_bps: Option<f64>,
    #[serde(default)]
    pub safety_bps: Option<f64>,
    #[serde(default)]
    pub min_margin_bps: Option<f64>,
    #[serde(default)]
    pub volatility_alpha: Option<f64>,
    #[serde(default)]
    pub asr_gamma: Option<f64>,
    #[serde(default)]
    pub asr_micro_coeff: Option<f64>,
    #[serde(default)]
    pub asr_ai_coeff: Option<f64>,
    #[serde(default)]
    pub asr_ofi_coeff: Option<f64>,
    #[serde(default)]
    pub top_levels: Option<usize>,
    #[serde(default)]
    pub ai_window_secs: Option<u64>,
    #[serde(default)]
    pub ofi_halflife_secs: Option<f64>,
    #[serde(default)]
    pub mid_return_halflife_secs: Option<f64>,
    #[serde(default)]
    pub refresh_interval_secs: Option<f64>,
    #[serde(default)]
    pub tick_size: Option<f64>,
    #[serde(default)]
    pub lot_size: Option<f64>,
    #[serde(default)]
    pub core_levels: Option<usize>,
    #[serde(default)]
    pub buffer_levels: Option<usize>,
    #[serde(default)]
    pub tail_levels: Option<usize>,
    #[serde(default)]
    pub core_weight: Option<f64>,
    #[serde(default)]
    pub buffer_weight: Option<f64>,
    #[serde(default)]
    pub tail_weight: Option<f64>,
    #[serde(default)]
    pub core_spacing_multiplier: Option<f64>,
    #[serde(default)]
    pub buffer_spacing_multiplier: Option<f64>,
    #[serde(default)]
    pub tail_spacing_multiplier: Option<f64>,
    #[serde(default)]
    pub target_depth_usd: Option<f64>,
    #[serde(default)]
    pub max_depth_steps: Option<usize>,
    #[serde(default)]
    pub min_top_depth_usd: Option<f64>,
    #[serde(default)]
    pub min_spread_bps: Option<f64>,
    #[serde(default)]
    pub max_spread_bps: Option<f64>,
    #[serde(default)]
    pub bias_micro_threshold_bps: Option<f64>,
    #[serde(default)]
    pub bias_ai_threshold: Option<f64>,
    #[serde(default)]
    pub bias_extra_ticks: Option<i32>,
}
