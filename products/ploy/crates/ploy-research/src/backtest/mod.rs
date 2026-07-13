pub mod engine;
pub mod metrics;
pub use engine::{run_binary_backtest, SimulatedFill};
pub use metrics::BacktestMetrics;
