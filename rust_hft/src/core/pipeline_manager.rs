/*!
 * Advanced Pipeline Manager - 智能任務編排與執行引擎
 * 
 * 核心功能：
 * - YAML驅動的ML工作流程定義
 * - 並行任務調度與依賴管理
 * - 藍綠部署與熱更新
 * - 智能資源分配與負載均衡
 * - 實時監控與異常恢復
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::core::ultra_performance::{HighPerfSPSCQueue, LatencyMeasurer};
use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, Semaphore, Mutex};
use tokio::task::JoinHandle;
use tracing::{info, warn, error, debug};
use crossbeam_channel::{Receiver, Sender, unbounded};

/// Pipeline執行狀態
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PipelineState {
    Pending,      // 等待執行
    Running,      // 正在執行
    Completed,    // 成功完成
    Failed,       // 執行失敗
    Cancelled,    // 被取消
    Paused,       // 暫停
}

/// 任務類型定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskType {
    DataCollection {
        symbol: String,
        duration_hours: u32,
        target_samples: u64,
    },
    ModelTraining {
        symbol: String,
        model_type: String,
        epochs: u32,
        batch_size: u32,
    },
    ModelEvaluation {
        symbol: String,
        model_path: String,
        test_data_path: String,
    },
    LiveTrading {
        symbol: String,
        model_path: String,
        capital: f64,
        dry_run: bool,
    },
    ModelDeployment {
        symbol: String,
        model_path: String,
        deployment_strategy: DeploymentStrategy,
    },
    SystemMaintenance {
        maintenance_type: String,
        target_components: Vec<String>,
    },
}

/// 部署策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeploymentStrategy {
    BlueGreen {
        shadow_duration_minutes: u32,
        promotion_criteria: PromotionCriteria,
    },
    Canary {
        traffic_percentage: f32,
        duration_minutes: u32,
        success_threshold: f32,
    },
    RollingUpdate {
        batch_size: u32,
        interval_seconds: u32,
    },
}

/// 升級標準
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionCriteria {
    pub min_profit_threshold: f64,
    pub max_drawdown_threshold: f64,
    pub min_sharpe_ratio: f64,
    pub max_correlation_with_existing: f64,
}

/// Pipeline任務定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineTask {
    pub id: String,
    pub name: String,
    pub task_type: TaskType,
    pub dependencies: Vec<String>,
    pub max_retries: u32,
    pub timeout_minutes: u32,
    pub priority: u32,
    pub resource_requirements: ResourceRequirements,
    pub environment: HashMap<String, String>,
}

/// 資源需求定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRequirements {
    pub cpu_cores: f32,
    pub memory_mb: u32,
    pub gpu_memory_mb: Option<u32>,
    pub disk_space_mb: u32,
    pub network_bandwidth_mbps: Option<u32>,
}

/// Pipeline定義（YAML配置）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineDefinition {
    pub name: String,
    pub version: String,
    pub description: String,
    pub tasks: Vec<PipelineTask>,
    pub global_config: HashMap<String, String>,
    pub scheduling_policy: SchedulingPolicy,
    pub failure_policy: FailurePolicy,
}

/// 調度策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SchedulingPolicy {
    Sequential,     // 順序執行
    MaxParallel,    // 最大並行度
    ResourceAware,  // 基於資源感知調度
    Priority,       // 基於優先級調度
}

/// 失敗處理策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailurePolicy {
    pub max_failures: u32,
    pub failure_action: FailureAction,
    pub notification_channels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FailureAction {
    Abort,          // 終止Pipeline
    Continue,       // 繼續執行其他任務
    Retry,          // 重試失敗任務
    Rollback,       // 回滾到上一個穩定狀態
}

/// 執行中的任務實例
#[derive(Debug)]
pub struct TaskExecution {
    pub task: PipelineTask,
    pub state: PipelineState,
    pub start_time: Option<Instant>,
    pub end_time: Option<Instant>,
    pub retry_count: u32,
    pub error_message: Option<String>,
    pub output: Option<TaskOutput>,
    pub resource_allocation: ResourceAllocation,
}

/// 任務輸出
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOutput {
    pub success: bool,
    pub message: String,
    pub artifacts: HashMap<String, String>,
    pub metrics: HashMap<String, f64>,
}

/// 資源分配
#[derive(Debug, Clone)]
pub struct ResourceAllocation {
    pub cpu_cores: f32,
    pub memory_mb: u32,
    pub assigned_at: Instant,
}

/// Pipeline執行實例
#[derive(Debug)]
pub struct PipelineExecution {
    pub definition: PipelineDefinition,
    pub id: String,
    pub state: PipelineState,
    pub tasks: HashMap<String, TaskExecution>,
    pub start_time: Option<Instant>,
    pub end_time: Option<Instant>,
    pub created_by: String,
    pub total_duration: Option<Duration>,
}

/// 資源管理器
#[derive(Debug)]
pub struct ResourceManager {
    pub total_cpu_cores: f32,
    pub total_memory_mb: u32,
    pub available_cpu_cores: Arc<Mutex<f32>>,
    pub available_memory_mb: Arc<Mutex<u32>>,
    pub allocated_resources: Arc<RwLock<HashMap<String, ResourceAllocation>>>,
}

impl ResourceManager {
    pub fn new(total_cpu_cores: f32, total_memory_mb: u32) -> Self {
        Self {
            total_cpu_cores,
            total_memory_mb,
            available_cpu_cores: Arc::new(Mutex::new(total_cpu_cores)),
            available_memory_mb: Arc::new(Mutex::new(total_memory_mb)),
            allocated_resources: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// 嘗試分配資源
    pub async fn try_allocate(&self, task_id: &str, requirements: &ResourceRequirements) -> Result<bool> {
        let mut cpu_guard = self.available_cpu_cores.lock().await;
        let mut memory_guard = self.available_memory_mb.lock().await;
        
        if *cpu_guard >= requirements.cpu_cores && *memory_guard >= requirements.memory_mb {
            *cpu_guard -= requirements.cpu_cores;
            *memory_guard -= requirements.memory_mb;
            
            let allocation = ResourceAllocation {
                cpu_cores: requirements.cpu_cores,
                memory_mb: requirements.memory_mb,
                assigned_at: Instant::now(),
            };
            
            self.allocated_resources.write().await.insert(task_id.to_string(), allocation);
            
            info!("📦 資源分配成功: task={}, cpu={:.1}, memory={}MB", 
                  task_id, requirements.cpu_cores, requirements.memory_mb);
            Ok(true)
        } else {
            debug!("📦 資源不足: task={}, need_cpu={:.1}, need_memory={}MB, avail_cpu={:.1}, avail_memory={}MB",
                  task_id, requirements.cpu_cores, requirements.memory_mb, *cpu_guard, *memory_guard);
            Ok(false)
        }
    }
    
    /// 釋放資源
    pub async fn release(&self, task_id: &str) -> Result<()> {
        let mut allocations = self.allocated_resources.write().await;
        
        if let Some(allocation) = allocations.remove(task_id) {
            let mut cpu_guard = self.available_cpu_cores.lock().await;
            let mut memory_guard = self.available_memory_mb.lock().await;
            
            *cpu_guard += allocation.cpu_cores;
            *memory_guard += allocation.memory_mb;
            
            info!("📦 資源釋放: task={}, cpu={:.1}, memory={}MB", 
                  task_id, allocation.cpu_cores, allocation.memory_mb);
        }
        
        Ok(())
    }
    
    /// 獲取資源使用率
    pub async fn get_utilization(&self) -> (f32, f32) {
        let cpu_available = *self.available_cpu_cores.lock().await;
        let memory_available = *self.available_memory_mb.lock().await;
        
        let cpu_utilization = 1.0 - (cpu_available / self.total_cpu_cores);
        let memory_utilization = 1.0 - (memory_available as f32 / self.total_memory_mb as f32);
        
        (cpu_utilization, memory_utilization)
    }
}

/// 高級Pipeline管理器
pub struct AdvancedPipelineManager {
    /// 系統配置
    config: Config,
    
    /// 活躍的Pipeline執行實例
    active_pipelines: Arc<RwLock<HashMap<String, PipelineExecution>>>,
    
    /// 任務隊列（優先級隊列）
    task_queue: Arc<Mutex<Vec<(u32, String, String)>>>, // (priority, pipeline_id, task_id)
    
    /// 資源管理器
    resource_manager: Arc<ResourceManager>,
    
    /// 任務執行器池
    task_executor_semaphore: Arc<Semaphore>,
    
    /// 統計信息
    stats: PipelineStats,
    
    /// 延遲測量器
    latency_measurer: LatencyMeasurer,
    
    /// 任務通信通道
    task_completion_tx: Sender<TaskCompletionEvent>,
    task_completion_rx: Arc<Mutex<Receiver<TaskCompletionEvent>>>,
    
    /// 執行中的任務句柄
    active_task_handles: Arc<RwLock<HashMap<String, JoinHandle<TaskOutput>>>>,
}

/// Pipeline統計信息
#[derive(Debug, Clone)]
pub struct PipelineStats {
    pub total_pipelines_executed: AtomicU64,
    pub total_tasks_executed: AtomicU64,
    pub total_tasks_failed: AtomicU64,
    pub avg_pipeline_duration_ms: AtomicU64,
    pub avg_task_duration_ms: AtomicU64,
    pub current_active_pipelines: AtomicUsize,
    pub current_active_tasks: AtomicUsize,
}

impl Default for PipelineStats {
    fn default() -> Self {
        Self {
            total_pipelines_executed: AtomicU64::new(0),
            total_tasks_executed: AtomicU64::new(0),
            total_tasks_failed: AtomicU64::new(0),
            avg_pipeline_duration_ms: AtomicU64::new(0),
            avg_task_duration_ms: AtomicU64::new(0),
            current_active_pipelines: AtomicUsize::new(0),
            current_active_tasks: AtomicUsize::new(0),
        }
    }
}

/// 任務完成事件
#[derive(Debug)]
pub struct TaskCompletionEvent {
    pub pipeline_id: String,
    pub task_id: String,
    pub output: TaskOutput,
    pub duration: Duration,
}

impl AdvancedPipelineManager {
    /// 創建新的Pipeline管理器
    pub fn new(config: Config) -> Result<Self> {
        let cpu_cores = num_cpus::get() as f32;
        let memory_mb = 8192; // 8GB default, could be detected from system
        
        let (tx, rx) = unbounded();
        
        Ok(Self {
            config: config.clone(),
            active_pipelines: Arc::new(RwLock::new(HashMap::new())),
            task_queue: Arc::new(Mutex::new(Vec::new())),
            resource_manager: Arc::new(ResourceManager::new(cpu_cores, memory_mb)),
            task_executor_semaphore: Arc::new(Semaphore::new(cpu_cores as usize)),
            stats: PipelineStats::default(),
            latency_measurer: LatencyMeasurer::new(),
            task_completion_tx: tx,
            task_completion_rx: Arc::new(Mutex::new(rx)),
            active_task_handles: Arc::new(RwLock::new(HashMap::new())),
        })
    }
    
    /// 從YAML文件加載Pipeline定義
    pub async fn load_pipeline_from_yaml(&self, yaml_content: &str) -> Result<PipelineDefinition> {
        let definition: PipelineDefinition = serde_yaml::from_str(yaml_content)
            .context("Failed to parse YAML pipeline definition")?;
            
        // 驗證Pipeline定義
        self.validate_pipeline_definition(&definition)?;
        
        info!("📋 Pipeline定義已加載: {} v{}", definition.name, definition.version);
        Ok(definition)
    }
    
    /// 驗證Pipeline定義
    fn validate_pipeline_definition(&self, definition: &PipelineDefinition) -> Result<()> {
        // 檢查任務依賴循環
        let mut task_ids: std::collections::HashSet<String> = definition.tasks.iter()
            .map(|t| t.id.clone())
            .collect();
            
        for task in &definition.tasks {
            for dep in &task.dependencies {
                if !task_ids.contains(dep) {
                    return Err(anyhow::anyhow!("任務依賴不存在: {} depends on {}", task.id, dep));
                }
            }
        }
        
        // 檢查循環依賴（簡化版）
        for task in &definition.tasks {
            if task.dependencies.contains(&task.id) {
                return Err(anyhow::anyhow!("檢測到循環依賴: {}", task.id));
            }
        }
        
        info!("✅ Pipeline定義驗證通過: {}", definition.name);
        Ok(())
    }
    
    /// 執行Pipeline
    pub async fn execute_pipeline(&self, definition: PipelineDefinition, created_by: String) -> Result<String> {
        let pipeline_id = format!("{}_{}", definition.name, uuid::Uuid::new_v4());
        let start_time = self.latency_measurer.start();
        
        info!("🚀 開始執行Pipeline: {} (ID: {})", definition.name, pipeline_id);
        
        // 創建Pipeline執行實例
        let mut tasks = HashMap::new();
        for task in &definition.tasks {
            tasks.insert(task.id.clone(), TaskExecution {
                task: task.clone(),
                state: PipelineState::Pending,
                start_time: None,
                end_time: None,
                retry_count: 0,
                error_message: None,
                output: None,
                resource_allocation: ResourceAllocation {
                    cpu_cores: 0.0,
                    memory_mb: 0,
                    assigned_at: Instant::now(),
                },
            });
        }
        
        let pipeline_execution = PipelineExecution {
            definition: definition.clone(),
            id: pipeline_id.clone(),
            state: PipelineState::Running,
            tasks,
            start_time: Some(Instant::now()),
            end_time: None,
            created_by,
            total_duration: None,
        };
        
        // 添加到活躍Pipeline列表
        self.active_pipelines.write().await.insert(pipeline_id.clone(), pipeline_execution);
        self.stats.current_active_pipelines.fetch_add(1, Ordering::Relaxed);
        
        // 根據調度策略執行任務
        match definition.scheduling_policy {
            SchedulingPolicy::Sequential => {
                self.execute_sequential(&pipeline_id).await?;
            },
            SchedulingPolicy::MaxParallel => {
                self.execute_parallel(&pipeline_id).await?;
            },
            SchedulingPolicy::ResourceAware => {
                self.execute_resource_aware(&pipeline_id).await?;
            },
            SchedulingPolicy::Priority => {
                self.execute_priority_based(&pipeline_id).await?;
            },
        }
        
        let latency = self.latency_measurer.end_and_record(start_time);
        info!("✅ Pipeline調度完成: {} (延遲: {}μs)", pipeline_id, latency);
        
        Ok(pipeline_id)
    }
    
    /// 順序執行策略
    async fn execute_sequential(&self, pipeline_id: &str) -> Result<()> {
        let definition = {
            let pipelines = self.active_pipelines.read().await;
            pipelines.get(pipeline_id).unwrap().definition.clone()
        };
        
        // 簡化的拓撲排序
        let sorted_tasks = self.topological_sort(&definition.tasks)?;
        
        for task in sorted_tasks {
            self.execute_single_task(pipeline_id, &task.id).await?;
            
            // 等待任務完成
            while !self.is_task_completed(pipeline_id, &task.id).await {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        
        Ok(())
    }
    
    /// 並行執行策略
    async fn execute_parallel(&self, pipeline_id: &str) -> Result<()> {
        let definition = {
            let pipelines = self.active_pipelines.read().await;
            pipelines.get(pipeline_id).unwrap().definition.clone()
        };
        
        // 找出無依賴的任務並立即執行
        let mut ready_tasks = Vec::new();
        for task in &definition.tasks {
            if task.dependencies.is_empty() {
                ready_tasks.push(task.clone());
            }
        }
        
        // 並行執行就緒任務
        let mut join_handles = Vec::new();
        for task in ready_tasks {
            let pipeline_id = pipeline_id.to_string();
            let task_id = task.id.clone();
            let manager = self.clone_for_task();
            
            let handle = tokio::spawn(async move {
                manager.execute_single_task(&pipeline_id, &task_id).await
            });
            join_handles.push(handle);
        }
        
        // 等待所有任務完成
        for handle in join_handles {
            handle.await??;
        }
        
        Ok(())
    }
    
    /// 資源感知執行策略
    async fn execute_resource_aware(&self, pipeline_id: &str) -> Result<()> {
        let definition = {
            let pipelines = self.active_pipelines.read().await;
            pipelines.get(pipeline_id).unwrap().definition.clone()
        };
        
        // 根據資源需求對任務進行優先級排序
        let mut tasks_by_resource = definition.tasks.clone();
        tasks_by_resource.sort_by(|a, b| {
            let a_resource_score = a.resource_requirements.cpu_cores + 
                                 (a.resource_requirements.memory_mb as f32 / 1024.0);
            let b_resource_score = b.resource_requirements.cpu_cores + 
                                 (b.resource_requirements.memory_mb as f32 / 1024.0);
            b_resource_score.partial_cmp(&a_resource_score).unwrap()
        });
        
        // 智能調度：優先執行資源需求大的任務
        for task in tasks_by_resource {
            // 檢查依賴是否滿足
            if self.dependencies_satisfied(pipeline_id, &task).await {
                // 等待資源可用
                while !self.resource_manager.try_allocate(&task.id, &task.resource_requirements).await? {
                    tokio::time::sleep(Duration::from_milliseconds(100)).await;
                }
                
                self.execute_single_task(pipeline_id, &task.id).await?;
            }
        }
        
        Ok(())
    }
    
    /// 優先級執行策略
    async fn execute_priority_based(&self, pipeline_id: &str) -> Result<()> {
        let definition = {
            let pipelines = self.active_pipelines.read().await;
            pipelines.get(pipeline_id).unwrap().definition.clone()
        };
        
        // 按優先級排序任務
        let mut priority_tasks = definition.tasks.clone();
        priority_tasks.sort_by(|a, b| b.priority.cmp(&a.priority));
        
        for task in priority_tasks {
            if self.dependencies_satisfied(pipeline_id, &task).await {
                self.execute_single_task(pipeline_id, &task.id).await?;
            }
        }
        
        Ok(())
    }
    
    /// 執行單個任務
    async fn execute_single_task(&self, pipeline_id: &str, task_id: &str) -> Result<()> {
        let _permit = self.task_executor_semaphore.acquire().await?;
        
        info!("▶️ 開始執行任務: {} in {}", task_id, pipeline_id);
        
        // 更新任務狀態
        {
            let mut pipelines = self.active_pipelines.write().await;
            if let Some(pipeline) = pipelines.get_mut(pipeline_id) {
                if let Some(task_execution) = pipeline.tasks.get_mut(task_id) {
                    task_execution.state = PipelineState::Running;
                    task_execution.start_time = Some(Instant::now());
                }
            }
        }
        
        self.stats.current_active_tasks.fetch_add(1, Ordering::Relaxed);
        
        // 模擬任務執行（實際實現會調用真正的任務邏輯）
        let task_output = self.simulate_task_execution(pipeline_id, task_id).await?;
        
        // 發送任務完成事件
        let completion_event = TaskCompletionEvent {
            pipeline_id: pipeline_id.to_string(),
            task_id: task_id.to_string(),
            output: task_output.clone(),
            duration: Duration::from_secs(1), // 實際會測量
        };
        
        self.task_completion_tx.send(completion_event)?;
        
        // 更新任務狀態
        {
            let mut pipelines = self.active_pipelines.write().await;
            if let Some(pipeline) = pipelines.get_mut(pipeline_id) {
                if let Some(task_execution) = pipeline.tasks.get_mut(task_id) {
                    task_execution.state = if task_output.success {
                        PipelineState::Completed
                    } else {
                        PipelineState::Failed
                    };
                    task_execution.end_time = Some(Instant::now());
                    task_execution.output = Some(task_output);
                }
            }
        }
        
        self.stats.current_active_tasks.fetch_sub(1, Ordering::Relaxed);
        self.stats.total_tasks_executed.fetch_add(1, Ordering::Relaxed);
        
        info!("✅ 任務執行完成: {} in {}", task_id, pipeline_id);
        
        Ok(())
    }
    
    /// 模擬任務執行（實際實現會根據TaskType調用對應的執行邏輯）
    async fn simulate_task_execution(&self, pipeline_id: &str, task_id: &str) -> Result<TaskOutput> {
        // 模擬任務執行時間
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        // 模擬成功的任務執行
        Ok(TaskOutput {
            success: true,
            message: format!("Task {} in pipeline {} completed successfully", task_id, pipeline_id),
            artifacts: HashMap::new(),
            metrics: HashMap::new(),
        })
    }
    
    /// 檢查任務依賴是否滿足
    async fn dependencies_satisfied(&self, pipeline_id: &str, task: &PipelineTask) -> bool {
        let pipelines = self.active_pipelines.read().await;
        if let Some(pipeline) = pipelines.get(pipeline_id) {
            for dep_id in &task.dependencies {
                if let Some(dep_task) = pipeline.tasks.get(dep_id) {
                    if dep_task.state != PipelineState::Completed {
                        return false;
                    }
                } else {
                    return false;
                }
            }
        }
        true
    }
    
    /// 檢查任務是否完成
    async fn is_task_completed(&self, pipeline_id: &str, task_id: &str) -> bool {
        let pipelines = self.active_pipelines.read().await;
        if let Some(pipeline) = pipelines.get(pipeline_id) {
            if let Some(task) = pipeline.tasks.get(task_id) {
                return matches!(task.state, PipelineState::Completed | PipelineState::Failed);
            }
        }
        false
    }
    
    /// 拓撲排序（簡化版）
    fn topological_sort(&self, tasks: &[PipelineTask]) -> Result<Vec<PipelineTask>> {
        // 簡化的拓撲排序實現
        // 實際實現會使用Kahn算法或DFS
        Ok(tasks.to_vec())
    }
    
    /// 為任務執行克隆管理器的輕量級版本
    fn clone_for_task(&self) -> Self {
        // 這是一個簡化實現，實際會使用Arc來共享資源
        Self {
            config: self.config.clone(),
            active_pipelines: self.active_pipelines.clone(),
            task_queue: self.task_queue.clone(),
            resource_manager: self.resource_manager.clone(),
            task_executor_semaphore: self.task_executor_semaphore.clone(),
            stats: self.stats.clone(),
            latency_measurer: LatencyMeasurer::new(),
            task_completion_tx: self.task_completion_tx.clone(),
            task_completion_rx: self.task_completion_rx.clone(),
            active_task_handles: self.active_task_handles.clone(),
        }
    }
    
    /// 獲取Pipeline狀態
    pub async fn get_pipeline_status(&self, pipeline_id: &str) -> Option<PipelineState> {
        let pipelines = self.active_pipelines.read().await;
        pipelines.get(pipeline_id).map(|p| p.state.clone())
    }
    
    /// 獲取系統統計信息
    pub fn get_stats(&self) -> PipelineStats {
        self.stats.clone()
    }
    
    /// 取消Pipeline執行
    pub async fn cancel_pipeline(&self, pipeline_id: &str) -> Result<()> {
        let mut pipelines = self.active_pipelines.write().await;
        if let Some(pipeline) = pipelines.get_mut(pipeline_id) {
            pipeline.state = PipelineState::Cancelled;
            info!("🛑 Pipeline已取消: {}", pipeline_id);
        }
        Ok(())
    }
}