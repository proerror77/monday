.schema == "monday.rust_lob_shadow_gate.v2"
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
and .markets.spot.sequence_gaps == 0
and (.markets.spot.upload_failure_count | type) == "number"
and .markets.spot.upload_failure_count == (.markets.spot.upload_failure_count | floor)
and .markets.spot.upload_failure_count >= 0
and (.markets.spot.health_samples | type) == "number"
and .markets.spot.health_samples == (.markets.spot.health_samples | floor)
and .markets.spot.health_samples >= 40
and (.markets.spot.max_health_silence_seconds | type) == "number"
and .markets.spot.max_health_silence_seconds >= 0
and .markets.spot.max_health_silence_seconds <= 90
and (.markets.spot.catalog_sha256 | type) == "string"
and (.markets.spot.catalog_sha256 | test("^[a-f0-9]{64}$"))
and (.markets.spot.session_id | type) == "string"
and (.markets.spot.session_id | length) > 0
and (.markets.spot.oss_roundtrips | type) == "number"
and .markets.spot.oss_roundtrips == (.markets.spot.oss_roundtrips | floor)
and .markets.spot.oss_roundtrips >= 2
and (.markets.usdm.symbol_count | type) == "number"
and .markets.usdm.symbol_count == (.markets.usdm.symbol_count | floor)
and .markets.usdm.symbol_count >= 400
and .markets.usdm.snapshot_ready_count == .markets.usdm.symbol_count
and .markets.usdm.sequence_gaps == 0
and (.markets.usdm.upload_failure_count | type) == "number"
and .markets.usdm.upload_failure_count == (.markets.usdm.upload_failure_count | floor)
and .markets.usdm.upload_failure_count >= 0
and (.markets.usdm.health_samples | type) == "number"
and .markets.usdm.health_samples == (.markets.usdm.health_samples | floor)
and .markets.usdm.health_samples >= 40
and (.markets.usdm.max_health_silence_seconds | type) == "number"
and .markets.usdm.max_health_silence_seconds >= 0
and .markets.usdm.max_health_silence_seconds <= 90
and (.markets.usdm.catalog_sha256 | type) == "string"
and (.markets.usdm.catalog_sha256 | test("^[a-f0-9]{64}$"))
and (.markets.usdm.session_id | type) == "string"
and (.markets.usdm.session_id | length) > 0
and (.markets.usdm.oss_roundtrips | type) == "number"
and .markets.usdm.oss_roundtrips == (.markets.usdm.oss_roundtrips | floor)
and .markets.usdm.oss_roundtrips >= 2
