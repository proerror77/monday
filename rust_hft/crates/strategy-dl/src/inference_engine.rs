//! 推理引擎：有界隊列 + Last-Wins + 背壓處理
//! 
//! Phase 2 核心特性：
//! - 有界隊列避免記憶體爆炸
//! - Last-wins 語義丟棄舊請求
//! - 超時/錯誤率觸發降級
//! - 推理延遲監控

use crate::{ModelHandle, InferenceConfig, DlRiskConfig};
use hft_core::{HftResult, HftError, Timestamp};
use ndarray::Array1;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{timeout, Duration};
use std::sync::{Arc, atomic::{AtomicU64, AtomicBool, Ordering}};
use tracing::{debug, warn, error, info};

/// 推理請求
#[derive(Debug)]
pub struct InferenceRequest {
    pub features: Array1<f32>,
    pub timestamp: Timestamp,
    pub symbol: String,
    pub response_tx: oneshot::Sender<HftResult<InferenceResult>>,
}

/// 推理結果
#[derive(Debug, Clone)]
pub struct InferenceResult {
    pub output: Vec<f32>,
    pub timestamp: Timestamp,
    pub inference_latency_us: u64,
    pub model_version: String,
}

/// 推理引擎狀態
#[derive(Debug, Clone)]
pub enum InferenceEngineState {
    Normal,         // 正常運行
    Degraded,       // 降級模式 (簡化推理)
    Paused,         // 暫停推理
    Error(String),  // 錯誤狀態
}

/// 推理引擎統計
#[derive(Debug, Clone)]
pub struct InferenceStats {
    pub total_requests: u64,
    pub successful_inferences: u64,
    pub failed_inferences: u64,
    pub dropped_requests: u64,
    pub timeout_count: u64,
    pub avg_latency_us: u64,
    pub error_rate: f64,
    pub timeout_rate: f64,
    pub queue_utilization: f64,
    pub current_state: InferenceEngineState,
}

/// 推理引擎
pub struct InferenceEngine {
    // 配置
    config: InferenceConfig,
    risk_config: DlRiskConfig,
    
    // 模型句柄
    model: Arc<ModelHandle>,
    
    // 通信通道
    request_tx: mpsc::Sender<InferenceRequest>,
    
    // 狀態與統計
    state: Arc<AtomicInferenceEngineState>,
    stats: Arc<InferenceEngineStatistics>,
    
    // 控制標誌
    shutdown_flag: Arc<AtomicBool>,
    
    // 後台任務句柄
    worker_handle: Option<tokio::task::JoinHandle<()>>,
    monitor_handle: Option<tokio::task::JoinHandle<()>>,
}

/// 原子化的推理引擎狀態
struct AtomicInferenceEngineState {
    state_code: AtomicU64, // 0=Normal, 1=Degraded, 2=Paused, 3=Error
    error_message: std::sync::Mutex<Option<String>>,
}

/// 推理統計 (原子操作)
struct InferenceEngineStatistics {
    total_requests: AtomicU64,
    successful_inferences: AtomicU64,
    failed_inferences: AtomicU64,
    dropped_requests: AtomicU64,
    timeout_count: AtomicU64,
    total_latency_us: AtomicU64,
    queue_size: AtomicU64,
}

impl InferenceEngine {
    /// 創建新的推理引擎
    pub fn new(
        model: Arc<ModelHandle>,
        config: InferenceConfig,
        risk_config: DlRiskConfig,
    ) -> HftResult<Self> {
        let (request_tx, request_rx) = mpsc::channel(config.queue_capacity);
        
        let state = Arc::new(AtomicInferenceEngineState::new());
        let stats = Arc::new(InferenceEngineStatistics::new());
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        
        let mut engine = Self {
            config: config.clone(),
            risk_config: risk_config.clone(),
            model: model.clone(),
            request_tx,
            state: state.clone(),
            stats: stats.clone(),
            shutdown_flag: shutdown_flag.clone(),
            worker_handle: None,
            monitor_handle: None,
        };
        
        // 啟動後台工作線程
        engine.start_worker_thread(request_rx)?;
        engine.start_monitor_thread()?;
        
        info!("推理引擎啟動完成");
        Ok(engine)
    }
    
    /// 提交推理請求
    pub async fn infer(&self, features: Array1<f32>, symbol: String) -> HftResult<InferenceResult> {
        let current_state = self.get_current_state();
        
        match current_state {
            InferenceEngineState::Paused => {
                return Err(HftError::Execution("推理引擎已暫停".to_string()));
            },
            InferenceEngineState::Error(msg) => {
                return Err(HftError::Execution(format!("推理引擎錯誤: {}", msg)));
            },
            _ => {} // Normal 或 Degraded 狀態繼續
        }
        
        // 檢查隊列是否已滿 (Last-Wins 語義)
        let queue_size = self.stats.queue_size.load(Ordering::Relaxed) as usize;
        if queue_size >= self.config.queue_capacity {
            if self.config.last_wins {
                warn!("推理隊列已滿，啟用 Last-Wins 丟棄策略");
                self.stats.dropped_requests.fetch_add(1, Ordering::Relaxed);
                // 在 Last-Wins 模式下，我們不等待，直接返回錯誤或使用緊急推理
                return self.emergency_inference(features).await;
            } else {
                return Err(HftError::RateLimit("推理隊列已滿".to_string()));
            }
        }
        
        // 創建推理請求
        let (response_tx, response_rx) = oneshot::channel();
        let request = InferenceRequest {
            features,
            timestamp: self.current_timestamp(),
            symbol,
            response_tx,
        };
        
        // 發送請求
        self.stats.total_requests.fetch_add(1, Ordering::Relaxed);
        
        match self.request_tx.try_send(request) {
            Ok(()) => {
                self.stats.queue_size.fetch_add(1, Ordering::Relaxed);
                
                // 等待響應 (帶超時)
                let timeout_duration = Duration::from_millis(self.config.timeout_ms);
                
                match timeout(timeout_duration, response_rx).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => {
                        self.stats.failed_inferences.fetch_add(1, Ordering::Relaxed);
                        Err(HftError::Execution("推理請求被取消".to_string()))
                    },
                    Err(_) => {
                        self.stats.timeout_count.fetch_add(1, Ordering::Relaxed);
                        Err(HftError::Timeout("推理超時".to_string()))
                    }
                }
            },
            Err(mpsc::error::TrySendError::Full(_)) => {
                // 隊列已滿但是統計可能有延遲，再次嘗試應急推理
                self.stats.dropped_requests.fetch_add(1, Ordering::Relaxed);
                self.emergency_inference(features).await
            },
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(HftError::Execution("推理引擎已關閉".to_string()))
            }
        }
    }
    
    /// 緊急推理 (降級模式)
    async fn emergency_inference(&self, features: Array1<f32>) -> HftResult<InferenceResult> {
        warn!("執行緊急推理");
        
        // 簡化推理：直接使用特徵的線性組合
        let output = if features.len() >= 10 {
            let sum: f32 = features.iter().take(10).sum();
            let simple_signal = (sum / 10.0).tanh(); // 簡單的歸一化信號
            vec![simple_signal]
        } else {
            vec![0.0] // 默認無信號
        };
        
        Ok(InferenceResult {
            output,
            timestamp: self.current_timestamp(),
            inference_latency_us: 1, // 極低延遲
            model_version: "emergency_fallback".to_string(),
        })
    }
    
    /// 啟動工作線程
    fn start_worker_thread(&mut self, mut request_rx: mpsc::Receiver<InferenceRequest>) -> HftResult<()> {
        let model = self.model.clone();
        let stats = self.stats.clone();
        let state = self.state.clone();
        let shutdown_flag = self.shutdown_flag.clone();
        let config = self.config.clone();
        
        let handle = tokio::spawn(async move {
            info!("推理工作線程啟動");
            
            while !shutdown_flag.load(Ordering::Relaxed) {
                match request_rx.recv().await {
                    Some(request) => {
                        stats.queue_size.fetch_sub(1, Ordering::Relaxed);
                        
                        let start_time = Self::current_timestamp_static();
                        
                        // 檢查請求是否過期
                        let staleness = start_time - request.timestamp;
                        if staleness > 10_000 { // 10ms 過期
                            debug!("推理請求過期，跳過: {}μs", staleness);
                            let _ = request.response_tx.send(Err(HftError::Timeout("請求過期".to_string())));
                            continue;
                        }
                        
                        // 執行推理
                        let result = Self::perform_inference(
                            &model, 
                            &request, 
                            start_time
                        ).await;
                        
                        // 更新統計
                        match &result {
                            Ok(inference_result) => {
                                stats.successful_inferences.fetch_add(1, Ordering::Relaxed);
                                stats.total_latency_us.fetch_add(inference_result.inference_latency_us, Ordering::Relaxed);
                            },
                            Err(_) => {
                                stats.failed_inferences.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        
                        // 發送結果
                        let _ = request.response_tx.send(result);
                    },
                    None => {
                        debug!("推理請求通道關閉");
                        break;
                    }
                }
            }
            
            info!("推理工作線程退出");
        });
        
        self.worker_handle = Some(handle);
        Ok(())
    }
    
    /// 執行單次推理
    async fn perform_inference(
        model: &ModelHandle,
        request: &InferenceRequest,
        start_time: Timestamp,
    ) -> HftResult<InferenceResult> {
        // 轉換特徵為模型輸入格式
        let input: Vec<f32> = request.features.to_vec();
        
        // 執行模型推理
        let output = model.predict(&input)?;
        
        let end_time = Self::current_timestamp_static();
        let inference_latency_us = end_time - start_time;
        
        Ok(InferenceResult {
            output,
            timestamp: end_time,
            inference_latency_us,
            model_version: "v1.0".to_string(), // 應該從模型獲取
        })
    }
    
    /// 啟動監控線程 (健康檢查與降級處理)
    fn start_monitor_thread(&mut self) -> HftResult<()> {
        let stats = self.stats.clone();
        let state = self.state.clone();
        let risk_config = self.risk_config.clone();
        let shutdown_flag = self.shutdown_flag.clone();
        
        let handle = tokio::spawn(async move {
            info!("推理監控線程啟動");
            
            let mut check_interval = tokio::time::interval(
                Duration::from_secs(risk_config.recovery_check_interval_sec)
            );
            
            while !shutdown_flag.load(Ordering::Relaxed) {
                check_interval.tick().await;
                
                // 計算當前統計
                let current_stats = stats.get_current_stats();
                
                // 檢查是否需要降級
                let should_degrade = 
                    current_stats.error_rate > risk_config.max_error_rate ||
                    current_stats.timeout_rate > risk_config.max_timeout_rate ||
                    current_stats.avg_latency_us > risk_config.max_inference_latency_ms * 1000;
                
                let current_state_code = state.state_code.load(Ordering::Relaxed);
                
                match current_state_code {
                    0 => { // Normal
                        if should_degrade {
                            warn!("檢測到推理性能問題，觸發降級: error_rate={:.3}, timeout_rate={:.3}, avg_latency={}μs",
                                  current_stats.error_rate, current_stats.timeout_rate, current_stats.avg_latency_us);
                            state.state_code.store(1, Ordering::Relaxed); // 切換到 Degraded
                        }
                    },
                    1 => { // Degraded
                        if !should_degrade {
                            info!("推理性能恢復正常，退出降級模式");
                            state.state_code.store(0, Ordering::Relaxed); // 恢復到 Normal
                        }
                    },
                    _ => {} // Paused 或 Error 狀態需要外部干預
                }
            }
            
            info!("推理監控線程退出");
        });
        
        self.monitor_handle = Some(handle);
        Ok(())
    }
    
    /// 獲取當前狀態
    pub fn get_current_state(&self) -> InferenceEngineState {
        let state_code = self.state.state_code.load(Ordering::Relaxed);
        match state_code {
            0 => InferenceEngineState::Normal,
            1 => InferenceEngineState::Degraded,
            2 => InferenceEngineState::Paused,
            3 => {
                let error_msg = self.state.error_message.lock().unwrap();
                InferenceEngineState::Error(
                    error_msg.as_ref().unwrap_or(&"未知錯誤".to_string()).clone()
                )
            },
            _ => InferenceEngineState::Error("無效狀態".to_string()),
        }
    }
    
    /// 獲取統計信息
    pub fn get_stats(&self) -> InferenceStats {
        let current_stats = self.stats.get_current_stats();
        InferenceStats {
            total_requests: current_stats.total_requests,
            successful_inferences: current_stats.successful_inferences,
            failed_inferences: current_stats.failed_inferences,
            dropped_requests: current_stats.dropped_requests,
            timeout_count: current_stats.timeout_count,
            avg_latency_us: current_stats.avg_latency_us,
            error_rate: current_stats.error_rate,
            timeout_rate: current_stats.timeout_rate,
            queue_utilization: current_stats.queue_size as f64 / self.config.queue_capacity as f64,
            current_state: self.get_current_state(),
        }
    }
    
    /// 手動設置狀態 (用於調試或緊急干預)
    pub fn set_state(&self, new_state: InferenceEngineState) {
        match new_state {
            InferenceEngineState::Normal => {
                self.state.state_code.store(0, Ordering::Relaxed);
            },
            InferenceEngineState::Degraded => {
                self.state.state_code.store(1, Ordering::Relaxed);
            },
            InferenceEngineState::Paused => {
                self.state.state_code.store(2, Ordering::Relaxed);
            },
            InferenceEngineState::Error(msg) => {
                self.state.state_code.store(3, Ordering::Relaxed);
                let mut error_msg = self.state.error_message.lock().unwrap();
                *error_msg = Some(msg);
            },
        }
        
        info!("推理引擎狀態手動設置為: {:?}", self.get_current_state());
    }
    
    /// 獲取當前時間戳
    fn current_timestamp(&self) -> Timestamp {
        Self::current_timestamp_static()
    }
    
    fn current_timestamp_static() -> Timestamp {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }
    
    /// 關閉推理引擎
    pub async fn shutdown(&mut self) -> HftResult<()> {
        info!("開始關閉推理引擎");
        
        // 設置關閉標誌
        self.shutdown_flag.store(true, Ordering::Relaxed);
        
        // 等待工作線程退出
        if let Some(handle) = self.worker_handle.take() {
            let _ = handle.await;
        }
        
        if let Some(handle) = self.monitor_handle.take() {
            let _ = handle.await;
        }
        
        info!("推理引擎已關閉");
        Ok(())
    }
}

impl AtomicInferenceEngineState {
    fn new() -> Self {
        Self {
            state_code: AtomicU64::new(0), // Normal
            error_message: std::sync::Mutex::new(None),
        }
    }
}

/// 內部統計數據結構
#[derive(Debug)]
struct InternalStats {
    total_requests: u64,
    successful_inferences: u64,
    failed_inferences: u64,
    dropped_requests: u64,
    timeout_count: u64,
    avg_latency_us: u64,
    error_rate: f64,
    timeout_rate: f64,
    queue_size: u64,
}

impl InferenceEngineStatistics {
    fn new() -> Self {
        Self {
            total_requests: AtomicU64::new(0),
            successful_inferences: AtomicU64::new(0),
            failed_inferences: AtomicU64::new(0),
            dropped_requests: AtomicU64::new(0),
            timeout_count: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
            queue_size: AtomicU64::new(0),
        }
    }
    
    fn get_current_stats(&self) -> InternalStats {
        let total_requests = self.total_requests.load(Ordering::Relaxed);
        let successful_inferences = self.successful_inferences.load(Ordering::Relaxed);
        let failed_inferences = self.failed_inferences.load(Ordering::Relaxed);
        let dropped_requests = self.dropped_requests.load(Ordering::Relaxed);
        let timeout_count = self.timeout_count.load(Ordering::Relaxed);
        let total_latency_us = self.total_latency_us.load(Ordering::Relaxed);
        
        let error_rate = if total_requests > 0 {
            failed_inferences as f64 / total_requests as f64
        } else {
            0.0
        };
        
        let timeout_rate = if total_requests > 0 {
            timeout_count as f64 / total_requests as f64
        } else {
            0.0
        };
        
        let avg_latency_us = if successful_inferences > 0 {
            total_latency_us / successful_inferences
        } else {
            0
        };
        
        InternalStats {
            total_requests,
            successful_inferences,
            failed_inferences,
            dropped_requests,
            timeout_count,
            avg_latency_us,
            error_rate,
            timeout_rate,
            queue_size: self.queue_size.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelLoader, ModelConfig};
    use std::path::PathBuf;

    fn create_test_model() -> Arc<ModelHandle> {
        let config = ModelConfig {
            model_path: PathBuf::from("test.pt"),
            device: "cpu".to_string(),
            batch_size: 1,
            input_dim: 10,
            output_dim: 1,
            model_version: None,
        };
        
        // 使用 Mock 模型進行測試
        ModelLoader::load_model(config).unwrap()
    }

    #[tokio::test]
    async fn test_inference_engine_creation() {
        let model = create_test_model();
        let inference_config = InferenceConfig::default();
        let risk_config = DlRiskConfig::default();
        
        let engine = InferenceEngine::new(model, inference_config, risk_config);
        assert!(engine.is_ok());
        
        if let Ok(mut engine) = engine {
            let stats = engine.get_stats();
            assert_eq!(stats.total_requests, 0);
            assert!(matches!(stats.current_state, InferenceEngineState::Normal));
            
            let _ = engine.shutdown().await;
        }
    }

    #[tokio::test]
    async fn test_simple_inference() {
        let model = create_test_model();
        let inference_config = InferenceConfig::default();
        let risk_config = DlRiskConfig::default();
        
        if let Ok(mut engine) = InferenceEngine::new(model, inference_config, risk_config) {
            let features = Array1::from(vec![0.1; 10]);
            let result = engine.infer(features, "BTCUSDT".to_string()).await;
            
            // Mock 模型應該能正常推理
            assert!(result.is_ok());
            
            if let Ok(inference_result) = result {
                assert_eq!(inference_result.output.len(), 1);
                assert!(inference_result.inference_latency_us > 0);
            }
            
            let _ = engine.shutdown().await;
        }
    }

    #[tokio::test]
    async fn test_emergency_inference() {
        let model = create_test_model();
        let inference_config = InferenceConfig::default();
        let risk_config = DlRiskConfig::default();
        
        if let Ok(mut engine) = InferenceEngine::new(model, inference_config, risk_config) {
            let features = Array1::from(vec![0.1; 10]);
            let result = engine.emergency_inference(features).await;
            
            assert!(result.is_ok());
            
            if let Ok(inference_result) = result {
                assert_eq!(inference_result.model_version, "emergency_fallback");
                assert!(inference_result.inference_latency_us <= 1000); // 應該很快
            }
            
            let _ = engine.shutdown().await;
        }
    }
}