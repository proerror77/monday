/*!
 * Utils Module - 工具和輔助功能
 * 
 * 包含性能優化、數據存儲、回測框架和真實延遲測量
 */

pub mod performance;
pub mod feature_store;
pub mod backtesting;
pub mod ultra_low_latency;
pub mod precise_timing;
pub mod network_latency;

pub use performance::{PerformanceManager, PerformanceConfig, HardwareCapabilities, detect_hardware_capabilities};
pub use feature_store::{FeatureStore, FeatureStoreConfig};
pub use backtesting::{BacktestEngine, BacktestConfig};

// Ultra Low Latency exports
pub use ultra_low_latency::{
    UltraLowLatencyExecutor,
    CpuAffinityManager,
    SIMDFeatureProcessor,
    LatencyMeasurer,
    LatencyStats,
    HighPerfSPSCQueue,
    ZeroAllocMemoryPool,
    HighPrecisionTimer,
};

// Precise Timing exports
pub use precise_timing::{
    PreciseTimeSync,
    TimeSyncConfig,
    TimeSyncStatus,
    LatencyAnalyzer,
    LatencyMeasurement,
    LatencyBreakdown,
    LatencyStats as PreciseLatencyStats,
};

// Network Latency exports
pub use network_latency::{
    NetworkLatencyMonitor,
    NetworkLatencyConfig,
    RealLatencyMeasurer,
    ComprehensiveLatencyStats,
    NetworkLatencyStats,
};