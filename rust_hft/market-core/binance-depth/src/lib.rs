//! Binance low-latency market-data fast lane.
//!
//! This module is intentionally narrow. It models the local hot path from raw
//! Binance depth frames to order-book updates, features, expiring signals, and
//! replay records. It is not wired into live execution.

pub mod book;
pub mod book_sync;
pub mod features;
pub mod latency;
pub mod parser;
pub mod pipeline;
pub mod raw;
pub mod replay;
pub mod signal;

pub use book::{BookSide, CorrectnessBook, Level, TopBook};
pub use book_sync::{
    BookSync, BookSyncState, BufferedApplyResult, BufferedDecision, SequenceDecision, UpdateMeta,
};
pub use features::{FeatureSnapshot, FeatureView};
pub use latency::LatencyTrace;
pub use parser::{
    normalize_depth_update, parse_depth_update, parse_fixed_6, BinanceDepthUpdate, ParseDepthError,
    ParseFixedError, ParsedDepthUpdate,
};
pub use pipeline::{BridgeOutcome, FastOutcome, MarketDataLane, ProcessOutcome};
pub use raw::{RawFrameBuf, RawFrameRef};
pub use replay::{
    read_replay_records, write_replay_batch, ReplayBatch, ReplayBridgeUpdate, ReplayKind,
    ReplayPayload, ReplayRecord,
};
pub use signal::{Signal, SignalRules, SignalSide};
