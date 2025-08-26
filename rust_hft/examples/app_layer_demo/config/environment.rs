/*!
 * 環境變數解析器 (Environment Resolver)
 *
 * 負責從環境變數加載配置，支援前綴、類型轉換和配置覆蓋。
 */

use anyhow::Result;
use std::collections::HashMap;
use std::env;
use std::str::FromStr;
use tracing::{debug, info, warn};

use super::config_types::*;
use super::{AppConfig, ConfigError};
use crate::domains::trading::AccountId;

/// 環境變數解析器
pub struct EnvironmentResolver {
    /// 環境變數前綴
    prefix: String,

    /// 分隔符
    separator: String,

    /// 已解析的環境變數緩存
    env_cache: HashMap<String, String>,
}

impl EnvironmentResolver {
    /// 創建新的環境變數解析器
    pub fn new() -> Self {
        Self::with_prefix("RUST_HFT")
    }

    /// 使用指定前綴創建解析器
    pub fn with_prefix(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_string(),
            separator: "_".to_string(),
            env_cache: HashMap::new(),
        }
    }

    /// 設置分隔符
    pub fn with_separator(mut self, separator: &str) -> Self {
        self.separator = separator.to_string();
        self
    }

    /// 從環境變數解析完整配置
    pub fn resolve_config(&self, mut config: AppConfig) -> Result<AppConfig> {
        info!("正在從環境變數解析配置覆蓋，前綴: {}", self.prefix);

        // 應用基本信息配置
        self.apply_app_config(&mut config.app)?;

        // 應用交易所配置
        self.apply_exchanges_config(&mut config.exchanges)?;

        // 應用賬戶配置
        self.apply_accounts_config(&mut config.accounts)?;

        // 應用服務配置
        self.apply_order_service_config(&mut config.order_service)?;
        self.apply_risk_service_config(&mut config.risk_service)?;
        self.apply_execution_service_config(&mut config.execution_service)?;

        // 應用其他配置
        self.apply_event_bus_config(&mut config.event_bus)?;
        self.apply_orderbook_config(&mut config.orderbook)?;
        self.apply_performance_config(&mut config.performance)?;
        self.apply_monitoring_config(&mut config.monitoring)?;
        self.apply_security_config(&mut config.security)?;

        info!("環境變數配置解析完成");
        Ok(config)
    }

    /// 從環境變數加載基本配置
    pub fn load_from_environment(&self) -> Result<AppConfig> {
        info!("正在從環境變數加載完整配置");

        let base_config = AppConfig::default();
        self.resolve_config(base_config)
    }

    /// 獲取環境變數值
    pub fn get_env_var(&self, key: &str) -> Option<String> {
        let full_key = format!("{}{}{}", self.prefix, self.separator, key);
        env::var(&full_key).ok()
    }

    /// 獲取必需的環境變數值
    pub fn get_required_env_var(&self, key: &str) -> Result<String> {
        self.get_env_var(key).ok_or_else(|| {
            ConfigError::EnvironmentError {
                var: format!("{}{}{}", self.prefix, self.separator, key),
                reason: "必需的環境變數未設置".to_string(),
            }
            .into()
        })
    }

    /// 獲取環境變數並解析為指定類型
    pub fn get_env_var_as<T>(&self, key: &str) -> Option<T>
    where
        T: FromStr,
        T::Err: std::fmt::Display,
    {
        self.get_env_var(key).and_then(|value| {
            value
                .parse::<T>()
                .map_err(|e| {
                    warn!("環境變數 {} 解析失敗: {}", key, e);
                    e
                })
                .ok()
        })
    }

    /// 獲取必需的環境變數並解析為指定類型
    pub fn get_required_env_var_as<T>(&self, key: &str) -> Result<T>
    where
        T: FromStr,
        T::Err: std::fmt::Display,
    {
        let value = self.get_required_env_var(key)?;
        value.parse::<T>().map_err(|e| {
            ConfigError::EnvironmentError {
                var: format!("{}{}{}", self.prefix, self.separator, key),
                reason: format!("類型解析失敗: {}", e),
            }
            .into()
        })
    }

    /// 獲取帶默認值的環境變數
    pub fn get_env_var_or_default<T>(&self, key: &str, default: T) -> T
    where
        T: FromStr,
        T::Err: std::fmt::Display,
    {
        self.get_env_var_as(key).unwrap_or(default)
    }

    /// 檢查環境變數是否存在
    pub fn has_env_var(&self, key: &str) -> bool {
        self.get_env_var(key).is_some()
    }

    /// 列出所有相關的環境變數
    pub fn list_relevant_env_vars(&self) -> HashMap<String, String> {
        let prefix_with_sep = format!("{}{}", self.prefix, self.separator);

        env::vars()
            .filter(|(key, _)| key.starts_with(&prefix_with_sep))
            .collect()
    }

    // ====================== 私有配置應用方法 ======================

    /// 應用應用基本信息配置
    fn apply_app_config(&self, app: &mut AppInfo) -> Result<()> {
        if let Some(name) = self.get_env_var("APP_NAME") {
            app.name = name;
        }

        if let Some(version) = self.get_env_var("APP_VERSION") {
            app.version = version;
        }

        if let Some(environment) = self.get_env_var("APP_ENVIRONMENT") {
            app.environment = environment;
        }

        if let Some(log_level) = self.get_env_var("APP_LOG_LEVEL") {
            app.log_level = log_level;
        }

        if let Some(debug_mode) = self.get_env_var_as::<bool>("APP_DEBUG_MODE") {
            app.debug_mode = debug_mode;
        }

        debug!("應用基本信息環境變數已應用");
        Ok(())
    }

    /// 應用交易所配置
    fn apply_exchanges_config(
        &self,
        exchanges: &mut HashMap<String, ExchangeConfig>,
    ) -> Result<()> {
        // 支援多個交易所配置，格式: RUST_HFT_EXCHANGE_<NAME>_<FIELD>
        let relevant_vars = self.list_relevant_env_vars();
        let mut exchange_vars: HashMap<String, HashMap<String, String>> = HashMap::new();

        for (key, value) in relevant_vars {
            if let Some(exchange_part) = key.strip_prefix(&format!("{}_EXCHANGE_", self.prefix)) {
                let parts: Vec<&str> = exchange_part.split('_').collect();
                if parts.len() >= 2 {
                    let exchange_name = parts[0].to_lowercase();
                    let field_path = parts[1..].join("_").to_lowercase();

                    exchange_vars
                        .entry(exchange_name)
                        .or_insert_with(HashMap::new)
                        .insert(field_path, value);
                }
            }
        }

        // 為每個檢測到的交易所創建或更新配置
        for (exchange_name, fields) in exchange_vars {
            let mut exchange_config = exchanges
                .get(&exchange_name)
                .cloned()
                .unwrap_or_else(|| self.create_default_exchange_config(&exchange_name));

            self.apply_exchange_fields(&mut exchange_config, &fields)?;
            exchanges.insert(exchange_name, exchange_config);
        }

        debug!("交易所環境變數已應用，配置了 {} 個交易所", exchanges.len());
        Ok(())
    }

    /// 創建默認交易所配置
    fn create_default_exchange_config(&self, name: &str) -> ExchangeConfig {
        ExchangeConfig {
            name: name.to_string(),
            api: ApiConfig {
                base_url: format!("https://api.{}.com", name),
                api_key: None,
                api_secret: None,
                passphrase: None,
                sandbox: true,
                timeout_ms: 5000,
                rate_limit: 100,
                max_retries: 3,
            },
            websocket: WebSocketConfig {
                url: format!("wss://ws.{}.com", name),
                heartbeat_interval_ms: 30000,
                connect_timeout_ms: 10000,
                subscribe_timeout_ms: 5000,
                max_subscriptions: 100,
                buffer_size: 8192,
            },
            symbols: vec!["BTCUSDT".to_string()],
            trading_rules: TradingRulesConfig {
                min_notional: 10.0,
                min_quantity: 0.001,
                max_quantity: 1000.0,
                quantity_precision: 8,
                price_precision: 2,
            },
            connection_pool: ConnectionPoolConfig {
                max_connections: 10,
                min_idle: 2,
                idle_timeout_sec: 300,
                max_lifetime_sec: 3600,
            },
            reconnect: ReconnectConfig {
                enabled: true,
                initial_delay_ms: 1000,
                max_delay_ms: 30000,
                backoff_multiplier: 2.0,
                max_attempts: 10,
            },
            enabled: true,
        }
    }

    /// 應用交易所字段配置
    fn apply_exchange_fields(
        &self,
        config: &mut ExchangeConfig,
        fields: &HashMap<String, String>,
    ) -> Result<()> {
        for (field_path, value) in fields {
            match field_path.as_str() {
                "api_base_url" => config.api.base_url = value.clone(),
                "api_key" => config.api.api_key = Some(value.clone()),
                "api_secret" => config.api.api_secret = Some(value.clone()),
                "passphrase" => config.api.passphrase = Some(value.clone()),
                "sandbox" => config.api.sandbox = value.parse().unwrap_or(true),
                "api_timeout_ms" => config.api.timeout_ms = value.parse().unwrap_or(5000),
                "websocket_url" => config.websocket.url = value.clone(),
                "enabled" => config.enabled = value.parse().unwrap_or(true),
                _ => {
                    debug!("跳過未知的交易所配置字段: {}", field_path);
                }
            }
        }
        Ok(())
    }

    /// 應用賬戶配置
    fn apply_accounts_config(&self, accounts: &mut HashMap<String, AccountConfig>) -> Result<()> {
        // 支援多個賬戶配置，格式: RUST_HFT_ACCOUNT_<ID>_<FIELD>
        let relevant_vars = self.list_relevant_env_vars();
        let mut account_vars: HashMap<String, HashMap<String, String>> = HashMap::new();

        for (key, value) in relevant_vars {
            if let Some(account_part) = key.strip_prefix(&format!("{}_ACCOUNT_", self.prefix)) {
                let parts: Vec<&str> = account_part.split('_').collect();
                if parts.len() >= 2 {
                    let account_id = parts[0].to_lowercase();
                    let field_path = parts[1..].join("_").to_lowercase();

                    account_vars
                        .entry(account_id)
                        .or_insert_with(HashMap::new)
                        .insert(field_path, value);
                }
            }
        }

        // 為每個檢測到的賬戶創建配置
        for (account_id_str, fields) in account_vars {
            if let Some(exchange) = fields.get("exchange") {
                let account_id = AccountId::new(exchange, &account_id_str);
                let mut account_config = AccountConfig {
                    account_id: account_id.clone(),
                    exchange: exchange.clone(),
                    initial_balance: fields
                        .get("initial_balance")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(10000.0),
                    trading_enabled: fields
                        .get("trading_enabled")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(true),
                    account_type: fields
                        .get("account_type")
                        .cloned()
                        .unwrap_or_else(|| "spot".to_string()),
                    metadata: HashMap::new(),
                };

                // 添加其他元數據
                for (key, value) in fields {
                    if ![
                        "exchange",
                        "initial_balance",
                        "trading_enabled",
                        "account_type",
                    ]
                    .contains(&key.as_str())
                    {
                        account_config.metadata.insert(key.clone(), value.clone());
                    }
                }

                accounts.insert(account_id.as_str(), account_config);
            }
        }

        debug!("賬戶環境變數已應用，配置了 {} 個賬戶", accounts.len());
        Ok(())
    }

    /// 應用訂單服務配置
    fn apply_order_service_config(&self, config: &mut OrderServiceConfig) -> Result<()> {
        if let Some(max_concurrent) =
            self.get_env_var_as::<usize>("ORDER_SERVICE_MAX_CONCURRENT_ORDERS")
        {
            config.max_concurrent_orders = max_concurrent;
        }

        if let Some(timeout) = self.get_env_var_as::<u64>("ORDER_SERVICE_TIMEOUT_MS") {
            config.order_timeout_ms = timeout;
        }

        // 重試配置
        if let Some(max_attempts) = self.get_env_var_as::<u32>("ORDER_SERVICE_RETRY_MAX_ATTEMPTS") {
            config.retry.max_attempts = max_attempts;
        }

        if let Some(initial_delay) =
            self.get_env_var_as::<u64>("ORDER_SERVICE_RETRY_INITIAL_DELAY_MS")
        {
            config.retry.initial_delay_ms = initial_delay;
        }

        debug!("訂單服務環境變數已應用");
        Ok(())
    }

    /// 應用風險服務配置
    fn apply_risk_service_config(&self, config: &mut RiskServiceConfig) -> Result<()> {
        if let Some(max_loss) = self.get_env_var_as::<f64>("RISK_SERVICE_MAX_DAILY_LOSS") {
            config.max_daily_loss = max_loss;
        }

        if let Some(max_order_value) = self.get_env_var_as::<f64>("RISK_SERVICE_MAX_ORDER_VALUE") {
            config.max_order_value = max_order_value;
        }

        if let Some(max_position_value) =
            self.get_env_var_as::<f64>("RISK_SERVICE_MAX_POSITION_VALUE")
        {
            config.max_position_value = max_position_value;
        }

        if let Some(max_leverage) = self.get_env_var_as::<f64>("RISK_SERVICE_MAX_LEVERAGE") {
            config.max_leverage = max_leverage;
        }

        if let Some(max_order_rate) = self.get_env_var_as::<u32>("RISK_SERVICE_MAX_ORDER_RATE") {
            config.max_order_rate = max_order_rate;
        }

        // 熔斷器配置
        if let Some(enabled) = self.get_env_var_as::<bool>("RISK_SERVICE_CIRCUIT_BREAKER_ENABLED") {
            config.circuit_breaker.enabled = enabled;
        }

        if let Some(threshold) =
            self.get_env_var_as::<u32>("RISK_SERVICE_CIRCUIT_BREAKER_FAILURE_THRESHOLD")
        {
            config.circuit_breaker.failure_threshold = threshold;
        }

        debug!("風險服務環境變數已應用");
        Ok(())
    }

    /// 應用執行服務配置
    fn apply_execution_service_config(&self, config: &mut ExecutionServiceConfig) -> Result<()> {
        if let Some(mode) = self.get_env_var("EXECUTION_SERVICE_MODE") {
            config.execution_mode = mode;
        }

        if let Some(latency) = self.get_env_var_as::<u64>("EXECUTION_SERVICE_TARGET_LATENCY_US") {
            config.target_latency_us = latency;
        }

        if let Some(threshold) = self.get_env_var_as::<f64>("EXECUTION_SERVICE_QUALITY_THRESHOLD") {
            config.quality_threshold = threshold;
        }

        debug!("執行服務環境變數已應用");
        Ok(())
    }

    /// 應用事件總線配置
    fn apply_event_bus_config(&self, config: &mut EventBusConfig) -> Result<()> {
        if let Some(buffer_size) = self.get_env_var_as::<usize>("EVENT_BUS_BUFFER_SIZE") {
            config.buffer_size = buffer_size;
        }

        if let Some(broadcast_size) =
            self.get_env_var_as::<usize>("EVENT_BUS_BROADCAST_BUFFER_SIZE")
        {
            config.broadcast_buffer_size = broadcast_size;
        }

        if let Some(max_subscribers) = self.get_env_var_as::<usize>("EVENT_BUS_MAX_SUBSCRIBERS") {
            config.max_subscribers = max_subscribers;
        }

        if let Some(enable_stats) = self.get_env_var_as::<bool>("EVENT_BUS_ENABLE_STATS") {
            config.enable_stats = enable_stats;
        }

        debug!("事件總線環境變數已應用");
        Ok(())
    }

    /// 應用 OrderBook 配置
    fn apply_orderbook_config(&self, config: &mut OrderBookConfig) -> Result<()> {
        if let Some(max_levels) = self.get_env_var_as::<usize>("ORDERBOOK_MAX_LEVELS") {
            config.max_levels = max_levels;
        }

        if let Some(threshold) = self.get_env_var_as::<f64>("ORDERBOOK_QUALITY_THRESHOLD") {
            config.quality_threshold = threshold;
        }

        if let Some(cleanup_threshold) = self.get_env_var_as::<usize>("ORDERBOOK_CLEANUP_THRESHOLD")
        {
            config.cleanup_threshold = cleanup_threshold;
        }

        debug!("OrderBook 環境變數已應用");
        Ok(())
    }

    /// 應用性能配置
    fn apply_performance_config(&self, config: &mut PerformanceConfig) -> Result<()> {
        if let Some(thread_pool_size) = self.get_env_var_as::<usize>("PERFORMANCE_THREAD_POOL_SIZE")
        {
            config.thread_pool_size = Some(thread_pool_size);
        }

        if let Some(tcp_nodelay) = self.get_env_var_as::<bool>("PERFORMANCE_TCP_NODELAY") {
            config.network.tcp_nodelay = tcp_nodelay;
        }

        if let Some(tcp_keepalive) = self.get_env_var_as::<bool>("PERFORMANCE_TCP_KEEPALIVE") {
            config.network.tcp_keepalive = tcp_keepalive;
        }

        debug!("性能環境變數已應用");
        Ok(())
    }

    /// 應用監控配置
    fn apply_monitoring_config(&self, config: &mut MonitoringConfig) -> Result<()> {
        if let Some(enabled) = self.get_env_var_as::<bool>("MONITORING_PROMETHEUS_ENABLED") {
            config.prometheus.enabled = enabled;
        }

        if let Some(address) = self.get_env_var("MONITORING_PROMETHEUS_LISTEN_ADDRESS") {
            config.prometheus.listen_address = address;
        }

        if let Some(path) = self.get_env_var("MONITORING_PROMETHEUS_METRICS_PATH") {
            config.prometheus.metrics_path = path;
        }

        if let Some(interval) =
            self.get_env_var_as::<u64>("MONITORING_PROMETHEUS_SCRAPE_INTERVAL_SEC")
        {
            config.prometheus.scrape_interval_sec = interval;
        }

        debug!("監控環境變數已應用");
        Ok(())
    }

    /// 應用安全配置
    fn apply_security_config(&self, config: &mut SecurityConfig) -> Result<()> {
        if let Some(tls_enabled) = self.get_env_var_as::<bool>("SECURITY_TLS_ENABLED") {
            config.tls.enabled = tls_enabled;
        }

        if let Some(cert_file) = self.get_env_var("SECURITY_TLS_CERT_FILE") {
            config.tls.cert_file = Some(cert_file);
        }

        if let Some(key_file) = self.get_env_var("SECURITY_TLS_KEY_FILE") {
            config.tls.key_file = Some(key_file);
        }

        if let Some(encryption_enabled) = self.get_env_var_as::<bool>("SECURITY_ENCRYPTION_ENABLED")
        {
            config.encryption.enabled = encryption_enabled;
        }

        debug!("安全環境變數已應用");
        Ok(())
    }
}

impl Default for EnvironmentResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_environment_resolver_creation() {
        let resolver = EnvironmentResolver::new();
        assert_eq!(resolver.prefix, "RUST_HFT");
        assert_eq!(resolver.separator, "_");
    }

    #[test]
    fn test_custom_prefix_and_separator() {
        let resolver = EnvironmentResolver::with_prefix("TEST_APP").with_separator("__");
        assert_eq!(resolver.prefix, "TEST_APP");
        assert_eq!(resolver.separator, "__");
    }

    #[test]
    fn test_env_var_operations() {
        let resolver = EnvironmentResolver::with_prefix("TEST");

        // 設置測試環境變數
        env::set_var("TEST_STRING_VAR", "test_value");
        env::set_var("TEST_INT_VAR", "42");
        env::set_var("TEST_BOOL_VAR", "true");
        env::set_var("TEST_FLOAT_VAR", "3.14");

        // 測試基本讀取
        assert_eq!(
            resolver.get_env_var("STRING_VAR"),
            Some("test_value".to_string())
        );
        assert_eq!(resolver.get_env_var("NONEXISTENT"), None);

        // 測試類型轉換
        assert_eq!(resolver.get_env_var_as::<i32>("INT_VAR"), Some(42));
        assert_eq!(resolver.get_env_var_as::<bool>("BOOL_VAR"), Some(true));
        assert_eq!(resolver.get_env_var_as::<f64>("FLOAT_VAR"), Some(3.14));

        // 測試帶默認值
        assert_eq!(resolver.get_env_var_or_default("NONEXISTENT", 100), 100);
        assert_eq!(resolver.get_env_var_or_default("INT_VAR", 100), 42);

        // 測試存在性檢查
        assert!(resolver.has_env_var("STRING_VAR"));
        assert!(!resolver.has_env_var("NONEXISTENT"));

        // 清理
        env::remove_var("TEST_STRING_VAR");
        env::remove_var("TEST_INT_VAR");
        env::remove_var("TEST_BOOL_VAR");
        env::remove_var("TEST_FLOAT_VAR");
    }

    #[test]
    fn test_required_env_var() {
        let resolver = EnvironmentResolver::with_prefix("TEST_REQ");

        // 測試不存在的必需變數
        let result = resolver.get_required_env_var("MISSING");
        assert!(result.is_err());

        // 設置並測試存在的必需變數
        env::set_var("TEST_REQ_EXISTING", "value");
        let result = resolver.get_required_env_var("EXISTING");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "value");

        // 清理
        env::remove_var("TEST_REQ_EXISTING");
    }

    #[test]
    fn test_list_relevant_env_vars() {
        let resolver = EnvironmentResolver::with_prefix("TEST_LIST");

        // 設置一些測試變數
        env::set_var("TEST_LIST_VAR1", "value1");
        env::set_var("TEST_LIST_VAR2", "value2");
        env::set_var("OTHER_VAR", "other");

        let relevant_vars = resolver.list_relevant_env_vars();

        assert!(relevant_vars.contains_key("TEST_LIST_VAR1"));
        assert!(relevant_vars.contains_key("TEST_LIST_VAR2"));
        assert!(!relevant_vars.contains_key("OTHER_VAR"));

        // 清理
        env::remove_var("TEST_LIST_VAR1");
        env::remove_var("TEST_LIST_VAR2");
        env::remove_var("OTHER_VAR");
    }
}
