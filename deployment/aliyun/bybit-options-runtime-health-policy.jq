# Runtime health policy for the Bybit Options archiver.
#
# Arguments:
#   $minimum_symbols      minimum number of distinct symbols that must be seen
#   $minimum_updated_ms   health.updated_at_ms must be strictly newer than this
#   $old_updated_ms       when non-zero, last_event_at_ms must be newer (used to
#                         prove a candidate session is fresh after cutover)
.schema == "monday.bybit_options_quote.v1"
and .venue == "bybit"
and .category == "option"
and .disk_warning == false
and .spool_warning == false
and (.upload_failure_count | type) == "number"
and .upload_failure_count == (.upload_failure_count | floor)
and .upload_failure_count == 0
and .upload_warning == false
and (.connected_workers | type) == "number"
and .connected_workers == (.connected_workers | floor)
and .connected_workers >= 1
and (.symbols_expected | type) == "number"
and .symbols_expected == (.symbols_expected | floor)
and .symbols_expected >= $minimum_symbols
and (.symbols_seen | type) == "number"
and .symbols_seen == (.symbols_seen | floor)
and .symbols_seen >= $minimum_symbols
and (.active_segment_bytes | type) == "number"
and .active_segment_bytes >= 0
and (.last_event_at_ms | type) == "number"
and .last_event_at_ms > 0
and ($old_updated_ms == 0 or .last_event_at_ms > $old_updated_ms)
and (.updated_at_ms | type) == "number"
and .updated_at_ms > $minimum_updated_ms
