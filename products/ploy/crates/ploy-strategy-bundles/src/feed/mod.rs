//! Data feed implementations.

mod historical;
mod live;
mod options;
#[cfg(feature = "parquet-feed")]
pub mod parquet;
#[cfg(feature = "parquet-feed")]
pub mod parquet_stream;
mod recorded;

pub use historical::HistoricalFeed;
pub use live::LiveFeed;
pub use options::HistoricalLoadOptions;
#[cfg(feature = "parquet-feed")]
pub use parquet_stream::StreamingParquetFeed;
pub use recorded::{
    RecordedFeed, RecordedFeedError, RecordedMarketUpdate, RecordingFeed, RecordingLimits,
};
