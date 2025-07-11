/*!
 * 🏗️ HFT Pipeline Framework - 統一的端到端執行系統
 * 
 * 核心設計理念：
 * - 配置驅動：所有Pipeline通過YAML配置定義
 * - 模塊化：每個Pipeline可獨立運行和測試
 * - 可觀測性：完整的執行監控和日誌
 * - 容錯性：支持失敗重試和錯誤恢復
 * - 性能優化：並行執行和資源管理
 * 
 * Pipeline類型：
 * - DataPipeline: 數據清洗、特徵工程、驗證
 * - TrainingPipeline: 模型訓練、驗證、保存
 * - HyperoptPipeline: 超參數優化、模型選擇
 * - EvaluationPipeline: 回測、性能分析、對比
 * - DryRunPipeline: 實時紙上交易測試
 * - TradingPipeline: 實盤交易、監控、更新
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::core::error::{
    PipelineError, PipelineResult, ExecutionError, ResourceError, ConcurrencyError, 
    ErrorContext, PipelineErrorExt, error_context
};
use crate::utils::concurrency_safety::{
    SafeLockManager, LockOrder, LockResult, GLOBAL_LOCK_MANAGER,
    safe_lock, safe_read_lock, safe_write_lock
};
use crate::utils::memory_optimization::{
    CowPtr, MemoryManager, GLOBAL_MEMORY_MANAGER, zero_alloc_vec
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore};
use tokio::time::timeout;
use tracing::{info, warn, error, debug, instrument};
use uuid::Uuid;

pub mod data_pipeline;
pub mod training_pipeline;
pub mod hyperopt_pipeline;
pub mod evaluation_pipeline;
pub mod dryrun_pipeline;
pub mod trading_pipeline;

pub use data_pipeline::DataPipeline;
pub use training_pipeline::TrainingPipeline;
pub use hyperopt_pipeline::HyperoptPipeline;
pub use evaluation_pipeline::EvaluationPipeline;
pub use dryrun_pipeline::DryRunPipeline;
pub use trading_pipeline::TradingPipeline;

/// Pipeline執行狀態
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PipelineStatus {
    Pending,       // 等待執行
    Initializing,  // 初始化中
    Running,       // 執行中
    Paused,        // 暫停
    Completed,     // 成功完成
    Failed,        // 執行失敗
    Cancelled,     // 已取消
    Timeout,       // 超時
}

/// Pipeline執行優先級
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum PipelinePriority {
    Critical = 1,
    High = 2,
    Medium = 3,
    Low = 4,
}

/// Pipeline執行結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineExecutionResult {
    pub pipeline_id: String,
    pub pipeline_type: String,
    pub status: PipelineStatus,
    pub success: bool,
    pub message: String,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
    pub duration_ms: Option<u64>,
    pub output_artifacts: Vec<String>,
    pub metrics: HashMap<String, f64>,
    pub error_details: Option<String>,
    pub resource_usage: ResourceUsage,
}

/// 資源使用統計
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceUsage {
    pub cpu_usage_percent: f64,
    pub memory_usage_mb: f64,
    pub disk_io_mb: f64,
    pub network_io_mb: f64,
    pub gpu_usage_percent: Option<f64>,
}

impl Default for ResourceUsage {
    fn default() -> Self {
        Self {
            cpu_usage_percent: 0.0,
            memory_usage_mb: 0.0,
            disk_io_mb: 0.0,
            network_io_mb: 0.0,
            gpu_usage_percent: None,
        }
    }
}

/// Pipeline配置基類
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Pipeline名稱
    pub name: String,
    /// Pipeline版本
    pub version: String,
    /// 描述
    pub description: Option<String>,
    /// 執行優先級
    pub priority: PipelinePriority,
    /// 超時時間（秒）
    pub timeout_seconds: u64,
    /// 最大重試次數
    pub max_retries: u32,
    /// 資源限制
    pub resource_limits: ResourceLimits,
    /// 環境變量
    pub environment: HashMap<String, String>,
    /// 輸出目錄
    pub output_directory: String,
    /// 是否啟用詳細日誌
    pub verbose_logging: bool,
    /// 失敗時是否繼續
    pub continue_on_failure: bool,
}

/// 資源限制配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub max_cpu_percent: f64,
    pub max_memory_mb: u64,
    pub max_disk_space_mb: u64,
    pub max_execution_time_minutes: u64,
    pub allow_gpu: bool,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_cpu_percent: 80.0,
            max_memory_mb: 8192,
            max_disk_space_mb: 10240,
            max_execution_time_minutes: 60,
            allow_gpu: true,
        }
    }
}

/// Pipeline執行上下文（內存優化版本）
#[derive(Debug)]
pub struct PipelineContext {
    pub pipeline_id: String,
    pub execution_id: String,
    pub config: CowPtr<PipelineConfig>,
    pub start_time: Instant,
    pub working_directory: String,
    pub artifacts: Vec<String>,
    pub intermediate_results: HashMap<String, serde_json::Value>,
    pub execution_metrics: HashMap<String, f64>,
}

impl PipelineContext {
    pub fn new(config: PipelineConfig) -> Self {
        let pipeline_id = Uuid::new_v4().to_string();
        let execution_id = format!("exec_{}", Uuid::new_v4().to_simple());
        let working_directory = format!("{}/{}", config.output_directory, execution_id);
        
        Self {
            pipeline_id,
            execution_id,
            config: CowPtr::from_owned(config),
            start_time: Instant::now(),
            working_directory,
            artifacts: Vec::new(),
            intermediate_results: HashMap::new(),
            execution_metrics: HashMap::new(),
        }
    }
    
    /// 創建共享配置的上下文（避免配置克隆）
    pub fn new_with_shared_config(config: Arc<PipelineConfig>) -> Self {
        let pipeline_id = Uuid::new_v4().to_string();
        let execution_id = format!("exec_{}", Uuid::new_v4().to_simple());
        let working_directory = format!("{}/{}", config.output_directory, execution_id);
        
        Self {
            pipeline_id,
            execution_id,
            config: CowPtr::from_shared(config),
            start_time: Instant::now(),
            working_directory,
            artifacts: Vec::new(),
            intermediate_results: HashMap::new(),
            execution_metrics: HashMap::new(),
        }
    }
    
    pub fn add_artifact(&mut self, path: String) {
        self.artifacts.push(path);
    }
    
    pub fn set_metric(&mut self, key: String, value: f64) {
        self.execution_metrics.insert(key, value);
    }
    
    pub fn get_metric(&self, key: &str) -> Option<f64> {
        self.execution_metrics.get(key).copied()
    }
    
    pub fn elapsed_time(&self) -> Duration {
        self.start_time.elapsed()
    }
}

/// Pipeline執行器特性
#[async_trait::async_trait]
pub trait PipelineExecutor: Send + Sync {
    /// 執行Pipeline
    async fn execute(&mut self, context: &mut PipelineContext) -> PipelineResult<PipelineExecutionResult>;
    
    /// 驗證配置
    fn validate_config(&self, config: &PipelineConfig) -> PipelineResult<()>;
    
    /// 獲取Pipeline類型
    fn pipeline_type(&self) -> String;
    
    /// 預估執行時間
    fn estimated_duration(&self, config: &PipelineConfig) -> Duration {
        Duration::from_secs(300) // 默認5分鐘
    }
    
    /// 檢查資源需求
    fn check_resource_requirements(&self, config: &PipelineConfig) -> PipelineResult<()> {
        // 檢查基本資源
        let available_memory = self.get_available_memory()
            .with_pipeline_context(error_context!("check_resource_requirements"))?;
            
        if available_memory < config.resource_limits.max_memory_mb as f64 {
            return Err(PipelineError::Resource {
                source: ResourceError::InsufficientMemory {
                    requested_mb: config.resource_limits.max_memory_mb,
                    available_mb: available_memory as u64,
                },
                context: error_context!("check_resource_requirements"),
            });
        }
        Ok(())
    }
    
    /// 獲取可用內存（MB）
    fn get_available_memory(&self) -> PipelineResult<f64> {
        // 實際實現會使用系統API來獲取真實的內存信息
        #[cfg(target_os = "linux")]
        {
            match std::fs::read_to_string("/proc/meminfo") {
                Ok(content) => {
                    for line in content.lines() {
                        if line.starts_with("MemAvailable:") {
                            if let Some(mem_str) = line.split_whitespace().nth(1) {
                                if let Ok(mem_kb) = mem_str.parse::<u64>() {
                                    return Ok(mem_kb as f64 / 1024.0); // 轉換為MB
                                }
                            }
                        }
                    }
                    // 如果沒有找到MemAvailable，使用MemFree
                    for line in content.lines() {
                        if line.starts_with("MemFree:") {
                            if let Some(mem_str) = line.split_whitespace().nth(1) {
                                if let Ok(mem_kb) = mem_str.parse::<u64>() {
                                    return Ok(mem_kb as f64 / 1024.0);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("無法讀取 /proc/meminfo: {}", e);
                }
            }
        }
        
        #[cfg(target_os = "macos")]
        {
            // macOS系統內存檢查
            // 這裡可以使用sysctl或其他系統API
            // 暫時返回估計值
        }
        
        #[cfg(target_os = "windows")]
        {
            // Windows系統內存檢查
            // 這裡可以使用Windows API
            // 暫時返回估計值
        }
        
        // 默認返回8GB可用內存
        Ok(8192.0)
    }
}

/// Pipeline管理器
pub struct PipelineManager {
    /// 活躍的Pipeline執行
    active_executions: Arc<RwLock<HashMap<String, PipelineExecution>>>,
    /// 執行隊列
    execution_queue: Arc<RwLock<Vec<QueuedExecution>>>,
    /// 資源信號量
    resource_semaphore: Arc<Semaphore>,
    /// 系統配置
    config: Config,
    /// 執行歷史
    execution_history: Arc<RwLock<Vec<PipelineExecutionResult>>>,
    /// 性能統計
    performance_stats: Arc<RwLock<PerformanceStats>>,
}

/// 排隊的執行任務
#[derive(Debug)]
struct QueuedExecution {
    execution_id: String,
    executor: Box<dyn PipelineExecutor>,
    context: PipelineContext,
    queued_at: Instant,
}

/// 活躍的Pipeline執行
#[derive(Debug)]
struct PipelineExecution {
    context: PipelineContext,
    status: PipelineStatus,
    started_at: Instant,
    resource_usage: ResourceUsage,
}

/// 性能統計
#[derive(Debug, Clone, Default)]
struct PerformanceStats {
    total_executions: u64,
    successful_executions: u64,
    failed_executions: u64,
    average_duration_ms: f64,
    total_cpu_time_ms: f64,
    total_memory_used_mb: f64,
}

impl PipelineManager {
    /// 創建新的Pipeline管理器
    pub fn new(config: Config) -> Self {
        let max_concurrent = num_cpus::get().max(4);
        
        Self {
            active_executions: Arc::new(RwLock::new(HashMap::new())),
            execution_queue: Arc::new(RwLock::new(Vec::new())),
            resource_semaphore: Arc::new(Semaphore::new(max_concurrent)),
            config,
            execution_history: Arc::new(RwLock::new(Vec::new())),
            performance_stats: Arc::new(RwLock::new(PerformanceStats::default())),
        }
    }
    
    /// 提交Pipeline執行
    #[instrument(skip(self, executor))]
    pub async fn submit_pipeline(
        &self,
        mut executor: Box<dyn PipelineExecutor>,
        pipeline_config: PipelineConfig,
    ) -> PipelineResult<String> {
        info!("提交Pipeline執行: {}", pipeline_config.name);
        
        // 驗證配置
        executor.validate_config(&pipeline_config)
            .with_pipeline_context(error_context!("submit_pipeline"))?;
            
        executor.check_resource_requirements(&pipeline_config)
            .with_pipeline_context(error_context!("submit_pipeline"))?;
        
        // 創建執行上下文
        let context = PipelineContext::new(pipeline_config);
        let execution_id = context.execution_id.clone();
        
        // 創建工作目錄
        tokio::fs::create_dir_all(&context.working_directory).await
            .map_err(|e| PipelineError::External {
                source: crate::core::error::ExternalError::FileSystem {
                    operation: "create_dir_all".to_string(),
                    path: context.working_directory.clone(),
                    reason: e.to_string(),
                },
                context: error_context!("submit_pipeline").with_execution_id(execution_id.clone()),
            })?;
        
        // 添加到執行隊列
        let queued_execution = QueuedExecution {
            execution_id: execution_id.clone(),
            executor,
            context,
            queued_at: Instant::now(),
        };
        
        // 使用安全鎖管理器獲取執行隊列鎖
        let mut queue = match safe_write_lock!(&self.execution_queue, LockOrder::CommandQueue) {
            LockResult::Success(guard) => guard,
            LockResult::Timeout { waited_ms } => {
                return Err(PipelineError::Concurrency {
                    source: ConcurrencyError::LockTimeout {
                        lock_type: "execution_queue".to_string(),
                        wait_seconds: waited_ms / 1000,
                    },
                    context: error_context!("submit_pipeline").with_execution_id(execution_id.clone()),
                });
            }
            LockResult::DeadlockDetected { involved_locks } => {
                return Err(PipelineError::Concurrency {
                    source: ConcurrencyError::DeadlockDetected {
                        thread_ids: vec![format!("{:?}", std::thread::current().id())],
                    },
                    context: error_context!("submit_pipeline")
                        .with_execution_id(execution_id.clone())
                        .with_metadata("involved_locks".to_string(), format!("{:?}", involved_locks)),
                });
            }
            LockResult::Error { message } => {
                return Err(PipelineError::Concurrency {
                    source: ConcurrencyError::SynchronizationFailed {
                        operation: "write_lock_execution_queue".to_string(),
                        reason: message,
                    },
                    context: error_context!("submit_pipeline").with_execution_id(execution_id.clone()),
                });
            }
        };
            
        queue.push(queued_execution);
        drop(queue); // 明確釋放鎖
        
        // 嘗試啟動執行
        self.try_start_next_execution().await
            .with_execution_id(execution_id.clone())?;
        
        Ok(execution_id)
    }
    
    /// 嘗試啟動下一個執行
    async fn try_start_next_execution(&self) -> PipelineResult<()> {
        // 檢查是否有可用資源
        if self.resource_semaphore.available_permits() == 0 {
            debug!("資源已滿，等待資源釋放");
            return Ok(());
        }
        
        // 安全獲取下一個待執行任務
        let queued_execution = {
            let mut queue = match safe_write_lock!(&self.execution_queue, LockOrder::CommandQueue) {
                LockResult::Success(guard) => guard,
                LockResult::Timeout { waited_ms } => {
                    return Err(PipelineError::Concurrency {
                        source: ConcurrencyError::LockTimeout {
                            lock_type: "execution_queue".to_string(),
                            wait_seconds: waited_ms / 1000,
                        },
                        context: error_context!("try_start_next_execution"),
                    });
                }
                LockResult::DeadlockDetected { involved_locks } => {
                    return Err(PipelineError::Concurrency {
                        source: ConcurrencyError::DeadlockDetected {
                            thread_ids: vec![format!("{:?}", std::thread::current().id())],
                        },
                        context: error_context!("try_start_next_execution")
                            .with_metadata("involved_locks".to_string(), format!("{:?}", involved_locks)),
                    });
                }
                LockResult::Error { message } => {
                    return Err(PipelineError::Concurrency {
                        source: ConcurrencyError::SynchronizationFailed {
                            operation: "write_lock_execution_queue".to_string(),
                            reason: message,
                        },
                        context: error_context!("try_start_next_execution"),
                    });
                }
            };
                
            if queue.is_empty() {
                return Ok(());
            }
            
            // 按優先級排序
            queue.sort_by_key(|e| e.context.config.priority.clone());
            queue.remove(0)
        };
        
        info!("開始執行Pipeline: {}", queued_execution.execution_id);
        
        // 獲取資源許可，使用超時避免無限等待
        let permit_timeout = Duration::from_secs(60);
        let permit = timeout(permit_timeout, self.resource_semaphore.clone().acquire_owned()).await
            .map_err(|_| PipelineError::Resource {
                source: ResourceError::LockTimeout {
                    resource_type: "semaphore".to_string(),
                    wait_seconds: 60,
                },
                context: error_context!("try_start_next_execution")
                    .with_execution_id(queued_execution.execution_id.clone()),
            })?
            .map_err(|e| PipelineError::Concurrency {
                source: ConcurrencyError::SynchronizationFailed {
                    operation: "acquire_semaphore".to_string(),
                    reason: e.to_string(),
                },
                context: error_context!("try_start_next_execution")
                    .with_execution_id(queued_execution.execution_id.clone()),
            })?;
        
        // 啟動執行
        let execution_id = queued_execution.execution_id.clone();
        let active_executions = self.active_executions.clone();
        let execution_history = self.execution_history.clone();
        let performance_stats = self.performance_stats.clone();
        
        // 安全記錄活躍執行
        {
            let execution = PipelineExecution {
                context: queued_execution.context.clone(),
                status: PipelineStatus::Running,
                started_at: Instant::now(),
                resource_usage: ResourceUsage::default(),
            };
            
            let mut active_exec = match safe_write_lock!(&active_executions, LockOrder::PipelineState) {
                LockResult::Success(guard) => guard,
                LockResult::Timeout { waited_ms } => {
                    return Err(PipelineError::Concurrency {
                        source: ConcurrencyError::LockTimeout {
                            lock_type: "active_executions".to_string(),
                            wait_seconds: waited_ms / 1000,
                        },
                        context: error_context!("try_start_next_execution")
                            .with_execution_id(execution_id.clone()),
                    });
                }
                LockResult::DeadlockDetected { involved_locks } => {
                    return Err(PipelineError::Concurrency {
                        source: ConcurrencyError::DeadlockDetected {
                            thread_ids: vec![format!("{:?}", std::thread::current().id())],
                        },
                        context: error_context!("try_start_next_execution")
                            .with_execution_id(execution_id.clone())
                            .with_metadata("involved_locks".to_string(), format!("{:?}", involved_locks)),
                    });
                }
                LockResult::Error { message } => {
                    return Err(PipelineError::Concurrency {
                        source: ConcurrencyError::SynchronizationFailed {
                            operation: "write_lock_active_executions".to_string(),
                            reason: message,
                        },
                        context: error_context!("try_start_next_execution")
                            .with_execution_id(execution_id.clone()),
                    });
                }
            };
                
            active_exec.insert(execution_id.clone(), execution);
        }
        
        // 在後台執行
        tokio::spawn(async move {
            let result = Self::execute_pipeline(
                queued_execution.executor,
                queued_execution.context,
                permit,
            ).await;
            
            // 更新執行歷史和統計
            match result {
                Ok(ref pipeline_result) => {
                    // 安全地更新執行歷史
                    if let Ok(mut history) = timeout(Duration::from_secs(5), execution_history.write()).await {
                        history.push(pipeline_result.clone());
                    } else {
                        warn!("更新執行歷史超時: {}", execution_id);
                    }
                    
                    // 更新性能統計
                    Self::update_performance_stats(&performance_stats, pipeline_result).await;
                }
                Err(ref e) => {
                    error!("Pipeline執行失敗: {} - {}", execution_id, e);
                    
                    // 創建失敗結果記錄
                    let failed_result = PipelineExecutionResult {
                        pipeline_id: execution_id.clone(),
                        pipeline_type: "unknown".to_string(),
                        status: PipelineStatus::Failed,
                        success: false,
                        message: e.to_string(),
                        start_time: chrono::Utc::now(),
                        end_time: Some(chrono::Utc::now()),
                        duration_ms: None,
                        output_artifacts: Vec::new(),
                        metrics: HashMap::new(),
                        error_details: Some(e.to_string()),
                        resource_usage: ResourceUsage::default(),
                    };
                    
                    if let Ok(mut history) = timeout(Duration::from_secs(5), execution_history.write()).await {
                        history.push(failed_result);
                    }
                }
            }
            
            // 從活躍執行中移除
            if let Ok(mut active_exec) = timeout(Duration::from_secs(5), active_executions.write()).await {
                active_exec.remove(&execution_id);
            } else {
                warn!("移除活躍執行超時: {}", execution_id);
            }
        });
        
        Ok(())
    }
    
    /// 執行Pipeline
    async fn execute_pipeline(
        mut executor: Box<dyn PipelineExecutor>,
        mut context: PipelineContext,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) -> PipelineResult<PipelineExecutionResult> {
        let start_time = chrono::Utc::now();
        let execution_start = Instant::now();
        
        info!("開始執行Pipeline: {} ({})", context.config.name, context.execution_id);
        
        // 設置超時
        let timeout_duration = Duration::from_secs(context.config.timeout_seconds);
        
        // 執行Pipeline（帶超時）
        let result = timeout(
            timeout_duration,
            executor.execute(&mut context)
        ).await;
        
        let end_time = chrono::Utc::now();
        let duration = execution_start.elapsed();
        
        match result {
            Ok(Ok(pipeline_result)) => {
                info!("Pipeline執行成功: {} (耗時: {:?})", context.execution_id, duration);
                Ok(PipelineExecutionResult {
                    pipeline_id: context.pipeline_id,
                    pipeline_type: executor.pipeline_type(),
                    status: PipelineStatus::Completed,
                    success: true,
                    message: pipeline_result.message,
                    start_time,
                    end_time: Some(end_time),
                    duration_ms: Some(duration.as_millis() as u64),
                    output_artifacts: context.artifacts,
                    metrics: context.execution_metrics,
                    error_details: None,
                    resource_usage: ResourceUsage::default(), // 實際會測量
                })
            },
            Ok(Err(e)) => {
                error!("Pipeline執行失敗: {} - {}", context.execution_id, e);
                Ok(PipelineExecutionResult {
                    pipeline_id: context.pipeline_id,
                    pipeline_type: executor.pipeline_type(),
                    status: PipelineStatus::Failed,
                    success: false,
                    message: format!("執行失敗: {}", e),
                    start_time,
                    end_time: Some(end_time),
                    duration_ms: Some(duration.as_millis() as u64),
                    output_artifacts: context.artifacts,
                    metrics: context.execution_metrics,
                    error_details: Some(e.to_string()),
                    resource_usage: ResourceUsage::default(),
                })
            },
            Err(_) => {
                error!("Pipeline執行超時: {}", context.execution_id);
                Err(PipelineError::Execution {
                    source: ExecutionError::Timeout {
                        pipeline_id: context.execution_id.clone(),
                        timeout_seconds: context.config.timeout_seconds,
                    },
                    context: error_context!("execute_pipeline")
                        .with_execution_id(context.execution_id.clone())
                        .with_metadata("timeout_seconds".to_string(), context.config.timeout_seconds.to_string()),
                })
            }
        }
    }
    
    /// 更新性能統計
    async fn update_performance_stats(
        performance_stats: &Arc<RwLock<PerformanceStats>>,
        result: &PipelineExecutionResult,
    ) {
        // 使用超時防止死鎖
        let stats_lock_timeout = Duration::from_secs(5);
        if let Ok(mut stats) = timeout(stats_lock_timeout, performance_stats.write()).await {
            stats.total_executions += 1;
            if result.success {
                stats.successful_executions += 1;
            } else {
                stats.failed_executions += 1;
            }
            
            if let Some(duration_ms) = result.duration_ms {
                stats.average_duration_ms = (stats.average_duration_ms * (stats.total_executions - 1) as f64 + duration_ms as f64) / stats.total_executions as f64;
            }
            
            stats.total_memory_used_mb += result.resource_usage.memory_usage_mb;
        } else {
            warn!("更新性能統計超時");
        }
    }
    
    /// 獲取執行狀態
    pub async fn get_execution_status(&self, execution_id: &str) -> Option<PipelineStatus> {
        self.active_executions.read().await
            .get(execution_id)
            .map(|exec| exec.status.clone())
    }
    
    /// 獲取執行歷史
    pub async fn get_execution_history(&self, limit: Option<usize>) -> Vec<PipelineExecutionResult> {
        let history = self.execution_history.read().await;
        match limit {
            Some(n) => history.iter().rev().take(n).cloned().collect(),
            None => history.clone(),
        }
    }
    
    /// 獲取性能統計
    pub async fn get_performance_stats(&self) -> PerformanceStats {
        self.performance_stats.read().await.clone()
    }
    
    /// 取消執行
    pub async fn cancel_execution(&self, execution_id: &str) -> PipelineResult<()> {
        // 實際實現會需要協作式取消機制
        info!("取消Pipeline執行: {}", execution_id);
        
        let mut active_exec = match safe_write_lock!(&self.active_executions, LockOrder::PipelineState) {
            LockResult::Success(guard) => guard,
            LockResult::Timeout { waited_ms } => {
                return Err(PipelineError::Concurrency {
                    source: ConcurrencyError::LockTimeout {
                        lock_type: "active_executions".to_string(),
                        wait_seconds: waited_ms / 1000,
                    },
                    context: error_context!("cancel_execution")
                        .with_execution_id(execution_id.to_string()),
                });
            }
            LockResult::DeadlockDetected { involved_locks } => {
                return Err(PipelineError::Concurrency {
                    source: ConcurrencyError::DeadlockDetected {
                        thread_ids: vec![format!("{:?}", std::thread::current().id())],
                    },
                    context: error_context!("cancel_execution")
                        .with_execution_id(execution_id.to_string())
                        .with_metadata("involved_locks".to_string(), format!("{:?}", involved_locks)),
                });
            }
            LockResult::Error { message } => {
                return Err(PipelineError::Concurrency {
                    source: ConcurrencyError::SynchronizationFailed {
                        operation: "write_lock_active_executions".to_string(),
                        reason: message,
                    },
                    context: error_context!("cancel_execution")
                        .with_execution_id(execution_id.to_string()),
                });
            }
        };
        
        if let Some(execution) = active_exec.get_mut(execution_id) {
            execution.status = PipelineStatus::Cancelled;
            info!("Pipeline執行已標記為取消: {}", execution_id);
        } else {
            warn!("未找到活躍的Pipeline執行: {}", execution_id);
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    // 測試用的Pipeline執行器
    struct TestPipelineExecutor {
        pipeline_type: String,
        should_fail: bool,
        execution_time_ms: u64,
    }
    
    impl TestPipelineExecutor {
        fn new(pipeline_type: String) -> Self {
            Self {
                pipeline_type,
                should_fail: false,
                execution_time_ms: 100,
            }
        }
    }
    
    #[async_trait::async_trait]
    impl PipelineExecutor for TestPipelineExecutor {
        async fn execute(&mut self, context: &mut PipelineContext) -> PipelineResult<PipelineExecutionResult> {
            tokio::time::sleep(Duration::from_millis(self.execution_time_ms)).await;
            
            if self.should_fail {
                return Err(PipelineError::Execution {
                    source: ExecutionError::InitializationFailed {
                        pipeline_id: context.execution_id.clone(),
                        reason: "測試失敗".to_string(),
                    },
                    context: error_context!("execute").with_execution_id(context.execution_id.clone()),
                });
            }
            
            context.set_metric("test_metric".to_string(), 42.0);
            context.add_artifact("test_output.json".to_string());
            
            Ok(PipelineExecutionResult {
                pipeline_id: context.pipeline_id.clone(),
                pipeline_type: self.pipeline_type.clone(),
                status: PipelineStatus::Completed,
                success: true,
                message: "測試成功".to_string(),
                start_time: chrono::Utc::now(),
                end_time: Some(chrono::Utc::now()),
                duration_ms: Some(self.execution_time_ms),
                output_artifacts: context.artifacts.clone(),
                metrics: context.execution_metrics.clone(),
                error_details: None,
                resource_usage: ResourceUsage::default(),
            })
        }
        
        fn validate_config(&self, _config: &PipelineConfig) -> PipelineResult<()> {
            Ok(())
        }
        
        fn pipeline_type(&self) -> String {
            self.pipeline_type.clone()
        }
    }
    
    #[tokio::test]
    async fn test_pipeline_manager_creation() {
        let config = Config::default();
        let manager = PipelineManager::new(config);
        
        let stats = manager.get_performance_stats().await;
        assert_eq!(stats.total_executions, 0);
    }
    
    #[tokio::test]
    async fn test_pipeline_execution() {
        let config = Config::default();
        let manager = PipelineManager::new(config);
        
        let executor = Box::new(TestPipelineExecutor::new("test".to_string()));
        let pipeline_config = PipelineConfig {
            name: "測試Pipeline".to_string(),
            version: "1.0".to_string(),
            description: Some("測試描述".to_string()),
            priority: PipelinePriority::Medium,
            timeout_seconds: 60,
            max_retries: 3,
            resource_limits: ResourceLimits::default(),
            environment: HashMap::new(),
            output_directory: "/tmp/test".to_string(),
            verbose_logging: true,
            continue_on_failure: false,
        };
        
        let execution_id = manager.submit_pipeline(executor, pipeline_config).await.unwrap();
        
        // 等待執行完成
        tokio::time::sleep(Duration::from_millis(200)).await;
        
        let history = manager.get_execution_history(Some(1)).await;
        assert_eq!(history.len(), 1);
        assert!(history[0].success);
    }
}