# Bybit Options shadow-gate evidence policy.
#
# A production-eligible gate must be a full-duration, non-test run whose shadow
# service stayed active without restarts, whose health stayed fail-closed (no
# disk/spool/upload warning, at least one connected worker, a fresh event, and
# the full symbol catalog), and whose upload drain succeeded with zero failures
# and an empty spool.
#
# Arguments:
#   $candidate_sha256            candidate binary SHA-256
#   $deployment_bundle_sha256    deployment bundle SHA-256
#   $deployment_source_revision  deployment source git revision
#   $minimum_symbols             minimum distinct symbols that must be observed
#   $test_only                   true when the run was test-shortened
def sha256:
  type == "string" and test("^[a-f0-9]{64}$");

def health_ok:
  (.health.schema | type) == "string"
  and .health.schema == "monday.bybit_options_quote.v1"
  and .health.disk_warning == false
  and .health.spool_warning == false
  and (.health.upload_failure_count | type) == "number"
  and .health.upload_failure_count == (.health.upload_failure_count | floor)
  and .health.upload_failure_count == 0
  and .health.upload_warning == false
  and (.health.connected_workers | type) == "number"
  and .health.connected_workers == (.health.connected_workers | floor)
  and .health.connected_workers >= 1
  and (.health.symbols_expected | type) == "number"
  and .health.symbols_expected == (.health.symbols_expected | floor)
  and .health.symbols_expected >= $minimum_symbols
  and (.health.symbols_seen | type) == "number"
  and .health.symbols_seen == (.health.symbols_seen | floor)
  and .health.symbols_seen >= $minimum_symbols
  and (.health.last_event_at_ms | type) == "number"
  and .health.last_event_at_ms > 0
  and (.health.updated_at_ms | type) == "number"
  and .health.updated_at_ms > 0;

.schema == "monday.bybit_options_shadow_gate.v1"
and .candidate_sha256 == $candidate_sha256
and .deployment_bundle_sha256 == $deployment_bundle_sha256
and .deployment_source_revision == $deployment_source_revision
and .passed == true
and (.duration_seconds | type) == "number"
and .duration_seconds == (.duration_seconds | floor)
and .duration_seconds >= 3600
and .test_only == false
and .production_eligible == true
and .service.unit == "bybit-options-shadow.service"
and .service.active == true
and (.service.restart_count | type) == "number"
and .service.restart_count == 0
and (.service.binary_sha256 | sha256)
and .service.binary_sha256 == $candidate_sha256
and .service.spool_dir == "/data/monday/spool/bybit-options-shadow"
and (health_ok)
and (.health.updated_at_ms >= .health.last_event_at_ms)
and (.health_sha256 | sha256)
and (.health_samples | type) == "number"
and .health_samples == (.health_samples | floor)
and .health_samples >= 1
and (.max_health_silence_seconds | type) == "number"
and .max_health_silence_seconds <= 120
and .upload_status.failure_count == 0
and .spool_drained == true
