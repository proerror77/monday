def sha256: type == "string" and test("^[a-f0-9]{64}$");
def positive_integer: type == "number" and floor == . and . > 0;
def nonnegative_integer: type == "number" and floor == . and . >= 0;

.schema == "monday.polymarket_shadow_gate.v1"
and (.candidate_sha256 | sha256)
and (.deployment_bundle_sha256 | sha256)
and (.deployment_source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
and (.oss_config_sha256 | sha256)
and (.duration_seconds | positive_integer and . >= 3900)
and (.parity_window_started_at_unix | positive_integer)
and (.parity_window_ended_at_unix | positive_integer)
and (.parity_window_ended_at_unix - .parity_window_started_at_unix >= 300)
and (.completed_at | type == "string" and (fromdateiso8601? | type == "number"))
and .production_eligible == true
and .passed == true
and .legacy_runtime.exec_start == "/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py"
and .legacy_runtime.cmdline == "/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py"
and .legacy_runtime.cmdline_sha256 == "dffeb118d105e9312898460249f514eb982c20433cd20840ffb2107c64bbca4a"
and .legacy_runtime.fragment_path == "/etc/systemd/system/polymarket-reference-collector.service"
and .legacy_runtime.drop_in_paths == []
and (.legacy_runtime.main_pid | positive_integer)
and (.legacy_runtime.restarts | nonnegative_integer)
and (.legacy_runtime.invocation_id | type == "string" and test("^[a-f0-9]{32}$"))
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
and .checks.byte_parity == true
and .checks.metadata_parity == true
and .checks.field_parity == true
and .checks.dedupe_parity == true
and .checks.settlement_parity == true
and .checks.rotation_parity == true
and .checks.asset_parity == true
and .checks.health_freshness == true
and .checks.candidate_identity == true
and .checks.oss_readback_parity == true
and .checks.market_oss_readback_parity == true
and (.metrics.oss_uploaded_segments | positive_integer)
and (.metrics.oss_canonical_uploaded_segments | positive_integer)
and (.metrics.market_oss_uploaded_segments | positive_integer)
and (.metrics.market_oss_canonical_uploaded_segments | positive_integer)
and (.metrics.rust_closed_tape_count | positive_integer)
and (.metrics.legacy_trade_count | positive_integer)
and (.metrics.rust_trade_count | positive_integer)
and (.metrics.legacy_metadata_count | positive_integer)
and (.metrics.rust_metadata_count | positive_integer)
and (.metrics.legacy_only_metadata_ids | type == "array" and length == 0)
and (.metrics.rust_only_metadata_ids | type == "array" and length == 0)
and (.metrics.legacy_settlement_count | positive_integer)
and (.metrics.rust_settlement_count | positive_integer)
and (.metrics.rust_duplicate_trade_ids | type == "array" and length == 0)
