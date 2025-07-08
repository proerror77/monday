/*!
 * Python 綁定模塊 - 使用 PyO3 將 Rust HFT 功能暴露給 Python
 * 
 * 這個模塊提供了Python和Rust之間的高性能接口，
 * 使得Python agno agent可以直接調用Rust HFT功能
 */

use pyo3::prelude::*;

// 核心HFT引擎綁定
pub mod hft_engine;

// 模型訓練綁定
pub mod model_trainer;

// 模型評估綁定  
pub mod model_evaluator;

// 交易執行綁定
pub mod trading_executor;

// 數據處理綁定
pub mod data_processor;

// 工具函數綁定
pub mod utils;

// 重新導出核心類型
pub use hft_engine::*;
pub use model_trainer::*;
pub use model_evaluator::*;
pub use trading_executor::*;
pub use data_processor::*;
pub use utils::*;

/// Python模塊定義
#[pymodule]
fn rust_hft_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // 添加版本信息
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    
    // 添加核心類
    m.add_class::<hft_engine::HftEngine>()?;
    m.add_class::<model_trainer::ModelTrainer>()?;
    m.add_class::<model_evaluator::ModelEvaluator>()?;
    m.add_class::<trading_executor::TradingExecutor>()?;
    m.add_class::<data_processor::DataProcessor>()?;
    
    // 添加工具函數
    m.add_function(wrap_pyfunction!(utils::get_system_info, m)?)?;
    m.add_function(wrap_pyfunction!(utils::validate_symbol, m)?)?;
    
    Ok(())
}