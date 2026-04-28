use super::book_sync::{BookSyncState, BufferedApplyResult, SequenceDecision};
use super::features::FeatureSnapshot;
use super::latency::LatencyTrace;
use super::signal::Signal;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplayKind {
    RawFrame = 1,
    DepthUpdate = 2,
    Feature = 3,
    Signal = 4,
    Snapshot = 5,
    Bridge = 6,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayBridgeUpdate {
    pub symbol_id: u32,
    pub exchange_ts_ns: i64,
    pub receive_ts_ns: i64,
    pub first_update_id: u64,
    pub final_update_id: u64,
    pub bids: Vec<(i64, i64)>,
    pub asks: Vec<(i64, i64)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        #[serde(default)]
        applied_updates: Vec<ReplayBridgeUpdate>,
    },
    Feature(FeatureSnapshot),
    Signal(Signal),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayBatch {
    pub raw: Option<ReplayRecord>,
    pub depth: Option<ReplayRecord>,
    pub snapshot: Option<ReplayRecord>,
    pub bridge: Option<ReplayRecord>,
    pub feature: Option<ReplayRecord>,
    pub signal: Option<ReplayRecord>,
}

impl ReplayBatch {
    pub fn records(&self) -> impl Iterator<Item = &ReplayRecord> {
        [
            self.raw.as_ref(),
            self.snapshot.as_ref(),
            self.bridge.as_ref(),
            self.depth.as_ref(),
            self.feature.as_ref(),
            self.signal.as_ref(),
        ]
        .into_iter()
        .flatten()
    }
}

pub fn write_replay_batch<W: Write>(
    writer: &mut W,
    batch: &ReplayBatch,
) -> Result<usize, serde_json::Error> {
    let mut written = 0;
    for record in batch.records() {
        serde_json::to_writer(&mut *writer, record)?;
        writer.write_all(b"\n").map_err(serde_json::Error::io)?;
        written += 1;
    }
    Ok(written)
}

pub fn read_replay_records<R: BufRead>(reader: R) -> Result<Vec<ReplayRecord>, serde_json::Error> {
    let mut records = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(serde_json::Error::io)?;
        if line.trim().is_empty() {
            continue;
        }
        records.push(serde_json::from_str(&line)?);
    }
    Ok(records)
}
