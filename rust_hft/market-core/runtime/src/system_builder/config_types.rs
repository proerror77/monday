//! 配置型別模組：逐步將 system_builder.rs 中的配置結構抽離於此
//! 注意：優先重用 shared_config 既有型別，僅保留 runtime 專屬欄位。

use serde::{Deserialize, Serialize};
use engine::dataflow::FlipPolicy;

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
