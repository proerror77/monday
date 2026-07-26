(.updated_at | type == "string" and length > 0)
and (.last_success_at | type == "string" and length > 0)
and (.cycle_started_at | type == "string" and length > 0)
and (.cycle_duration_ms | type == "number" and floor == . and . >= 0)
and (.cycle_duration_ms <= 180000)
and (.target_markets | type == "number" and floor == . and . > 0)
and (.trade_poll_budget | type == "number" and floor == . and . > 0)
and (.trade_poll_budget == 112)
and (.trade_poll_concurrency | type == "number" and floor == . and . > 0)
and (.trade_poll_concurrency <= .trade_poll_budget)
and (.trade_poll_concurrency <= 4)
and (.priority_trade_markets_before_market_details | type == "number" and floor == . and . >= 0)
and (.priority_trade_markets_before_market_details <= .trade_poll_budget)
and (.priority_trade_markets_before_market_details <= .target_markets)
and (.market_detail_budget | type == "number" and floor == . and . >= 0)
and (.market_detail_budget == ([.trade_poll_concurrency,
  (((.trade_poll_budget - .priority_trade_markets_before_market_details) / 2) | floor)] | min))
and (.market_detail_eligible | type == "number" and floor == . and . >= 0)
and (.market_detail_priority | type == "number" and floor == . and . >= 0)
and (.market_detail_selected | type == "number" and floor == . and . >= 0)
and (.market_detail_deferred | type == "number" and floor == . and . >= 0)
and (.market_detail_priority_deferred | type == "number" and floor == . and . >= 0)
and (.market_detail_priority <= .market_detail_eligible)
and (.market_detail_selected == ([.market_detail_eligible, .market_detail_budget] | min))
and (.market_detail_deferred == (.market_detail_eligible - .market_detail_selected))
and (.market_detail_priority_deferred == ([.market_detail_priority - .market_detail_budget, 0] | max))
and (.trade_poll_budget_after_market_details | type == "number" and floor == . and . >= 0)
and (.trade_poll_budget_after_market_details == (.trade_poll_budget - .market_detail_selected))
and (.trade_request_spacing_ms | type == "number" and floor == . and . >= 100)
and (.eligible_trade_markets | type == "number" and floor == . and . >= 0)
and (.priority_trade_markets | type == "number" and floor == . and . >= 0)
and (.selected_trade_markets | type == "number" and floor == . and . >= 0)
and (.deferred_trade_markets | type == "number" and floor == . and . >= 0)
and (.priority_trade_backlog | type == "number" and floor == . and . >= 0)
and (.trade_polls | type == "number" and floor == . and . >= 0)
and (.successful_trade_polls | type == "number" and floor == . and . >= 0)
and (.priority_trade_markets <= .eligible_trade_markets)
and (.priority_trade_markets_before_market_details <= .priority_trade_markets)
and (.priority_trade_markets <= (.priority_trade_markets_before_market_details + .market_detail_selected))
and (.selected_trade_markets == ([.eligible_trade_markets, .trade_poll_budget_after_market_details] | min))
and (.deferred_trade_markets == (.eligible_trade_markets - .selected_trade_markets))
and (.priority_trade_backlog == ([.priority_trade_markets - .trade_poll_budget_after_market_details, 0] | max))
and ((.market_detail_selected + .selected_trade_markets) <= .trade_poll_budget)
and (.selected_trade_markets == .trade_polls)
and (.successful_trade_polls == .trade_polls)
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
