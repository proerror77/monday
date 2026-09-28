//! Parquet-based segment index for fast market data discovery
//!
//! This module provides a zero-deployment alternative to filesystem scanning.
//! Instead of traversing OSS directories, we maintain a Parquet index that
//! DuckDB can query in <1 second.
//!
//! Architecture:
//! 1. Collector writes manifest.json → also appends to segments.parquet
//! 2. Preparation queries Parquet → gets filtered file list directly
//! 3. No additional services required (DuckDB runs in-process)

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SEGMENT_INDEX_SCHEMA_V1: &str = "monday.segment_index.v1";

/// Segment metadata for the Parquet index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentMetadata {
    pub segment_id: String,
    pub schema_version: String,
    pub market: String,
    pub dataset: String,
    pub symbol: String,
    pub date: String, // YYYY-MM-DD
    pub hour: u32,
    pub shard_id: String,

    // Time range
    pub start_received_at_ns: u64,
    pub end_received_at_ns: u64,

    // Replay safety flags (critical for filtering)
    pub has_replay_safe_checkpoint: bool,
    pub all_symbols_bridged: bool,
    pub all_stream_coverage_verified: bool,
    pub venue_depth_complete: bool,

    // Computed field
    pub replay_safe: bool,

    // Paths
    pub manifest_path: String,
    pub tape_path: String,
    pub tape_sha256: Option<String>,

    // Metadata
    pub verified_bytes: u64,
    pub snapshot_only_symbols: Vec<String>,
    pub raw_trade_incomplete_symbols: Vec<String>,
    pub stream_types: Vec<String>,

    // Audit
    pub ingested_at_ms: i64,
}

impl SegmentMetadata {
    /// Create from a manifest JSON object
    pub fn from_manifest(
        manifest: &serde_json::Map<String, serde_json::Value>,
        manifest_path: &str,
        tape_path: &str,
    ) -> Result<Self> {
        let segment_id = format!(
            "{}_{}_{}_{}",
            required_string(manifest, "market")?,
            required_string(manifest, "dataset")?,
            required_string(manifest, "date")?,
            required_string(manifest, "hour")?,
        );

        let has_replay = manifest
            .get("has_replay_safe_checkpoint")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let symbols_bridged = manifest
            .get("all_symbols_bridged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let coverage = manifest
            .get("all_stream_coverage_verified")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let depth_complete = manifest
            .get("venue_depth_complete")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // Compute replay_safe flag
        let replay_safe = has_replay && symbols_bridged && coverage && !depth_complete;

        Ok(Self {
            segment_id,
            schema_version: required_string(manifest, "schema")?.to_string(),
            market: required_string(manifest, "market")?.to_string(),
            dataset: required_string(manifest, "dataset")?.to_string(),
            symbol: manifest
                .get("symbols")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
                .unwrap_or("UNKNOWN")
                .to_string(),
            date: required_string(manifest, "date")?.to_string(),
            hour: manifest
                .get("hour")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .context("invalid hour")?,
            shard_id: required_string(manifest, "shard_id")?.to_string(),
            start_received_at_ns: manifest
                .get("start_received_at_ns")
                .and_then(|v| v.as_u64())
                .context("missing start_received_at_ns")?,
            end_received_at_ns: manifest
                .get("end_received_at_ns")
                .and_then(|v| v.as_u64())
                .context("missing end_received_at_ns")?,
            has_replay_safe_checkpoint: has_replay,
            all_symbols_bridged: symbols_bridged,
            all_stream_coverage_verified: coverage,
            venue_depth_complete: depth_complete,
            replay_safe,
            manifest_path: manifest_path.to_string(),
            tape_path: tape_path.to_string(),
            tape_sha256: manifest
                .get("tape_sha256")
                .and_then(|v| v.as_str())
                .map(String::from),
            verified_bytes: manifest
                .get("verified_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            snapshot_only_symbols: manifest
                .get("snapshot_only_symbols")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            raw_trade_incomplete_symbols: manifest
                .get("raw_trade_incomplete_symbols")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            stream_types: manifest
                .get("stream_types")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            ingested_at_ms: chrono::Utc::now().timestamp_millis(),
        })
    }
}

fn required_string<'a>(
    map: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a str> {
    map.get(key)
        .and_then(|v| v.as_str())
        .with_context(|| format!("missing or invalid field: {}", key))
}

/// Append segment metadata to Parquet index
pub fn append_to_parquet_index(index_path: &Path, metadata: &SegmentMetadata) -> Result<()> {
    #[cfg(not(feature = "duckdb"))]
    {
        let _ = (index_path, metadata);
        bail!("segment index requires building hft-collector with --features duckdb");
    }
    #[cfg(feature = "duckdb")]
    {
        // Use DuckDB to append (simpler than raw Parquet writing)
        let conn = duckdb::Connection::open(":memory:")?;

        // Create temp table
        conn.execute(
            r#"
        CREATE TABLE segments (
            segment_id VARCHAR,
            schema_version VARCHAR,
            market VARCHAR,
            dataset VARCHAR,
            symbol VARCHAR,
            date DATE,
            hour INTEGER,
            shard_id VARCHAR,
            start_received_at_ns BIGINT,
            end_received_at_ns BIGINT,
            has_replay_safe_checkpoint BOOLEAN,
            all_symbols_bridged BOOLEAN,
            all_stream_coverage_verified BOOLEAN,
            venue_depth_complete BOOLEAN,
            replay_safe BOOLEAN,
            manifest_path VARCHAR,
            tape_path VARCHAR,
            tape_sha256 VARCHAR,
            verified_bytes BIGINT,
            snapshot_only_symbols VARCHAR[],
            raw_trade_incomplete_symbols VARCHAR[],
            stream_types VARCHAR[],
            ingested_at_ms BIGINT
        )
        "#,
            [],
        )?;

        // Insert new record
        conn.execute(
            r#"
        INSERT INTO segments VALUES (
            ?, ?, ?, ?, ?, ?::DATE, ?, ?,
            ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
            ?, ?::VARCHAR[], ?::VARCHAR[], ?::VARCHAR[], ?
        )
        "#,
            duckdb::params![
                &metadata.segment_id,
                &metadata.schema_version,
                &metadata.market,
                &metadata.dataset,
                &metadata.symbol,
                &metadata.date,
                metadata.hour as i32,
                &metadata.shard_id,
                metadata.start_received_at_ns as i64,
                metadata.end_received_at_ns as i64,
                metadata.has_replay_safe_checkpoint,
                metadata.all_symbols_bridged,
                metadata.all_stream_coverage_verified,
                metadata.venue_depth_complete,
                metadata.replay_safe,
                &metadata.manifest_path,
                &metadata.tape_path,
                metadata.tape_sha256.as_deref().unwrap_or(""),
                metadata.verified_bytes as i64,
                &metadata.snapshot_only_symbols,
                &metadata.raw_trade_incomplete_symbols,
                &metadata.stream_types,
                metadata.ingested_at_ms,
            ],
        )?;

        // Append to Parquet (creates if not exists)
        if index_path.exists() {
            conn.execute(
                &format!(
                    "COPY segments TO '{}' (FORMAT PARQUET, APPEND true)",
                    index_path.display()
                ),
                [],
            )?;
        } else {
            conn.execute(
                &format!(
                    "COPY segments TO '{}' (FORMAT PARQUET)",
                    index_path.display()
                ),
                [],
            )?;
        }

        Ok(())
    }
}

/// Query segments from Parquet index
pub fn query_segments(
    index_path: &Path,
    market: &str,
    symbol: &str,
    start_ns: u64,
    end_ns: u64,
    require_replay_safe: bool,
) -> Result<Vec<SegmentMetadata>> {
    if !index_path.exists() {
        bail!("Segment index not found: {}", index_path.display());
    }

    #[cfg(not(feature = "duckdb"))]
    {
        let _ = (market, symbol, start_ns, end_ns, require_replay_safe);
        bail!("segment index requires building hft-collector with --features duckdb");
    }
    #[cfg(feature = "duckdb")]
    {
        let conn = duckdb::Connection::open(":memory:")?;

        // Build query
        let query = format!(
            r#"
        SELECT *
        FROM read_parquet('{}')
        WHERE market = ?
          AND symbol = ?
          AND start_received_at_ns <= ?
          AND end_received_at_ns >= ?
          {}
        ORDER BY start_received_at_ns
        "#,
            index_path.display(),
            if require_replay_safe {
                "AND replay_safe = TRUE"
            } else {
                ""
            }
        );

        let mut stmt = conn.prepare(&query)?;
        let rows = stmt.query_map(
            duckdb::params![market, symbol, end_ns as i64, start_ns as i64],
            |row| {
                Ok(SegmentMetadata {
                    segment_id: row.get(0)?,
                    schema_version: row.get(1)?,
                    market: row.get(2)?,
                    dataset: row.get(3)?,
                    symbol: row.get(4)?,
                    date: row.get::<_, String>(5)?,
                    hour: row.get::<_, i32>(6)? as u32,
                    shard_id: row.get(7)?,
                    start_received_at_ns: row.get::<_, i64>(8)? as u64,
                    end_received_at_ns: row.get::<_, i64>(9)? as u64,
                    has_replay_safe_checkpoint: row.get(10)?,
                    all_symbols_bridged: row.get(11)?,
                    all_stream_coverage_verified: row.get(12)?,
                    venue_depth_complete: row.get(13)?,
                    replay_safe: row.get(14)?,
                    manifest_path: row.get(15)?,
                    tape_path: row.get(16)?,
                    tape_sha256: Some(row.get::<_, String>(17)?),
                    verified_bytes: row.get::<_, i64>(18)? as u64,
                    snapshot_only_symbols: row.get(19)?,
                    raw_trade_incomplete_symbols: row.get(20)?,
                    stream_types: row.get(21)?,
                    ingested_at_ms: row.get(22)?,
                })
            },
        )?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("Failed to query segments: {}", e))
    }
}

/// Diagnose why no segments were found
pub fn diagnose_empty_result(
    index_path: &Path,
    market: &str,
    symbol: &str,
    start_ns: u64,
    end_ns: u64,
) -> Result<String> {
    #[cfg(not(feature = "duckdb"))]
    {
        let _ = (index_path, market, symbol, start_ns, end_ns);
        bail!("segment index requires building hft-collector with --features duckdb");
    }
    #[cfg(feature = "duckdb")]
    {
        let conn = duckdb::Connection::open(":memory:")?;

        let query = format!(
            r#"
        SELECT
            COUNT(*) as total,
            SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe_count,
            SUM(CASE WHEN NOT has_replay_safe_checkpoint THEN 1 ELSE 0 END) as missing_checkpoint,
            SUM(CASE WHEN NOT all_symbols_bridged THEN 1 ELSE 0 END) as missing_bridge,
            SUM(CASE WHEN NOT all_stream_coverage_verified THEN 1 ELSE 0 END) as missing_coverage,
            SUM(CASE WHEN venue_depth_complete THEN 1 ELSE 0 END) as wrong_depth_flag
        FROM read_parquet('{}')
        WHERE market = ?
          AND symbol = ?
          AND start_received_at_ns <= ?
          AND end_received_at_ns >= ?
        "#,
            index_path.display()
        );

        let result = conn.query_row(
            &query,
            duckdb::params![market, symbol, end_ns as i64, start_ns as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )?;

        let (total, safe, missing_checkpoint, missing_bridge, missing_coverage, wrong_depth) =
            result;

        if total == 0 {
            return Ok(format!(
                "No segments found for {} {} in range {}-{}",
                market, symbol, start_ns, end_ns
            ));
        }

        let mut reasons = Vec::new();
        if missing_checkpoint > 0 {
            reasons.push(format!(
                "{} missing replay_safe_checkpoint",
                missing_checkpoint
            ));
        }
        if missing_bridge > 0 {
            reasons.push(format!("{} missing symbols_bridged", missing_bridge));
        }
        if missing_coverage > 0 {
            reasons.push(format!("{} missing coverage_verified", missing_coverage));
        }
        if wrong_depth > 0 {
            reasons.push(format!("{} wrong venue_depth_complete flag", wrong_depth));
        }

        Ok(format!(
            "Found {} segments, but only {} are replay-safe. Issues: {}",
            total,
            safe,
            reasons.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_metadata_from_manifest() {
        let manifest = serde_json::json!({
            "schema": "binance.market_tape.v2",
            "market": "usdm",
            "dataset": "usdm_perpetual_top100_lob",
            "symbols": ["SOLUSDT"],
            "date": "2026-09-01",
            "hour": "12",
            "shard_id": "shard-01",
            "start_received_at_ns": 1725192000000000000u64,
            "end_received_at_ns": 1725195599999999999u64,
            "has_replay_safe_checkpoint": true,
            "all_symbols_bridged": true,
            "all_stream_coverage_verified": true,
            "venue_depth_complete": false,
            "tape_sha256": "abc123",
            "verified_bytes": 1024000,
        });

        let metadata = SegmentMetadata::from_manifest(
            manifest.as_object().unwrap(),
            "/path/to/manifest.json",
            "/path/to/tape.jsonl.zst",
        )
        .unwrap();

        assert_eq!(metadata.market, "usdm");
        assert_eq!(metadata.symbol, "SOLUSDT");
        assert_eq!(metadata.hour, 12);
        assert!(metadata.replay_safe);
    }

    #[test]
    fn test_replay_safe_computation() {
        let mut manifest = serde_json::json!({
            "schema": "binance.market_tape.v2",
            "market": "usdm",
            "dataset": "test",
            "symbols": ["TEST"],
            "date": "2026-01-01",
            "hour": "0",
            "shard_id": "test",
            "start_received_at_ns": 0u64,
            "end_received_at_ns": 1u64,
            "has_replay_safe_checkpoint": true,
            "all_symbols_bridged": true,
            "all_stream_coverage_verified": true,
            "venue_depth_complete": false,
        });

        let metadata =
            SegmentMetadata::from_manifest(manifest.as_object().unwrap(), "/test", "/test")
                .unwrap();
        assert!(metadata.replay_safe);

        // Missing checkpoint
        manifest["has_replay_safe_checkpoint"] = serde_json::json!(false);
        let metadata =
            SegmentMetadata::from_manifest(manifest.as_object().unwrap(), "/test", "/test")
                .unwrap();
        assert!(!metadata.replay_safe);
    }
}
