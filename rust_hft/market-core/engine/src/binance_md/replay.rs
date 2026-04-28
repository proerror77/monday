use super::book_sync::{BookSyncState, BufferedApplyResult, SequenceDecision};
use super::features::FeatureSnapshot;
use super::latency::LatencyTrace;
use super::signal::Signal;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayKind {
    RawFrame = 1,
    DepthUpdate = 2,
    Feature = 3,
    Signal = 4,
    Snapshot = 5,
    Bridge = 6,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayRecord {
    pub kind: ReplayKind,
    pub symbol_id: u32,
    pub exchange_ts_ns: i64,
    pub receive_ts_ns: i64,
    pub first_update_id: u64,
    pub final_update_id: u64,
    pub sync_state: BookSyncState,
    pub last_book_update_id: u64,
    pub latency: LatencyTrace,
    pub payload: ReplayPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayPayload {
    RawFrame {
        bytes: Vec<u8>,
    },
    DepthUpdate {
        decision: SequenceDecision,
        bids: Vec<(i64, i64)>,
        asks: Vec<(i64, i64)>,
    },
    Snapshot {
        bids: Vec<(i64, i64)>,
        asks: Vec<(i64, i64)>,
    },
    Bridge {
        result: BufferedApplyResult,
    },
    Feature(FeatureSnapshot),
    Signal(Signal),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayBatch {
    pub raw: Option<ReplayRecord>,
    pub depth: Option<ReplayRecord>,
    pub snapshot: Option<ReplayRecord>,
    pub bridge: Option<ReplayRecord>,
    pub feature: Option<ReplayRecord>,
    pub signal: Option<ReplayRecord>,
}
