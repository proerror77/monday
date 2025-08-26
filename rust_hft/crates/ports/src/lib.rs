//! 穩定介面定義 - Never Break Userspace
//!
//! 此 crate 定義系統的所有穩定接口：
//! - 事件模型 (MarketEvent, ExecutionEvent)
//! - 核心 traits (MarketStream, ExecutionClient, Strategy, RiskManager)
//! - 適配器必須實現的契約

pub mod events;
pub mod traits;

// 重新導出
pub use events::*;
pub use traits::*;
pub use hft_core::{HftResult, HftError};
