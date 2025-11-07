//! Bitget 行情 adapter（實作 `ports::MarketStream`）
//! - 快照+增量/序號/（可選）checksum → 統一 MarketEvent

mod bitget_stream;
mod zero_copy_stream;
// cleanup: avoid duplicate helper modules; keep adapter self-contained

pub use bitget_stream::BitgetMarketStream;
pub use zero_copy_stream::{ZeroCopyBitgetStream, ZeroCopyMessageHandler};

pub mod capabilities {
    #[derive(Debug, Clone)]
    pub struct BitgetCapabilities {
        pub snapshot_crc: bool,
        pub all_in_one_topics: bool,
    }
    impl Default for BitgetCapabilities {
        fn default() -> Self {
            Self {
                snapshot_crc: false,
                all_in_one_topics: true,
            }
        }
    }
}
