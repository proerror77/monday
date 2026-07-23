. as $gate
| .schema == "monday.rust_lob_shadow_gate.v3"
and .candidate_sha256 == $candidate_sha256
and .deployment_bundle_sha256 == $deployment_bundle_sha256
and .deployment_source_revision == $deployment_source_revision
and .passed == true
and .production_eligible == true
and .checks_passed == true
and (.duration_seconds | type) == "number"
and .duration_seconds == (.duration_seconds | floor)
and .duration_seconds >= 3600
and (.markets.spot.symbol_count | type) == "number"
and .markets.spot.symbol_count == (.markets.spot.symbol_count | floor)
and .markets.spot.symbol_count >= 1000
and .markets.spot.snapshot_ready_count == .markets.spot.symbol_count
and .markets.spot.stream_coverage_verified_count == .markets.spot.symbol_count
and .markets.spot.all_stream_coverage_verified == true
and .markets.spot.sequence_gaps == 0
and (.markets.spot.upload_failure_count | type) == "number"
and .markets.spot.upload_failure_count == (.markets.spot.upload_failure_count | floor)
and .markets.spot.upload_failure_count >= 0
and (.markets.spot.health_samples | type) == "number"
and .markets.spot.health_samples == (.markets.spot.health_samples | floor)
and .markets.spot.health_samples >= 40
and (.markets.spot.max_health_silence_seconds | type) == "number"
and .markets.spot.max_health_silence_seconds >= 0
and .markets.spot.max_health_silence_seconds <= 120
and (.markets.spot.catalog_sha256 | type) == "string"
and (.markets.spot.catalog_sha256 | test("^[a-f0-9]{64}$"))
and (.markets.spot.session_id | type) == "string"
and (.markets.spot.session_id | length) > 0
and (.markets.spot.oss_roundtrips | type) == "number"
and .markets.spot.oss_roundtrips == (.markets.spot.oss_roundtrips | floor)
and .markets.spot.oss_roundtrips >= 2
and (.markets.spot.agg_trade_segments | type) == "number"
and .markets.spot.agg_trade_segments == (.markets.spot.agg_trade_segments | floor)
and .markets.spot.agg_trade_segments >= 2
and .markets.spot.agg_trade_segments == .markets.spot.oss_roundtrips
and (.markets.spot.agg_trade_count | type) == "number"
and .markets.spot.agg_trade_count == (.markets.spot.agg_trade_count | floor)
and .markets.spot.agg_trade_count > 0
and .markets.spot.strict_trade_summary_readback == true
and .markets.spot.strict_lob_continuity_readback == true
and (.markets.spot.lob_reconnect_boundaries | type) == "number"
and .markets.spot.lob_reconnect_boundaries == 0
and (.markets.spot.min_lob_source_latency_ms | type) == "number"
and .markets.spot.min_lob_source_latency_ms >= -1000
and (.markets.spot.max_lob_source_latency_ms | type) == "number"
and .markets.spot.max_lob_source_latency_ms <= 30000
and .markets.spot.max_lob_source_latency_ms >= .markets.spot.min_lob_source_latency_ms
and (.markets.spot.min_lob_bid_levels | type) == "number"
and .markets.spot.min_lob_bid_levels > 0
and (.markets.spot.min_lob_ask_levels | type) == "number"
and .markets.spot.min_lob_ask_levels > 0
and (.markets.spot.max_segment_gap_ns | type) == "number"
and .markets.spot.max_segment_gap_ns == (.markets.spot.max_segment_gap_ns | floor)
and .markets.spot.max_segment_gap_ns >= 0
and .markets.spot.max_segment_gap_ns <= 90000000000
and (.markets.spot.oss_roundtrip_evidence | type) == "array"
and (.markets.spot.oss_roundtrip_evidence | length) == .markets.spot.oss_roundtrips
and all(.markets.spot.oss_roundtrip_evidence[];
  (.success_uri | type) == "string" and (.success_uri | length) > 0
  and (.sha256 | type) == "string"
  and (.sha256 | test("^[a-f0-9]{64}$"))
  and (.manifest_sha256 | type) == "string"
  and (.manifest_sha256 | test("^[a-f0-9]{64}$"))
  and (.gap_from_previous_ns | type) == "number"
  and .gap_from_previous_ns == (.gap_from_previous_ns | floor)
  and .gap_from_previous_ns >= 0
  and (.start_received_at_ns | type) == "number"
  and .start_received_at_ns == (.start_received_at_ns | floor)
  and (.end_received_at_ns | type) == "number"
  and .end_received_at_ns == (.end_received_at_ns | floor)
  and .end_received_at_ns >= .start_received_at_ns
  and (.agg_trade_count | type) == "number"
  and .agg_trade_count == (.agg_trade_count | floor)
  and .agg_trade_count > 0
  and .lob_capture_session_id == $gate.markets.spot.session_id
  and (.lob_reconnect_boundary | type) == "boolean"
  and .lob_sequence_gaps == 0
  and .lob_source_time_rollbacks == 0
  and .lob_declared_symbol_count == $gate.markets.spot.symbol_count
  and .lob_covered_symbol_count == $gate.markets.spot.symbol_count
  and .stream_coverage_verified_count == $gate.markets.spot.symbol_count
  and .all_stream_coverage_verified == true
  and (.lob_min_source_latency_ms | type) == "number"
  and .lob_min_source_latency_ms >= -1000
  and (.lob_max_source_latency_ms | type) == "number"
  and .lob_max_source_latency_ms <= 30000
  and .lob_max_source_latency_ms >= .lob_min_source_latency_ms
  and (.lob_min_bid_levels | type) == "number"
  and .lob_min_bid_levels > 0
  and (.lob_min_ask_levels | type) == "number"
  and .lob_min_ask_levels > 0)
and (.markets.spot.oss_roundtrip_evidence as $round_trips
  | $round_trips[0].gap_from_previous_ns == 0
  and all($round_trips[].lob_reconnect_boundary; . == false)
  and .markets.spot.lob_reconnect_boundaries
    == ([$round_trips[].lob_reconnect_boundary] | map(select(.)) | length)
  and .markets.spot.min_lob_source_latency_ms == ([$round_trips[].lob_min_source_latency_ms] | min)
  and .markets.spot.max_lob_source_latency_ms == ([$round_trips[].lob_max_source_latency_ms] | max)
  and .markets.spot.min_lob_bid_levels == ([$round_trips[].lob_min_bid_levels] | min)
  and .markets.spot.min_lob_ask_levels == ([$round_trips[].lob_min_ask_levels] | min)
  and .markets.spot.max_segment_gap_ns == ([$round_trips[].gap_from_previous_ns] | max)
  and all(range(1; ($round_trips | length));
      $round_trips[.].start_received_at_ns >= $round_trips[. - 1].end_received_at_ns
      and $round_trips[.].gap_from_previous_ns
        == ($round_trips[.].start_received_at_ns - $round_trips[. - 1].end_received_at_ns)))
and (.markets.usdm.symbol_count | type) == "number"
and .markets.usdm.symbol_count == (.markets.usdm.symbol_count | floor)
and .markets.usdm.symbol_count >= 400
and .markets.usdm.snapshot_ready_count == .markets.usdm.symbol_count
and .markets.usdm.stream_coverage_verified_count == .markets.usdm.symbol_count
and .markets.usdm.all_stream_coverage_verified == true
and .markets.usdm.sequence_gaps == 0
and (.markets.usdm.upload_failure_count | type) == "number"
and .markets.usdm.upload_failure_count == (.markets.usdm.upload_failure_count | floor)
and .markets.usdm.upload_failure_count >= 0
and (.markets.usdm.health_samples | type) == "number"
and .markets.usdm.health_samples == (.markets.usdm.health_samples | floor)
and .markets.usdm.health_samples >= 40
and (.markets.usdm.max_health_silence_seconds | type) == "number"
and .markets.usdm.max_health_silence_seconds >= 0
and .markets.usdm.max_health_silence_seconds <= 120
and (.markets.usdm.catalog_sha256 | type) == "string"
and (.markets.usdm.catalog_sha256 | test("^[a-f0-9]{64}$"))
and (.markets.usdm.session_id | type) == "string"
and (.markets.usdm.session_id | length) > 0
and (.markets.usdm.oss_roundtrips | type) == "number"
and .markets.usdm.oss_roundtrips == (.markets.usdm.oss_roundtrips | floor)
and .markets.usdm.oss_roundtrips >= 2
and (.markets.usdm.agg_trade_segments | type) == "number"
and .markets.usdm.agg_trade_segments == (.markets.usdm.agg_trade_segments | floor)
and .markets.usdm.agg_trade_segments >= 2
and .markets.usdm.agg_trade_segments == .markets.usdm.oss_roundtrips
and (.markets.usdm.agg_trade_count | type) == "number"
and .markets.usdm.agg_trade_count == (.markets.usdm.agg_trade_count | floor)
and .markets.usdm.agg_trade_count > 0
and .markets.usdm.strict_trade_summary_readback == true
and .markets.usdm.strict_lob_continuity_readback == true
and (.markets.usdm.lob_reconnect_boundaries | type) == "number"
and .markets.usdm.lob_reconnect_boundaries == 0
and (.markets.usdm.min_lob_source_latency_ms | type) == "number"
and .markets.usdm.min_lob_source_latency_ms >= -1000
and (.markets.usdm.max_lob_source_latency_ms | type) == "number"
and .markets.usdm.max_lob_source_latency_ms <= 30000
and .markets.usdm.max_lob_source_latency_ms >= .markets.usdm.min_lob_source_latency_ms
and (.markets.usdm.min_lob_bid_levels | type) == "number"
and .markets.usdm.min_lob_bid_levels > 0
and (.markets.usdm.min_lob_ask_levels | type) == "number"
and .markets.usdm.min_lob_ask_levels > 0
and (.markets.usdm.max_segment_gap_ns | type) == "number"
and .markets.usdm.max_segment_gap_ns == (.markets.usdm.max_segment_gap_ns | floor)
and .markets.usdm.max_segment_gap_ns >= 0
and .markets.usdm.max_segment_gap_ns <= 90000000000
and (.markets.usdm.oss_roundtrip_evidence | type) == "array"
and (.markets.usdm.oss_roundtrip_evidence | length) == .markets.usdm.oss_roundtrips
and all(.markets.usdm.oss_roundtrip_evidence[];
  (.success_uri | type) == "string" and (.success_uri | length) > 0
  and (.sha256 | type) == "string"
  and (.sha256 | test("^[a-f0-9]{64}$"))
  and (.manifest_sha256 | type) == "string"
  and (.manifest_sha256 | test("^[a-f0-9]{64}$"))
  and (.gap_from_previous_ns | type) == "number"
  and .gap_from_previous_ns == (.gap_from_previous_ns | floor)
  and .gap_from_previous_ns >= 0
  and (.start_received_at_ns | type) == "number"
  and .start_received_at_ns == (.start_received_at_ns | floor)
  and (.end_received_at_ns | type) == "number"
  and .end_received_at_ns == (.end_received_at_ns | floor)
  and .end_received_at_ns >= .start_received_at_ns
  and (.agg_trade_count | type) == "number"
  and .agg_trade_count == (.agg_trade_count | floor)
  and .agg_trade_count > 0
  and .lob_capture_session_id == $gate.markets.usdm.session_id
  and (.lob_reconnect_boundary | type) == "boolean"
  and .lob_sequence_gaps == 0
  and .lob_source_time_rollbacks == 0
  and .lob_declared_symbol_count == $gate.markets.usdm.symbol_count
  and .lob_covered_symbol_count == $gate.markets.usdm.symbol_count
  and .stream_coverage_verified_count == $gate.markets.usdm.symbol_count
  and .all_stream_coverage_verified == true
  and (.lob_min_source_latency_ms | type) == "number"
  and .lob_min_source_latency_ms >= -1000
  and (.lob_max_source_latency_ms | type) == "number"
  and .lob_max_source_latency_ms <= 30000
  and .lob_max_source_latency_ms >= .lob_min_source_latency_ms
  and (.lob_min_bid_levels | type) == "number"
  and .lob_min_bid_levels > 0
  and (.lob_min_ask_levels | type) == "number"
  and .lob_min_ask_levels > 0)
and (.markets.usdm.oss_roundtrip_evidence as $round_trips
  | $round_trips[0].gap_from_previous_ns == 0
  and all($round_trips[].lob_reconnect_boundary; . == false)
  and .markets.usdm.lob_reconnect_boundaries
    == ([$round_trips[].lob_reconnect_boundary] | map(select(.)) | length)
  and .markets.usdm.min_lob_source_latency_ms == ([$round_trips[].lob_min_source_latency_ms] | min)
  and .markets.usdm.max_lob_source_latency_ms == ([$round_trips[].lob_max_source_latency_ms] | max)
  and .markets.usdm.min_lob_bid_levels == ([$round_trips[].lob_min_bid_levels] | min)
  and .markets.usdm.min_lob_ask_levels == ([$round_trips[].lob_min_ask_levels] | min)
  and .markets.usdm.max_segment_gap_ns == ([$round_trips[].gap_from_previous_ns] | max)
  and all(range(1; ($round_trips | length));
      $round_trips[.].start_received_at_ns >= $round_trips[. - 1].end_received_at_ns
      and $round_trips[.].gap_from_previous_ns
        == ($round_trips[.].start_received_at_ns - $round_trips[. - 1].end_received_at_ns)))
