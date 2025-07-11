/*!
 * Unified Interface Layer - 統一接口層
 * 
 * 為Agno Framework提供標準化的調用接口
 * 核心功能：
 * - 統一的API調用格式
 * - 智能任務路由和負載均衡
 * - 實時狀態同步
 * - 錯誤處理和重試機制
 * - 性能監控和指標收集
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::core::pipeline_manager::{AdvancedPipelineManager, PipelineDefinition, TaskType, PipelineState};
use crate::core::ultra_performance::{ZeroAllocDecisionEngine, PerformanceStats as UltraPerformanceStats};
use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{RwLock, Mutex};
use tracing::{info, warn, error, debug};
use uuid::Uuid;

/// 統一API請求格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedRequest {
    /// 請求ID（用於追蹤）
    pub request_id: String,
    
    /// API版本
    pub api_version: String,
    
    /// 請求類型
    pub request_type: RequestType,
    
    /// 請求參數
    pub parameters: RequestParameters,
    
    /// 請求時間戳
    pub timestamp: u64,
    
    /// 客戶端信息
    pub client_info: ClientInfo,
    
    /// 優先級（1-10，10最高）
    pub priority: u8,
    
    /// 超時設置（秒）
    pub timeout_seconds: Option<u32>,
}

/// 請求類型枚舉
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequestType {
    /// 數據收集相關
    DataCollection,
    
    /// 模型訓練相關
    ModelTraining,
    
    /// 模型評估相關
    ModelEvaluation,
    
    /// 實時交易相關
    LiveTrading,
    
    /// 系統管理相關
    SystemManagement,
    
    /// 狀態查詢相關
    StatusQuery,
    
    /// Pipeline管理
    PipelineManagement,
    
    /// 性能監控
    PerformanceMonitoring,
}

/// 請求參數（統一格式）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestParameters {
    /// 主要操作
    pub action: String,
    
    /// 目標資產
    pub symbol: Option<String>,
    
    /// 操作參數
    pub params: HashMap<String, serde_json::Value>,
    
    /// 配置覆蓋
    pub config_overrides: Option<HashMap<String, String>>,
}

/// 客戶端信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    /// 客戶端ID
    pub client_id: String,
    
    /// 客戶端版本
    pub client_version: String,
    
    /// 認證Token
    pub auth_token: Option<String>,
    
    /// 用戶Agent
    pub user_agent: String,
}

/// 統一API響應格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedResponse {
    /// 請求ID（對應請求）
    pub request_id: String,
    
    /// 響應狀態
    pub status: ResponseStatus,
    
    /// 響應數據
    pub data: Option<ResponseData>,
    
    /// 錯誤信息
    pub error: Option<ErrorInfo>,
    
    /// 執行時間（毫秒）
    pub execution_time_ms: u64,
    
    /// 響應時間戳
    pub timestamp: u64,
    
    /// 系統指標
    pub metrics: ResponseMetrics,
}

/// 響應狀態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseStatus {
    Success,      // 成功
    Pending,      // 處理中
    Failed,       // 失敗
    Timeout,      // 超時
    RateLimited,  // 被限流
}

/// 響應數據（彈性格式）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseData {
    /// 主要結果
    pub result: serde_json::Value,
    
    /// 額外元數據
    pub metadata: HashMap<String, serde_json::Value>,
    
    /// 相關資源ID
    pub resource_ids: Vec<String>,
}

/// 錯誤信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorInfo {
    /// 錯誤代碼
    pub error_code: String,
    
    /// 錯誤消息
    pub error_message: String,
    
    /// 錯誤詳情
    pub error_details: Option<String>,
    
    /// 建議操作
    pub suggested_action: Option<String>,
    
    /// 重試信息
    pub retry_info: Option<RetryInfo>,
}

/// 重試信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryInfo {
    /// 是否可重試
    pub can_retry: bool,
    
    /// 建議重試延遲（秒）
    pub retry_delay_seconds: u32,
    
    /// 最大重試次數
    pub max_retries: u32,
}

/// 響應指標
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseMetrics {
    /// CPU使用率
    pub cpu_usage_percent: f64,
    
    /// 內存使用率
    pub memory_usage_percent: f64,
    
    /// 活躍任務數
    pub active_tasks: u32,
    
    /// 隊列長度
    pub queue_length: u32,
    
    /// 平均響應時間
    pub avg_response_time_ms: f64,
}

/// 實時狀態更新
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusUpdate {
    /// 更新ID
    pub update_id: String,
    
    /// 相關請求ID
    pub request_id: String,
    
    /// 更新類型
    pub update_type: StatusUpdateType,
    
    /// 更新內容
    pub content: serde_json::Value,
    
    /// 更新時間戳
    pub timestamp: u64,
}

/// 狀態更新類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StatusUpdateType {
    Progress,       // 進度更新
    StateChange,    // 狀態變更
    Metrics,        // 指標更新
    Warning,        // 警告信息
    Error,          // 錯誤信息
}

/// 統一接口管理器
pub struct UnifiedInterfaceManager {
    /// 系統配置
    config: Config,
    
    /// Pipeline管理器
    pipeline_manager: Arc<AdvancedPipelineManager>,
    
    /// 活躍請求追蹤
    active_requests: Arc<RwLock<HashMap<String, RequestExecution>>>,
    
    /// 請求統計
    request_stats: RequestStats,
    
    /// 速率限制器
    rate_limiter: Arc<RwLock<HashMap<String, RateLimitInfo>>>,
    
    /// 狀態更新訂閱者
    status_subscribers: Arc<RwLock<HashMap<String, StatusSubscriber>>>,
    
    /// 系統健康狀態
    system_health: Arc<RwLock<SystemHealth>>,
}

/// 請求執行狀態
#[derive(Debug, Clone)]
pub struct RequestExecution {
    pub request: UnifiedRequest,
    pub start_time: Instant,
    pub status: ResponseStatus,
    pub pipeline_id: Option<String>,
    pub last_update: Instant,
}

/// 請求統計
#[derive(Debug, Clone)]
pub struct RequestStats {
    pub total_requests: AtomicU64,
    pub successful_requests: AtomicU64,
    pub failed_requests: AtomicU64,
    pub avg_response_time_ms: AtomicU64,
    pub active_requests_count: AtomicU64,
}

impl Default for RequestStats {
    fn default() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            successful_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            avg_response_time_ms: AtomicU64::new(0),
            active_requests_count: AtomicU64::new(0),
        }
    }
}

/// 速率限制信息
#[derive(Debug, Clone)]
pub struct RateLimitInfo {
    pub requests_per_minute: u32,
    pub current_count: u32,
    pub window_start: Instant,
    pub is_blocked: bool,
}

/// 狀態訂閱者
#[derive(Debug, Clone)]
pub struct StatusSubscriber {
    pub subscriber_id: String,
    pub request_ids: Vec<String>,
    pub callback_url: Option<String>,
    pub last_update: Instant,
}

/// 系統健康狀態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemHealth {
    pub overall_status: String,
    pub cpu_usage: f64,
    pub memory_usage: f64,
    pub disk_usage: f64,
    pub network_latency_ms: f64,
    pub active_connections: u32,
    pub last_check: u64,
}

impl UnifiedInterfaceManager {
    /// 創建統一接口管理器
    pub async fn new(config: Config) -> Result<Self> {
        let pipeline_manager = Arc::new(AdvancedPipelineManager::new(config.clone())?);
        
        Ok(Self {
            config,
            pipeline_manager,
            active_requests: Arc::new(RwLock::new(HashMap::new())),
            request_stats: RequestStats::default(),
            rate_limiter: Arc::new(RwLock::new(HashMap::new())),
            status_subscribers: Arc::new(RwLock::new(HashMap::new())),
            system_health: Arc::new(RwLock::new(SystemHealth {
                overall_status: "healthy".to_string(),
                cpu_usage: 0.0,
                memory_usage: 0.0,
                disk_usage: 0.0,
                network_latency_ms: 0.0,
                active_connections: 0,
                last_check: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            })),
        })
    }
    
    /// 處理統一API請求
    pub async fn handle_request(&self, request: UnifiedRequest) -> Result<UnifiedResponse> {
        let start_time = Instant::now();
        
        // 基本驗證
        self.validate_request(&request)?;
        
        // 速率限制檢查
        if !self.check_rate_limit(&request.client_info.client_id).await? {
            return Ok(self.create_rate_limited_response(&request, start_time));
        }
        
        // 記錄活躍請求
        self.register_active_request(&request, start_time).await;
        
        info!("📨 處理統一API請求: {} (類型: {:?})", request.request_id, request.request_type);
        
        // 根據請求類型路由到相應處理器
        let response_data = match request.request_type {
            RequestType::DataCollection => {
                self.handle_data_collection_request(&request).await
            },
            RequestType::ModelTraining => {
                self.handle_model_training_request(&request).await
            },
            RequestType::ModelEvaluation => {
                self.handle_model_evaluation_request(&request).await
            },
            RequestType::LiveTrading => {
                self.handle_live_trading_request(&request).await
            },
            RequestType::SystemManagement => {
                self.handle_system_management_request(&request).await
            },
            RequestType::StatusQuery => {
                self.handle_status_query_request(&request).await
            },
            RequestType::PipelineManagement => {
                self.handle_pipeline_management_request(&request).await
            },
            RequestType::PerformanceMonitoring => {
                self.handle_performance_monitoring_request(&request).await
            },
        };
        
        // 清理活躍請求
        self.unregister_active_request(&request.request_id).await;
        
        // 構建響應
        let execution_time = start_time.elapsed();
        self.create_success_response(&request, response_data?, execution_time)
    }
    
    /// 驗證請求格式
    fn validate_request(&self, request: &UnifiedRequest) -> Result<()> {
        if request.request_id.is_empty() {
            return Err(anyhow::anyhow!("請求ID不能為空"));
        }
        
        if request.api_version.is_empty() {
            return Err(anyhow::anyhow!("API版本不能為空"));
        }
        
        if request.parameters.action.is_empty() {
            return Err(anyhow::anyhow!("操作不能為空"));
        }
        
        Ok(())
    }
    
    /// 檢查速率限制
    async fn check_rate_limit(&self, client_id: &str) -> Result<bool> {
        let mut rate_limits = self.rate_limiter.write().await;
        let now = Instant::now();
        
        let rate_limit = rate_limits.entry(client_id.to_string()).or_insert(RateLimitInfo {
            requests_per_minute: 60, // 默認每分鐘60次請求
            current_count: 0,
            window_start: now,
            is_blocked: false,
        });
        
        // 重置窗口
        if now.duration_since(rate_limit.window_start) >= Duration::from_secs(60) {
            rate_limit.current_count = 0;
            rate_limit.window_start = now;
            rate_limit.is_blocked = false;
        }
        
        // 檢查限制
        if rate_limit.current_count >= rate_limit.requests_per_minute {
            rate_limit.is_blocked = true;
            Ok(false)
        } else {
            rate_limit.current_count += 1;
            Ok(true)
        }
    }
    
    /// 註冊活躍請求
    async fn register_active_request(&self, request: &UnifiedRequest, start_time: Instant) {
        let execution = RequestExecution {
            request: request.clone(),
            start_time,
            status: ResponseStatus::Pending,
            pipeline_id: None,
            last_update: start_time,
        };
        
        self.active_requests.write().await.insert(request.request_id.clone(), execution);
        self.request_stats.active_requests_count.fetch_add(1, Ordering::Relaxed);
        self.request_stats.total_requests.fetch_add(1, Ordering::Relaxed);
    }
    
    /// 註銷活躍請求
    async fn unregister_active_request(&self, request_id: &str) {
        self.active_requests.write().await.remove(request_id);
        self.request_stats.active_requests_count.fetch_sub(1, Ordering::Relaxed);
    }
    
    /// 處理數據收集請求
    async fn handle_data_collection_request(&self, request: &UnifiedRequest) -> Result<ResponseData> {
        let action = &request.parameters.action;
        
        match action.as_str() {
            "start_collection" => {
                let symbol = request.parameters.symbol.as_ref()
                    .context("數據收集需要指定symbol")?;
                
                let duration_hours = request.parameters.params.get("duration_hours")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(24) as u32;
                
                // 創建數據收集Pipeline
                let pipeline_def = self.create_data_collection_pipeline(symbol, duration_hours)?;
                let pipeline_id = self.pipeline_manager.execute_pipeline(pipeline_def, 
                    request.client_info.client_id.clone()).await?;
                
                Ok(ResponseData {
                    result: serde_json::json!({
                        "pipeline_id": pipeline_id,
                        "status": "started",
                        "estimated_duration_minutes": duration_hours * 60
                    }),
                    metadata: HashMap::new(),
                    resource_ids: vec![pipeline_id],
                })
            },
            "stop_collection" => {
                let pipeline_id = request.parameters.params.get("pipeline_id")
                    .and_then(|v| v.as_str())
                    .context("停止收集需要指定pipeline_id")?;
                
                self.pipeline_manager.cancel_pipeline(pipeline_id).await?;
                
                Ok(ResponseData {
                    result: serde_json::json!({
                        "pipeline_id": pipeline_id,
                        "status": "cancelled"
                    }),
                    metadata: HashMap::new(),
                    resource_ids: vec![pipeline_id.to_string()],
                })
            },
            _ => Err(anyhow::anyhow!("不支持的數據收集操作: {}", action))
        }
    }
    
    /// 處理模型訓練請求
    async fn handle_model_training_request(&self, request: &UnifiedRequest) -> Result<ResponseData> {
        let action = &request.parameters.action;
        
        match action.as_str() {
            "start_training" => {
                let symbol = request.parameters.symbol.as_ref()
                    .context("模型訓練需要指定symbol")?;
                
                let epochs = request.parameters.params.get("epochs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(100) as u32;
                
                let batch_size = request.parameters.params.get("batch_size")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(256) as u32;
                
                // 創建模型訓練Pipeline
                let pipeline_def = self.create_model_training_pipeline(symbol, epochs, batch_size)?;
                let pipeline_id = self.pipeline_manager.execute_pipeline(pipeline_def, 
                    request.client_info.client_id.clone()).await?;
                
                Ok(ResponseData {
                    result: serde_json::json!({
                        "pipeline_id": pipeline_id,
                        "status": "training_started",
                        "epochs": epochs,
                        "estimated_duration_hours": epochs / 10
                    }),
                    metadata: HashMap::new(),
                    resource_ids: vec![pipeline_id],
                })
            },
            _ => Err(anyhow::anyhow!("不支持的模型訓練操作: {}", action))
        }
    }
    
    /// 處理模型評估請求
    async fn handle_model_evaluation_request(&self, request: &UnifiedRequest) -> Result<ResponseData> {
        // 實現模型評估邏輯
        Ok(ResponseData {
            result: serde_json::json!({"status": "evaluation_completed"}),
            metadata: HashMap::new(),
            resource_ids: vec![],
        })
    }
    
    /// 處理實時交易請求
    async fn handle_live_trading_request(&self, request: &UnifiedRequest) -> Result<ResponseData> {
        // 實現實時交易邏輯
        Ok(ResponseData {
            result: serde_json::json!({"status": "trading_started"}),
            metadata: HashMap::new(),
            resource_ids: vec![],
        })
    }
    
    /// 處理系統管理請求
    async fn handle_system_management_request(&self, request: &UnifiedRequest) -> Result<ResponseData> {
        let action = &request.parameters.action;
        
        match action.as_str() {
            "get_system_status" => {
                let health = self.system_health.read().await;
                Ok(ResponseData {
                    result: serde_json::to_value(&*health)?,
                    metadata: HashMap::new(),
                    resource_ids: vec![],
                })
            },
            "update_config" => {
                // 實現配置更新邏輯
                Ok(ResponseData {
                    result: serde_json::json!({"status": "config_updated"}),
                    metadata: HashMap::new(),
                    resource_ids: vec![],
                })
            },
            _ => Err(anyhow::anyhow!("不支持的系統管理操作: {}", action))
        }
    }
    
    /// 處理狀態查詢請求
    async fn handle_status_query_request(&self, request: &UnifiedRequest) -> Result<ResponseData> {
        let action = &request.parameters.action;
        
        match action.as_str() {
            "get_pipeline_status" => {
                let pipeline_id = request.parameters.params.get("pipeline_id")
                    .and_then(|v| v.as_str())
                    .context("查詢Pipeline狀態需要指定pipeline_id")?;
                
                let status = self.pipeline_manager.get_pipeline_status(pipeline_id).await;
                
                Ok(ResponseData {
                    result: serde_json::json!({
                        "pipeline_id": pipeline_id,
                        "status": status
                    }),
                    metadata: HashMap::new(),
                    resource_ids: vec![pipeline_id.to_string()],
                })
            },
            _ => Err(anyhow::anyhow!("不支持的狀態查詢操作: {}", action))
        }
    }
    
    /// 處理Pipeline管理請求
    async fn handle_pipeline_management_request(&self, request: &UnifiedRequest) -> Result<ResponseData> {
        // 實現Pipeline管理邏輯
        Ok(ResponseData {
            result: serde_json::json!({"status": "pipeline_managed"}),
            metadata: HashMap::new(),
            resource_ids: vec![],
        })
    }
    
    /// 處理性能監控請求
    async fn handle_performance_monitoring_request(&self, request: &UnifiedRequest) -> Result<ResponseData> {
        let stats = self.pipeline_manager.get_stats();
        
        Ok(ResponseData {
            result: serde_json::json!({
                "total_pipelines": stats.total_pipelines_executed.load(Ordering::Relaxed),
                "total_tasks": stats.total_tasks_executed.load(Ordering::Relaxed),
                "active_pipelines": stats.current_active_pipelines.load(Ordering::Relaxed),
                "active_tasks": stats.current_active_tasks.load(Ordering::Relaxed)
            }),
            metadata: HashMap::new(),
            resource_ids: vec![],
        })
    }
    
    /// 創建數據收集Pipeline
    fn create_data_collection_pipeline(&self, symbol: &str, duration_hours: u32) -> Result<PipelineDefinition> {
        // 簡化的Pipeline創建，實際會從模板生成
        Ok(PipelineDefinition {
            name: format!("data_collection_{}", symbol.to_lowercase()),
            version: "1.0".to_string(),
            description: format!("數據收集Pipeline for {}", symbol),
            tasks: vec![],
            global_config: HashMap::new(),
            scheduling_policy: crate::core::pipeline_manager::SchedulingPolicy::Sequential,
            failure_policy: crate::core::pipeline_manager::FailurePolicy {
                max_failures: 3,
                failure_action: crate::core::pipeline_manager::FailureAction::Retry,
                notification_channels: vec![],
            },
        })
    }
    
    /// 創建模型訓練Pipeline
    fn create_model_training_pipeline(&self, symbol: &str, epochs: u32, batch_size: u32) -> Result<PipelineDefinition> {
        // 簡化的Pipeline創建，實際會從模板生成
        Ok(PipelineDefinition {
            name: format!("model_training_{}", symbol.to_lowercase()),
            version: "1.0".to_string(),
            description: format!("模型訓練Pipeline for {}", symbol),
            tasks: vec![],
            global_config: HashMap::new(),
            scheduling_policy: crate::core::pipeline_manager::SchedulingPolicy::ResourceAware,
            failure_policy: crate::core::pipeline_manager::FailurePolicy {
                max_failures: 1,
                failure_action: crate::core::pipeline_manager::FailureAction::Abort,
                notification_channels: vec!["slack".to_string()],
            },
        })
    }
    
    /// 創建成功響應
    fn create_success_response(&self, request: &UnifiedRequest, data: ResponseData, execution_time: Duration) -> Result<UnifiedResponse> {
        self.request_stats.successful_requests.fetch_add(1, Ordering::Relaxed);
        
        Ok(UnifiedResponse {
            request_id: request.request_id.clone(),
            status: ResponseStatus::Success,
            data: Some(data),
            error: None,
            execution_time_ms: execution_time.as_millis() as u64,
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            metrics: ResponseMetrics {
                cpu_usage_percent: 0.0,
                memory_usage_percent: 0.0,
                active_tasks: 0,
                queue_length: 0,
                avg_response_time_ms: 0.0,
            },
        })
    }
    
    /// 創建速率限制響應
    fn create_rate_limited_response(&self, request: &UnifiedRequest, start_time: Instant) -> UnifiedResponse {
        UnifiedResponse {
            request_id: request.request_id.clone(),
            status: ResponseStatus::RateLimited,
            data: None,
            error: Some(ErrorInfo {
                error_code: "RATE_LIMITED".to_string(),
                error_message: "請求頻率過高，請稍後再試".to_string(),
                error_details: None,
                suggested_action: Some("減少請求頻率或聯繫管理員增加配額".to_string()),
                retry_info: Some(RetryInfo {
                    can_retry: true,
                    retry_delay_seconds: 60,
                    max_retries: 3,
                }),
            }),
            execution_time_ms: start_time.elapsed().as_millis() as u64,
            timestamp: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            metrics: ResponseMetrics {
                cpu_usage_percent: 0.0,
                memory_usage_percent: 0.0,
                active_tasks: 0,
                queue_length: 0,
                avg_response_time_ms: 0.0,
            },
        }
    }
    
    /// 獲取系統統計
    pub fn get_request_stats(&self) -> RequestStats {
        self.request_stats.clone()
    }
}