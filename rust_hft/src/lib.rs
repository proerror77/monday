/*!
 * Rust HFT - High-Frequency Trading System Library
 * 
 * Pure Rust implementation targeting sub-100μs latency
 * 
 * ## Architecture
 * 
 * - **core**: 核心類型、配置和訂單簿實現
 * - **data**: 數據接收、處理和記錄
 * - **engine**: 交易策略和執行引擎  
 * - **integrations**: 外部交易所和框架集成
 * - **ml**: 機器學習和特徵工程
 * - **utils**: 性能優化、存儲和回測工具
 */

// 核心模塊 - 基礎類型和配置
pub mod core;

// 數據處理模塊 - 網絡、解析、記錄
pub mod data;

// 交易引擎模塊 - 策略和執行
pub mod engine;

// 外部集成模塊 - 交易所連接器和適配器
pub mod integrations;

// 交易所抽象模塊 - 統一的交易所接口
pub mod exchanges;

// 機器學習模塊 - 特徵工程和模型
pub mod ml;

// 工具模塊 - 性能、存儲、回測
pub mod utils;

// 資料庫模塊 - ClickHouse 客戶端和存儲
pub mod database;

// 測試模塊 - 模擬和驗證框架
pub mod testing;


// Python 綁定模塊 - PyO3 接口
#[cfg(feature = "python")]
pub mod python_bindings;

// Re-export commonly used types from core
pub use crate::core::*;

// Re-export specific types for examples
pub use crate::core::workflow::{WorkflowExecutor, WorkflowStep, StepResult, WorkflowConfig, WorkflowReport};
pub use crate::core::app_runner::{HftAppRunner, UnifiedReporter, run_simple_app, run_timed_app};
// Note: cli and types exports removed as unused
pub use crate::core::config::Config;
pub use crate::core::orderbook::OrderBook;

// Re-export utils
pub use crate::utils::{HardwareCapabilities, detect_hardware_capabilities};

// 當編譯為Python擴展時，重新導出Python模塊函數
#[cfg(feature = "python")]
pub use python_bindings::rust_hft_py;