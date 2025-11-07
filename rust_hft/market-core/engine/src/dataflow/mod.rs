//! 數據流管線與零拷貝消息傳遞
//!
//! # 架構設計
//!
//! 提供兩套實現，根據場景選擇：
//!
//! 1. **標準版** (`ring_buffer` + `ingestion`)
//!    - 完整監控（metrics, logging）
//!    - 安全錯誤處理
//!    - 適用於：非關鍵路徑、開發調試
//!
//! 2. **極致性能版** (`ring_buffer_ultra` + `ingestion_ultra`)
//!    - 零開銷（無 metrics, 無 logging）
//!    - Unsafe API（調用者保證前置條件）
//!    - 適用於：熱路徑、高頻交易
//!
//! # 性能對比
//!
//! | 操作 | 標準版 | Ultra版 | 改進 |
//! |------|--------|---------|------|
//! | Ring push | ~15-20ns | ~3-5ns | 3-4x |
//! | Ring pop | ~10-15ns | ~3-5ns | 2-3x |
//! | ingest() | ~150-200ns | ~10-15ns | 10-15x |
//!
//! # 使用建議
//!
//! ```rust
//! // 非關鍵路徑：使用標準版（保留完整監控）
//! use hft_engine::dataflow::{EventIngester, IngestionConfig};
//!
//! let (ingester, consumer) = EventIngester::new(IngestionConfig::default());
//! ingester.ingest(event)?; // 完整錯誤處理
//!
//! // 關鍵路徑：使用 Ultra 版（極致性能）
//! use hft_engine::dataflow::{UltraEventIngester, UltraIngestionConfig};
//!
//! let (ingester, consumer) = UltraEventIngester::new(UltraIngestionConfig::hft());
//!
//! // 熱路徑：預檢查 + unsafe unchecked
//! if !ingester.is_full() {
//!     unsafe { ingester.ingest_fast(event); }
//! }
//! ```

pub mod ring_buffer;
pub mod ring_buffer_ultra;
pub mod ingestion;
pub mod ingestion_ultra;

#[cfg(feature = "bench-lockfree")]
pub mod lockfree_queue;

// 標準版 API（保留完整監控）
pub use ingestion::{
    BackpressurePolicy, BackpressureStatus, EventConsumer, EventIngester, FlipPolicy,
    IngestionConfig, IngestionMetrics,
};

// 極致性能版 API（零開銷）
pub use ingestion_ultra::{UltraEventConsumer, UltraEventIngester, UltraIngestionConfig};
pub use ring_buffer_ultra::{ultra_ring_buffer, UltraConsumer, UltraProducer, UltraRingBuffer};

// 直接從 ports 導出 TrackedMarketEvent（而非從 ingestion 轉導）
pub use ports::events::TrackedMarketEvent;
