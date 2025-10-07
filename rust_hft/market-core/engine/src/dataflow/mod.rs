//! 數據流管線與零拷貝消息傳遞（骨架）
pub mod ring_buffer;
// 實驗性 MPMC 隊列：僅在基準/實驗需要時導出，避免污染主API
pub mod ingestion;
#[cfg(feature = "bench-lockfree")]
pub mod lockfree_queue;

pub use ingestion::{
    BackpressurePolicy, BackpressureStatus, EventConsumer, EventIngester, FlipPolicy,
    IngestionConfig, IngestionMetrics,
};
// 直接從 ports 導出 TrackedMarketEvent（而非從 ingestion 轉導）
pub use ports::events::TrackedMarketEvent;
