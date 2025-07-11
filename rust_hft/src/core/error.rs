/*!
 * 🚨 統一錯誤處理系統
 * 
 * 提供結構化、可恢復的錯誤處理機制，替換整個Pipeline系統中的unwrap()調用
 * 
 * 特性：
 * - 分層錯誤類型，便於錯誤分類和處理
 * - 錯誤上下文追蹤，提供詳細的錯誤路徑
 * - 可恢復錯誤策略，支持自動重試
 * - 結構化錯誤報告，便於監控和調試
 */

use thiserror::Error;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::fmt;
use chrono::{DateTime, Utc};

/// 創建錯誤上下文的宏
#[macro_export]
macro_rules! error_context {
    () => {
        ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!()))
    };
    ($func:expr) => {
        ErrorContext::new(module_path!(), $func, &format!("{}:{}", file!(), line!()))
    };
}

/// 頂層Pipeline錯誤類型
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum PipelineError {
    /// 配置相關錯誤
    #[error("配置錯誤: {source}")]
    Configuration {
        source: ConfigurationError,
        context: ErrorContext,
    },
    
    /// 數據處理錯誤
    #[error("數據處理錯誤: {source}")]
    DataProcessing {
        source: DataProcessingError,
        context: ErrorContext,
    },
    
    /// 模型相關錯誤
    #[error("模型錯誤: {source}")]
    Model {
        source: ModelError,
        context: ErrorContext,
    },
    
    /// 執行引擎錯誤
    #[error("執行引擎錯誤: {source}")]
    Execution {
        source: ExecutionError,
        context: ErrorContext,
    },
    
    /// 資源管理錯誤
    #[error("資源管理錯誤: {source}")]
    Resource {
        source: ResourceError,
        context: ErrorContext,
    },
    
    /// 並發相關錯誤
    #[error("並發錯誤: {source}")]
    Concurrency {
        source: ConcurrencyError,
        context: ErrorContext,
    },
    
    /// 外部依賴錯誤
    #[error("外部依賴錯誤: {source}")]
    External {
        source: ExternalError,
        context: ErrorContext,
    },
    
    /// 系統錯誤
    #[error("系統錯誤: {source}")]
    System {
        source: SystemError,
        context: ErrorContext,
    },
}

/// 配置錯誤
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum ConfigurationError {
    #[error("配置文件不存在: {path}")]
    FileNotFound { path: String },
    
    #[error("配置格式無效: {reason}")]
    InvalidFormat { reason: String },
    
    #[error("必需配置項缺失: {field}")]
    MissingRequired { field: String },
    
    #[error("配置值無效: {field} = {value}, 原因: {reason}")]
    InvalidValue { field: String, value: String, reason: String },
    
    #[error("配置驗證失敗: {details}")]
    ValidationFailed { details: String },
    
    #[error("配置版本不兼容: 期望 {expected}, 實際 {actual}")]
    VersionMismatch { expected: String, actual: String },
}

/// 數據處理錯誤
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum DataProcessingError {
    #[error("數據源不可用: {source}")]
    SourceUnavailable { source: String },
    
    #[error("數據格式錯誤: {expected}, 實際: {actual}")]
    FormatMismatch { expected: String, actual: String },
    
    #[error("數據質量不合格: {metric} = {value}, 閾值: {threshold}")]
    QualityBelowThreshold { metric: String, value: f64, threshold: f64 },
    
    #[error("特徵工程失敗: {feature}, 原因: {reason}")]
    FeatureEngineering { feature: String, reason: String },
    
    #[error("數據驗證失敗: {check}, 詳情: {details}")]
    ValidationFailed { check: String, details: String },
    
    #[error("數據轉換失敗: {from} -> {to}, 原因: {reason}")]
    TransformationFailed { from: String, to: String, reason: String },
    
    #[error("批處理失敗: 批次 {batch_id}, 原因: {reason}")]
    BatchProcessingFailed { batch_id: String, reason: String },
}

/// 模型錯誤
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum ModelError {
    #[error("模型文件不存在: {path}")]
    ModelNotFound { path: String },
    
    #[error("模型加載失敗: {path}, 原因: {reason}")]
    LoadFailed { path: String, reason: String },
    
    #[error("模型版本不兼容: 期望 {expected}, 實際 {actual}")]
    VersionMismatch { expected: String, actual: String },
    
    #[error("訓練失敗: 輪次 {epoch}/{total}, 損失: {loss}, 原因: {reason}")]
    TrainingFailed { epoch: u32, total: u32, loss: f64, reason: String },
    
    #[error("預測失敗: 輸入形狀 {input_shape}, 原因: {reason}")]
    PredictionFailed { input_shape: String, reason: String },
    
    #[error("模型評估失敗: 指標 {metric}, 原因: {reason}")]
    EvaluationFailed { metric: String, reason: String },
    
    #[error("超參數優化失敗: 試驗 {trial}, 原因: {reason}")]
    HyperoptFailed { trial: u32, reason: String },
    
    #[error("模型收斂失敗: 最大輪次 {max_epochs}, 最終損失: {final_loss}")]
    ConvergenceFailed { max_epochs: u32, final_loss: f64 },
}

/// 執行引擎錯誤
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionError {
    #[error("Pipeline執行超時: {pipeline_id}, 超時時間: {timeout_seconds}秒")]
    Timeout { pipeline_id: String, timeout_seconds: u64 },
    
    #[error("Pipeline初始化失敗: {pipeline_id}, 原因: {reason}")]
    InitializationFailed { pipeline_id: String, reason: String },
    
    #[error("執行被取消: {pipeline_id}, 原因: {reason}")]
    Cancelled { pipeline_id: String, reason: String },
    
    #[error("依賴Pipeline失敗: {dependency}, 影響: {pipeline_id}")]
    DependencyFailed { dependency: String, pipeline_id: String },
    
    #[error("重試次數超限: {pipeline_id}, 最大重試: {max_retries}")]
    MaxRetriesExceeded { pipeline_id: String, max_retries: u32 },
    
    #[error("執行器不可用: {executor_type}, 原因: {reason}")]
    ExecutorUnavailable { executor_type: String, reason: String },
}

/// 資源管理錯誤
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum ResourceError {
    #[error("內存不足: 請求 {requested_mb}MB, 可用 {available_mb}MB")]
    InsufficientMemory { requested_mb: u64, available_mb: u64 },
    
    #[error("CPU資源不足: 請求 {requested_cores}核, 可用 {available_cores}核")]
    InsufficientCpu { requested_cores: u32, available_cores: u32 },
    
    #[error("磁盤空間不足: 請求 {requested_gb}GB, 可用 {available_gb}GB")]
    InsufficientDisk { requested_gb: u64, available_gb: u64 },
    
    #[error("GPU資源不足: 請求 {requested_gpus}個GPU, 可用 {available_gpus}個")]
    InsufficientGpu { requested_gpus: u32, available_gpus: u32 },
    
    #[error("資源鎖定超時: {resource_type}, 等待時間: {wait_seconds}秒")]
    LockTimeout { resource_type: String, wait_seconds: u64 },
    
    #[error("資源池耗盡: {pool_type}, 池大小: {pool_size}")]
    PoolExhausted { pool_type: String, pool_size: u32 },
}

/// 並發錯誤
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum ConcurrencyError {
    #[error("死鎖檢測: 涉及線程 {thread_ids:?}")]
    DeadlockDetected { thread_ids: Vec<String> },
    
    #[error("鎖定超時: {lock_type}, 等待時間: {wait_seconds}秒")]
    LockTimeout { lock_type: String, wait_seconds: u64 },
    
    #[error("通道已關閉: {channel_type}")]
    ChannelClosed { channel_type: String },
    
    #[error("通道滿載: {channel_type}, 容量: {capacity}")]
    ChannelFull { channel_type: String, capacity: usize },
    
    #[error("任務被取消: {task_id}")]
    TaskCancelled { task_id: String },
    
    #[error("同步失敗: {operation}, 原因: {reason}")]
    SynchronizationFailed { operation: String, reason: String },
}

/// 外部依賴錯誤
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum ExternalError {
    #[error("網絡連接失敗: {endpoint}, 原因: {reason}")]
    NetworkConnection { endpoint: String, reason: String },
    
    #[error("API調用失敗: {api}, 狀態碼: {status_code}, 響應: {response}")]
    ApiCall { api: String, status_code: u16, response: String },
    
    #[error("數據庫連接失敗: {database}, 原因: {reason}")]
    DatabaseConnection { database: String, reason: String },
    
    #[error("文件系統錯誤: {operation}, 路徑: {path}, 原因: {reason}")]
    FileSystem { operation: String, path: String, reason: String },
    
    #[error("第三方服務不可用: {service}, 原因: {reason}")]
    ServiceUnavailable { service: String, reason: String },
    
    #[error("認證失敗: {service}, 原因: {reason}")]
    AuthenticationFailed { service: String, reason: String },
}

/// 系統錯誤
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum SystemError {
    #[error("操作系統錯誤: {operation}, 錯誤碼: {error_code}")]
    OperatingSystem { operation: String, error_code: i32 },
    
    #[error("內存分配失敗: {size_bytes}字節")]
    MemoryAllocation { size_bytes: usize },
    
    #[error("線程創建失敗: {reason}")]
    ThreadCreation { reason: String },
    
    #[error("信號處理失敗: {signal}, 原因: {reason}")]
    SignalHandling { signal: String, reason: String },
    
    #[error("系統調用失敗: {syscall}, 錯誤: {error}")]
    SystemCall { syscall: String, error: String },
    
    #[error("環境變數錯誤: {variable}, 原因: {reason}")]
    Environment { variable: String, reason: String },
}

/// 錯誤上下文信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorContext {
    /// 錯誤發生時間
    pub timestamp: DateTime<Utc>,
    
    /// Pipeline ID
    pub pipeline_id: Option<String>,
    
    /// 執行ID
    pub execution_id: Option<String>,
    
    /// 模塊路徑
    pub module_path: String,
    
    /// 函數名稱
    pub function_name: String,
    
    /// 文件和行號
    pub file_location: String,
    
    /// 額外的上下文數據
    pub metadata: HashMap<String, String>,
    
    /// 錯誤鏈
    pub error_chain: Vec<String>,
}

impl ErrorContext {
    /// 創建新的錯誤上下文
    pub fn new(module_path: &str, function_name: &str, file_location: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            pipeline_id: None,
            execution_id: None,
            module_path: module_path.to_string(),
            function_name: function_name.to_string(),
            file_location: file_location.to_string(),
            metadata: HashMap::new(),
            error_chain: Vec::new(),
        }
    }
    
    /// 設置Pipeline ID
    pub fn with_pipeline_id(mut self, pipeline_id: String) -> Self {
        self.pipeline_id = Some(pipeline_id);
        self
    }
    
    /// 設置執行ID
    pub fn with_execution_id(mut self, execution_id: String) -> Self {
        self.execution_id = Some(execution_id);
        self
    }
    
    /// 添加元數據
    pub fn with_metadata(mut self, key: String, value: String) -> Self {
        self.metadata.insert(key, value);
        self
    }
    
    /// 添加到錯誤鏈
    pub fn add_to_chain(mut self, error: String) -> Self {
        self.error_chain.push(error);
        self
    }
}

/// 錯誤恢復策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RecoveryStrategy {
    /// 不進行恢復，直接失敗
    Fail,
    
    /// 重試指定次數
    Retry { 
        max_attempts: u32, 
        delay_seconds: u64,
        backoff_multiplier: f64,
    },
    
    /// 降級服務
    Degrade { 
        fallback_mode: String 
    },
    
    /// 跳過當前操作
    Skip,
    
    /// 使用默認值
    UseDefault { 
        default_value: String 
    },
    
    /// 切換到備用資源
    Failover { 
        backup_resource: String 
    },
}

/// 錯誤嚴重程度
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum ErrorSeverity {
    /// 調試信息
    Debug = 1,
    
    /// 信息性錯誤
    Info = 2,
    
    /// 警告錯誤
    Warning = 3,
    
    /// 一般錯誤
    Error = 4,
    
    /// 嚴重錯誤
    Critical = 5,
    
    /// 系統致命錯誤
    Fatal = 6,
}

impl PipelineError {
    /// 獲取錯誤嚴重程度
    pub fn severity(&self) -> ErrorSeverity {
        match self {
            PipelineError::Configuration { source, .. } => match source {
                ConfigurationError::MissingRequired { .. } => ErrorSeverity::Critical,
                ConfigurationError::ValidationFailed { .. } => ErrorSeverity::Error,
                _ => ErrorSeverity::Warning,
            },
            PipelineError::DataProcessing { source, .. } => match source {
                DataProcessingError::SourceUnavailable { .. } => ErrorSeverity::Critical,
                DataProcessingError::QualityBelowThreshold { .. } => ErrorSeverity::Warning,
                _ => ErrorSeverity::Error,
            },
            PipelineError::Model { source, .. } => match source {
                ModelError::ModelNotFound { .. } => ErrorSeverity::Critical,
                ModelError::ConvergenceFailed { .. } => ErrorSeverity::Warning,
                _ => ErrorSeverity::Error,
            },
            PipelineError::Execution { source, .. } => match source {
                ExecutionError::Timeout { .. } => ErrorSeverity::Error,
                ExecutionError::MaxRetriesExceeded { .. } => ErrorSeverity::Critical,
                _ => ErrorSeverity::Error,
            },
            PipelineError::Resource { .. } => ErrorSeverity::Critical,
            PipelineError::Concurrency { source, .. } => match source {
                ConcurrencyError::DeadlockDetected { .. } => ErrorSeverity::Fatal,
                _ => ErrorSeverity::Critical,
            },
            PipelineError::External { .. } => ErrorSeverity::Error,
            PipelineError::System { .. } => ErrorSeverity::Fatal,
        }
    }
    
    /// 獲取建議的恢復策略
    pub fn recovery_strategy(&self) -> RecoveryStrategy {
        match self {
            PipelineError::Configuration { .. } => RecoveryStrategy::Fail,
            PipelineError::DataProcessing { source, .. } => match source {
                DataProcessingError::SourceUnavailable { .. } => RecoveryStrategy::Retry {
                    max_attempts: 3,
                    delay_seconds: 5,
                    backoff_multiplier: 2.0,
                },
                DataProcessingError::QualityBelowThreshold { .. } => RecoveryStrategy::Degrade {
                    fallback_mode: "low_quality_mode".to_string(),
                },
                _ => RecoveryStrategy::Retry {
                    max_attempts: 2,
                    delay_seconds: 1,
                    backoff_multiplier: 1.5,
                },
            },
            PipelineError::Model { source, .. } => match source {
                ModelError::ModelNotFound { .. } => RecoveryStrategy::UseDefault {
                    default_value: "fallback_model".to_string(),
                },
                ModelError::TrainingFailed { .. } => RecoveryStrategy::Retry {
                    max_attempts: 2,
                    delay_seconds: 10,
                    backoff_multiplier: 1.0,
                },
                _ => RecoveryStrategy::Fail,
            },
            PipelineError::Execution { source, .. } => match source {
                ExecutionError::Timeout { .. } => RecoveryStrategy::Retry {
                    max_attempts: 1,
                    delay_seconds: 5,
                    backoff_multiplier: 1.0,
                },
                _ => RecoveryStrategy::Fail,
            },
            PipelineError::Resource { .. } => RecoveryStrategy::Retry {
                max_attempts: 3,
                delay_seconds: 2,
                backoff_multiplier: 2.0,
            },
            PipelineError::Concurrency { source, .. } => match source {
                ConcurrencyError::DeadlockDetected { .. } => RecoveryStrategy::Fail,
                ConcurrencyError::LockTimeout { .. } => RecoveryStrategy::Retry {
                    max_attempts: 3,
                    delay_seconds: 1,
                    backoff_multiplier: 1.5,
                },
                _ => RecoveryStrategy::Retry {
                    max_attempts: 2,
                    delay_seconds: 1,
                    backoff_multiplier: 1.0,
                },
            },
            PipelineError::External { .. } => RecoveryStrategy::Retry {
                max_attempts: 3,
                delay_seconds: 5,
                backoff_multiplier: 2.0,
            },
            PipelineError::System { .. } => RecoveryStrategy::Fail,
        }
    }
    
    /// 檢查錯誤是否可恢復
    pub fn is_recoverable(&self) -> bool {
        !matches!(self.recovery_strategy(), RecoveryStrategy::Fail)
    }
    
    /// 獲取錯誤的結構化數據
    pub fn to_structured_data(&self) -> HashMap<String, serde_json::Value> {
        let mut data = HashMap::new();
        
        data.insert("error_type".to_string(), serde_json::Value::String(self.error_type()));
        data.insert("severity".to_string(), serde_json::Value::String(format!("{:?}", self.severity())));
        data.insert("recoverable".to_string(), serde_json::Value::Bool(self.is_recoverable()));
        data.insert("message".to_string(), serde_json::Value::String(self.to_string()));
        
        // 添加錯誤特定的數據
        match self {
            PipelineError::Configuration { context, .. } |
            PipelineError::DataProcessing { context, .. } |
            PipelineError::Model { context, .. } |
            PipelineError::Execution { context, .. } |
            PipelineError::Resource { context, .. } |
            PipelineError::Concurrency { context, .. } |
            PipelineError::External { context, .. } |
            PipelineError::System { context, .. } => {
                data.insert("timestamp".to_string(), serde_json::Value::String(context.timestamp.to_rfc3339()));
                data.insert("module_path".to_string(), serde_json::Value::String(context.module_path.clone()));
                data.insert("function_name".to_string(), serde_json::Value::String(context.function_name.clone()));
                
                if let Some(pipeline_id) = &context.pipeline_id {
                    data.insert("pipeline_id".to_string(), serde_json::Value::String(pipeline_id.clone()));
                }
                
                if let Some(execution_id) = &context.execution_id {
                    data.insert("execution_id".to_string(), serde_json::Value::String(execution_id.clone()));
                }
                
                // 添加元數據
                for (key, value) in &context.metadata {
                    data.insert(format!("metadata_{}", key), serde_json::Value::String(value.clone()));
                }
            }
        }
        
        data
    }
    
    /// 獲取錯誤類型字符串
    fn error_type(&self) -> String {
        match self {
            PipelineError::Configuration { .. } => "Configuration".to_string(),
            PipelineError::DataProcessing { .. } => "DataProcessing".to_string(),
            PipelineError::Model { .. } => "Model".to_string(),
            PipelineError::Execution { .. } => "Execution".to_string(),
            PipelineError::Resource { .. } => "Resource".to_string(),
            PipelineError::Concurrency { .. } => "Concurrency".to_string(),
            PipelineError::External { .. } => "External".to_string(),
            PipelineError::System { .. } => "System".to_string(),
        }
    }
}

// From implementations for converting individual error types to PipelineError
impl From<ConfigurationError> for PipelineError {
    fn from(err: ConfigurationError) -> Self {
        PipelineError::Configuration {
            source: err,
            context: ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())),
        }
    }
}

impl From<DataProcessingError> for PipelineError {
    fn from(err: DataProcessingError) -> Self {
        PipelineError::DataProcessing {
            source: err,
            context: ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())),
        }
    }
}

impl From<ModelError> for PipelineError {
    fn from(err: ModelError) -> Self {
        PipelineError::Model {
            source: err,
            context: ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())),
        }
    }
}

impl From<ExecutionError> for PipelineError {
    fn from(err: ExecutionError) -> Self {
        PipelineError::Execution {
            source: err,
            context: ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())),
        }
    }
}

impl From<ResourceError> for PipelineError {
    fn from(err: ResourceError) -> Self {
        PipelineError::Resource {
            source: err,
            context: ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())),
        }
    }
}

impl From<ConcurrencyError> for PipelineError {
    fn from(err: ConcurrencyError) -> Self {
        PipelineError::Concurrency {
            source: err,
            context: ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())),
        }
    }
}

impl From<ExternalError> for PipelineError {
    fn from(err: ExternalError) -> Self {
        PipelineError::External {
            source: err,
            context: ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())),
        }
    }
}

impl From<SystemError> for PipelineError {
    fn from(err: SystemError) -> Self {
        PipelineError::System {
            source: err,
            context: ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())),
        }
    }
}

/// 錯誤處理宏
#[macro_export]
macro_rules! pipeline_error {
    ($error_type:expr, $context:expr) => {
        PipelineError::from($error_type).with_context($context)
    };
}

/// 創建錯誤上下文的宏
#[macro_export]
macro_rules! error_context {
    () => {
        ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!()))
    };
    ($func:expr) => {
        ErrorContext::new(module_path!(), $func, &format!("{}:{}", file!(), line!()))
    };
}

/// 便利的Result類型別名
pub type PipelineResult<T> = Result<T, PipelineError>;

/// 錯誤處理特性擴展
pub trait PipelineErrorExt<T> {
    /// 添加錯誤上下文
    fn with_pipeline_context(self, context: ErrorContext) -> PipelineResult<T>;
    
    /// 添加Pipeline ID到錯誤上下文
    fn with_pipeline_id(self, pipeline_id: String) -> PipelineResult<T>;
    
    /// 添加執行ID到錯誤上下文
    fn with_execution_id(self, execution_id: String) -> PipelineResult<T>;
    
    /// 添加元數據到錯誤上下文
    fn with_metadata(self, key: String, value: String) -> PipelineResult<T>;
}

impl<T, E> PipelineErrorExt<T> for Result<T, E>
where
    E: Into<PipelineError>,
{
    fn with_pipeline_context(self, context: ErrorContext) -> PipelineResult<T> {
        self.map_err(|e| {
            let mut error: PipelineError = e.into();
            // 更新錯誤中的上下文信息
            match &mut error {
                PipelineError::Configuration { context: ctx, .. } |
                PipelineError::DataProcessing { context: ctx, .. } |
                PipelineError::Model { context: ctx, .. } |
                PipelineError::Execution { context: ctx, .. } |
                PipelineError::Resource { context: ctx, .. } |
                PipelineError::Concurrency { context: ctx, .. } |
                PipelineError::External { context: ctx, .. } |
                PipelineError::System { context: ctx, .. } => {
                    *ctx = context;
                }
            }
            error
        })
    }
    
    fn with_pipeline_id(self, pipeline_id: String) -> PipelineResult<T> {
        self.with_pipeline_context(ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())).with_pipeline_id(pipeline_id))
    }
    
    fn with_execution_id(self, execution_id: String) -> PipelineResult<T> {
        self.with_pipeline_context(ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())).with_execution_id(execution_id))
    }
    
    fn with_metadata(self, key: String, value: String) -> PipelineResult<T> {
        self.with_pipeline_context(ErrorContext::new(module_path!(), "", &format!("{}:{}", file!(), line!())).with_metadata(key, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_error_severity() {
        let config_error = PipelineError::Configuration {
            source: ConfigurationError::MissingRequired {
                field: "test_field".to_string(),
            },
            context: ErrorContext::new("test", "test_fn", "test.rs:1"),
        };
        
        assert_eq!(config_error.severity(), ErrorSeverity::Critical);
        assert!(!config_error.is_recoverable());
    }
    
    #[test]
    fn test_error_context_creation() {
        let context = ErrorContext::new("test_module", "test_function", "test.rs:42")
            .with_pipeline_id("test_pipeline".to_string())
            .with_metadata("key".to_string(), "value".to_string());
        
        assert_eq!(context.module_path, "test_module");
        assert_eq!(context.function_name, "test_function");
        assert_eq!(context.pipeline_id, Some("test_pipeline".to_string()));
        assert_eq!(context.metadata.get("key"), Some(&"value".to_string()));
    }
    
    #[test]
    fn test_recovery_strategy() {
        let data_error = PipelineError::DataProcessing {
            source: DataProcessingError::SourceUnavailable {
                source: "test_source".to_string(),
            },
            context: ErrorContext::new("test", "test_fn", "test.rs:1"),
        };
        
        match data_error.recovery_strategy() {
            RecoveryStrategy::Retry { max_attempts, .. } => {
                assert_eq!(max_attempts, 3);
            }
            _ => panic!("Expected retry strategy"),
        }
    }
    
    #[test]
    fn test_structured_data() {
        let error = PipelineError::Model {
            source: ModelError::ModelNotFound {
                path: "/path/to/model".to_string(),
            },
            context: ErrorContext::new("test", "test_fn", "test.rs:1"),
        };
        
        let data = error.to_structured_data();
        assert_eq!(data.get("error_type"), Some(&serde_json::Value::String("Model".to_string())));
        assert_eq!(data.get("recoverable"), Some(&serde_json::Value::Bool(true)));
    }
}