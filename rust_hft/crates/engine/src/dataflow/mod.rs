//! 數據流管線與零拷貝消息傳遞（骨架）
pub mod ring_buffer;
// 實驗性 MPMC 隊列：僅在基準/實驗需要時導出，避免污染主API
#[cfg(feature = "bench-lockfree")]
pub mod lockfree_queue;
pub mod ingestion;

pub use ingestion::{
    EventIngester, EventConsumer, IngestionConfig, FlipPolicy, BackpressurePolicy
};
