//! 配置型別模組：逐步將 system_builder.rs 中的配置結構抽離於此
//! 注意：優先重用 shared_config 既有型別，僅保留 runtime 專屬欄位。

use serde::{Deserialize, Serialize};
use engine::dataflow::FlipPolicy;
use rust_decimal::Decimal;
use std::collections::HashMap;

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemEngineConfig {
    pub queue_capacity: usize,
    pub stale_us: u64,
    pub top_n: usize,
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
}

fn default_ack_timeout_ms() -> u64 { 3000 }
fn default_reconcile_interval_ms() -> u64 { 5000 }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CpuAffinityConfig {
    /// 將引擎主循環綁定到指定 CPU 核心（0-based）
    pub engine_core: Option<usize>,
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

fn default_risk_type() -> String { "Default".to_string() }

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
