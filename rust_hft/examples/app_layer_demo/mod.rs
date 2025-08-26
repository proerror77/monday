/*!
 * 應用層模組 (Application Layer)
 *
 * 包含應用服務、門面封裝和業務流程編排。
 * 此層連接域模型與基礎設施，提供清晰的業務 API。
 */

pub mod config;
pub mod events;
pub mod routing;
pub mod services;

// 預留應用服務模組
// pub mod facades;       // 門面封裝
// pub mod workflows;     // 業務流程編排
// pub mod handlers;      // 事件處理器

// 重新導出常用類型
pub use config::*;
pub use events::*;
pub use routing::*;
pub use services::*;
