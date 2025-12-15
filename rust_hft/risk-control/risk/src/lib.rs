//! Pre-trade risk management primitives
//! - Limits, rate control, staleness, slippage guards
//! - Runs in the same single-writer thread as the engine
//! - Sentinel: automated risk guard (replaces Python control_ws)

pub mod default_risk_manager;
pub mod enhanced_risk_manager;
pub mod sentinel;
pub mod simplified_professional_risk;

pub use default_risk_manager::{DefaultRiskManager, PrecisionNormalizer, RiskConfig};
pub use enhanced_risk_manager::{
    EnhancedRiskConfig, EnhancedRiskManager, RiskReport, TradingWindow,
};
pub use simplified_professional_risk::{
    SimplifiedProfessionalRiskManager, SimplifiedRiskConfig, SimplifiedRiskDecision,
    SimplifiedRiskStats,
};
pub use sentinel::{
    Sentinel, SentinelAction, SentinelConfig, SentinelState, SentinelStats, SystemStats,
};

// 重新导出 ports 中的 trait
pub use ports::RiskManager;
