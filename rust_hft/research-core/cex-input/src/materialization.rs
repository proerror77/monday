//! Shared materialization metadata and execution-schema admission.
//! Historical snapshots remain readable for validation but cannot execute.
use anyhow::{bail, Context};
use chrono::Utc;
use hft_research_manifest::{
    CexReplaySnapshotV1, CexReplaySnapshotV2, CexReplaySnapshotV3, CexReplaySnapshotV4,
    CexReplaySnapshotV5, BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2,
    BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3, BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V4,
    BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V5, BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V6,
    BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7,
};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Materialization {
    pub dataset_kind: String,
    pub schema_version: String,
    pub mission_id: String,
    pub symbol: String,
    pub market: String,
    pub source_revision: String,
    pub source_segments: Vec<SourceSegment>,
    pub artifact_sha256: String,
    pub bucket_ms: u64,
    pub label_horizon_buckets: usize,
    pub top_depth: usize,
    #[serde(default = "default_series_count")]
    pub series_count: usize,
    pub rows: usize,
    pub first_event_time: chrono::DateTime<Utc>,
    pub last_event_time: chrono::DateTime<Utc>,
    pub snapshot: CexReplaySnapshotV5,
    pub snapshot_sha256: String,
}

pub const fn default_series_count() -> usize {
    1
}

#[derive(Debug, Deserialize)]
pub struct SourceSegment {
    pub path: PathBuf,
    pub sha256: String,
    pub collector_manifest_sha256: String,
    pub success_marker_sha256: String,
    pub start_received_at_ns: u64,
    pub end_received_at_ns: u64,
    pub events: u64,
}

pub fn decode_materialization(bytes: &[u8]) -> anyhow::Result<Materialization> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("materialization manifest is invalid JSON")?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
    {
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7) => {
            serde_json::from_value(value).context("V7 materialization manifest is invalid")
        }
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V6) => {
            let snapshot: CexReplaySnapshotV4 = serde_json::from_value(
                value
                    .get("snapshot")
                    .cloned()
                    .context("historical V6 materialization has no snapshot")?,
            )
            .context("historical V6 materialization snapshot is invalid")?;
            snapshot.validate().map_err(anyhow::Error::new)?;
            bail!("historical V6 materialization is read-only and cannot execute")
        }
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V5) => {
            let snapshot: CexReplaySnapshotV4 = serde_json::from_value(
                value
                    .get("snapshot")
                    .cloned()
                    .context("historical V5 materialization has no snapshot")?,
            )
            .context("historical V5 materialization snapshot is invalid")?;
            snapshot.validate().map_err(anyhow::Error::new)?;
            bail!("historical V5 materialization is read-only and cannot execute")
        }
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2) => {
            let snapshot: CexReplaySnapshotV1 = serde_json::from_value(
                value
                    .get("snapshot")
                    .cloned()
                    .context("historical V2 materialization has no snapshot")?,
            )
            .context("historical V2 materialization snapshot is invalid")?;
            snapshot.validate().map_err(anyhow::Error::new)?;
            bail!("historical V2 materialization is read-only and cannot execute")
        }
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3) => {
            let snapshot: CexReplaySnapshotV2 = serde_json::from_value(
                value
                    .get("snapshot")
                    .cloned()
                    .context("historical V3 materialization has no snapshot")?,
            )
            .context("historical V3 materialization snapshot is invalid")?;
            snapshot.validate().map_err(anyhow::Error::new)?;
            bail!("historical V3 materialization is read-only and cannot execute")
        }
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V4) => {
            let snapshot: CexReplaySnapshotV3 = serde_json::from_value(
                value
                    .get("snapshot")
                    .cloned()
                    .context("historical V4 materialization has no snapshot")?,
            )
            .context("historical V4 materialization snapshot is invalid")?;
            snapshot.validate().map_err(anyhow::Error::new)?;
            bail!("historical V4 materialization is read-only and cannot execute")
        }
        _ => bail!("materialization kind or schema is unsupported"),
    }
}
