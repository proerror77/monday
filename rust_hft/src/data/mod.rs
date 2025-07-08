/*!
 * Data Module - 數據接收和處理
 * 
 * 包含網絡數據接收、消息處理和訂單簿記錄功能
 */

pub mod network;
pub mod processor;
pub mod orderbook_recorder;

// Re-export main functions
pub use network::run as network_run;
pub use processor::run as processor_run;
pub use orderbook_recorder::SymbolRecorder;