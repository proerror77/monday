/*!
 * Unified Trading Engine - 重构入口
 * 
 * 从768行的巨大文件重构为模块化结构
 */

// Re-export the modular version
pub use self::unified::*;

// Import modular unified engine
pub mod unified;