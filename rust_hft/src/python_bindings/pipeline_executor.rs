/*!
 * 🐍 Pipeline Executor Python Bindings
 * 
 * 為Python Agno Agent提供統一的Pipeline執行接口
 * 
 * 主要功能：
 * - 統一的Pipeline調用接口
 * - 實時執行狀態監控
 * - 結果數據序列化
 * - 錯誤處理和日誌記錄
 * - 異步執行支持
 */

use crate::core::config::Config;
use crate::pipelines::{
    PipelineManager, PipelineConfig, PipelineResult, PipelineStatus,
    data_pipeline::{DataPipeline, DataPipelineConfig},
    training_pipeline::{TrainingPipeline, TrainingPipelineConfig},
    hyperopt_pipeline::{HyperoptPipeline, HyperoptPipelineConfig},
    evaluation_pipeline::{EvaluationPipeline, EvaluationPipelineConfig},
    dryrun_pipeline::{DryRunPipeline, DryRunPipelineConfig},
    trading_pipeline::{TradingPipeline, TradingPipelineConfig},
};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::Result;
use serde_json;
use tracing::{info, warn, error};

/// Python可見的Pipeline執行器
#[pyclass]
pub struct PipelineExecutor {
    /// 內部Pipeline管理器
    manager: Arc<PipelineManager>,
    /// 執行歷史緩存
    execution_cache: Arc<RwLock<HashMap<String, PipelineResult>>>,
    /// 系統配置
    config: Config,
}

/// Python可見的Pipeline結果
#[pyclass]
#[derive(Clone)]
pub struct PyPipelineResult {
    #[pyo3(get)]
    pub pipeline_id: String,
    #[pyo3(get)]
    pub pipeline_type: String,
    #[pyo3(get)]
    pub success: bool,
    #[pyo3(get)]
    pub message: String,
    #[pyo3(get)]
    pub duration_ms: u64,
    #[pyo3(get)]
    pub output_files: Vec<String>,
    #[pyo3(get)]
    pub metrics: HashMap<String, f64>,
    #[pyo3(get)]
    pub error_details: Option<String>,
}

/// Python可見的Pipeline狀態
#[pyclass]
#[derive(Clone)]
pub struct PyPipelineStatus {
    #[pyo3(get)]
    pub execution_id: String,
    #[pyo3(get)]
    pub status: String,
    #[pyo3(get)]
    pub progress: f64,
    #[pyo3(get)]
    pub current_step: String,
    #[pyo3(get)]
    pub elapsed_time_ms: u64,
    #[pyo3(get)]
    pub estimated_remaining_ms: Option<u64>,
}

/// Pipeline配置構建器
#[pyclass]
pub struct PyPipelineConfigBuilder {
    config_data: HashMap<String, serde_json::Value>,
}

#[pymethods]
impl PipelineExecutor {
    /// 創建新的Pipeline執行器
    #[new]
    #[pyo3(signature = (config_path = None))]
    pub fn new(config_path: Option<String>) -> PyResult<Self> {
        let config = if let Some(path) = config_path {
            Config::from_file(&path)
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(format!("配置加載失敗: {}", e)))?
        } else {
            Config::default()
        };
        
        let manager = Arc::new(PipelineManager::new(config.clone()));
        let execution_cache = Arc::new(RwLock::new(HashMap::new()));
        
        Ok(Self {
            manager,
            execution_cache,
            config,
        })
    }
    
    /// 執行數據處理Pipeline
    #[pyo3(signature = (config_dict, **kwargs))]
    pub fn run_data_pipeline<'py>(
        &self,
        py: Python<'py>,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<&'py PyAny> {
        let config = self.parse_data_pipeline_config(config_dict, kwargs)?;
        let manager = self.manager.clone();
        let cache = self.execution_cache.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let pipeline = DataPipeline::new(config);
            let pipeline_config = pipeline_config_from_data_config(&pipeline);
            
            let execution_id = manager.submit_pipeline(
                Box::new(pipeline),
                pipeline_config,
            ).await.map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Pipeline提交失敗: {}", e))
            })?;
            
            // 等待執行完成
            let result = wait_for_pipeline_completion(&manager, &execution_id).await?;
            
            // 緩存結果
            cache.write().await.insert(execution_id.clone(), result.clone());
            
            Ok(PyPipelineResult::from_result(result))
        })
    }
    
    /// 執行模型訓練Pipeline
    #[pyo3(signature = (config_dict, **kwargs))]
    pub fn run_training_pipeline<'py>(
        &self,
        py: Python<'py>,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<&'py PyAny> {
        let config = self.parse_training_pipeline_config(config_dict, kwargs)?;
        let manager = self.manager.clone();
        let cache = self.execution_cache.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let pipeline = TrainingPipeline::new(config);
            let pipeline_config = pipeline_config_from_training_config(&pipeline);
            
            let execution_id = manager.submit_pipeline(
                Box::new(pipeline),
                pipeline_config,
            ).await.map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Pipeline提交失敗: {}", e))
            })?;
            
            // 等待執行完成
            let result = wait_for_pipeline_completion(&manager, &execution_id).await?;
            
            // 緩存結果
            cache.write().await.insert(execution_id.clone(), result.clone());
            
            Ok(PyPipelineResult::from_result(result))
        })
    }
    
    /// 執行超參數優化Pipeline
    #[pyo3(signature = (config_dict, **kwargs))]
    pub fn run_hyperopt_pipeline<'py>(
        &self,
        py: Python<'py>,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<&'py PyAny> {
        let config = self.parse_hyperopt_pipeline_config(config_dict, kwargs)?;
        let manager = self.manager.clone();
        let cache = self.execution_cache.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let pipeline = HyperoptPipeline::new(config);
            let pipeline_config = pipeline_config_from_hyperopt_config(&pipeline);
            
            let execution_id = manager.submit_pipeline(
                Box::new(pipeline),
                pipeline_config,
            ).await.map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Pipeline提交失敗: {}", e))
            })?;
            
            // 等待執行完成
            let result = wait_for_pipeline_completion(&manager, &execution_id).await?;
            
            // 緩存結果
            cache.write().await.insert(execution_id.clone(), result.clone());
            
            Ok(PyPipelineResult::from_result(result))
        })
    }
    
    /// 執行模型評估Pipeline
    #[pyo3(signature = (config_dict, **kwargs))]
    pub fn run_evaluation_pipeline<'py>(
        &self,
        py: Python<'py>,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<&'py PyAny> {
        let config = self.parse_evaluation_pipeline_config(config_dict, kwargs)?;
        let manager = self.manager.clone();
        let cache = self.execution_cache.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let pipeline = EvaluationPipeline::new(config);
            let pipeline_config = pipeline_config_from_evaluation_config(&pipeline);
            
            let execution_id = manager.submit_pipeline(
                Box::new(pipeline),
                pipeline_config,
            ).await.map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Pipeline提交失敗: {}", e))
            })?;
            
            // 等待執行完成
            let result = wait_for_pipeline_completion(&manager, &execution_id).await?;
            
            // 緩存結果
            cache.write().await.insert(execution_id.clone(), result.clone());
            
            Ok(PyPipelineResult::from_result(result))
        })
    }
    
    /// 執行DryRun測試Pipeline
    #[pyo3(signature = (config_dict, **kwargs))]
    pub fn run_dryrun_pipeline<'py>(
        &self,
        py: Python<'py>,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<&'py PyAny> {
        let config = self.parse_dryrun_pipeline_config(config_dict, kwargs)?;
        let manager = self.manager.clone();
        let cache = self.execution_cache.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let mut pipeline = DryRunPipeline::new(config);
            let pipeline_config = pipeline_config_from_dryrun_config(&pipeline);
            
            let execution_id = manager.submit_pipeline(
                Box::new(pipeline),
                pipeline_config,
            ).await.map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Pipeline提交失敗: {}", e))
            })?;
            
            // 等待執行完成
            let result = wait_for_pipeline_completion(&manager, &execution_id).await?;
            
            // 緩存結果
            cache.write().await.insert(execution_id.clone(), result.clone());
            
            Ok(PyPipelineResult::from_result(result))
        })
    }
    
    /// 執行實盤交易Pipeline
    #[pyo3(signature = (config_dict, **kwargs))]
    pub fn run_trading_pipeline<'py>(
        &self,
        py: Python<'py>,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<&'py PyAny> {
        let config = self.parse_trading_pipeline_config(config_dict, kwargs)?;
        let manager = self.manager.clone();
        let cache = self.execution_cache.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let mut pipeline = TradingPipeline::new(config);
            let pipeline_config = pipeline_config_from_trading_config(&pipeline);
            
            let execution_id = manager.submit_pipeline(
                Box::new(pipeline),
                pipeline_config,
            ).await.map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Pipeline提交失敗: {}", e))
            })?;
            
            // 等待執行完成
            let result = wait_for_pipeline_completion(&manager, &execution_id).await?;
            
            // 緩存結果
            cache.write().await.insert(execution_id.clone(), result.clone());
            
            Ok(PyPipelineResult::from_result(result))
        })
    }
    
    /// 批量執行多個Pipeline
    pub fn run_pipeline_batch<'py>(
        &self,
        py: Python<'py>,
        pipeline_configs: &PyList,
    ) -> PyResult<&'py PyAny> {
        let configs = self.parse_pipeline_batch(pipeline_configs)?;
        let manager = self.manager.clone();
        let cache = self.execution_cache.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let mut execution_ids = Vec::new();
            
            // 提交所有Pipeline
            for config in configs {
                let execution_id = match config {
                    BatchPipelineConfig::Data(data_config) => {
                        let pipeline = DataPipeline::new(data_config);
                        let pipeline_config = pipeline_config_from_data_config(&pipeline);
                        manager.submit_pipeline(Box::new(pipeline), pipeline_config).await?
                    },
                    BatchPipelineConfig::Training(training_config) => {
                        let pipeline = TrainingPipeline::new(training_config);
                        let pipeline_config = pipeline_config_from_training_config(&pipeline);
                        manager.submit_pipeline(Box::new(pipeline), pipeline_config).await?
                    },
                };
                execution_ids.push(execution_id);
            }
            
            // 等待所有執行完成
            let mut results = Vec::new();
            for execution_id in execution_ids {
                let result = wait_for_pipeline_completion(&manager, &execution_id).await?;
                cache.write().await.insert(execution_id.clone(), result.clone());
                results.push(PyPipelineResult::from_result(result));
            }
            
            Ok(results)
        })
    }
    
    /// 獲取Pipeline執行狀態
    pub fn get_pipeline_status<'py>(
        &self,
        py: Python<'py>,
        execution_id: String,
    ) -> PyResult<&'py PyAny> {
        let manager = self.manager.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let status = manager.get_execution_status(&execution_id).await;
            
            match status {
                Some(pipeline_status) => {
                    let py_status = PyPipelineStatus {
                        execution_id: execution_id.clone(),
                        status: format!("{:?}", pipeline_status),
                        progress: calculate_progress(&pipeline_status),
                        current_step: get_current_step(&pipeline_status),
                        elapsed_time_ms: 0, // 實際實現會計算
                        estimated_remaining_ms: None,
                    };
                    Ok(Some(py_status))
                },
                None => Ok(None),
            }
        })
    }
    
    /// 取消Pipeline執行
    pub fn cancel_pipeline<'py>(
        &self,
        py: Python<'py>,
        execution_id: String,
    ) -> PyResult<&'py PyAny> {
        let manager = self.manager.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            manager.cancel_execution(&execution_id).await.map_err(|e| {
                PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("取消執行失敗: {}", e))
            })?;
            Ok(true)
        })
    }
    
    /// 獲取執行歷史
    pub fn get_execution_history<'py>(
        &self,
        py: Python<'py>,
        limit: Option<usize>,
    ) -> PyResult<&'py PyAny> {
        let manager = self.manager.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let history = manager.get_execution_history(limit).await;
            let py_results: Vec<PyPipelineResult> = history.into_iter()
                .map(PyPipelineResult::from_result)
                .collect();
            Ok(py_results)
        })
    }
    
    /// 獲取性能統計
    pub fn get_performance_stats<'py>(&self, py: Python<'py>) -> PyResult<&'py PyAny> {
        let manager = self.manager.clone();
        
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let stats = manager.get_performance_stats().await;
            
            let py_stats = py.eval(
                "{'total_executions': 0, 'success_rate': 0.0, 'avg_duration_ms': 0.0}",
                None,
                None,
            )?;
            
            // 更新統計數據
            let stats_dict = py_stats.downcast::<PyDict>()?;
            stats_dict.set_item("total_executions", stats.total_executions)?;
            stats_dict.set_item("success_rate", stats.successful_executions as f64 / stats.total_executions.max(1) as f64)?;
            stats_dict.set_item("avg_duration_ms", stats.average_duration_ms)?;
            
            Ok(py_stats.to_object(py))
        })
    }
    
    /// 創建配置構建器
    #[staticmethod]
    pub fn config_builder() -> PyPipelineConfigBuilder {
        PyPipelineConfigBuilder::new()
    }
    
    /// 驗證Pipeline配置
    pub fn validate_config(&self, config_dict: &PyDict, pipeline_type: String) -> PyResult<bool> {
        match pipeline_type.as_str() {
            "data" => {
                let config = self.parse_data_pipeline_config(config_dict, None)?;
                let pipeline = DataPipeline::new(config);
                // 配置驗證邏輯
                Ok(true)
            },
            "training" => {
                let config = self.parse_training_pipeline_config(config_dict, None)?;
                let pipeline = TrainingPipeline::new(config);
                // 配置驗證邏輯
                Ok(true)
            },
            _ => Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("不支持的Pipeline類型: {}", pipeline_type)
            )),
        }
    }
    
    /// 獲取默認配置模板
    #[staticmethod]
    pub fn get_default_config(pipeline_type: String) -> PyResult<PyObject> {
        Python::with_gil(|py| {
            let config_json = match pipeline_type.as_str() {
                "data" => get_default_data_config_json(),
                "training" => get_default_training_config_json(),
                _ => return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                    format!("不支持的Pipeline類型: {}", pipeline_type)
                )),
            };
            
            let json_module = py.import("json")?;
            let config_dict = json_module.call_method1("loads", (config_json,))?;
            Ok(config_dict.to_object(py))
        })
    }
    
    /// 從文件加載配置
    #[staticmethod]
    pub fn load_config_from_file(file_path: String) -> PyResult<PyObject> {
        Python::with_gil(|py| {
            let content = std::fs::read_to_string(&file_path)
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(
                    format!("讀取配置文件失敗: {}", e)
                ))?;
            
            let yaml_module = py.import("yaml")?;
            let config_dict = yaml_module.call_method1("safe_load", (content,))?;
            Ok(config_dict.to_object(py))
        })
    }
    
    /// 保存配置到文件
    #[staticmethod]
    pub fn save_config_to_file(config_dict: &PyDict, file_path: String) -> PyResult<()> {
        Python::with_gil(|py| {
            let yaml_module = py.import("yaml")?;
            let yaml_content = yaml_module.call_method1("dump", (config_dict,))?;
            let content: String = yaml_content.extract()?;
            
            std::fs::write(&file_path, content)
                .map_err(|e| PyErr::new::<pyo3::exceptions::PyIOError, _>(
                    format!("保存配置文件失敗: {}", e)
                ))?;
            
            Ok(())
        })
    }
}

#[pymethods]
impl PyPipelineConfigBuilder {
    #[new]
    pub fn new() -> Self {
        Self {
            config_data: HashMap::new(),
        }
    }
    
    /// 設置基礎配置
    pub fn set_basic_config(
        &mut self,
        name: String,
        version: String,
        description: Option<String>,
        timeout_seconds: u64,
    ) -> PyResult<()> {
        self.config_data.insert("name".to_string(), serde_json::Value::String(name));
        self.config_data.insert("version".to_string(), serde_json::Value::String(version));
        if let Some(desc) = description {
            self.config_data.insert("description".to_string(), serde_json::Value::String(desc));
        }
        self.config_data.insert("timeout_seconds".to_string(), serde_json::Value::Number(timeout_seconds.into()));
        Ok(())
    }
    
    /// 設置數據Pipeline配置
    pub fn set_data_config(
        &mut self,
        input_paths: Vec<String>,
        output_dir: String,
        batch_size: usize,
    ) -> PyResult<()> {
        let input_config = serde_json::json!({
            "source_paths": input_paths,
            "batch_size": batch_size,
            "parallel_reading": true
        });
        
        let output_config = serde_json::json!({
            "output_directory": output_dir,
            "format": "Parquet",
            "compression": "Snappy"
        });
        
        self.config_data.insert("input".to_string(), input_config);
        self.config_data.insert("output".to_string(), output_config);
        Ok(())
    }
    
    /// 設置訓練Pipeline配置
    pub fn set_training_config(
        &mut self,
        data_path: String,
        model_configs: &PyList,
    ) -> PyResult<()> {
        let data_config = serde_json::json!({
            "feature_data_path": data_path,
            "train_ratio": 0.7,
            "validation_ratio": 0.2,
            "test_ratio": 0.1
        });
        
        let models: Result<Vec<serde_json::Value>, PyErr> = model_configs.iter()
            .map(|item| {
                let model_dict = item.downcast::<PyDict>()?;
                let json_str = Python::with_gil(|py| {
                    let json_module = py.import("json")?;
                    let json_str = json_module.call_method1("dumps", (model_dict,))?;
                    json_str.extract::<String>()
                })?;
                serde_json::from_str(&json_str)
                    .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
                        format!("模型配置解析失敗: {}", e)
                    ))
            })
            .collect();
        
        self.config_data.insert("data".to_string(), data_config);
        self.config_data.insert("models".to_string(), serde_json::Value::Array(models?));
        Ok(())
    }
    
    /// 構建配置字典
    pub fn build(&self, py: Python) -> PyResult<PyObject> {
        let json_str = serde_json::to_string(&self.config_data)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("配置序列化失敗: {}", e)
            ))?;
        
        let json_module = py.import("json")?;
        let config_dict = json_module.call_method1("loads", (json_str,))?;
        Ok(config_dict.to_object(py))
    }
    
    /// 添加自定義配置項
    pub fn add_custom_config(&mut self, key: String, value: &PyAny) -> PyResult<()> {
        let json_value = python_to_json_value(value)?;
        self.config_data.insert(key, json_value);
        Ok(())
    }
}

impl PyPipelineResult {
    fn from_result(result: PipelineResult) -> Self {
        Self {
            pipeline_id: result.pipeline_id,
            pipeline_type: result.pipeline_type,
            success: result.success,
            message: result.message,
            duration_ms: result.duration_ms.unwrap_or(0),
            output_files: result.output_artifacts,
            metrics: result.metrics,
            error_details: result.error_details,
        }
    }
}

// 內部實現的Pipeline執行器
impl PipelineExecutor {
    /// 解析數據Pipeline配置
    fn parse_data_pipeline_config(
        &self,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<DataPipelineConfig> {
        let mut config_json = python_dict_to_json_value(config_dict)?;
        
        // 合併kwargs
        if let Some(kwargs) = kwargs {
            let kwargs_json = python_dict_to_json_value(kwargs)?;
            merge_json_values(&mut config_json, kwargs_json);
        }
        
        serde_json::from_value(config_json)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("數據Pipeline配置解析失敗: {}", e)
            ))
    }
    
    /// 解析訓練Pipeline配置
    fn parse_training_pipeline_config(
        &self,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<TrainingPipelineConfig> {
        let mut config_json = python_dict_to_json_value(config_dict)?;
        
        // 合併kwargs
        if let Some(kwargs) = kwargs {
            let kwargs_json = python_dict_to_json_value(kwargs)?;
            merge_json_values(&mut config_json, kwargs_json);
        }
        
        serde_json::from_value(config_json)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("訓練Pipeline配置解析失敗: {}", e)
            ))
    }
    
    /// 解析超參數優化Pipeline配置
    fn parse_hyperopt_pipeline_config(
        &self,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<HyperoptPipelineConfig> {
        let mut config_json = python_dict_to_json_value(config_dict)?;
        
        // 合併kwargs
        if let Some(kwargs) = kwargs {
            let kwargs_json = python_dict_to_json_value(kwargs)?;
            merge_json_values(&mut config_json, kwargs_json);
        }
        
        serde_json::from_value(config_json)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("超參數優化Pipeline配置解析失敗: {}", e)
            ))
    }
    
    /// 解析評估Pipeline配置
    fn parse_evaluation_pipeline_config(
        &self,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<EvaluationPipelineConfig> {
        let mut config_json = python_dict_to_json_value(config_dict)?;
        
        // 合併kwargs
        if let Some(kwargs) = kwargs {
            let kwargs_json = python_dict_to_json_value(kwargs)?;
            merge_json_values(&mut config_json, kwargs_json);
        }
        
        serde_json::from_value(config_json)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("評估Pipeline配置解析失敗: {}", e)
            ))
    }
    
    /// 解析DryRun Pipeline配置
    fn parse_dryrun_pipeline_config(
        &self,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<DryRunPipelineConfig> {
        let mut config_json = python_dict_to_json_value(config_dict)?;
        
        // 合併kwargs
        if let Some(kwargs) = kwargs {
            let kwargs_json = python_dict_to_json_value(kwargs)?;
            merge_json_values(&mut config_json, kwargs_json);
        }
        
        serde_json::from_value(config_json)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("DryRun Pipeline配置解析失敗: {}", e)
            ))
    }
    
    /// 解析實盤交易Pipeline配置
    fn parse_trading_pipeline_config(
        &self,
        config_dict: &PyDict,
        kwargs: Option<&PyDict>,
    ) -> PyResult<TradingPipelineConfig> {
        let mut config_json = python_dict_to_json_value(config_dict)?;
        
        // 合併kwargs
        if let Some(kwargs) = kwargs {
            let kwargs_json = python_dict_to_json_value(kwargs)?;
            merge_json_values(&mut config_json, kwargs_json);
        }
        
        serde_json::from_value(config_json)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
                format!("實盤交易Pipeline配置解析失敗: {}", e)
            ))
    }
    
    /// 解析批量Pipeline配置
    fn parse_pipeline_batch(&self, pipeline_configs: &PyList) -> PyResult<Vec<BatchPipelineConfig>> {
        let mut configs = Vec::new();
        
        for item in pipeline_configs.iter() {
            let config_dict = item.downcast::<PyDict>()?;
            let pipeline_type = config_dict.get_item("pipeline_type")?
                .ok_or_else(|| PyErr::new::<pyo3::exceptions::PyKeyError, _>("缺少pipeline_type字段"))?
                .extract::<String>()?;
            
            match pipeline_type.as_str() {
                "data" => {
                    let data_config = self.parse_data_pipeline_config(config_dict, None)?;
                    configs.push(BatchPipelineConfig::Data(data_config));
                },
                "training" => {
                    let training_config = self.parse_training_pipeline_config(config_dict, None)?;
                    configs.push(BatchPipelineConfig::Training(training_config));
                },
                _ => return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
                    format!("不支持的Pipeline類型: {}", pipeline_type)
                )),
            }
        }
        
        Ok(configs)
    }
}

// 輔助函數
async fn wait_for_pipeline_completion(
    manager: &PipelineManager,
    execution_id: &str,
) -> Result<PipelineResult, PyErr> {
    // 輪詢等待執行完成
    loop {
        if let Some(status) = manager.get_execution_status(execution_id).await {
            match status {
                PipelineStatus::Completed | PipelineStatus::Failed | PipelineStatus::Cancelled | PipelineStatus::Timeout => {
                    // 從歷史中獲取結果
                    let history = manager.get_execution_history(Some(100)).await;
                    if let Some(result) = history.iter().find(|r| r.pipeline_id == execution_id) {
                        return Ok(result.clone());
                    }
                    break;
                },
                _ => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
            }
        } else {
            break;
        }
    }
    
    Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
        format!("無法獲取執行結果: {}", execution_id)
    ))
}

fn calculate_progress(status: &PipelineStatus) -> f64 {
    match status {
        PipelineStatus::Pending => 0.0,
        PipelineStatus::Initializing => 0.1,
        PipelineStatus::Running => 0.5,
        PipelineStatus::Paused => 0.5,
        PipelineStatus::Completed => 1.0,
        PipelineStatus::Failed => 0.0,
        PipelineStatus::Cancelled => 0.0,
        PipelineStatus::Timeout => 0.0,
    }
}

fn get_current_step(status: &PipelineStatus) -> String {
    match status {
        PipelineStatus::Pending => "等待執行".to_string(),
        PipelineStatus::Initializing => "初始化中".to_string(),
        PipelineStatus::Running => "執行中".to_string(),
        PipelineStatus::Paused => "已暫停".to_string(),
        PipelineStatus::Completed => "已完成".to_string(),
        PipelineStatus::Failed => "執行失敗".to_string(),
        PipelineStatus::Cancelled => "已取消".to_string(),
        PipelineStatus::Timeout => "執行超時".to_string(),
    }
}

fn python_dict_to_json_value(py_dict: &PyDict) -> PyResult<serde_json::Value> {
    let json_str = Python::with_gil(|py| {
        let json_module = py.import("json")?;
        let json_str = json_module.call_method1("dumps", (py_dict,))?;
        json_str.extract::<String>()
    })?;
    
    serde_json::from_str(&json_str)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
            format!("JSON解析失敗: {}", e)
        ))
}

fn python_to_json_value(py_obj: &PyAny) -> PyResult<serde_json::Value> {
    let json_str = Python::with_gil(|py| {
        let json_module = py.import("json")?;
        let json_str = json_module.call_method1("dumps", (py_obj,))?;
        json_str.extract::<String>()
    })?;
    
    serde_json::from_str(&json_str)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
            format!("JSON解析失敗: {}", e)
        ))
}

fn merge_json_values(base: &mut serde_json::Value, other: serde_json::Value) {
    if let (serde_json::Value::Object(base_map), serde_json::Value::Object(other_map)) = (base, other) {
        for (key, value) in other_map {
            base_map.insert(key, value);
        }
    }
}

// 默認配置模板
fn get_default_data_config_json() -> String {
    serde_json::json!({
        "base": {
            "name": "data_pipeline",
            "version": "1.0",
            "priority": "Medium",
            "timeout_seconds": 3600,
            "max_retries": 3,
            "output_directory": "./output"
        },
        "input": {
            "source_paths": ["./data/raw"],
            "format": "Parquet",
            "batch_size": 1000,
            "parallel_reading": true
        },
        "cleaning": {
            "outlier_threshold": 3.0,
            "remove_outliers": true,
            "missing_value_strategy": "ForwardFill",
            "deduplicate": true,
            "validate_timestamps": true
        },
        "feature_engineering": {
            "time_windows": [30, 60, 120, 300],
            "feature_types": {
                "include_obi": true,
                "include_depth_features": true,
                "include_spread_features": true,
                "include_momentum_features": true,
                "include_volatility_features": true
            }
        },
        "validation": {
            "min_quality_score": 0.9,
            "min_records": 1000,
            "completeness_check": true,
            "distribution_check": true
        }
    }).to_string()
}

fn get_default_training_config_json() -> String {
    serde_json::json!({
        "base": {
            "name": "training_pipeline",
            "version": "1.0",
            "priority": "High",
            "timeout_seconds": 7200,
            "max_retries": 2,
            "output_directory": "./models"
        },
        "data": {
            "feature_data_path": "./features/processed.parquet",
            "train_ratio": 0.7,
            "validation_ratio": 0.2,
            "test_ratio": 0.1,
            "time_series_split": true
        },
        "models": [
            {
                "name": "transformer_model",
                "model_type": "Transformer",
                "enabled": true,
                "ensemble_weight": 0.4,
                "architecture": {
                    "sequence_length": 128,
                    "hidden_size": 256,
                    "num_heads": 8,
                    "num_layers": 6,
                    "input_size": 64,
                    "output_size": 1,
                    "dropout": 0.1
                },
                "hyperparameters": {
                    "learning_rate": 0.0001,
                    "batch_size": 64,
                    "epochs": 100,
                    "optimizer": "AdamW",
                    "weight_decay": 0.01
                }
            }
        ],
        "training": {
            "parallel_training": 1,
            "random_seed": 42,
            "use_gpu": true,
            "mixed_precision": false,
            "checkpoint_interval": 10
        },
        "validation": {
            "metrics": ["RMSE", "R2", "SharpeRatio"],
            "primary_metric": "R2",
            "validation_frequency": 5
        }
    }).to_string()
}

// 從Pipeline配置創建基礎配置的輔助函數
fn pipeline_config_from_data_config(pipeline: &DataPipeline) -> PipelineConfig {
    // 這裡需要從DataPipeline提取配置
    // 暫時返回默認配置
    PipelineConfig {
        name: "data_pipeline".to_string(),
        version: "1.0".to_string(),
        description: Some("數據處理Pipeline".to_string()),
        priority: crate::pipelines::PipelinePriority::Medium,
        timeout_seconds: 3600,
        max_retries: 3,
        resource_limits: crate::pipelines::ResourceLimits::default(),
        environment: HashMap::new(),
        output_directory: "./output".to_string(),
        verbose_logging: true,
        continue_on_failure: false,
    }
}

fn pipeline_config_from_training_config(pipeline: &TrainingPipeline) -> PipelineConfig {
    // 這裡需要從TrainingPipeline提取配置
    // 暫時返回默認配置
    PipelineConfig {
        name: "training_pipeline".to_string(),
        version: "1.0".to_string(),
        description: Some("模型訓練Pipeline".to_string()),
        priority: crate::pipelines::PipelinePriority::High,
        timeout_seconds: 7200,
        max_retries: 2,
        resource_limits: crate::pipelines::ResourceLimits::default(),
        environment: HashMap::new(),
        output_directory: "./models".to_string(),
        verbose_logging: true,
        continue_on_failure: false,
    }
}

fn pipeline_config_from_hyperopt_config(pipeline: &HyperoptPipeline) -> PipelineConfig {
    PipelineConfig {
        name: "hyperopt_pipeline".to_string(),
        version: "1.0".to_string(),
        description: Some("超參數優化Pipeline".to_string()),
        priority: crate::pipelines::PipelinePriority::High,
        timeout_seconds: 14400, // 4小時
        max_retries: 1,
        resource_limits: crate::pipelines::ResourceLimits::default(),
        environment: HashMap::new(),
        output_directory: "./hyperopt_results".to_string(),
        verbose_logging: true,
        continue_on_failure: false,
    }
}

fn pipeline_config_from_evaluation_config(pipeline: &EvaluationPipeline) -> PipelineConfig {
    PipelineConfig {
        name: "evaluation_pipeline".to_string(),
        version: "1.0".to_string(),
        description: Some("模型評估Pipeline".to_string()),
        priority: crate::pipelines::PipelinePriority::Medium,
        timeout_seconds: 3600,
        max_retries: 2,
        resource_limits: crate::pipelines::ResourceLimits::default(),
        environment: HashMap::new(),
        output_directory: "./evaluation_results".to_string(),
        verbose_logging: true,
        continue_on_failure: false,
    }
}

fn pipeline_config_from_dryrun_config(pipeline: &DryRunPipeline) -> PipelineConfig {
    PipelineConfig {
        name: "dryrun_pipeline".to_string(),
        version: "1.0".to_string(),
        description: Some("DryRun測試Pipeline".to_string()),
        priority: crate::pipelines::PipelinePriority::Medium,
        timeout_seconds: 7200,
        max_retries: 1,
        resource_limits: crate::pipelines::ResourceLimits::default(),
        environment: HashMap::new(),
        output_directory: "./dryrun_results".to_string(),
        verbose_logging: true,
        continue_on_failure: false,
    }
}

fn pipeline_config_from_trading_config(pipeline: &TradingPipeline) -> PipelineConfig {
    PipelineConfig {
        name: "trading_pipeline".to_string(),
        version: "1.0".to_string(),
        description: Some("實盤交易Pipeline".to_string()),
        priority: crate::pipelines::PipelinePriority::Critical,
        timeout_seconds: 86400, // 24小時
        max_retries: 0, // 實盤交易不允許重試
        resource_limits: crate::pipelines::ResourceLimits::default(),
        environment: HashMap::new(),
        output_directory: "./trading_results".to_string(),
        verbose_logging: true,
        continue_on_failure: false,
    }
}

/// 批量Pipeline配置枚舉
enum BatchPipelineConfig {
    Data(DataPipelineConfig),
    Training(TrainingPipelineConfig),
    Hyperopt(HyperoptPipelineConfig),
    Evaluation(EvaluationPipelineConfig),
    DryRun(DryRunPipelineConfig),
    Trading(TradingPipelineConfig),
}

// 註冊Python類
pub fn register_pipeline_classes(m: &PyModule) -> PyResult<()> {
    m.add_class::<PipelineExecutor>()?;
    m.add_class::<PyPipelineResult>()?;
    m.add_class::<PyPipelineStatus>()?;
    m.add_class::<PyPipelineConfigBuilder>()?;
    Ok(())
}