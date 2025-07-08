/*!
 * 數據處理器 Python 綁定
 */

use pyo3::prelude::*;
use numpy::PyArray2;
use std::collections::HashMap;

/// 數據處理器
#[pyclass]
pub struct DataProcessor {
    buffer_size: usize,
}

#[pymethods]
impl DataProcessor {
    #[new]
    #[pyo3(signature = (buffer_size=1000))]
    pub fn new(buffer_size: usize) -> Self {
        Self {
            buffer_size,
        }
    }
    
    /// 處理LOB數據
    pub fn process_lob_data<'py>(
        &self,
        py: Python<'py>,
        data: Vec<Vec<f64>>,
    ) -> PyResult<Bound<'py, PyArray2<f64>>> {
        // 將數據轉換為numpy數組
        if data.is_empty() {
            return Ok(PyArray2::zeros_bound(py, (0, 0), false));
        }
        
        // 創建numpy數組
        let array = PyArray2::from_vec2_bound(py, &data)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("Failed to create array: {}", e)))?;
        
        Ok(array)
    }
    
    /// 提取特徵
    pub fn extract_features(&self, symbol: String) -> PyResult<HashMap<String, f64>> {
        let mut features = HashMap::new();
        features.insert("spread".to_string(), 0.001);
        features.insert("mid_price".to_string(), 45000.0);
        features.insert("volume".to_string(), 1000.0);
        Ok(features)
    }
}