/*!
 * LOB Time Series Extractor - 模块化版本
 * 
 * 将853行的大文件拆分为可管理的模块
 * 严格遵守CLAUDE.md的750行限制
 */

pub mod config;
pub mod snapshot;
pub mod sequence;
pub mod extractor;

// Re-export main types
pub use config::LobTimeSeriesConfig;
pub use snapshot::LobTimeSeriesSnapshot;
pub use sequence::LobTimeSeriesSequence;
pub use extractor::{LobTimeSeriesExtractor, LobTimeSeriesExtractorStats};

// Legacy compatibility exports
pub use extractor::LobTimeSeriesExtractor as LobTimeSeriesExtractorLegacy;