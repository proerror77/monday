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
pub mod memory_preallocation;
pub mod cpu_optimization;
pub mod timestamp_optimization;
pub mod network_optimization;

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
    LockFreeRingBuffer,
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
    NetworkLatencyMonitor as NetworkLatencyTracker,
    NetworkLatencyConfig,
    RealLatencyMeasurer,
    ComprehensiveLatencyStats,
    NetworkLatencyStats,
};

// Memory Optimization exports
pub use memory_preallocation::{
    HftAllocator,
    MemoryPool,
    PooledObject,
    CacheAlignedAllocator,
    PreAllocatedRingBuffer,
    MemoryMappedRegion,
    NumaAllocator,
    MemoryOptimizer,
    AllocatorStats,
    PoolStats,
};

// CPU Optimization exports
pub use cpu_optimization::{
    CpuOptimizer,
    CpuOptConfig,
    SchedulingPolicy,
    ThreadInfo,
    CpuUsageMonitor,
    SimdProcessor,
    PerformanceCounters,
    CpuStats,
    PerfCounters,
    SimdCapabilities,
};

// Memory Preallocation exports (updated)
pub use memory_preallocation::{
    PreallocatedMemoryManager,
    MemoryConfig,
    AdvancedMemoryStats,
    // MemoryOptimizer is already exported above, don't duplicate
    // All other original exports are already defined above
};

// Network Optimization exports
pub use network_optimization::{
    OptimizedTcpConnection,
    NetworkOptConfig,
    MessageBatchConfig,
    NetworkStats,
    NetworkLatencyMeasurer,
    LatencyStats as NetworkOptLatencyStats,
};