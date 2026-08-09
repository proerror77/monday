def sha256:
  type == "string" and test("^[a-f0-9]{64}$");

def invocation_id:
  type == "string" and test("^[a-f0-9]{32}$");

def endpoints:
  . == [
    "https://fapi.binance.com/fapi/v1/time",
    "https://fapi.binance.com/fapi/v1/exchangeInfo",
    "https://fapi.binance.com/fapi/v1/premiumIndex",
    "https://fapi.binance.com/fapi/v1/openInterest"
  ];

def complete_coverage:
  (.active_contracts | type) == "number"
  and .active_contracts == (.active_contracts | floor)
  and .active_contracts >= 400
  and .metadata_observations == .active_contracts
  and .mark_index_funding_observations == .active_contracts
  and .open_interest_observations == .active_contracts
  and .stale_metadata == 0
  and .stale_mark_index_funding == 0
  # stale_open_interest is evidence-only: the exchange openInterest `time`
  # is a per-instrument last-change timestamp that legitimately lags for
  # quiet instruments; the count must be present and non-negative.
  and (.stale_open_interest | type) == "number"
  and .stale_open_interest >= 0
  and .api_error_count == 0;

def canonical_artifact:
  .canonical_readback == true
  and .venue == "binance_usdm"
  and .dataset == "reference"
  and .manifest_schema == "binance.usdm_reference_manifest.v1"
  and .data_schema == "binance.usdm_reference.v3"
  and .source_origin == "https://fapi.binance.com"
  and (.source_endpoints | endpoints)
  and .max_staleness_ms == 30000
  and (.data_sha256 | sha256)
  and (.manifest_sha256 | sha256)
  and .success_sha256 == .data_sha256
  and .content_rows_verified == true
  and (.observed_at_ns | type) == "number"
  and .observed_at_ns == (.observed_at_ns | floor)
  and .observed_at_ns > 0
  and (.coverage | complete_coverage)
  and .time_bounds.min_source_time_ms <= .time_bounds.max_source_time_ms
  and .time_bounds.min_received_at_ns <= .time_bounds.max_received_at_ns
  and .time_bounds.max_received_at_ns <= .observed_at_ns;

.schema == "monday.binance_usdm_reference_shadow_gate.v1"
and .candidate_sha256 == $candidate_sha256
and .deployment_bundle_sha256 == $deployment_bundle_sha256
and .deployment_source_revision == $deployment_source_revision
and .passed == true
and .production_eligible == true
and (.duration_seconds | type) == "number"
and .duration_seconds == (.duration_seconds | floor)
and .duration_seconds >= 3600
and .service.unit == ("binance-usdm-reference-collector-shadow@" + $candidate_sha256 + ".service")
and .service.active == true
and .service.restart_count == 0
and .service.binary_sha256 == $candidate_sha256
and (.service.invocation_id_start | invocation_id)
and .service.invocation_id_end == .service.invocation_id_start
and .health.schema == "binance.usdm_reference_health.v1"
and .health.status == "healthy"
and .health.source_origin == "https://fapi.binance.com"
and .health.api_error_count == 0
and .health.total_api_errors == 0
and .health.artifact_error_count == 0
and .health.total_artifact_errors == 0
and (.health.data_sha256 | sha256)
and (.health.manifest_sha256 | sha256)
and (.artifact_count | type) == "number"
and .artifact_count == (.artifact_count | floor)
and .artifact_count >= 3
and (.artifacts | type) == "array"
and .artifact_count == (.artifacts | length)
and all(.artifacts[]; canonical_artifact)
and ([.artifacts[].data_sha256] | unique | length) == .artifact_count
and ([.artifacts[].manifest_sha256] | unique | length) == .artifact_count
and ([.artifacts[].observed_at_ns] as $times
  | $times == ($times | sort)
  and ($times | unique | length) == ($times | length)
  and $times[-1] - $times[0] >= (.duration_seconds * 1000000000)
  and ([range(1; $times | length) as $index
    | $times[$index] - $times[$index - 1]] as $gaps
    | all($gaps[]; . > 0 and . <= 90000000000)
    and .max_artifact_gap_ns == ($gaps | max)))
and .health.data_sha256 == .artifacts[-1].data_sha256
and .health.manifest_sha256 == .artifacts[-1].manifest_sha256
and .health.last_success_at_ns >= .artifacts[-1].observed_at_ns
and .health.last_success_at_ns - .artifacts[-1].observed_at_ns <= 90000000000
