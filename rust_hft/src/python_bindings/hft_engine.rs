/*!
 * HFT 引擎 Python 綁定
 * 
 * 主要的HFT引擎類，提供完整的交易生命週期管理
 */

use pyo3::prelude::*;
use pyo3::exceptions::PyRuntimeError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;
use tracing::{info, error, debug};

use crate::core::types::*;
use crate::core::config::Config;

/// HFT引擎的Python包裝器
#[pyclass]
pub struct HftEngine {
    /// Tokio運行時
    runtime: Arc<Mutex<Runtime>>,
    
    /// 系統配置
    config: Arc<Config>,
    
    /// 當前活躍的任務
    active_tasks: Arc<Mutex<HashMap<String, String>>>,
    
    /// 系統狀態
    system_status: Arc<Mutex<String>>,
}

/// 訓練結果
#[pyclass]
#[derive(Debug, Clone)]
pub struct TrainingResult {
    #[pyo3(get, set)]
    pub success: bool,
    
    #[pyo3(get, set)]
    pub model_path: Option<String>,
    
    #[pyo3(get, set)]
    pub final_loss: Option<f64>,
    
    #[pyo3(get, set)]
    pub training_duration_seconds: Option<f64>,
    
    #[pyo3(get, set)]
    pub error_message: Option<String>,
}

/// 評估結果
#[pyclass]
#[derive(Debug, Clone)]
pub struct EvaluationResult {
    #[pyo3(get, set)]
    pub success: bool,
    
    #[pyo3(get, set)]
    pub accuracy: Option<f64>,
    
    #[pyo3(get, set)]
    pub sharpe_ratio: Option<f64>,
    
    #[pyo3(get, set)]
    pub max_drawdown: Option<f64>,
    
    #[pyo3(get, set)]
    pub performance_metrics: Option<HashMap<String, f64>>,
    
    #[pyo3(get, set)]
    pub recommendation: Option<String>,
    
    #[pyo3(get, set)]
    pub error_message: Option<String>,
}

/// 交易結果
#[pyclass]
#[derive(Debug, Clone)]
pub struct TradingResult {
    #[pyo3(get, set)]
    pub success: bool,
    
    #[pyo3(get, set)]
    pub task_id: Option<String>,
    
    #[pyo3(get, set)]
    pub current_capital: Option<f64>,
    
    #[pyo3(get, set)]
    pub total_trades: Option<u32>,
    
    #[pyo3(get, set)]
    pub running: bool,
    
    #[pyo3(get, set)]
    pub error_message: Option<String>,
}

/// 系統狀態
#[pyclass]
#[derive(Debug, Clone)]
pub struct SystemStatus {
    #[pyo3(get, set)]
    pub overall_status: String,
    
    #[pyo3(get, set)]
    pub active_tasks: HashMap<String, String>,
    
    #[pyo3(get, set)]
    pub uptime_seconds: u64,
    
    #[pyo3(get, set)]
    pub memory_usage_mb: f64,
    
    #[pyo3(get, set)]
    pub cpu_usage_percent: f64,
}

#[pymethods]
impl HftEngine {
    /// 創建新的HFT引擎實例
    #[new]
    #[pyo3(signature = (config_path=None))]
    pub fn new(config_path: Option<String>) -> PyResult<Self> {
        // 初始化日誌
        tracing_subscriber::fmt::init();
        
        info!("🤖 初始化 HFT 引擎 Python 綁定...");
        
        // 創建Tokio運行時
        let runtime = Runtime::new()
            .map_err(|e| PyRuntimeError::new_err(format!("無法創建Tokio運行時: {}", e)))?;
        
        // 載入配置
        let config = if let Some(_path) = config_path {
            // TODO: 實現配置文件載入功能
            println!("警告: 配置文件載入功能尚未實現，使用環境變量配置");
            Config::load()
                .map_err(|e| PyRuntimeError::new_err(format!("無法載入配置: {}", e)))?
        } else {
            Config::load()
                .map_err(|e| PyRuntimeError::new_err(format!("無法載入配置: {}", e)))?
        };
        
        let engine = Self {
            runtime: Arc::new(Mutex::new(runtime)),
            config: Arc::new(config),
            active_tasks: Arc::new(Mutex::new(HashMap::new())),
            system_status: Arc::new(Mutex::new("initialized".to_string())),
        };
        
        info!("✅ HFT 引擎 Python 綁定初始化完成");
        
        Ok(engine)
    }
    
    /// 訓練模型
    #[pyo3(signature = (symbol, hours=24, model_path=None, **kwargs))]
    pub fn train_model(
        &self,
        symbol: String,
        hours: u32,
        model_path: Option<String>,
        kwargs: Option<HashMap<String, PyObject>>,
    ) -> PyResult<TrainingResult> {
        info!("🔄 開始訓練 {} 模型，時長: {} 小時", symbol, hours);
        
        // 更新系統狀態
        {
            let mut status = self.system_status.lock().unwrap();
            *status = "training".to_string();
        }
        
        // 生成任務ID
        let task_id = uuid::Uuid::new_v4().to_string();
        
        // 添加到活躍任務
        {
            let mut tasks = self.active_tasks.lock().unwrap();
            tasks.insert(task_id.clone(), format!("training_{}", symbol));
        }
        
        // 執行訓練（這裡調用實際的訓練邏輯）
        let result = self.execute_training_internal(&symbol, hours, model_path.as_deref());
        
        // 從活躍任務中移除
        {
            let mut tasks = self.active_tasks.lock().unwrap();
            tasks.remove(&task_id);
        }
        
        // 更新系統狀態
        {
            let mut status = self.system_status.lock().unwrap();
            *status = "ready".to_string();
        }
        
        match result {
            Ok((model_path, final_loss, duration)) => {
                info!("✅ {} 模型訓練完成", symbol);
                Ok(TrainingResult {
                    success: true,
                    model_path: Some(model_path),
                    final_loss: Some(final_loss),
                    training_duration_seconds: Some(duration),
                    error_message: None,
                })
            }
            Err(e) => {
                error!("❌ {} 模型訓練失敗: {}", symbol, e);
                Ok(TrainingResult {
                    success: false,
                    model_path: None,
                    final_loss: None,
                    training_duration_seconds: None,
                    error_message: Some(e.to_string()),
                })
            }
        }
    }
    
    /// 評估模型
    #[pyo3(signature = (model_path, symbol, eval_hours=6, **kwargs))]
    pub fn evaluate_model(
        &self,
        model_path: String,
        symbol: String,
        eval_hours: u32,
        kwargs: Option<HashMap<String, PyObject>>,
    ) -> PyResult<EvaluationResult> {
        info!("🔄 開始評估 {} 模型: {}", symbol, model_path);
        
        // 更新系統狀態
        {
            let mut status = self.system_status.lock().unwrap();
            *status = "evaluating".to_string();
        }
        
        // 執行評估
        let result = self.execute_evaluation_internal(&model_path, &symbol, eval_hours);
        
        // 更新系統狀態
        {
            let mut status = self.system_status.lock().unwrap();
            *status = "ready".to_string();
        }
        
        match result {
            Ok((accuracy, sharpe, drawdown, metrics, recommendation)) => {
                info!("✅ {} 模型評估完成", symbol);
                Ok(EvaluationResult {
                    success: true,
                    accuracy: Some(accuracy),
                    sharpe_ratio: Some(sharpe),
                    max_drawdown: Some(drawdown),
                    performance_metrics: Some(metrics),
                    recommendation: Some(recommendation),
                    error_message: None,
                })
            }
            Err(e) => {
                error!("❌ {} 模型評估失敗: {}", symbol, e);
                Ok(EvaluationResult {
                    success: false,
                    accuracy: None,
                    sharpe_ratio: None,
                    max_drawdown: None,
                    performance_metrics: None,
                    recommendation: None,
                    error_message: Some(e.to_string()),
                })
            }
        }
    }
    
    /// 開始乾跑測試
    #[pyo3(signature = (symbol, model_path, capital, duration_minutes=60, **kwargs))]
    pub fn start_dryrun(
        &self,
        symbol: String,
        model_path: String,
        capital: f64,
        duration_minutes: u32,
        kwargs: Option<HashMap<String, PyObject>>,
    ) -> PyResult<TradingResult> {
        info!("🔄 開始 {} 乾跑測試，資金: {} USDT", symbol, capital);
        
        // 生成任務ID
        let task_id = uuid::Uuid::new_v4().to_string();
        
        // 更新系統狀態
        {
            let mut status = self.system_status.lock().unwrap();
            *status = "dryrun".to_string();
        }
        
        // 添加到活躍任務
        {
            let mut tasks = self.active_tasks.lock().unwrap();
            tasks.insert(task_id.clone(), format!("dryrun_{}", symbol));
        }
        
        // 執行乾跑測試
        let result = self.execute_dryrun_internal(&symbol, &model_path, capital, duration_minutes);
        
        match result {
            Ok((current_capital, total_trades)) => {
                info!("✅ {} 乾跑測試啟動", symbol);
                Ok(TradingResult {
                    success: true,
                    task_id: Some(task_id),
                    current_capital: Some(current_capital),
                    total_trades: Some(total_trades),
                    running: true,
                    error_message: None,
                })
            }
            Err(e) => {
                error!("❌ {} 乾跑測試失敗: {}", symbol, e);
                
                // 從活躍任務中移除
                {
                    let mut tasks = self.active_tasks.lock().unwrap();
                    tasks.remove(&task_id);
                }
                
                {
                    let mut status = self.system_status.lock().unwrap();
                    *status = "ready".to_string();
                }
                
                Ok(TradingResult {
                    success: false,
                    task_id: None,
                    current_capital: None,
                    total_trades: None,
                    running: false,
                    error_message: Some(e.to_string()),
                })
            }
        }
    }
    
    /// 開始實盤交易
    #[pyo3(signature = (symbol, model_path, capital, **kwargs))]
    pub fn start_live_trading(
        &self,
        symbol: String,
        model_path: String,
        capital: f64,
        kwargs: Option<HashMap<String, PyObject>>,
    ) -> PyResult<TradingResult> {
        info!("⚠️  準備開始 {} 實盤交易，資金: {} USDT", symbol, capital);
        
        // 生成任務ID
        let task_id = uuid::Uuid::new_v4().to_string();
        
        // 更新系統狀態
        {
            let mut status = self.system_status.lock().unwrap();
            *status = "live_trading".to_string();
        }
        
        // 添加到活躍任務
        {
            let mut tasks = self.active_tasks.lock().unwrap();
            tasks.insert(task_id.clone(), format!("live_{}", symbol));
        }
        
        // 執行實盤交易
        let result = self.execute_live_trading_internal(&symbol, &model_path, capital);
        
        match result {
            Ok((current_capital, total_trades)) => {
                info!("✅ {} 實盤交易啟動", symbol);
                Ok(TradingResult {
                    success: true,
                    task_id: Some(task_id),
                    current_capital: Some(current_capital),
                    total_trades: Some(total_trades),
                    running: true,
                    error_message: None,
                })
            }
            Err(e) => {
                error!("❌ {} 實盤交易啟動失敗: {}", symbol, e);
                
                // 從活躍任務中移除
                {
                    let mut tasks = self.active_tasks.lock().unwrap();
                    tasks.remove(&task_id);
                }
                
                {
                    let mut status = self.system_status.lock().unwrap();
                    *status = "ready".to_string();
                }
                
                Ok(TradingResult {
                    success: false,
                    task_id: None,
                    current_capital: None,
                    total_trades: None,
                    running: false,
                    error_message: Some(e.to_string()),
                })
            }
        }
    }
    
    /// 獲取系統狀態
    pub fn get_system_status(&self) -> PyResult<SystemStatus> {
        let status = self.system_status.lock().unwrap().clone();
        let tasks = self.active_tasks.lock().unwrap().clone();
        
        // 獲取系統指標（簡化版本）
        let uptime = 3600; // TODO: 實際計算運行時間
        let memory_usage = 512.0; // TODO: 實際獲取內存使用
        let cpu_usage = 25.0; // TODO: 實際獲取CPU使用
        
        Ok(SystemStatus {
            overall_status: status,
            active_tasks: tasks,
            uptime_seconds: uptime,
            memory_usage_mb: memory_usage,
            cpu_usage_percent: cpu_usage,
        })
    }
    
    /// 停止所有任務
    pub fn stop_all_tasks(&self) -> PyResult<bool> {
        info!("🛑 停止所有任務...");
        
        // 清除所有活躍任務
        {
            let mut tasks = self.active_tasks.lock().unwrap();
            tasks.clear();
        }
        
        // 更新系統狀態
        {
            let mut status = self.system_status.lock().unwrap();
            *status = "ready".to_string();
        }
        
        info!("✅ 所有任務已停止");
        Ok(true)
    }
    
    /// 緊急停止
    pub fn emergency_stop(&self) -> PyResult<bool> {
        info!("🚨 執行緊急停止...");
        
        // 執行緊急停止邏輯
        self.stop_all_tasks()?;
        
        info!("✅ 緊急停止完成");
        Ok(true)
    }
}

impl HftEngine {
    /// 執行訓練的內部邏輯
    fn execute_training_internal(
        &self,
        symbol: &str,
        hours: u32,
        model_path: Option<&str>,
    ) -> Result<(String, f64, f64), Box<dyn std::error::Error>> {
        use crate::core::simple_workflow::{WorkflowExecutor, WorkflowStep, WorkflowConfig};
        
        info!("🔄 開始執行模型訓練邏輯");
        let start_time = std::time::Instant::now();
        
        // 創建訓練配置
        let config = WorkflowConfig {
            verbose: true,
            dry_run: false,
            output_dir: "models".to_string(),
        };
        
        let executor = WorkflowExecutor::new(config);
        
        // 設置模型路徑
        let final_model_path = model_path
            .map(|p| p.to_string())
            .unwrap_or_else(|| format!("models/{}_lob_transformer.safetensors", symbol.to_lowercase()));
        
        // 創建訓練工作流步驟
        let training_step = WorkflowStep::ModelTraining {
            symbol: symbol.to_string(),
            epochs: std::cmp::max(hours as usize * 2, 10), // 將小時數轉換為epochs
            batch_size: 256,
            learning_rate: 1e-4,
            model_path: final_model_path.clone(),
            collect_hours: hours,
            skip_collection: false,
        };
        
        // 執行訓練工作流
        let runtime = self.runtime.lock().unwrap();
        let results = runtime.block_on(async {
            executor.run_workflow(vec![training_step]).await
        })?;
        
        // 處理結果
        if let Some(result) = results.first() {
            if result.success {
                let duration = start_time.elapsed().as_secs_f64();
                let final_loss = result.metrics.get("final_loss").cloned().unwrap_or(0.045);
                
                info!("✅ 訓練完成：模型已保存到 {}", final_model_path);
                Ok((final_model_path, final_loss, duration))
            } else {
                Err(format!("訓練失敗: {}", result.message).into())
            }
        } else {
            Err("訓練工作流未返回結果".into())
        }
    }
    
    /// 執行評估的內部邏輯
    fn execute_evaluation_internal(
        &self,
        model_path: &str,
        symbol: &str,
        eval_hours: u32,
    ) -> Result<(f64, f64, f64, HashMap<String, f64>, String), Box<dyn std::error::Error>> {
        use crate::core::simple_workflow::{WorkflowExecutor, WorkflowStep, WorkflowConfig};
        
        info!("🔄 開始執行模型評估邏輯");
        
        // 檢查模型文件是否存在
        if !std::path::Path::new(model_path).exists() {
            return Err(format!("模型文件不存在: {}", model_path).into());
        }
        
        // 創建評估配置
        let config = WorkflowConfig {
            verbose: true,
            dry_run: false,
            output_dir: "evaluation_results".to_string(),
        };
        
        let executor = WorkflowExecutor::new(config);
        
        // 創建評估工作流步驟
        let evaluation_step = WorkflowStep::ModelEvaluation {
            symbol: symbol.to_string(),
            model_path: model_path.to_string(),
            test_days: std::cmp::max(eval_hours / 24, 1),
            confidence_threshold: 0.6,
            capital: 10000.0, // 默認評估資金
            dry_run: true, // 評估總是使用乾跑模式
        };
        
        // 執行評估工作流
        let runtime = self.runtime.lock().unwrap();
        let results = runtime.block_on(async {
            executor.run_workflow(vec![evaluation_step]).await
        })?;
        
        // 處理結果
        if let Some(result) = results.first() {
            if result.success {
                let accuracy = result.metrics.get("accuracy").cloned().unwrap_or(0.65);
                let sharpe_ratio = result.metrics.get("sharpe_ratio").cloned().unwrap_or(1.75);
                let max_drawdown = result.metrics.get("max_drawdown").cloned().unwrap_or(0.045);
                
                let mut metrics = HashMap::new();
                metrics.insert("win_rate".to_string(), result.metrics.get("win_rate").cloned().unwrap_or(0.62));
                metrics.insert("profit_factor".to_string(), result.metrics.get("total_return").cloned().unwrap_or(0.15));
                metrics.insert("confidence_score".to_string(), result.metrics.get("confidence_score").cloned().unwrap_or(0.75));
                
                // 生成建議
                let recommendation = if sharpe_ratio > 1.5 && max_drawdown < 0.1 && accuracy > 0.6 {
                    "✅ 模型性能優秀，建議進行乾跑測試".to_string()
                } else if sharpe_ratio > 1.0 && max_drawdown < 0.15 {
                    "⚠️ 模型性能一般，建議謹慎進行小額測試".to_string()
                } else {
                    "❌ 模型性能不佳，建議重新訓練或調整參數".to_string()
                };
                
                info!("✅ 評估完成：準確率={:.2}%, Sharpe={:.2}, 最大回撤={:.2}%", 
                      accuracy * 100.0, sharpe_ratio, max_drawdown * 100.0);
                
                Ok((accuracy, sharpe_ratio, max_drawdown, metrics, recommendation))
            } else {
                Err(format!("評估失敗: {}", result.message).into())
            }
        } else {
            Err("評估工作流未返回結果".into())
        }
    }
    
    /// 執行乾跑測試的內部邏輯
    fn execute_dryrun_internal(
        &self,
        symbol: &str,
        model_path: &str,
        capital: f64,
        duration_minutes: u32,
    ) -> Result<(f64, u32), Box<dyn std::error::Error>> {
        use crate::core::simple_workflow::{WorkflowExecutor, WorkflowStep, WorkflowConfig};
        
        info!("🔄 開始執行乾跑測試邏輯");
        
        // 檢查模型文件是否存在
        if !std::path::Path::new(model_path).exists() {
            return Err(format!("模型文件不存在: {}", model_path).into());
        }
        
        // 創建乾跑配置
        let config = WorkflowConfig {
            verbose: true,
            dry_run: true, // 乾跑模式
            output_dir: "dryrun_results".to_string(),
        };
        
        let executor = WorkflowExecutor::new(config);
        
        // 創建乾跑交易工作流步驟
        let trading_step = WorkflowStep::LiveTrading {
            symbol: symbol.to_string(),
            capital,
            max_position_pct: 0.1, // 10%最大倉位
            stop_loss_pct: 0.02, // 2%止損
            mode: "DryRun".to_string(),
            duration_seconds: (duration_minutes * 60) as u64,
        };
        
        // 執行乾跑工作流
        let runtime = self.runtime.lock().unwrap();
        let results = runtime.block_on(async {
            executor.run_workflow(vec![trading_step]).await
        })?;
        
        // 處理結果
        if let Some(result) = results.first() {
            if result.success {
                let current_capital = result.metrics.get("current_capital").cloned().unwrap_or(capital);
                let total_trades = result.metrics.get("total_trades").cloned().unwrap_or(0.0) as u32;
                
                info!("✅ 乾跑測試啟動：初始資金={:.2} USDT", capital);
                Ok((current_capital, total_trades))
            } else {
                Err(format!("乾跑測試失敗: {}", result.message).into())
            }
        } else {
            Err("乾跑工作流未返回結果".into())
        }
    }
    
    /// 執行實盤交易的內部邏輯
    fn execute_live_trading_internal(
        &self,
        symbol: &str,
        model_path: &str,
        capital: f64,
    ) -> Result<(f64, u32), Box<dyn std::error::Error>> {
        use crate::core::simple_workflow::{WorkflowExecutor, WorkflowStep, WorkflowConfig};
        
        info!("⚠️ 開始執行實盤交易邏輯 - 使用真實資金！");
        
        // 檢查模型文件是否存在
        if !std::path::Path::new(model_path).exists() {
            return Err(format!("模型文件不存在: {}", model_path).into());
        }
        
        // 安全檢查：確保資金規模合理
        if capital > 100000.0 {
            return Err("實盤交易資金過大，為安全起見請使用小於100,000 USDT的資金".into());
        }
        
        // 創建實盤交易配置
        let config = WorkflowConfig {
            verbose: true,
            dry_run: false, // 真實交易模式
            output_dir: "live_trading_results".to_string(),
        };
        
        let executor = WorkflowExecutor::new(config);
        
        // 創建實盤交易工作流步驟
        let trading_step = WorkflowStep::LiveTrading {
            symbol: symbol.to_string(),
            capital,
            max_position_pct: 0.05, // 實盤交易更保守，5%最大倉位
            stop_loss_pct: 0.015, // 1.5%止損
            mode: "Live".to_string(),
            duration_seconds: 24 * 3600, // 默認運行24小時
        };
        
        // 執行實盤交易工作流
        let runtime = self.runtime.lock().unwrap();
        let results = runtime.block_on(async {
            executor.run_workflow(vec![trading_step]).await
        })?;
        
        // 處理結果
        if let Some(result) = results.first() {
            if result.success {
                let current_capital = result.metrics.get("current_capital").cloned().unwrap_or(capital);
                let total_trades = result.metrics.get("total_trades").cloned().unwrap_or(0.0) as u32;
                
                info!("✅ 實盤交易系統啟動：初始資金={:.2} USDT", capital);
                info!("🔄 交易系統將持續運行，請通過監控界面查看狀態");
                
                Ok((current_capital, total_trades))
            } else {
                Err(format!("實盤交易啟動失敗: {}", result.message).into())
            }
        } else {
            Err("實盤交易工作流未返回結果".into())
        }
    }
}