(.updated_at | type == "string" and length > 0)
and (.last_success_at | type == "string" and length > 0)
and (.target_markets | type == "number" and floor == . and . > 0)
and (.trade_poll_budget | type == "number" and floor == . and . > 0)
and (.eligible_trade_markets | type == "number" and floor == . and . >= 0)
and (.priority_trade_markets | type == "number" and floor == . and . >= 0)
and (.selected_trade_markets | type == "number" and floor == . and . >= 0)
and (.deferred_trade_markets | type == "number" and floor == . and . >= 0)
and (.priority_trade_backlog | type == "number" and floor == . and . >= 0)
and (.trade_polls | type == "number" and floor == . and . >= 0)
and (.successful_trade_polls | type == "number" and floor == . and . >= 0)
and (.priority_trade_markets <= .eligible_trade_markets)
and (.selected_trade_markets == ([.eligible_trade_markets, .trade_poll_budget] | min))
and (.deferred_trade_markets == (.eligible_trade_markets - .selected_trade_markets))
and (.priority_trade_backlog == ([.priority_trade_markets - .trade_poll_budget, 0] | max))
and (.selected_trade_markets == .trade_polls)
and (.successful_trade_polls <= .trade_polls)
and (.priority_trade_backlog == 0)
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
