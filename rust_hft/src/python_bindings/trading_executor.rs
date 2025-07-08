/*!
 * 交易執行器 Python 綁定
 */

use pyo3::prelude::*;
use std::collections::HashMap;

/// 交易執行器
#[pyclass]
pub struct TradingExecutor {
    is_running: bool,
    mode: String,
}

#[pymethods]
impl TradingExecutor {
    #[new]
    pub fn new() -> Self {
        Self {
            is_running: false,
            mode: "dryrun".to_string(),
        }
    }
    
    /// 設置交易模式
    pub fn set_mode(&mut self, mode: String) -> PyResult<()> {
        if mode == "dryrun" || mode == "live" {
            self.mode = mode;
            Ok(())
        } else {
            Err(pyo3::exceptions::PyValueError::new_err("Invalid mode"))
        }
    }
    
    /// 開始交易
    pub fn start_trading(&mut self, symbol: String, capital: f64) -> PyResult<String> {
        self.is_running = true;
        let task_id = uuid::Uuid::new_v4().to_string();
        Ok(task_id)
    }
    
    /// 停止交易
    pub fn stop_trading(&mut self) -> PyResult<bool> {
        self.is_running = false;
        Ok(true)
    }
    
    /// 獲取交易狀態
    pub fn get_status(&self) -> PyResult<HashMap<String, String>> {
        let mut status = HashMap::new();
        status.insert("running".to_string(), self.is_running.to_string());
        status.insert("mode".to_string(), self.mode.clone());
        Ok(status)
    }
}