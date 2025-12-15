//! 執行層韌性工具
//!
//! 提供統一的錯誤處理機制：
//! - 指數退避重試 (Exponential Backoff Retry)
//! - 熔斷器模式 (Circuit Breaker)
//! - 告警通知 (Alert Notification)

use hft_core::{HftError, HftResult};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

// ============================================================================
// 重試配置
// ============================================================================

/// 重試策略配置
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// 最大重試次數
    pub max_retries: u32,
    /// 初始延遲 (毫秒)
    pub initial_delay_ms: u64,
    /// 最大延遲 (毫秒)
    pub max_delay_ms: u64,
    /// 退避乘數
    pub backoff_multiplier: f64,
    /// 是否對初始化錯誤重試
    pub retry_on_init_error: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 100,
            max_delay_ms: 5000,
            backoff_multiplier: 2.0,
            retry_on_init_error: true,
        }
    }
}

/// 判斷錯誤是否可重試
pub fn is_retryable(error: &HftError, config: &RetryConfig) -> bool {
    match error {
        // 網路錯誤 - 可重試
        HftError::Network(_) => true,
        // 速率限制 - 可重試（需要等待）
        HftError::RateLimit(_) => true,
        // 執行錯誤（如初始化失敗）- 根據配置
        HftError::Execution(msg) => {
            if config.retry_on_init_error {
                // 初始化相關錯誤可重試
                msg.contains("not initialized") || msg.contains("initialization")
            } else {
                false
            }
        }
        // 超時 - 可重試
        HftError::Timeout(_) => true,
        // 其他錯誤 - 不可重試
        _ => false,
    }
}

/// 執行帶重試的操作
pub async fn retry_with_backoff<T, F, Fut>(
    operation_name: &str,
    config: &RetryConfig,
    mut operation: F,
) -> HftResult<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = HftResult<T>>,
{
    let mut attempts = 0u32;
    let mut delay_ms = config.initial_delay_ms;

    loop {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                attempts += 1;

                if attempts >= config.max_retries {
                    error!(
                        "[{}] 操作失敗，已達最大重試次數 {}: {}",
                        operation_name, config.max_retries, e
                    );
                    return Err(e);
                }

                if !is_retryable(&e, config) {
                    error!("[{}] 不可重試的錯誤: {}", operation_name, e);
                    return Err(e);
                }

                warn!(
                    "[{}] 操作失敗，{}ms 後重試 ({}/{}): {}",
                    operation_name, delay_ms, attempts, config.max_retries, e
                );

                tokio::time::sleep(Duration::from_millis(delay_ms)).await;

                // 指數退避
                delay_ms = ((delay_ms as f64 * config.backoff_multiplier) as u64)
                    .min(config.max_delay_ms);
            }
        }
    }
}

// ============================================================================
// 熔斷器 (Circuit Breaker)
// ============================================================================

/// 熔斷器狀態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// 正常運行
    Closed,
    /// 熔斷開啟（拒絕請求）
    Open,
    /// 半開（試探性允許部分請求）
    HalfOpen,
}

/// 熔斷器配置
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// 觸發熔斷的連續失敗次數
    pub failure_threshold: u32,
    /// 熔斷持續時間（秒）
    pub open_duration_secs: u64,
    /// 半開狀態允許的試探請求數
    pub half_open_max_requests: u32,
    /// 半開狀態成功閾值（達到後關閉熔斷）
    pub half_open_success_threshold: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            open_duration_secs: 30,
            half_open_max_requests: 3,
            half_open_success_threshold: 2,
        }
    }
}

/// 熔斷器
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    state: RwLock<CircuitState>,
    failure_count: AtomicU32,
    success_count: AtomicU32,
    last_failure_time: RwLock<Option<Instant>>,
    half_open_requests: AtomicU32,
    /// 告警回調
    alert_callback: Option<Arc<dyn Fn(CircuitBreakerAlert) + Send + Sync>>,
}

/// 熔斷器告警
#[derive(Debug, Clone)]
pub struct CircuitBreakerAlert {
    pub name: String,
    pub state: CircuitState,
    pub failure_count: u32,
    pub message: String,
    pub timestamp: u64,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            state: RwLock::new(CircuitState::Closed),
            failure_count: AtomicU32::new(0),
            success_count: AtomicU32::new(0),
            last_failure_time: RwLock::new(None),
            half_open_requests: AtomicU32::new(0),
            alert_callback: None,
        }
    }

    /// 設置告警回調
    pub fn with_alert_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(CircuitBreakerAlert) + Send + Sync + 'static,
    {
        self.alert_callback = Some(Arc::new(callback));
        self
    }

    /// 檢查是否允許請求
    pub async fn allow_request(&self) -> bool {
        let state = *self.state.read().await;

        match state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // 檢查是否應該轉為半開
                if self.should_try_half_open().await {
                    self.transition_to_half_open().await;
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                // 半開狀態限制請求數
                let current = self.half_open_requests.fetch_add(1, Ordering::SeqCst);
                current < self.config.half_open_max_requests
            }
        }
    }

    /// 記錄成功
    pub async fn record_success(&self, name: &str) {
        let state = *self.state.read().await;

        match state {
            CircuitState::Closed => {
                self.failure_count.store(0, Ordering::SeqCst);
            }
            CircuitState::HalfOpen => {
                let successes = self.success_count.fetch_add(1, Ordering::SeqCst) + 1;
                if successes >= self.config.half_open_success_threshold {
                    self.transition_to_closed(name).await;
                }
            }
            CircuitState::Open => {}
        }
    }

    /// 記錄失敗
    pub async fn record_failure(&self, name: &str, error: &HftError) {
        let failures = self.failure_count.fetch_add(1, Ordering::SeqCst) + 1;
        *self.last_failure_time.write().await = Some(Instant::now());

        let state = *self.state.read().await;

        match state {
            CircuitState::Closed => {
                if failures >= self.config.failure_threshold {
                    self.transition_to_open(name, error).await;
                }
            }
            CircuitState::HalfOpen => {
                // 半開狀態任何失敗都重新開啟熔斷
                self.transition_to_open(name, error).await;
            }
            CircuitState::Open => {}
        }
    }

    /// 獲取當前狀態
    pub async fn state(&self) -> CircuitState {
        *self.state.read().await
    }

    /// 獲取失敗計數
    pub fn failure_count(&self) -> u32 {
        self.failure_count.load(Ordering::SeqCst)
    }

    /// 強制重置
    pub async fn reset(&self) {
        *self.state.write().await = CircuitState::Closed;
        self.failure_count.store(0, Ordering::SeqCst);
        self.success_count.store(0, Ordering::SeqCst);
        self.half_open_requests.store(0, Ordering::SeqCst);
        *self.last_failure_time.write().await = None;
    }

    async fn should_try_half_open(&self) -> bool {
        if let Some(last_failure) = *self.last_failure_time.read().await {
            last_failure.elapsed() >= Duration::from_secs(self.config.open_duration_secs)
        } else {
            true
        }
    }

    async fn transition_to_open(&self, name: &str, error: &HftError) {
        let mut state = self.state.write().await;
        if *state != CircuitState::Open {
            *state = CircuitState::Open;
            self.success_count.store(0, Ordering::SeqCst);
            self.half_open_requests.store(0, Ordering::SeqCst);

            let failures = self.failure_count.load(Ordering::SeqCst);
            error!(
                "[{}] 熔斷器開啟！連續失敗 {} 次，最後錯誤: {}",
                name, failures, error
            );

            self.send_alert(name, CircuitState::Open, format!("連續失敗 {} 次: {}", failures, error));
        }
    }

    async fn transition_to_half_open(&self) {
        let mut state = self.state.write().await;
        if *state == CircuitState::Open {
            *state = CircuitState::HalfOpen;
            self.success_count.store(0, Ordering::SeqCst);
            self.half_open_requests.store(0, Ordering::SeqCst);
            info!("熔斷器進入半開狀態，開始試探請求");
        }
    }

    async fn transition_to_closed(&self, name: &str) {
        let mut state = self.state.write().await;
        *state = CircuitState::Closed;
        self.failure_count.store(0, Ordering::SeqCst);
        self.success_count.store(0, Ordering::SeqCst);
        self.half_open_requests.store(0, Ordering::SeqCst);

        info!("[{}] 熔斷器恢復正常", name);
        self.send_alert(name, CircuitState::Closed, "熔斷器恢復正常".to_string());
    }

    fn send_alert(&self, name: &str, state: CircuitState, message: String) {
        if let Some(ref callback) = self.alert_callback {
            let alert = CircuitBreakerAlert {
                name: name.to_string(),
                state,
                failure_count: self.failure_count.load(Ordering::SeqCst),
                message,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64,
            };
            callback(alert);
        }
    }
}

// ============================================================================
// 彈性執行器 (Resilient Executor)
// ============================================================================

/// 彈性執行器 - 結合重試和熔斷器
pub struct ResilientExecutor {
    pub name: String,
    pub retry_config: RetryConfig,
    pub circuit_breaker: CircuitBreaker,
    /// 執行統計
    total_calls: AtomicU64,
    successful_calls: AtomicU64,
    failed_calls: AtomicU64,
    retried_calls: AtomicU64,
    circuit_rejections: AtomicU64,
}

impl ResilientExecutor {
    pub fn new(name: impl Into<String>, retry_config: RetryConfig, cb_config: CircuitBreakerConfig) -> Self {
        Self {
            name: name.into(),
            retry_config,
            circuit_breaker: CircuitBreaker::new(cb_config),
            total_calls: AtomicU64::new(0),
            successful_calls: AtomicU64::new(0),
            failed_calls: AtomicU64::new(0),
            retried_calls: AtomicU64::new(0),
            circuit_rejections: AtomicU64::new(0),
        }
    }

    /// 設置告警回調
    pub fn with_alert_callback<F>(mut self, callback: F) -> Self
    where
        F: Fn(CircuitBreakerAlert) + Send + Sync + 'static,
    {
        self.circuit_breaker = self.circuit_breaker.with_alert_callback(callback);
        self
    }

    /// 執行帶韌性保護的操作
    pub async fn execute<T, F, Fut>(&self, operation: F) -> HftResult<T>
    where
        F: FnMut() -> Fut + Clone,
        Fut: std::future::Future<Output = HftResult<T>>,
    {
        self.total_calls.fetch_add(1, Ordering::SeqCst);

        // 檢查熔斷器
        if !self.circuit_breaker.allow_request().await {
            self.circuit_rejections.fetch_add(1, Ordering::SeqCst);
            return Err(HftError::Execution(format!(
                "[{}] 熔斷器開啟，請求被拒絕",
                self.name
            )));
        }

        // 執行帶重試的操作
        let mut attempts = 0u32;
        let mut delay_ms = self.retry_config.initial_delay_ms;
        let mut op = operation;

        loop {
            match op().await {
                Ok(result) => {
                    self.successful_calls.fetch_add(1, Ordering::SeqCst);
                    self.circuit_breaker.record_success(&self.name).await;
                    return Ok(result);
                }
                Err(e) => {
                    attempts += 1;

                    // 記錄到熔斷器
                    self.circuit_breaker.record_failure(&self.name, &e).await;

                    if attempts >= self.retry_config.max_retries {
                        self.failed_calls.fetch_add(1, Ordering::SeqCst);
                        error!(
                            "[{}] 操作失敗，已達最大重試次數 {}: {}",
                            self.name, self.retry_config.max_retries, e
                        );
                        return Err(e);
                    }

                    if !is_retryable(&e, &self.retry_config) {
                        self.failed_calls.fetch_add(1, Ordering::SeqCst);
                        error!("[{}] 不可重試的錯誤: {}", self.name, e);
                        return Err(e);
                    }

                    self.retried_calls.fetch_add(1, Ordering::SeqCst);
                    warn!(
                        "[{}] 操作失敗，{}ms 後重試 ({}/{}): {}",
                        self.name, delay_ms, attempts, self.retry_config.max_retries, e
                    );

                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = ((delay_ms as f64 * self.retry_config.backoff_multiplier) as u64)
                        .min(self.retry_config.max_delay_ms);
                }
            }
        }
    }

    /// 獲取統計信息
    pub fn stats(&self) -> ExecutorStats {
        ExecutorStats {
            name: self.name.clone(),
            total_calls: self.total_calls.load(Ordering::SeqCst),
            successful_calls: self.successful_calls.load(Ordering::SeqCst),
            failed_calls: self.failed_calls.load(Ordering::SeqCst),
            retried_calls: self.retried_calls.load(Ordering::SeqCst),
            circuit_rejections: self.circuit_rejections.load(Ordering::SeqCst),
            circuit_state: futures::executor::block_on(self.circuit_breaker.state()),
        }
    }
}

/// 執行器統計
#[derive(Debug, Clone)]
pub struct ExecutorStats {
    pub name: String,
    pub total_calls: u64,
    pub successful_calls: u64,
    pub failed_calls: u64,
    pub retried_calls: u64,
    pub circuit_rejections: u64,
    pub circuit_state: CircuitState,
}

// ============================================================================
// 告警通知
// ============================================================================

/// 執行告警類型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionAlertType {
    /// 初始化失敗
    InitializationFailed,
    /// 連續失敗
    ConsecutiveFailures,
    /// 熔斷器開啟
    CircuitOpen,
    /// 熔斷器恢復
    CircuitRecovered,
    /// 重試耗盡
    RetriesExhausted,
}

/// 執行告警
#[derive(Debug, Clone)]
pub struct ExecutionAlert {
    pub alert_type: ExecutionAlertType,
    pub venue: String,
    pub operation: String,
    pub message: String,
    pub error: Option<String>,
    pub failure_count: u32,
    pub timestamp: u64,
}

impl ExecutionAlert {
    pub fn new(
        alert_type: ExecutionAlertType,
        venue: impl Into<String>,
        operation: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            alert_type,
            venue: venue.into(),
            operation: operation.into(),
            message: message.into(),
            error: None,
            failure_count: 0,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as u64,
        }
    }

    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    pub fn with_failure_count(mut self, count: u32) -> Self {
        self.failure_count = count;
        self
    }

    /// 轉換為 Redis ops.alert 格式的 JSON
    pub fn to_ops_alert_json(&self) -> String {
        let alert_type_str = match self.alert_type {
            ExecutionAlertType::InitializationFailed => "exec_init_failed",
            ExecutionAlertType::ConsecutiveFailures => "exec_consecutive_failures",
            ExecutionAlertType::CircuitOpen => "exec_circuit_open",
            ExecutionAlertType::CircuitRecovered => "exec_circuit_recovered",
            ExecutionAlertType::RetriesExhausted => "exec_retries_exhausted",
        };

        format!(
            r#"{{"type":"{}","venue":"{}","operation":"{}","message":"{}","error":{},"failure_count":{},"ts":{}}}"#,
            alert_type_str,
            self.venue,
            self.operation,
            self.message,
            self.error.as_ref().map(|e| format!("\"{}\"", e)).unwrap_or_else(|| "null".to_string()),
            self.failure_count,
            self.timestamp
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_retryable() {
        let config = RetryConfig::default();

        assert!(is_retryable(&HftError::Network("connection refused".into()), &config));
        assert!(is_retryable(&HftError::RateLimit("too many requests".into()), &config));
        assert!(is_retryable(&HftError::Execution("HTTP client not initialized".into()), &config));
        assert!(is_retryable(&HftError::Timeout("request timeout".into()), &config));

        assert!(!is_retryable(&HftError::InvalidOrder("bad order".into()), &config));
        assert!(!is_retryable(&HftError::InsufficientFunds("no money".into()), &config));
    }

    #[tokio::test]
    async fn test_circuit_breaker_basic() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            open_duration_secs: 1,
            ..Default::default()
        };
        let cb = CircuitBreaker::new(config);

        // 初始狀態為關閉
        assert_eq!(cb.state().await, CircuitState::Closed);
        assert!(cb.allow_request().await);

        // 連續失敗
        let error = HftError::Network("test error".into());
        cb.record_failure("test", &error).await;
        cb.record_failure("test", &error).await;
        assert_eq!(cb.state().await, CircuitState::Closed);

        cb.record_failure("test", &error).await;
        assert_eq!(cb.state().await, CircuitState::Open);
        assert!(!cb.allow_request().await);
    }

    #[test]
    fn test_execution_alert_json() {
        let alert = ExecutionAlert::new(
            ExecutionAlertType::CircuitOpen,
            "binance",
            "place_order",
            "Circuit breaker opened",
        )
        .with_error("Connection refused")
        .with_failure_count(5);

        let json = alert.to_ops_alert_json();
        assert!(json.contains("exec_circuit_open"));
        assert!(json.contains("binance"));
        assert!(json.contains("place_order"));
    }
}
