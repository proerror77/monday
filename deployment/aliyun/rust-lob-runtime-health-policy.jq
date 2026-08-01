.market == $expected_market
and .dataset == $expected_dataset
and .status == "synced"
and .sequence_gaps == 0
and (.symbol_count | type) == "number"
and .symbol_count == (.symbol_count | floor)
and .symbol_count >= $minimum_symbols
and (.snapshot_ready_count | type) == "number"
and .snapshot_ready_count == (.snapshot_ready_count | floor)
and .snapshot_ready_count == .symbol_count
and .bridged_count == .symbol_count
and .stream_coverage_verified_count == .symbol_count
and .snapshot_only_symbols == []
and .all_symbols_bridged == true
and .all_stream_coverage_verified == true
and ((.full_stream_coverage_verified == null)
  or (.full_stream_coverage_verified == true))
and .pending_upload_segments == 0
and .queue_saturated == false
and .disk_warning == false
and .upload_warning == false
and (.updated_at_ns | type) == "number"
and .updated_at_ns > $minimum_updated_ns
and (.session_id | type) == "string"
and (.session_id | length) > 0
and ($old_session == "" or .session_id != $old_session)
