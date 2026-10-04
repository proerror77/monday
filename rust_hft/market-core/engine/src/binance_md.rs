//! Compatibility path for the depth leaf.
//! The implementation lives in `hft-binance-depth` and has no order path.

pub use hft_binance_depth::{
    normalize_depth_update, parse_depth_update, parse_fixed_6, read_replay_records,
    write_replay_batch, BinanceDepthUpdate, BookSide, BookSync, BookSyncState, BridgeOutcome,
    BufferedApplyResult, BufferedDecision, CorrectnessBook, FastOutcome, FeatureSnapshot,
    FeatureView, LatencyTrace, Level, MarketDataLane, ParseDepthError, ParseFixedError,
    ParsedDepthUpdate, ProcessOutcome, RawFrameBuf, RawFrameRef, ReplayBatch, ReplayBridgeUpdate,
    ReplayKind, ReplayPayload, ReplayRecord, SequenceDecision, Signal, SignalRules, SignalSide,
    TopBook, UpdateMeta,
};
