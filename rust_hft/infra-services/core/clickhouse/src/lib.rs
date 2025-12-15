//! ClickHouse 整合（可選，旁路，不進熱路徑）
//!
//! 功能:
//! - HTTP + RowBinary 批量插入（比 JSONEachRow 更高效）
//! - 異步批量寫入，不阻塞主循環
//! - 連接池與重試機制
//! - 支持多種資料類型的序列化

#[cfg(feature = "clickhouse")]
mod writer;

#[cfg(feature = "clickhouse")]
pub use writer::*;

// Re-export for convenience
#[cfg(feature = "clickhouse")]
pub mod prelude {
    pub use super::writer::{
        ClickHouseConfig, ClickHouseWriter, RowBinaryValue, RowBinaryWriter, WriteError,
    };
}

/// 空實作，當 clickhouse feature 未啟用時使用
#[cfg(not(feature = "clickhouse"))]
pub mod prelude {
    /// 佔位類型
    pub struct ClickHouseWriter;

    impl ClickHouseWriter {
        pub fn new(_config: ()) -> Self {
            Self
        }
    }
}
