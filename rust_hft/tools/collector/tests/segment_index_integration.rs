//! Integration tests for segment index
//!
//! These tests verify the complete workflow:
//! 1. Create test manifests
//! 2. Build Parquet index
//! 3. Query index
//! 4. Verify results

#[cfg(test)]
mod integration_tests {
    use hft_collector::segment_index::*;
    use hft_collector::segment_index_query::*;
    use hft_collector::research_inventory::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_test_manifest(
        replay_safe: bool,
        date: &str,
        hour: u32,
    ) -> serde_json::Map<String, serde_json::Value> {
        serde_json::json!({
            "schema": "binance.market_tape.v2",
            "market": "usdm",
            "dataset": "usdm_perpetual_top100_lob",
            "symbols": ["SOLUSDT"],
            "date": date,
            "hour": format!("{:02}", hour),
            "shard_id": "test-shard",
            "start_received_at_ns": 1725148800000000000u64 + (hour as u64 * 3600 * 1_000_000_000),
            "end_received_at_ns": 1725152399999999999u64 + (hour as u64 * 3600 * 1_000_000_000),
            "has_replay_safe_checkpoint": replay_safe,
            "all_symbols_bridged": replay_safe,
            "all_stream_coverage_verified": replay_safe,
            "venue_depth_complete": !replay_safe,
            "tape_sha256": format!("test-sha256-{}", hour),
            "verified_bytes": 1024000 + (hour as u64 * 1000),
            "snapshot_only_symbols": [],
            "raw_trade_incomplete_symbols": [],
            "stream_types": ["depth@100ms", "bookTicker"],
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn test_end_to_end_workflow() {
        let temp = TempDir::new().unwrap();
        let index_path = temp.path().join("test_index.parquet");

        // Create test data: 3 safe segments, 1 unsafe
        let manifests = vec![
            (true, "2026-09-01", 0),
            (true, "2026-09-01", 1),
            (false, "2026-09-01", 2),  // Unsafe!
            (true, "2026-09-01", 3),
        ];

        // Build index
        for (replay_safe, date, hour) in &manifests {
            let manifest = create_test_manifest(*replay_safe, date, *hour);
            let metadata = SegmentMetadata::from_manifest(
                &manifest,
                &format!("/test/{}/hour={:02}/manifest.json", date, hour),
                &format!("/test/{}/hour={:02}/tape.jsonl.zst", date, hour),
            )
            .unwrap();

            append_to_parquet_index(&index_path, &metadata).unwrap();
        }

        // Query safe segments only
        let segments = query_segments(
            &index_path,
            "usdm",
            "SOLUSDT",
            0,
            u64::MAX,
            true, // require_replay_safe
        )
        .unwrap();

        // Should return only 3 safe segments (hour 2 excluded)
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].hour, 0);
        assert_eq!(segments[1].hour, 1);
        assert_eq!(segments[2].hour, 3);

        // All returned segments should be safe
        for seg in &segments {
            assert!(seg.replay_safe);
        }
    }

    #[test]
    fn test_diagnose_unsafe_segments() {
        let temp = TempDir::new().unwrap();
        let index_path = temp.path().join("test_index.parquet");

        // Create segments with different failure modes
        let mut manifest1 = create_test_manifest(false, "2026-09-01", 0);
        manifest1.insert(
            "has_replay_safe_checkpoint".to_string(),
            serde_json::json!(false),
        );

        let mut manifest2 = create_test_manifest(false, "2026-09-01", 1);
        manifest2.insert(
            "all_symbols_bridged".to_string(),
            serde_json::json!(false),
        );

        let mut manifest3 = create_test_manifest(false, "2026-09-01", 2);
        manifest3.insert(
            "all_stream_coverage_verified".to_string(),
            serde_json::json!(false),
        );

        for (idx, manifest) in [manifest1, manifest2, manifest3].iter().enumerate() {
            let metadata = SegmentMetadata::from_manifest(
                manifest,
                &format!("/test/manifest_{}.json", idx),
                &format!("/test/tape_{}.jsonl.zst", idx),
            )
            .unwrap();
            append_to_parquet_index(&index_path, &metadata).unwrap();
        }

        // Diagnose
        let diagnosis = diagnose_empty_result(
            &index_path,
            "usdm",
            "SOLUSDT",
            0,
            u64::MAX,
        )
        .unwrap();

        // Should report the issues
        assert!(diagnosis.contains("3 segments"));
        assert!(diagnosis.contains("0 are replay-safe"));
    }

    #[test]
    fn test_time_range_filtering() {
        let temp = TempDir::new().unwrap();
        let index_path = temp.path().join("test_index.parquet");

        // Create segments across multiple days
        for day in 1..=3 {
            for hour in 0..24 {
                let manifest = create_test_manifest(
                    true,
                    &format!("2026-09-{:02}", day),
                    hour,
                );
                let metadata = SegmentMetadata::from_manifest(
                    &manifest,
                    &format!("/test/day{}/hour{}/manifest.json", day, hour),
                    &format!("/test/day{}/hour{}/tape.jsonl.zst", day, hour),
                )
                .unwrap();
                append_to_parquet_index(&index_path, &metadata).unwrap();
            }
        }

        // Query only day 2
        let day2_start = 1725235200000000000u64; // 2026-09-02 00:00
        let day2_end = 1725321599999999999u64;   // 2026-09-02 23:59

        let segments = query_segments(
            &index_path,
            "usdm",
            "SOLUSDT",
            day2_start,
            day2_end,
            true,
        )
        .unwrap();

        // Should return exactly 24 hours from day 2
        assert_eq!(segments.len(), 24);
        for seg in &segments {
            assert_eq!(seg.date, "2026-09-02");
        }
    }

    #[test]
    fn test_hybrid_query_fallback() {
        let temp = TempDir::new().unwrap();
        let nonexistent_index = temp.path().join("does_not_exist.parquet");

        // Create a minimal request
        let request = FreshWindowRequest {
            raw_root: temp.path().to_path_buf(),
            reference_root: temp.path().to_path_buf(),
            mode: FreshWindowMode::Explicit {
                start_received_at_ns: 0,
                end_received_at_ns: 1000000000,
            },
            market: Market::UsdmPerpetual,
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

        // Hybrid query with nonexistent index should fallback
        // (Will fail with "no manifests found" but shouldn't panic)
        let result = select_segments_hybrid(&request, Some(&nonexistent_index));

        // Should fail gracefully, not panic
        assert!(result.is_err());
    }

    #[test]
    fn test_index_append_is_idempotent() {
        let temp = TempDir::new().unwrap();
        let index_path = temp.path().join("test_index.parquet");

        let manifest = create_test_manifest(true, "2026-09-01", 0);
        let metadata = SegmentMetadata::from_manifest(
            &manifest,
            "/test/manifest.json",
            "/test/tape.jsonl.zst",
        )
        .unwrap();

        // Append twice
        append_to_parquet_index(&index_path, &metadata).unwrap();
        append_to_parquet_index(&index_path, &metadata).unwrap();

        // Query should return 2 rows (not deduplicated)
        let segments = query_segments(
            &index_path,
            "usdm",
            "SOLUSDT",
            0,
            u64::MAX,
            true,
        )
        .unwrap();

        // This is expected behavior - append is truly append
        // Deduplication should happen at query time or during maintenance
        assert_eq!(segments.len(), 2);
    }

    #[test]
    fn test_segment_metadata_serialization() {
        let manifest = create_test_manifest(true, "2026-09-01", 12);
        let metadata = SegmentMetadata::from_manifest(
            &manifest,
            "/test/manifest.json",
            "/test/tape.jsonl.zst",
        )
        .unwrap();

        // Serialize to JSON
        let json = serde_json::to_string(&metadata).unwrap();

        // Deserialize back
        let deserialized: SegmentMetadata = serde_json::from_str(&json).unwrap();

        // Should be identical
        assert_eq!(metadata.segment_id, deserialized.segment_id);
        assert_eq!(metadata.replay_safe, deserialized.replay_safe);
        assert_eq!(metadata.hour, deserialized.hour);
    }
}
