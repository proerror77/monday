//! Integration of Parquet index into research_inventory
//!
//! This module extends the existing research inventory to support
//! DuckDB-based Parquet index queries as an alternative to filesystem scanning.

use crate::segment_index::{query_segments, diagnose_empty_result, SegmentMetadata};
use crate::research_inventory::{FreshWindowRequest, FreshWindowSelection, FrozenInput};
use anyhow::{bail, Context, Result};
use std::path::Path;

/// Select segments using Parquet index instead of filesystem scanning
pub fn select_segments_from_index(
    request: &FreshWindowRequest,
    index_path: &Path,
) -> Result<FreshWindowSelection> {
    let start_ns = match &request.mode {
        crate::research_inventory::FreshWindowMode::Explicit {
            start_received_at_ns,
            ..
        } => *start_received_at_ns,
        crate::research_inventory::FreshWindowMode::Latest {
            cutoff_received_at_ns,
            duration_ns,
            ..
        } => cutoff_received_at_ns.saturating_sub(*duration_ns),
    };

    let end_ns = match &request.mode {
        crate::research_inventory::FreshWindowMode::Explicit {
            end_received_at_ns,
            ..
        } => *end_received_at_ns,
        crate::research_inventory::FreshWindowMode::Latest {
            cutoff_received_at_ns,
            ..
        } => *cutoff_received_at_ns,
    };

    // Query Parquet index (fast!)
    let segments = query_segments(
        index_path,
        request.market.as_str(),
        &request.symbol,
        start_ns,
        end_ns,
        true, // require_replay_safe
    )
    .context("Failed to query segment index")?;

    if segments.is_empty() {
        // Provide diagnostic information
        let diagnosis = diagnose_empty_result(
            index_path,
            request.market.as_str(),
            &request.symbol,
            start_ns,
            end_ns,
        )?;

        bail!("No replayable segments found. {}", diagnosis);
    }

    // Convert to FrozenInput format
    let raw_inputs: Vec<FrozenInput> = segments
        .iter()
        .map(|seg| FrozenInput {
            path: seg.tape_path.clone(),
            sha256: seg.tape_sha256.clone().unwrap_or_default(),
            verified_bytes: seg.verified_bytes,
        })
        .collect();

    let verified_bytes = segments.iter().map(|s| s.verified_bytes).sum();

    let selected_start = segments
        .iter()
        .map(|s| s.start_received_at_ns)
        .min()
        .unwrap_or(start_ns);
    let selected_end = segments
        .iter()
        .map(|s| s.end_received_at_ns)
        .max()
        .unwrap_or(end_ns);

    // Compute fingerprint
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    for seg in &segments {
        hasher.update(seg.segment_id.as_bytes());
        hasher.update(&seg.start_received_at_ns.to_le_bytes());
        hasher.update(&seg.end_received_at_ns.to_le_bytes());
    }
    let input_fingerprint_sha256 = format!("{:x}", hasher.finalize());

    Ok(FreshWindowSelection {
        schema_version: crate::research_inventory::FRESH_WINDOW_SELECTION_SCHEMA.to_string(),
        mode: request.mode.clone(),
        market: request.market,
        selected_start_received_at_ns: selected_start,
        selected_end_received_at_ns: selected_end,
        raw: raw_inputs,
        references: vec![], // TODO: handle reference data
        verified_bytes,
        verification_bytes: 0, // Index query doesn't scan bytes
        input_fingerprint_sha256,
        inventory_eligible: true,
        materialized_pit_admitted: true,
    })
}

/// Hybrid approach: try index first, fallback to filesystem scan
pub fn select_segments_hybrid(
    request: &FreshWindowRequest,
    index_path: Option<&Path>,
) -> Result<FreshWindowSelection> {
    // Try index first if available
    if let Some(path) = index_path {
        if path.exists() {
            match select_segments_from_index(request, path) {
                Ok(selection) => {
                    eprintln!("✓ Used Parquet index (fast path)");
                    return Ok(selection);
                }
                Err(e) => {
                    eprintln!("⚠ Parquet index query failed: {}", e);
                    eprintln!("  Falling back to filesystem scan...");
                }
            }
        } else {
            eprintln!("⚠ Parquet index not found at {}", path.display());
            eprintln!("  Falling back to filesystem scan...");
        }
    }

    // Fallback to original filesystem scan
    eprintln!("Using filesystem scan (slow path)");
    crate::research_inventory::select_fresh_window(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment_index::append_to_parquet_index;
    use tempfile::TempDir;

    #[test]
    fn test_select_from_index() {
        let temp = TempDir::new().unwrap();
        let index_path = temp.path().join("test_index.parquet");

        // Create test segments
        let manifest1 = serde_json::json!({
            "schema": "binance.market_tape.v2",
            "market": "usdm",
            "dataset": "test",
            "symbols": ["SOLUSDT"],
            "date": "2026-09-01",
            "hour": "12",
            "shard_id": "test",
            "start_received_at_ns": 1000000u64,
            "end_received_at_ns": 2000000u64,
            "has_replay_safe_checkpoint": true,
            "all_symbols_bridged": true,
            "all_stream_coverage_verified": true,
            "venue_depth_complete": false,
            "tape_sha256": "abc123",
            "verified_bytes": 1024,
        });

        let metadata = crate::segment_index::SegmentMetadata::from_manifest(
            manifest1.as_object().unwrap(),
            "/test/manifest.json",
            "/test/tape.jsonl.zst",
        )
        .unwrap();

        append_to_parquet_index(&index_path, &metadata).unwrap();

        // Query
        let request = FreshWindowRequest {
            raw_root: temp.path().to_path_buf(),
            reference_root: temp.path().to_path_buf(),
            mode: crate::research_inventory::FreshWindowMode::Explicit {
                start_received_at_ns: 500000,
                end_received_at_ns: 2500000,
            },
            market: crate::research_inventory::Market::UsdmPerpetual,
            symbol: "SOLUSDT".to_string(),
            source_revision: "test".to_string(),
            image_ref: "test".to_string(),
            mission_id: "test".to_string(),
            output_prefix: "test".to_string(),
            bucket_ms: 1000,
            label_horizon_buckets: 1,
            top_depth: 5,
            max_scan_entries: 1000,
            max_inputs: 100,
            max_input_bytes: 1_000_000,
            discovery_index: None,
        };

        let selection = select_segments_from_index(&request, &index_path).unwrap();

        assert_eq!(selection.raw.len(), 1);
        assert_eq!(selection.raw[0].path, "/test/tape.jsonl.zst");
        assert_eq!(selection.verified_bytes, 1024);
    }
}
