/*!
 * Trading Strategies - 策略模块
 * 
 * 将761行的strategy.rs拆分为可管理的模块
 */

pub mod traits;
pub mod basic;
pub mod advanced;

// Re-export main types
pub use traits::{TradingStrategy, StrategyContext, StrategyResult};
pub use basic::{BasicStrategy, MomentumStrategy, MeanReversionStrategy};
pub use advanced::{MLStrategy, CompositeStrategy, AdaptiveStrategy};