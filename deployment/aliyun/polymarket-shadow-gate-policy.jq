def sha256: type == "string" and test("^[a-f0-9]{64}$");
def positive_integer: type == "number" and floor == . and . > 0;
def nonnegative_integer: type == "number" and floor == . and . >= 0;
def runtime_identity($exec; $digest):
  .exec_start == $exec and .cmdline == $exec
  and .cmdline_sha256 == $digest
  and .fragment_path == "/etc/systemd/system/polymarket-reference-collector.service" and .drop_in_paths == []
  and (.main_pid | positive_integer) and (.restarts | nonnegative_integer)
  and (.invocation_id | type == "string" and test("^[a-f0-9]{32}$"));
def nonnegative_sub($left; $right):
  if $left < $right then 0 else ($left - $right) end;

.schema == "monday.polymarket_shadow_gate.v1"
and (.candidate_sha256 | sha256)
and (.deployment_bundle_sha256 | sha256)
and (.deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
and (.release_manifest_sha256 | sha256)
and (.control_archive_sha256 | sha256)
and (.oss_config_sha256 | sha256)
and (.duration_seconds | positive_integer and . >= 4201)
and (.parity_window_started_at_unix | positive_integer)
and (.parity_window_ended_at_unix | positive_integer)
and (.parity_window_ended_at_unix - .parity_window_started_at_unix >= 601)
and (.completed_at | type == "string" and (fromdateiso8601? | type == "number"))
and .production_eligible == true
and .passed == true
and (
  (
    .baseline_mode == "legacy_python"
    and (.legacy_runtime |
      runtime_identity("/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py";
        "dffeb118d105e9312898460249f514eb982c20433cd20840ffb2107c64bbca4a")
      and ([has("release_path"),has("release_sha256"),has("proc_exe")] | any | not))
  )
  or
  (
    .baseline_mode == "rust_release"
    and (.legacy_runtime |
      runtime_identity("/opt/monday/bin/polymarket-raw-ops collect-reference";
        "7b06db4beb374f013a090e023289f8b026f39c324ee527f194b706656f6a1f94"))
    and (.legacy_runtime.release_sha256 | sha256)
    and .candidate_sha256 != .legacy_runtime.release_sha256
    and .legacy_runtime.release_path == ("/opt/monday/releases/polymarket-raw-ops/"
      + .legacy_runtime.release_sha256 + "/polymarket-raw-ops")
    and .legacy_runtime.proc_exe == .legacy_runtime.release_path
  )
)
and (
  .shadow_runtime.exec_start == (
    "/opt/monday/releases/polymarket-raw-ops/" + .candidate_sha256
    + "/polymarket-raw-ops collect-reference --spool-dir ${MONDAY_POLYMARKET_SHADOW_SPOOL}"
  )
  or .shadow_runtime.exec_start == .shadow_runtime.cmdline
)
and .shadow_runtime.cmdline == (
  "/opt/monday/releases/polymarket-raw-ops/" + .candidate_sha256
  + "/polymarket-raw-ops collect-reference --spool-dir "
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
and .checks.settlement_parity == true
and .checks.rotation_parity == true
and .checks.asset_parity == true
and .checks.health_freshness == true
and .checks.candidate_identity == true
and .checks.memory_events_stable == true
and .checks.oss_readback_parity == true
and .checks.market_oss_readback_parity == true
and (.metrics.oss_uploaded_segments | positive_integer)
and (.metrics.oss_canonical_uploaded_segments | positive_integer)
and (.metrics.market_oss_uploaded_segments | positive_integer)
and (.metrics.market_oss_canonical_uploaded_segments | positive_integer)
and (.metrics.rust_closed_tape_count | positive_integer)
and (.metrics.legacy_trade_count | positive_integer)
and (.metrics.rust_trade_count | positive_integer)
and (.metrics.legacy_only_trade_ids | type == "array" and length == 0)
and (.metrics.rust_only_trade_ids | type == "array" and length == 0)
and (.metrics.legacy_metadata_count | positive_integer)
and (.metrics.rust_metadata_count | positive_integer)
and (.metrics.legacy_only_metadata_ids | type == "array" and length == 0)
and (.metrics.rust_only_metadata_ids | type == "array")
and .metrics.metadata_shared_values_match == true
and (.metrics.metadata_shared_value_mismatch_ids | type == "array" and length == 0)
and (.metrics.legacy_settlement_count | positive_integer)
and (.metrics.rust_settlement_count | positive_integer)
and (.metrics.legacy_only_settlement_ids | type == "array" and length == 0)
and (.metrics.rust_only_settlement_ids | type == "array")
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
