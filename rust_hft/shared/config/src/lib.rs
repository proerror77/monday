use hft_core::{Symbol, VenueId};
use rust_decimal::Decimal;
use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use shared_instrument::{InstrumentId, VenueCapabilities};
use std::collections::HashMap;
use thiserror::Error;

/// 系統配置根節點。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemConfig {
    #[serde(default)]
    pub schema_version: Option<String>,
    pub engine: EngineConfig,
    #[serde(default)]
    pub venues: Vec<VenueConfig>,
    #[serde(default)]
    pub strategies: Vec<StrategyConfig>,
    pub risk: RiskConfig,
    #[serde(default)]
    pub strategy_accounts: HashMap<String, String>,
    #[serde(default)]
    pub portfolios: Vec<PortfolioSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EngineConfig {
    pub queue_capacity: usize,
    pub stale_us: u64,
    pub top_n: usize,
    #[serde(default = "default_ack_timeout_ms")]
    pub ack_timeout_ms: u64,
    #[serde(default = "default_reconcile_interval_ms")]
    pub reconcile_interval_ms: u64,
    #[serde(default)]
    pub auto_cancel_exchange_only: bool,
}

fn default_ack_timeout_ms() -> u64 {
    3000
}
fn default_reconcile_interval_ms() -> u64 {
    5000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueConfig {
    pub name: String,
    #[serde(
        deserialize_with = "venue_id_deserialize",
        serialize_with = "venue_id_serialize"
    )]
    pub venue_type: VenueId,
    #[serde(default)]
    pub symbol_catalog: Vec<InstrumentId>,
    #[serde(default)]
    pub capabilities: VenueCapabilities,
    #[serde(default)]
    pub inst_type: Option<String>,
    #[serde(default)]
    pub simulate_execution: bool,
    #[serde(default)]
    pub data_config: Option<YamlValue>,
    #[serde(default)]
    pub execution_config: Option<YamlValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub name: String,
    pub strategy_type: StrategyType,
    pub symbols: Vec<Symbol>,
    #[serde(default)]
    pub params: StrategyParams,
    #[serde(default)]
    pub risk_limits: Option<StrategyRiskLimits>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PortfolioSpec {
    pub name: String,
    #[serde(default)]
    pub strategies: Vec<String>,
    #[serde(default)]
    pub max_notional: Option<Decimal>,
    #[serde(default)]
    pub max_position: Option<Decimal>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StrategyParams {
    #[default]
    None,
    Trend {
        ema_fast: u32,
        ema_slow: u32,
        rsi_period: u32,
    },
    Imbalance {
        obi_threshold: f64,
        lot: Decimal,
        top_levels: usize,
    },
    Dl {
        model_path: String,
        device: String,
        top_n: usize,
        queue_capacity: usize,
    },
    LobFlowGrid {
        #[serde(flatten)]
        config: HashMap<String, serde_yaml::Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StrategyRiskLimits {
    #[serde(default)]
    pub max_notional: Option<Decimal>,
    #[serde(default)]
    pub max_position: Option<Decimal>,
    #[serde(default)]
    pub daily_loss_limit: Option<Decimal>,
    #[serde(default)]
    pub cooldown_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RiskConfig {
    #[serde(default = "default_risk_type")]
    pub risk_type: String,
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
    #[serde(default)]
    pub enhanced: Option<serde_yaml::Value>,
    #[serde(default)]
    pub strategy_overrides: HashMap<String, serde_yaml::Value>,
}

fn default_risk_type() -> String {
    "Default".to_string()
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to parse config: {0}")]
    Parse(String),
}

impl SystemConfig {
    pub fn from_yaml_str(s: &str) -> Result<Self, ConfigError> {
        serde_yaml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))
    }
}

#[cfg(test)]
mod tests;

impl SystemConfig {
    pub fn example_config() -> &'static str {
        include_str!("../examples/system_v2.yaml")
    }
}

fn venue_id_deserialize<'de, D>(deserializer: D) -> Result<VenueId, D::Error>
where
    D: Deserializer<'de>,
{
    struct VenueIdVisitor;

    impl<'de> serde::de::Visitor<'de> for VenueIdVisitor {
        type Value = VenueId;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("string (e.g. BINANCE) 或數值 venue id")
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(VenueId(value as u16))
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            VenueId::from_str(value).ok_or_else(|| E::custom(format!("未知的 venue_id: {}", value)))
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }
    }

    deserializer.deserialize_any(VenueIdVisitor)
}

fn venue_id_serialize<S>(venue: &VenueId, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(venue.as_str())
}
