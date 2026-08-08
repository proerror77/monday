def sha256: type == "string" and test("^[a-f0-9]{64}$");
def positive_integer: type == "number" and floor == . and . > 0;
def nonnegative_integer: type == "number" and floor == . and . >= 0;
def file_identity:
  type == "string" and test("^[0-9]+:[0-9]+$");
def oss_triplet($dataset):
  . as $triplet
  | .dataset == $dataset
  and ($triplet.uri | type == "string"
    and test("^oss://monday-lob-apne1-1045353359/lake/raw/venue=polymarket/dataset="
      + $dataset
      + "/date=[0-9]{4}-[0-9]{2}-[0-9]{2}/hour=[0-9]{2}/"
      + "(sha256=[a-f0-9]{64}/)?market-updates\\.[A-Za-z0-9._-]+\\.ndjson\\.zst$"))
  and (.file | type == "string"
    and test("^market-updates\\.[A-Za-z0-9._-]+\\.ndjson\\.zst$"))
  and (.bytes | positive_integer) and (.source_bytes | positive_integer)
  and (.sha256 | sha256) and (.manifest_sha256 | sha256)
  and .success_sha256 == .sha256
  and ((($triplet.uri | test("/sha256=")) | not)
    or ($triplet.uri | contains("/sha256=" + $triplet.sha256 + "/")));
def utc_iso8601_unix:
  if type == "string"
    and test("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]+)?Z$")
  then sub("\\.[0-9]+Z$"; "Z") as $timestamp
    | ($timestamp | fromdateiso8601?) as $epoch
    | if ($epoch | type == "number")
        and ($epoch | todateiso8601) == $timestamp
      then $epoch
      else null
      end
  else null
  end;
def runtime_identity($exec; $digest):
  .exec_start == $exec and .cmdline == $exec
  and .cmdline_sha256 == $digest
  and .fragment_path == "/etc/systemd/system/polymarket-reference-collector.service" and .drop_in_paths == []
  and (.main_pid | positive_integer) and (.restarts | nonnegative_integer)
  and (.invocation_id | type == "string" and test("^[a-f0-9]{32}$"));
def nonnegative_sub($left; $right):
  if $left < $right then 0 else ($left - $right) end;
def bounded_legacy_trade_rate_limits:
  . as $snapshot
  | ($snapshot.target_markets
    | select(positive_integer)
    | ((. + 99) / 100 | floor)
    | if . < 3 then 3 elif . > 32 then 32 else . end) as $limit
  | ($snapshot.api_errors
    | type == "array" and length <= $limit
    and all(.[];
      type == "string"
      and test("^trades 0x[0-9A-Fa-f]{64}: HTTP Error 429: Too Many Requests\\z")));
def legacy_health_snapshot($allow_bounded_rate_limits):
  (.updated_at | utc_iso8601_unix | type == "number")
  and (.last_success_at | type == "string" and length > 0)
  and (.target_markets | positive_integer)
  and (if $allow_bounded_rate_limits then bounded_legacy_trade_rate_limits
    else .api_errors == [] end)
  and .malformed_trade_rows == 0
  and .truncated_trade_markets == []
  and .stale_trade_markets == []
  and .stale_settlement_markets == []
  and (.overdue_unresolved_markets
    | type == "array" and all(.[]; type == "string" and length > 0));
def contained_bootstrap_recovery($candidate; $source):
  .mode == "gamma_closed_200"
  and (.candidate_probe.schema
    == "monday.polymarket_gamma_closed_200_recovery_probe.v1")
  and .candidate_probe.candidate_sha256 == $candidate
  and .candidate_probe.source_revision == $source
  and (.candidate_probe.sha256 | sha256)
  and (.candidate_probe.observed_at | utc_iso8601_unix | type == "number")
  and .candidate_probe.gamma.tagged_closed
    == {query:"closed=true&tag_id=21",attempts:3,http_status:200}
  and .candidate_probe.gamma.untagged_closed
    == {query:"closed=true",attempts:3,http_status:200}
  and .candidate_probe.candidate_once.exit_status == 0
  and (.candidate_probe.candidate_once.duration_seconds | positive_integer and . <= 180)
  and .candidate_probe.candidate_once.health_updated_at
    == .candidate_probe.observed_at
  and .baseline.active_state == "inactive"
  and .baseline.main_pid == 0
  and .baseline.exec_start
    == "/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200"
  and .baseline.fragment_path
    == "/etc/systemd/system/polymarket-reference-collector.service"
  and .baseline.drop_in_paths == []
  and (.baseline.restarts | nonnegative_integer)
  and (.baseline.invocation_id | type == "string" and test("^[a-f0-9]{32}$"))
  and .baseline.binary_path == "/opt/monday/bin/polymarket-raw-ops"
  and (.baseline.binary_sha256 | sha256)
  and .baseline.binary_sha256 != $candidate;
def recovery_matches_gate:
  . as $gate
  | ($gate.recovery | contained_bootstrap_recovery(
      $gate.candidate_sha256; $gate.deployment_source_revision));

.schema == "monday.polymarket_shadow_gate.v1"
and (.candidate_sha256 | sha256)
and (.deployment_bundle_sha256 | sha256)
and (.deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
and (.release_manifest_sha256 | sha256)
and (.control_archive_sha256 | sha256)
and (.oss_config_sha256 | sha256)
and .real_market_preflight.schema == "monday.polymarket_real_market_preflight.v2"
and .real_market_preflight.status == "passed"
and .real_market_preflight.candidate_sha256 == .candidate_sha256
and (.real_market_preflight.deployment_source_revision
  == .deployment_source_revision)
and (.real_market_preflight.deployment_bundle_sha256
  == .deployment_bundle_sha256)
and (.real_market_preflight.release_manifest_sha256
  == .release_manifest_sha256)
and (.real_market_preflight.control_archive_sha256
  == .control_archive_sha256)
and .real_market_preflight.oss_config_sha256 == .oss_config_sha256
and (.real_market_preflight.dataset | type == "string"
  and test("^crypto_expiry_preflight_[a-f0-9]{12}_[a-z0-9_-]+$"))
and (.real_market_preflight.dataset == ("crypto_expiry_preflight_"
  + .candidate_sha256[0:12] + "_" + (.shadow_run_id | ascii_downcase)))
and (.real_market_preflight.source_quote_records | positive_integer)
and .real_market_preflight.source_recorded_hours == 1
and .real_market_preflight.source_scan_bounded == true
and (.real_market_preflight.source_content_sha256 | sha256)
and (.real_market_preflight.source_segment as $segment
  | ($segment.file | type == "string"
    and test("^market-updates\\.[0-9]{8}T[0-9]{6}([0-9]{6})?(\\.[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12})?\\.ndjson$"))
  and $segment.path == ("/data/monday/spool/polymarket/" + $segment.file)
  and ($segment.bytes | positive_integer)
  and ($segment.sha256 | sha256)
  and $segment.sha256 == .real_market_preflight.source_content_sha256
  and ($segment.file_identity | file_identity)
  and ($segment.modified_at_unix | nonnegative_integer))
and (.real_market_preflight.uploaded_content_sha256
  == .real_market_preflight.source_content_sha256)
and (.real_market_preflight.uploaded_triplet.dataset
  == .real_market_preflight.dataset)
and (.real_market_preflight.uploaded_triplet
  | oss_triplet(.dataset))
and (.real_market_preflight.uploaded_triplet
  | (.canonical | type == "boolean")
  and (.segment_complete | type == "boolean")
  and .canonical == .segment_complete)
and (.real_market_preflight.source_segment.bytes
  == .real_market_preflight.uploaded_triplet.source_bytes)
and .real_market_preflight.upload_summary.uploaded_segments == 1
and (.real_market_preflight.upload_summary.canonical_uploaded_segments
  | nonnegative_integer)
and (.real_market_preflight.upload_summary.canonical_uploaded_segments
  == (if .real_market_preflight.uploaded_triplet.canonical then 1 else 0 end))
and .real_market_preflight.upload_summary.pending_segments == 0
and .real_market_preflight.upload_summary.failed_segments == []
and .real_market_preflight.upload_summary.last_error == null
and (.real_market_preflight.started_at | utc_iso8601_unix | type == "number")
and (.real_market_preflight.completed_at | utc_iso8601_unix | type == "number")
and ((.real_market_preflight.started_at | utc_iso8601_unix)
  <= (.real_market_preflight.completed_at | utc_iso8601_unix))
and ((.real_market_preflight.completed_at | utc_iso8601_unix)
  <= (.started_at | utc_iso8601_unix))
and (.duration_seconds | positive_integer and . >= 3600 and . <= 3601)
and (.started_at | utc_iso8601_unix | type == "number")
and (.parity_window_started_at_unix | positive_integer)
and (.parity_window_ended_at_unix | positive_integer)
and (.parity_window_ended_at_unix - .parity_window_started_at_unix >= 601)
and (.completed_at | utc_iso8601_unix | type == "number")
and ((.completed_at | utc_iso8601_unix)
  - (.started_at | utc_iso8601_unix) >= .duration_seconds)
and (.parity_window_started_at_unix >= (.started_at | utc_iso8601_unix))
and (.parity_window_ended_at_unix
  <= ((.started_at | utc_iso8601_unix) + .duration_seconds))
and .production_eligible == true
and .passed == true
and (
  (
    .baseline_mode == "legacy_python"
    and .recovery == null
    and .baseline_runtime_stability_required == true
    and (
      .baseline_health_start_required == false
      or (
        .baseline_health_start_required == true
        and (.baseline_health_snapshot | legacy_health_snapshot(true))
    and (.baseline_health_start_success_unix | positive_integer)
    and ((.baseline_health_snapshot.last_success_at | utc_iso8601_unix)
      == .baseline_health_start_success_unix)
    and (.baseline_health_start_written_at_unix | positive_integer)
    and (.baseline_health_start_file_identity | file_identity)
    and .baseline_health_start_success_unix
      <= .baseline_health_start_written_at_unix
    and .baseline_health_start_written_at_unix
      <= (.started_at | utc_iso8601_unix)
    and ((.started_at | utc_iso8601_unix)
      - .baseline_health_start_written_at_unix <= 2700)
    and (.baseline_health_completion_required | type == "boolean")
    and (
      if .baseline_health_completion_required then
        (.baseline_health_completion_snapshot | legacy_health_snapshot(false))
        and (.baseline_health_completion_snapshot.updated_at
          != .baseline_health_snapshot.updated_at)
        and (.baseline_health_completion_snapshot.last_success_at
          != .baseline_health_snapshot.last_success_at)
        and (.baseline_health_cutoff_unix | positive_integer)
        and ((.baseline_health_completion_snapshot.last_success_at | utc_iso8601_unix)
          == .baseline_health_cutoff_unix)
        and .baseline_health_cutoff_unix > .baseline_health_start_success_unix
        and (.baseline_health_completion_written_at_unix | positive_integer)
        and (.baseline_health_completion_file_identity | file_identity)
        and .baseline_health_completion_file_identity
          != .baseline_health_start_file_identity
        and .baseline_health_cutoff_unix
          <= .baseline_health_completion_written_at_unix
        and .baseline_health_completion_written_at_unix
          >= (.started_at | utc_iso8601_unix)
        and .baseline_health_completion_written_at_unix
          <= (.completed_at | utc_iso8601_unix)
        and .parity_window_ended_at_unix <= .baseline_health_cutoff_unix
      else
        .baseline_health_completion_snapshot == null
        and .baseline_health_cutoff_unix == null
        and .baseline_health_completion_written_at_unix == null
        and .baseline_health_completion_file_identity == null
      end
    )
      )
    )
    and (.legacy_runtime |
      runtime_identity("/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py";
        "dffeb118d105e9312898460249f514eb982c20433cd20840ffb2107c64bbca4a")
      and ([has("release_path"),has("release_sha256"),has("proc_exe")] | any | not))
  )
  or
  (
    .baseline_mode == "rust_release"
    and .recovery == null
    and .baseline_health_start_required == false
    and .baseline_runtime_stability_required == true
    and .baseline_health_completion_required == false
    and .baseline_health_snapshot == null
    and .baseline_health_completion_snapshot == null
    and .baseline_health_start_success_unix == null
    and .baseline_health_cutoff_unix == null
    and .baseline_health_start_written_at_unix == null
    and .baseline_health_completion_written_at_unix == null
    and .baseline_health_start_file_identity == null
    and .baseline_health_completion_file_identity == null
    and (.legacy_runtime |
      runtime_identity("/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200";
        "6d942117a378a42f06376acb78dbc312c85e1146fc05ff17d1699b8a6007edec"))
    and (.legacy_runtime.release_sha256 | sha256)
    and .candidate_sha256 != .legacy_runtime.release_sha256
    and .legacy_runtime.release_path == ("/opt/monday/releases/polymarket-raw-ops/"
      + .legacy_runtime.release_sha256 + "/polymarket-raw-ops")
    and .legacy_runtime.proc_exe == .legacy_runtime.release_path
  )
  or
  (
    .baseline_mode == "rust_bootstrap"
    and .recovery == null
    and .baseline_degraded == true
    and .baseline_health_start_required == false
    and .baseline_runtime_stability_required == true
    and .baseline_health_completion_required == false
    and .baseline_health_snapshot == null
    and .baseline_health_completion_snapshot == null
    and .baseline_health_start_success_unix == null
    and .baseline_health_cutoff_unix == null
    and .baseline_health_start_written_at_unix == null
    and .baseline_health_completion_written_at_unix == null
    and .baseline_health_start_file_identity == null
    and .baseline_health_completion_file_identity == null
    and (.legacy_runtime |
      runtime_identity("/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200";
        "6d942117a378a42f06376acb78dbc312c85e1146fc05ff17d1699b8a6007edec"))
    and (.legacy_runtime.release_sha256 | sha256)
    and .candidate_sha256 != .legacy_runtime.release_sha256
    and .legacy_runtime.release_path == "/opt/monday/bin/polymarket-raw-ops"
    and .legacy_runtime.proc_exe == .legacy_runtime.release_path
  )
  or
  (
    .baseline_mode == "rust_bootstrap"
    and .baseline_degraded == true
    and .baseline_health_start_required == false
    and .baseline_runtime_stability_required == false
    and .baseline_health_completion_required == false
    and .baseline_health_snapshot == null
    and .baseline_health_completion_snapshot == null
    and .baseline_health_start_success_unix == null
    and .baseline_health_cutoff_unix == null
    and .baseline_health_start_written_at_unix == null
    and .baseline_health_completion_written_at_unix == null
    and .baseline_health_start_file_identity == null
    and .baseline_health_completion_file_identity == null
    and .legacy_runtime == null
    and recovery_matches_gate
  )
)
and (
  .shadow_runtime.exec_start == (
    "/opt/monday/releases/polymarket-raw-ops/" + .candidate_sha256
    + "/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200"
    + " --spool-dir ${MONDAY_POLYMARKET_SHADOW_SPOOL}"
  )
  or .shadow_runtime.exec_start == .shadow_runtime.cmdline
)
and .shadow_runtime.cmdline == (
  "/opt/monday/releases/polymarket-raw-ops/" + .candidate_sha256
  + "/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200"
  + " --spool-dir "
  + "/data/monday/spool/polymarket-reference-rust-shadow/"
  + .candidate_sha256 + "/" + .shadow_run_id
)
and .shadow_runtime.fragment_path == "/etc/systemd/system/polymarket-reference-collector-shadow@.service"
and .shadow_runtime.drop_in_paths == []
and (.shadow_runtime.main_pid | positive_integer)
and .shadow_runtime.restarts == 0
and (.shadow_runtime.invocation_id | type == "string" and test("^[a-f0-9]{32}$"))
and .shadow_runtime.memory_events.start.high == 0
and .shadow_runtime.memory_events.start.max == 0
and .shadow_runtime.memory_events.start.oom == 0
and .shadow_runtime.memory_events.start.oom_kill == 0
and .shadow_runtime.memory_events.start.oom_group_kill == 0
and (.shadow_runtime.memory_events.end.high | nonnegative_integer)
and (.shadow_runtime.memory_events.end.high ==
  .shadow_runtime.memory_events.start.high)
and .shadow_runtime.memory_events.end.max == 0
and .shadow_runtime.memory_events.end.oom == 0
and .shadow_runtime.memory_events.end.oom_kill == 0
and .shadow_runtime.memory_events.end.oom_group_kill == 0
and .checks.byte_parity == true
and .checks.metadata_parity == true
and .checks.field_parity == true
and .checks.dedupe_parity == true
and .checks.trade_coverage_parity == true
and .checks.trade_contract_parity == true
and .checks.settlement_parity == true
and .checks.rotation_parity == true
and .checks.asset_parity == true
and .checks.health_freshness == true
and .checks.candidate_identity == true
and .checks.memory_events_stable == true
and .checks.oss_readback_parity == true
and .checks.market_oss_readback_parity == true
and .checks.real_market_segment_preflight == true
and (.comparison_mode == "legacy_overlap" or (
  .comparison_mode == "rust_self"
  and (.baseline_mode == "legacy_python" or .baseline_mode == "rust_bootstrap")
  and (.baseline_runtime_stability_required == true
    or recovery_matches_gate)
))
and (.metrics.oss_uploaded_segments | positive_integer)
and (.metrics.oss_canonical_uploaded_segments | positive_integer)
and (.metrics.market_oss_uploaded_segments | positive_integer)
and (.metrics.market_oss_canonical_uploaded_segments | nonnegative_integer)
and (.metrics.market_oss_uploaded_segments
  == .real_market_preflight.upload_summary.uploaded_segments)
and (.metrics.market_oss_canonical_uploaded_segments
  == .real_market_preflight.upload_summary.canonical_uploaded_segments)
and (.metrics.rust_closed_tape_count | positive_integer)
and (.metrics.rust_trade_count | positive_integer)
and (.metrics.legacy_only_trade_ids | type == "array" and length == 0)
and (.metrics.rust_only_trade_ids | type == "array")
and (.metrics.rust_metadata_count | positive_integer)
and (.metrics.legacy_only_metadata_ids | type == "array" and length == 0)
and (.metrics.rust_only_metadata_ids | type == "array")
and .metrics.metadata_shared_values_match == true
and (.metrics.metadata_shared_value_mismatch_ids | type == "array" and length == 0)
and (.metrics.rust_settlement_count | positive_integer)
and (.metrics.legacy_only_settlement_ids | type == "array" and length == 0)
and (.metrics.rust_only_settlement_ids | type == "array")
and (if .comparison_mode == "legacy_overlap" then
  (.metrics.legacy_trade_count | positive_integer)
  and (.metrics.legacy_metadata_count | positive_integer)
  and (.metrics.legacy_settlement_count | positive_integer)
else
  .metrics.legacy_trade_count == 0
  and .metrics.legacy_settlement_count == 0
  and (
    .metrics.legacy_metadata_count == 0
    or (
      .baseline_mode == "rust_bootstrap"
      and (.metrics.legacy_metadata_count | positive_integer)
    )
  )
end)
and .metrics.settlement_shared_values_match == true
and (.metrics.settlement_shared_value_mismatch_ids | type == "array" and length == 0)
and .metrics.trade_shared_values_match == true
and (.metrics.trade_shared_value_mismatch_ids | type == "array" and length == 0)
and .metrics.trade_metadata_shared_values_match == true
and (.metrics.trade_metadata_shared_value_mismatch_market_ids
  | type == "array" and length == 0)
and .metrics.trade_maturity_lag_seconds == 600
and .metrics.trade_event_window_started_at_unix == .parity_window_started_at_unix
and (.metrics.trade_event_window_ended_at_unix ==
  nonnegative_sub(.parity_window_ended_at_unix;
    .metrics.trade_maturity_lag_seconds))
and (.metrics.trade_event_window_ended_at_unix
  > .metrics.trade_event_window_started_at_unix)
and .metrics.legacy_trade_metadata_context_match == true
and .metrics.rust_trade_metadata_context_match == true
and (.metrics.legacy_trade_metadata_context_mismatch_market_ids
  | type == "array" and length == 0)
and (.metrics.rust_trade_metadata_context_mismatch_market_ids
  | type == "array" and length == 0)
and .metrics.settlement_event_lookback_seconds == 900
and .metrics.settlement_maturity_lag_seconds == 600
and (.metrics.settlement_event_window_started_at_unix ==
  nonnegative_sub(.parity_window_started_at_unix;
    .metrics.settlement_event_lookback_seconds))
and (.metrics.settlement_event_window_ended_at_unix ==
  nonnegative_sub(.parity_window_ended_at_unix;
    .metrics.settlement_maturity_lag_seconds))
and .metrics.legacy_settlement_metadata_context_match == true
and .metrics.rust_settlement_metadata_context_match == true
and (.metrics.legacy_settlement_metadata_context_mismatch_market_ids
  | type == "array" and length == 0)
and (.metrics.rust_settlement_metadata_context_mismatch_market_ids
  | type == "array" and length == 0)
and (.metrics.legacy_duplicate_trade_ids | type == "array" and length == 0)
and (.metrics.rust_duplicate_trade_ids | type == "array" and length == 0)
and (.metrics.normalized_trade_sha256 | sha256)
and (.metrics.normalized_metadata_sha256 | sha256)
and (.metrics.normalized_settlement_sha256 | sha256)
