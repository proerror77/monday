//! HFT Ops Agent Configuration

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Main agent configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Anthropic API configuration
    pub anthropic: AnthropicConfig,

    /// gRPC server configuration
    pub grpc: GrpcConfig,

    /// Agent behavior configuration
    pub agent: AgentBehaviorConfig,

    /// Alert configuration
    pub alerts: AlertConfig,

    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,
}

/// Anthropic API configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    /// API key (can also be set via ANTHROPIC_API_KEY env var)
    #[serde(default)]
    pub api_key: Option<String>,

    /// Model to use (default: claude-sonnet-4-20250514)
    #[serde(default = "default_model")]
    pub model: String,

    /// Maximum tokens for response
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,

    /// Temperature for response generation
    #[serde(default = "default_temperature")]
    pub temperature: f32,

    /// API base URL
    #[serde(default = "default_api_url")]
    pub api_url: String,
}

fn default_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_temperature() -> f32 {
    0.0
}

fn default_api_url() -> String {
    "https://api.anthropic.com".to_string()
}

/// gRPC server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrpcConfig {
    /// gRPC server address
    #[serde(default = "default_grpc_addr")]
    pub address: String,

    /// Connection timeout in seconds
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,

    /// Request timeout in seconds
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
}

fn default_grpc_addr() -> String {
    "http://127.0.0.1:50051".to_string()
}

fn default_connect_timeout() -> u64 {
    5
}

fn default_request_timeout() -> u64 {
    30
}

/// Agent behavior configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBehaviorConfig {
    /// Monitoring interval in seconds
    #[serde(default = "default_monitor_interval")]
    pub monitor_interval_secs: u64,

    /// Enable automatic anomaly handling
    #[serde(default = "default_auto_handle")]
    pub auto_handle_anomalies: bool,

    /// Dry run mode (log actions without executing)
    #[serde(default)]
    pub dry_run: bool,

    /// Maximum consecutive errors before alerting
    #[serde(default = "default_max_errors")]
    pub max_consecutive_errors: u32,

    /// Anomaly detection thresholds
    #[serde(default)]
    pub thresholds: AnomalyThresholds,
}

fn default_monitor_interval() -> u64 {
    60
}

fn default_auto_handle() -> bool {
    true
}

fn default_max_errors() -> u32 {
    3
}

/// Anomaly detection thresholds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyThresholds {
    /// Latency p99 threshold in microseconds
    #[serde(default = "default_latency_threshold")]
    pub latency_p99_us: u64,

    /// Drawdown warning threshold percentage
    #[serde(default = "default_drawdown_warn")]
    pub drawdown_warn_pct: f64,

    /// Drawdown critical threshold percentage
    #[serde(default = "default_drawdown_critical")]
    pub drawdown_critical_pct: f64,

    /// Maximum reconnect count before alerting
    #[serde(default = "default_max_reconnects")]
    pub max_ws_reconnects: u32,

    /// Order rejection rate threshold (percentage)
    #[serde(default = "default_rejection_rate")]
    pub order_rejection_rate_pct: f64,
}

impl Default for AnomalyThresholds {
    fn default() -> Self {
        Self {
            latency_p99_us: default_latency_threshold(),
            drawdown_warn_pct: default_drawdown_warn(),
            drawdown_critical_pct: default_drawdown_critical(),
            max_ws_reconnects: default_max_reconnects(),
            order_rejection_rate_pct: default_rejection_rate(),
        }
    }
}

fn default_latency_threshold() -> u64 {
    25_000 // 25ms
}

fn default_drawdown_warn() -> f64 {
    3.0
}

fn default_drawdown_critical() -> f64 {
    5.0
}

fn default_max_reconnects() -> u32 {
    5
}

fn default_rejection_rate() -> f64 {
    10.0
}

/// Alert configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertConfig {
    /// Enable alerting
    #[serde(default = "default_alerts_enabled")]
    pub enabled: bool,

    /// Webhook URL for alerts (Slack, Discord, etc.)
    pub webhook_url: Option<String>,

    /// Email for critical alerts
    pub email: Option<String>,

    /// Minimum interval between alerts of same type (seconds)
    #[serde(default = "default_alert_cooldown")]
    pub cooldown_secs: u64,
}

fn default_alerts_enabled() -> bool {
    true
}

fn default_alert_cooldown() -> u64 {
    300 // 5 minutes
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub level: String,

    /// Log file path
    pub file: Option<PathBuf>,

    /// Enable JSON logging
    #[serde(default)]
    pub json: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: None,
            json: false,
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

impl AgentConfig {
    /// Load configuration from file
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }

    /// Get API key from config or environment
    pub fn get_api_key(&self) -> Option<String> {
        self.anthropic
            .api_key
            .clone()
            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            anthropic: AnthropicConfig {
                api_key: None,
                model: default_model(),
                max_tokens: default_max_tokens(),
                temperature: default_temperature(),
                api_url: default_api_url(),
            },
            grpc: GrpcConfig {
                address: default_grpc_addr(),
                connect_timeout_secs: default_connect_timeout(),
                request_timeout_secs: default_request_timeout(),
            },
            agent: AgentBehaviorConfig {
                monitor_interval_secs: default_monitor_interval(),
                auto_handle_anomalies: default_auto_handle(),
                dry_run: false,
                max_consecutive_errors: default_max_errors(),
                thresholds: AnomalyThresholds::default(),
            },
            alerts: AlertConfig {
                enabled: default_alerts_enabled(),
                webhook_url: None,
                email: None,
                cooldown_secs: default_alert_cooldown(),
            },
            logging: LoggingConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AgentConfig::default();
        assert_eq!(config.anthropic.model, "claude-sonnet-4-20250514");
        assert_eq!(config.agent.monitor_interval_secs, 60);
        assert!(config.agent.auto_handle_anomalies);
    }

    #[test]
    fn test_config_serialization() {
        let config = AgentConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: AgentConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config.anthropic.model, parsed.anthropic.model);
    }
}
