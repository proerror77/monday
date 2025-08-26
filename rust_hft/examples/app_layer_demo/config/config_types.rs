/*!
 * 配置類型定義 (Configuration Types)
 *
 * 定義系統所有配置結構體，支援序列化/反序列化、驗證和默認值。
 */

use crate::app::events::EventPriority;
use crate::domains::marketdata::OrderBookType;
use crate::domains::trading::AccountId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 應用程序根配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// 應用基本信息
    pub app: AppInfo,

    /// 交易所配置
    pub exchanges: HashMap<String, ExchangeConfig>,

    /// 賬戶配置
    pub accounts: HashMap<String, AccountConfig>,

    /// 訂單服務配置
    pub order_service: OrderServiceConfig,

    /// 風險服務配置
    pub risk_service: RiskServiceConfig,

    /// 執行服務配置
    pub execution_service: ExecutionServiceConfig,

    /// 事件總線配置
    pub event_bus: EventBusConfig,

    /// OrderBook 配置
    pub orderbook: OrderBookConfig,

    /// 性能配置
    pub performance: PerformanceConfig,

    /// 監控配置
    pub monitoring: MonitoringConfig,

    /// 安全配置
    pub security: SecurityConfig,
}

/// 應用基本信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    /// 應用名稱
    pub name: String,

    /// 版本號
    pub version: String,

    /// 環境 (dev, test, prod)
    pub environment: String,

    /// 日誌級別
    pub log_level: String,

    /// 是否啟用調試模式
    pub debug_mode: bool,
}

/// 交易所配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeConfig {
    /// 交易所名稱
    pub name: String,

    /// API 基礎配置
    pub api: ApiConfig,

    /// WebSocket 配置
    pub websocket: WebSocketConfig,

    /// 支援的交易對
    pub symbols: Vec<String>,

    /// 交易規則
    pub trading_rules: TradingRulesConfig,

    /// 連接池配置
    pub connection_pool: ConnectionPoolConfig,

    /// 重連配置
    pub reconnect: ReconnectConfig,

    /// 是否啟用
    pub enabled: bool,
}

/// API 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// REST API 基礎 URL
    pub base_url: String,

    /// API 密鑰
    pub api_key: Option<String>,

    /// API 密鑰
    pub api_secret: Option<String>,

    /// API 通行短語 (部分交易所需要)
    pub passphrase: Option<String>,

    /// 是否沙盒模式
    pub sandbox: bool,

    /// 請求超時 (毫秒)
    pub timeout_ms: u64,

    /// 速率限制 (每秒請求數)
    pub rate_limit: u32,

    /// 重試次數
    pub max_retries: u32,
}

/// WebSocket 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketConfig {
    /// WebSocket URL
    pub url: String,

    /// 心跳間隔 (毫秒)
    pub heartbeat_interval_ms: u64,

    /// 連接超時 (毫秒)
    pub connect_timeout_ms: u64,

    /// 訂閱超時 (毫秒)
    pub subscribe_timeout_ms: u64,

    /// 最大訂閱數
    pub max_subscriptions: usize,

    /// 緩衝區大小
    pub buffer_size: usize,
}

/// 交易規則配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingRulesConfig {
    /// 最小訂單金額
    pub min_notional: f64,

    /// 最小訂單數量
    pub min_quantity: f64,

    /// 最大訂單數量
    pub max_quantity: f64,

    /// 數量精度
    pub quantity_precision: u32,

    /// 價格精度
    pub price_precision: u32,
}

/// 連接池配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionPoolConfig {
    /// 最大連接數
    pub max_connections: usize,

    /// 最小空閒連接數
    pub min_idle: usize,

    /// 連接空閒超時 (秒)
    pub idle_timeout_sec: u64,

    /// 連接最大生存時間 (秒)
    pub max_lifetime_sec: u64,
}

/// 重連配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectConfig {
    /// 是否啟用自動重連
    pub enabled: bool,

    /// 初始重連延遲 (毫秒)
    pub initial_delay_ms: u64,

    /// 最大重連延遲 (毫秒)
    pub max_delay_ms: u64,

    /// 重連嘗試指數退避倍數
    pub backoff_multiplier: f64,

    /// 最大重連次數
    pub max_attempts: u32,
}

/// 賬戶配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    /// 賬戶ID
    pub account_id: AccountId,

    /// 所屬交易所
    pub exchange: String,

    /// 初始資金
    pub initial_balance: f64,

    /// 是否啟用交易
    pub trading_enabled: bool,

    /// 賬戶類型 (spot, margin, futures)
    pub account_type: String,

    /// 特定配置
    pub metadata: HashMap<String, String>,
}

/// 訂單服務配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderServiceConfig {
    /// 最大並發訂單數
    pub max_concurrent_orders: usize,

    /// 訂單超時 (毫秒)
    pub order_timeout_ms: u64,

    /// 訂單重試配置
    pub retry: RetryConfig,

    /// 訂單驗證配置
    pub validation: OrderValidationConfig,

    /// 統計配置
    pub stats: StatsConfig,
}

/// 風險服務配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskServiceConfig {
    /// 每日最大虧損
    pub max_daily_loss: f64,

    /// 單筆訂單最大金額
    pub max_order_value: f64,

    /// 最大持倉價值
    pub max_position_value: f64,

    /// 最大槓桿倍數
    pub max_leverage: f64,

    /// 交易頻率限制 (每秒)
    pub max_order_rate: u32,

    /// 風險檢查間隔 (毫秒)
    pub check_interval_ms: u64,

    /// 熔斷器配置
    pub circuit_breaker: CircuitBreakerConfig,

    /// 風險模型配置
    pub risk_models: HashMap<String, RiskModelConfig>,
}

/// 執行服務配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionServiceConfig {
    /// 執行模式 (dry_run, live)
    pub execution_mode: String,

    /// 延遲目標 (微秒)
    pub target_latency_us: u64,

    /// 執行質量閾值
    pub quality_threshold: f64,

    /// 執行監控配置
    pub monitoring: ExecutionMonitoringConfig,

    /// 重試配置
    pub retry: RetryConfig,
}

/// 事件總線配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBusConfig {
    /// 緩衝區大小
    pub buffer_size: usize,

    /// 廣播通道大小
    pub broadcast_buffer_size: usize,

    /// 最大訂閱者數
    pub max_subscribers: usize,

    /// 事件持久化
    pub persistence: EventPersistenceConfig,

    /// 優先級配置
    pub priority_config: PriorityConfig,

    /// 是否啟用統計
    pub enable_stats: bool,
}

/// OrderBook 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookConfig {
    /// 預設實現類型
    pub default_implementation: OrderBookType,

    /// 最大價格層數
    pub max_levels: usize,

    /// 數據質量閾值
    pub quality_threshold: f64,

    /// 驗證間隔 (微秒)
    pub validation_interval_us: u64,

    /// 清理閾值
    pub cleanup_threshold: usize,

    /// 統計配置
    pub stats: StatsConfig,
}

impl Default for OrderBookConfig {
    fn default() -> Self {
        use crate::domains::marketdata::OrderBookType;
        Self {
            default_implementation: OrderBookType::Standard,
            max_levels: 100,
            quality_threshold: 0.95,
            validation_interval_us: 1000,
            cleanup_threshold: 1000,
            stats: StatsConfig::default(),
        }
    }
}

/// 性能配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    /// CPU 親和性設定
    pub cpu_affinity: Option<Vec<usize>>,

    /// 執行緒池大小
    pub thread_pool_size: Option<usize>,

    /// 記憶體配置
    pub memory: MemoryConfig,

    /// 網路配置
    pub network: NetworkConfig,

    /// JIT 編譯配置
    pub jit: JitConfig,
}

/// 監控配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringConfig {
    /// Prometheus 配置
    pub prometheus: PrometheusConfig,

    /// 指標配置
    pub metrics: MetricsConfig,

    /// 告警配置
    pub alerting: AlertingConfig,

    /// 追蹤配置
    pub tracing: TracingConfig,
}

/// 安全配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// TLS 配置
    pub tls: TlsConfig,

    /// 加密配置
    pub encryption: EncryptionConfig,

    /// 訪問控制
    pub access_control: AccessControlConfig,

    /// 審計配置
    pub audit: AuditConfig,
}

// ====================== 輔助配置結構體 ======================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_multiplier: f64,
    pub jitter: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderValidationConfig {
    pub enable_pre_trade_checks: bool,
    pub enable_post_trade_checks: bool,
    pub max_order_size_validation: bool,
    pub symbol_validation: bool,
    pub balance_validation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsConfig {
    pub enabled: bool,
    pub collection_interval_ms: u64,
    pub retention_period_sec: u64,
    pub enable_histograms: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    pub enabled: bool,
    pub failure_threshold: u32,
    pub timeout_ms: u64,
    pub half_open_max_calls: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskModelConfig {
    pub model_type: String,
    pub parameters: HashMap<String, f64>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionMonitoringConfig {
    pub enable_latency_tracking: bool,
    pub enable_quality_scoring: bool,
    pub sample_rate: f64,
    pub alert_threshold_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventPersistenceConfig {
    pub enabled: bool,
    pub storage_type: String, // "memory", "redis", "postgres"
    pub retention_hours: u64,
    pub batch_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorityConfig {
    pub default_priority: EventPriority,
    pub priority_queue_sizes: HashMap<String, usize>,
    pub priority_weights: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub initial_heap_size_mb: Option<usize>,
    pub max_heap_size_mb: Option<usize>,
    pub gc_strategy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub tcp_nodelay: bool,
    pub tcp_keepalive: bool,
    pub buffer_sizes: BufferSizeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferSizeConfig {
    pub read_buffer_size: usize,
    pub write_buffer_size: usize,
    pub receive_buffer_size: usize,
    pub send_buffer_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JitConfig {
    pub enabled: bool,
    pub optimization_level: u32,
    pub inline_threshold: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrometheusConfig {
    pub enabled: bool,
    pub listen_address: String,
    pub metrics_path: String,
    pub scrape_interval_sec: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub buffer_size: usize,
    pub flush_interval_ms: u64,
    pub tags: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertingConfig {
    pub enabled: bool,
    pub channels: Vec<AlertChannelConfig>,
    pub rules: Vec<AlertRuleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertChannelConfig {
    pub name: String,
    pub channel_type: String, // "slack", "email", "webhook"
    pub endpoint: String,
    pub severity_filter: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRuleConfig {
    pub name: String,
    pub metric: String,
    pub condition: String,
    pub threshold: f64,
    pub duration_sec: u64,
    pub severity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingConfig {
    pub enabled: bool,
    pub sampler_type: String,
    pub sampler_param: f64,
    pub jaeger_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsConfig {
    pub enabled: bool,
    pub cert_file: Option<String>,
    pub key_file: Option<String>,
    pub ca_file: Option<String>,
    pub verify_peer: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionConfig {
    pub enabled: bool,
    pub algorithm: String,
    pub key_rotation_interval_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessControlConfig {
    pub enabled: bool,
    pub default_policy: String, // "allow", "deny"
    pub rules: Vec<AccessRuleConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessRuleConfig {
    pub resource: String,
    pub action: String,
    pub principal: String,
    pub effect: String, // "allow", "deny"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    pub enabled: bool,
    pub log_level: String,
    pub events: Vec<String>,
    pub retention_days: u64,
}

// ====================== Default 實現 ======================

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            app: AppInfo::default(),
            exchanges: HashMap::new(),
            accounts: HashMap::new(),
            order_service: OrderServiceConfig::default(),
            risk_service: RiskServiceConfig::default(),
            execution_service: ExecutionServiceConfig::default(),
            event_bus: EventBusConfig::default(),
            orderbook: OrderBookConfig::default(),
            performance: PerformanceConfig::default(),
            monitoring: MonitoringConfig::default(),
            security: SecurityConfig::default(),
        }
    }
}

impl Default for AppInfo {
    fn default() -> Self {
        Self {
            name: "rust_hft".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            environment: "development".to_string(),
            log_level: "info".to_string(),
            debug_mode: true,
        }
    }
}

impl Default for OrderServiceConfig {
    fn default() -> Self {
        Self {
            max_concurrent_orders: 100,
            order_timeout_ms: 5000,
            retry: RetryConfig::default(),
            validation: OrderValidationConfig::default(),
            stats: StatsConfig::default(),
        }
    }
}

impl Default for RiskServiceConfig {
    fn default() -> Self {
        Self {
            max_daily_loss: 1000.0,
            max_order_value: 10000.0,
            max_position_value: 50000.0,
            max_leverage: 3.0,
            max_order_rate: 10,
            check_interval_ms: 1000,
            circuit_breaker: CircuitBreakerConfig::default(),
            risk_models: HashMap::new(),
        }
    }
}

impl Default for ExecutionServiceConfig {
    fn default() -> Self {
        Self {
            execution_mode: "dry_run".to_string(),
            target_latency_us: 100,
            quality_threshold: 0.95,
            monitoring: ExecutionMonitoringConfig::default(),
            retry: RetryConfig::default(),
        }
    }
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self {
            buffer_size: 10000,
            broadcast_buffer_size: 1000,
            max_subscribers: 100,
            persistence: EventPersistenceConfig::default(),
            priority_config: PriorityConfig::default(),
            enable_stats: true,
        }
    }
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            cpu_affinity: None,
            thread_pool_size: None,
            memory: MemoryConfig::default(),
            network: NetworkConfig::default(),
            jit: JitConfig::default(),
        }
    }
}

impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            prometheus: PrometheusConfig::default(),
            metrics: MetricsConfig::default(),
            alerting: AlertingConfig::default(),
            tracing: TracingConfig::default(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            tls: TlsConfig::default(),
            encryption: EncryptionConfig::default(),
            access_control: AccessControlConfig::default(),
            audit: AuditConfig::default(),
        }
    }
}

// 為所有輔助結構體提供合理的默認值
impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay_ms: 100,
            max_delay_ms: 5000,
            backoff_multiplier: 2.0,
            jitter: true,
        }
    }
}

impl Default for OrderValidationConfig {
    fn default() -> Self {
        Self {
            enable_pre_trade_checks: true,
            enable_post_trade_checks: true,
            max_order_size_validation: true,
            symbol_validation: true,
            balance_validation: true,
        }
    }
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            collection_interval_ms: 1000,
            retention_period_sec: 3600,
            enable_histograms: true,
        }
    }
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            failure_threshold: 5,
            timeout_ms: 60000,
            half_open_max_calls: 3,
        }
    }
}

impl Default for ExecutionMonitoringConfig {
    fn default() -> Self {
        Self {
            enable_latency_tracking: true,
            enable_quality_scoring: true,
            sample_rate: 1.0,
            alert_threshold_us: 1000,
        }
    }
}

impl Default for EventPersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            storage_type: "memory".to_string(),
            retention_hours: 24,
            batch_size: 100,
        }
    }
}

impl Default for PriorityConfig {
    fn default() -> Self {
        Self {
            default_priority: EventPriority::Normal,
            priority_queue_sizes: HashMap::new(),
            priority_weights: HashMap::new(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            initial_heap_size_mb: None,
            max_heap_size_mb: None,
            gc_strategy: "default".to_string(),
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            tcp_nodelay: true,
            tcp_keepalive: true,
            buffer_sizes: BufferSizeConfig::default(),
        }
    }
}

impl Default for BufferSizeConfig {
    fn default() -> Self {
        Self {
            read_buffer_size: 8192,
            write_buffer_size: 8192,
            receive_buffer_size: 65536,
            send_buffer_size: 65536,
        }
    }
}

impl Default for JitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            optimization_level: 2,
            inline_threshold: 100,
        }
    }
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen_address: "0.0.0.0:9090".to_string(),
            metrics_path: "/metrics".to_string(),
            scrape_interval_sec: 15,
        }
    }
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            buffer_size: 1000,
            flush_interval_ms: 1000,
            tags: HashMap::new(),
        }
    }
}

impl Default for AlertingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channels: Vec::new(),
            rules: Vec::new(),
        }
    }
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sampler_type: "const".to_string(),
            sampler_param: 1.0,
            jaeger_endpoint: None,
        }
    }
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cert_file: None,
            key_file: None,
            ca_file: None,
            verify_peer: true,
        }
    }
}

impl Default for EncryptionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            algorithm: "AES256-GCM".to_string(),
            key_rotation_interval_hours: 24,
        }
    }
}

impl Default for AccessControlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_policy: "deny".to_string(),
            rules: Vec::new(),
        }
    }
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            log_level: "info".to_string(),
            events: Vec::new(),
            retention_days: 30,
        }
    }
}
