def valid_io_full_psi_windows:
  type == "array"
  and length >= 3
  and all(.[];
    (.phase | type) == "string" and (.phase | length) > 0
    and (.phase_run | type) == "number"
    and .phase_run == (.phase_run | floor) and .phase_run > 0
    and (.stage == "calibration" or .stage == "runtime")
    and (.started_at | type) == "string"
    and (.started_at | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
    and (.finished_at | type) == "string"
    and (.finished_at | test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"))
    and .finished_at >= .started_at
    and (.previous_total_us | type) == "number"
    and .previous_total_us == (.previous_total_us | floor)
    and .previous_total_us >= 0
    and (.current_total_us | type) == "number"
    and .current_total_us == (.current_total_us | floor)
    and .current_total_us >= .previous_total_us
    and .delta_us == (.current_total_us - .previous_total_us)
    and (.window_us | type) == "number"
    and .window_us == (.window_us | floor) and .window_us > 0
    and (.ratio | type) == "number" and .ratio >= 0
    and (((.ratio - (.delta_us / .window_us)) as $difference
      | (if $difference < 0 then -$difference else $difference end)) <= 0.000000001)
    and .hit == ((.delta_us / .window_us) >= (150000 / 15000000))
    and (.consecutive_hits | type) == "number"
    and .consecutive_hits == (.consecutive_hits | floor)
    and .consecutive_hits >= 0 and .consecutive_hits < 3)
  and (reduce .[] as $window
    ({key:null,hits:0,current:null,valid:true};
      ([$window.phase,$window.phase_run,$window.stage]) as $key
      | (if .key == $key then .hits else 0 end) as $previous_hits
      | (if $window.hit then ($previous_hits + 1) else 0 end) as $expected_hits
      | .valid = (.valid
          and $window.consecutive_hits == $expected_hits
          and (if .key == $key then
            $window.previous_total_us == .current
          else true end))
      | .key = $key
      | .hits = $expected_hits
      | .current = $window.current_total_us)
    | .valid);

. as $gate
| .schema == "monday.rust_lob_shadow_gate.v4"
and .candidate_sha256 == $candidate_sha256
and .runtime_contract_sha256 == $runtime_contract_sha256
and (.deployment_bundle_sha256 | type) == "string"
and (.deployment_bundle_sha256 | test("^[a-f0-9]{64}$"))
and (.deployment_source_revision | type) == "string"
and (.deployment_source_revision | test("^[a-f0-9]{40,64}$"))
and (.run_id | type) == "string"
and (.run_id | test("^[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$"))
and .run_spool == ("/data/monday/spool/binance-lob-rust-shadow/runs/"
  + $candidate_sha256 + "/" + .run_id)
and (.observation_started_ns | type) == "number"
and .observation_started_ns == (.observation_started_ns | floor)
and .observation_started_ns > 0
and (.markets.spot.observation_started_ns | type) == "number"
and .markets.spot.observation_started_ns == (.markets.spot.observation_started_ns | floor)
and .markets.spot.observation_started_ns > 0
and (.markets.usdm.observation_started_ns | type) == "number"
and .markets.usdm.observation_started_ns == (.markets.usdm.observation_started_ns | floor)
and .markets.usdm.observation_started_ns > 0
and .observation_started_ns == .markets.spot.observation_started_ns
and .markets.spot.observation_started_ns <= .markets.usdm.observation_started_ns
and .required_duration_seconds == 240
and .requested_duration_seconds >= .required_duration_seconds
and .health_settle_seconds == 240
and .segment_seconds == 120
and .test_only == false
and .passed == true
and .production_eligible == true
and .checks_passed == true
and (.io_full_psi_windows | valid_io_full_psi_windows)
and (["resource-preflight","shadow-spot","upload-drain-spot","shadow-usdm",
    "upload-drain-usdm","oss-roundtrip-spot","oss-roundtrip-usdm"] as $required
  | all($required[]; . as $phase
      | ([$gate.io_full_psi_windows[]
          | select(.phase == $phase and .stage == "calibration")] | length) >= 3)
  and all($required[1:][]; . as $phase
      | ([$gate.io_full_psi_windows[]
          | select(.phase == $phase and .stage == "runtime")] | length) >= 1))
and ([.io_full_psi_windows[] | select(.phase | startswith("strict-verifier-"))]
  | length >= 4
  and ([.[].phase] | unique) as $strict_phases
  | all($strict_phases[]; . as $phase
      | ([$gate.io_full_psi_windows[]
          | select(.phase == $phase and .stage == "calibration")] | length) >= 3
      and ([$gate.io_full_psi_windows[]
          | select(.phase == $phase and .stage == "runtime")] | length) >= 1))
and (.duration_seconds | type) == "number"
and .duration_seconds == (.duration_seconds | floor)
and .duration_seconds >= 240
and (.markets.spot.symbol_count | type) == "number"
and .markets.spot.symbol_count == (.markets.spot.symbol_count | floor)
and .markets.spot.symbol_count >= 1000
and .markets.spot.symbols_config == "ALL"
and .markets.spot.snapshot_ready_count == .markets.spot.symbol_count
and .markets.spot.stream_coverage_verified_count == .markets.spot.symbol_count
and .markets.spot.all_stream_coverage_verified == true
and .markets.spot.sequence_gaps == 0
and (.markets.spot.upload_failure_count | type) == "number"
and .markets.spot.upload_failure_count == (.markets.spot.upload_failure_count | floor)
and .markets.spot.upload_failure_count >= 0
and (.markets.spot.health_samples | type) == "number"
and .markets.spot.health_samples == (.markets.spot.health_samples | floor)
and .markets.spot.health_samples >= 8
and (.markets.spot.max_health_silence_seconds | type) == "number"
and .markets.spot.max_health_silence_seconds >= 0
and .markets.spot.max_health_silence_seconds <= 120
and (.markets.spot.catalog_sha256 | type) == "string"
and (.markets.spot.catalog_sha256 | test("^[a-f0-9]{64}$"))
and .markets.spot.configured_catalog_sha256 == .markets.spot.catalog_sha256
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
  | $round_trips[0].start_received_at_ns <= $gate.markets.spot.observation_started_ns
  and $round_trips[0].end_received_at_ns > $gate.markets.spot.observation_started_ns
  and $round_trips[0].gap_from_previous_ns == 0
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
and .markets.usdm.symbol_count == 100
and (.markets.usdm.symbols_config | type) == "string"
and (.markets.usdm.symbols_config | test("^[A-Z0-9]+(,[A-Z0-9]+)*$"))
and (.markets.usdm.symbols_config | split(",") | length) == 100
and (.markets.usdm.symbols_config | split(",") | unique | length) == 100
and .markets.usdm.snapshot_ready_count == .markets.usdm.symbol_count
and .markets.usdm.stream_coverage_verified_count == .markets.usdm.symbol_count
and .markets.usdm.all_stream_coverage_verified == true
and .markets.usdm.sequence_gaps == 0
and (.markets.usdm.upload_failure_count | type) == "number"
and .markets.usdm.upload_failure_count == (.markets.usdm.upload_failure_count | floor)
and .markets.usdm.upload_failure_count >= 0
and (.markets.usdm.health_samples | type) == "number"
and .markets.usdm.health_samples == (.markets.usdm.health_samples | floor)
and .markets.usdm.health_samples >= 8
and (.markets.usdm.max_health_silence_seconds | type) == "number"
and .markets.usdm.max_health_silence_seconds >= 0
and .markets.usdm.max_health_silence_seconds <= 120
and (.markets.usdm.catalog_sha256 | type) == "string"
and (.markets.usdm.catalog_sha256 | test("^[a-f0-9]{64}$"))
and .markets.usdm.configured_catalog_sha256 == .markets.usdm.catalog_sha256
and (.markets.usdm.session_id | type) == "string"
and (.markets.usdm.session_id | length) > 0
and (.markets.usdm.oss_roundtrips | type) == "number"
and .markets.usdm.oss_roundtrips == (.markets.usdm.oss_roundtrips | floor)
and .markets.usdm.oss_roundtrips >= 2
and (.markets.usdm.agg_trade_segments | type) == "number"
and .markets.usdm.agg_trade_segments == (.markets.usdm.agg_trade_segments | floor)
and .markets.usdm.agg_trade_segments == 0
and (.markets.usdm.agg_trade_count | type) == "number"
and .markets.usdm.agg_trade_count == (.markets.usdm.agg_trade_count | floor)
and .markets.usdm.agg_trade_count == 0
and .markets.usdm.strict_trade_summary_readback == false
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
  and .agg_trade_count == 0
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
  | $round_trips[0].start_received_at_ns <= $gate.markets.usdm.observation_started_ns
  and $round_trips[0].end_received_at_ns > $gate.markets.usdm.observation_started_ns
  and $round_trips[0].gap_from_previous_ns == 0
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
and .markets.spot.tape_schema == .markets.usdm.tape_schema
and (.markets.spot.tape_schema == "binance.market_tape.v1"
  or .markets.spot.tape_schema == "binance.market_tape.v2")
and (if .markets.spot.tape_schema == "binance.market_tape.v1" then
  (.markets.spot | has("stream_types") | not)
  and (.markets.spot | has("raw_trade_segments") | not)
  and (.markets.spot | has("raw_trade_count") | not)
  and (.markets.spot | has("book_ticker_count") | not)
  and (.markets.spot | has("force_order_count") | not)
  and (.markets.spot | has("strict_raw_trade_continuity_readback") | not)
  and (.markets.spot.full_stream_coverage_verified == null
    or .markets.spot.full_stream_coverage_verified == true)
  and all(.markets.spot.oss_roundtrip_evidence[];
    (has("raw_trade_count") | not)
    and (has("book_ticker_count") | not)
    and (has("force_order_count") | not))
else
  .markets.spot.stream_types == ["aggTrade","bookTicker","depth@100ms","trade"]
  and (.markets.spot.raw_trade_segments | type) == "number"
  and .markets.spot.raw_trade_segments == (.markets.spot.raw_trade_segments | floor)
  and .markets.spot.raw_trade_segments >= 2
  and .markets.spot.raw_trade_segments == .markets.spot.oss_roundtrips
  and (.markets.spot.raw_trade_count | type) == "number"
  and .markets.spot.raw_trade_count == (.markets.spot.raw_trade_count | floor)
  and .markets.spot.raw_trade_count > 0
  and .markets.spot.raw_trade_count
    == ([.markets.spot.oss_roundtrip_evidence[].raw_trade_count] | add)
  and (.markets.spot.book_ticker_count | type) == "number"
  and .markets.spot.book_ticker_count == (.markets.spot.book_ticker_count | floor)
  and .markets.spot.book_ticker_count > 0
  and .markets.spot.book_ticker_count
    == ([.markets.spot.oss_roundtrip_evidence[].book_ticker_count] | add)
  and (.markets.spot | has("force_order_count") | not)
  and .markets.spot.strict_raw_trade_continuity_readback == true
  and .markets.spot.full_stream_coverage_verified == true
  and all(.markets.spot.oss_roundtrip_evidence[];
    (.raw_trade_count | type) == "number"
    and .raw_trade_count == (.raw_trade_count | floor)
    and .raw_trade_count > 0
    and (.book_ticker_count | type) == "number"
    and .book_ticker_count == (.book_ticker_count | floor)
    and .book_ticker_count > 0
    and (has("force_order_count") | not))
end)
and .markets.usdm.tape_schema == "binance.market_tape.v2"
and .markets.usdm.stream_types == ["depth@100ms"]
and (.markets.usdm.raw_trade_segments | type) == "number"
and .markets.usdm.raw_trade_segments == 0
and (.markets.usdm.raw_trade_count | type) == "number"
and .markets.usdm.raw_trade_count == 0
and (.markets.usdm.book_ticker_count | type) == "number"
and .markets.usdm.book_ticker_count == ([.markets.usdm.oss_roundtrip_evidence[].book_ticker_count] | add)
and .markets.usdm.book_ticker_count == 0
and (.markets.usdm.force_order_count | type) == "number"
and .markets.usdm.force_order_count == 0
and .markets.usdm.strict_raw_trade_continuity_readback == false
and .markets.usdm.full_stream_coverage_verified == true
and all(.markets.usdm.oss_roundtrip_evidence[];
  .raw_trade_count == 0
  and .book_ticker_count == 0
  and .force_order_count == 0)
