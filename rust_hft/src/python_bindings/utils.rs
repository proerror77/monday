/*!
 * 工具函數 Python 綁定
 */

use pyo3::prelude::*;
use std::collections::HashMap;

/// 獲取系統信息
#[pyfunction]
pub fn get_system_info() -> PyResult<HashMap<String, String>> {
    let mut info = HashMap::new();
    
    info.insert("version".to_string(), env!("CARGO_PKG_VERSION").to_string());
    info.insert("rust_version".to_string(), "1.75+".to_string());
    info.insert("platform".to_string(), std::env::consts::OS.to_string());
    info.insert("arch".to_string(), std::env::consts::ARCH.to_string());
    
    Ok(info)
}

/// 驗證交易對符號
#[pyfunction]
pub fn validate_symbol(symbol: &str) -> PyResult<bool> {
    // 簡單的符號驗證邏輯
    let valid_symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "ADAUSDT"];
    Ok(valid_symbols.contains(&symbol.to_uppercase().as_str()))
}