/*!
 * Utils Module - 工具和輔助功能
 * 
 * 包含性能優化、數據存儲、回測框架和真實延遲測量
 */

pub mod performance;
pub mod performance_monitor;
pub mod feature_store;
pub mod backtesting;
pub mod ultra_low_latency;
pub mod precise_timing;
pub mod network_latency;
pub mod concurrency_safety;
pub mod memory_optimization;
pub mod parallel_processing;

pub use performance::{PerformanceManager, PerformanceConfig, HardwareCapabilities, detect_hardware_capabilities};
pub use performance_monitor::{
    PerformanceMonitor, MonitorConfig, LatencyStats, ThroughputStats, 
    MemoryStats, SystemStats, PerformanceReport, create_default_monitor, 
    create_high_performance_config
};
pub use feature_store::{FeatureStore, FeatureStoreConfig};
pub use backtesting::{BacktestEngine, BacktestConfig};

// Ultra Low Latency exports
pub use ultra_low_latency::{
    UltraLowLatencyExecutor,
    CpuAffinityManager,
    SIMDFeatureProcessor,
    LatencyMeasurer,
    LatencyStats as UltraLatencyStats,
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

// Concurrency Safety exports
pub use concurrency_safety::{
    SafeLockManager,
    LockOrder,
    LockResult,
    LockContentionStats,
    LockFreeCounter,
    DEFAULT_LOCK_TIMEOUT,
    GLOBAL_LOCK_MANAGER,
};

// Memory Optimization exports
pub use memory_optimization::{
    MemoryStats,
    PoolStats,
    MemoryTracker,
    ZeroAllocVec,
    MemoryPool,
    CowPtr,
    StackAllocator,
    ZeroCopyStr,
    MemoryManager,
    GLOBAL_MEMORY_MANAGER,
};

// Parallel Processing exports
pub use parallel_processing::{
    ParallelConfig,
    ParallelStats,
    ParallelProcessor,
    FeatureTask,
    TechnicalIndicators,
    ValidationResult,
    OHLCVData,
    GLOBAL_PARALLEL_PROCESSOR,
};