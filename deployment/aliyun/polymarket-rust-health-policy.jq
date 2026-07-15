(.updated_at | type == "string" and length > 0)
and (.last_success_at | type == "string" and length > 0)
and (.target_markets | type == "number" and floor == . and . > 0)
and .missing_target_symbols == []
and .api_errors == []
and .malformed_trade_rows == 0
and .truncated_trade_markets == []
and .non_object_trade_markets == []
and .invalid_settlement_markets == []
and .invalid_end_time_markets == []
and .stale_trade_markets == []
and .stale_settlement_markets == []
and .overdue_unresolved_markets == []
