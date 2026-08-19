# Bounded pagination-defer adjudication (#933/#934): the Polymarket data-api
# enforces a hard pagination offset ceiling (10000) on the trades endpoint, so
# a market whose trade history grows past it can no longer be fully fetched.
# The collector defers such a market to the next cycle instead of crashing the
# process; the defer is designed behavior, not data corruption. api_errors is
# admissible only when every entry is a pagination-defer notice and their
# count stays within the same bound as the legacy bounded HTTP 429 tolerance;
# truncated_trade_markets must be exactly the set of condition_ids those
# notices cover. Any other api_errors entry, a truncated market with no
# matching defer notice, a defer notice with no matching truncated market, or
# a defer count above the bound still fails closed.
def pagination_defer_limit:
  (.target_markets
    | select(type == "number" and floor == . and . > 0)
    | ((. + 99) / 100 | floor)
    | if . < 3 then 3 elif . > 32 then 32 else . end) // 0;
def pagination_defer_pattern:
  "^trades 0x[0-9a-f]{64}: trade pagination exceeded API offset limit; deferred market after [0-9]+ rows fetched through offset [0-9]+\\z";
def pagination_deferred_markets:
  [.api_errors[]?
    | select(type == "string")
    | (capture("trades (?<market>0x[0-9a-f]{64}): ") | .market)?]
  | unique;
def bounded_pagination_defer_adjudication:
  . as $snapshot
  | ($snapshot.api_errors
      | type == "array" and length <= ($snapshot | pagination_defer_limit)
      and all(.[]; type == "string" and test(pagination_defer_pattern)))
    and ($snapshot.truncated_trade_markets
      | type == "array" and all(.[]; type == "string")
      and length == (unique | length)
      and (unique | sort) == ($snapshot | pagination_deferred_markets | sort));
(.updated_at | type == "string" and length > 0)
and (.last_success_at | type == "string" and length > 0)
and (.cycle_started_at | type == "string" and length > 0)
and (.cycle_duration_ms | type == "number" and floor == . and . >= 0)
and (.cycle_duration_ms <= 180000)
and (.target_markets | type == "number" and floor == . and . > 0)
and (.trade_poll_budget | type == "number" and floor == . and . > 0)
and (.trade_poll_budget == 200)
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
and (.trade_request_spacing_ms | type == "number" and floor == . and . >= 125)
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
and bounded_pagination_defer_adjudication
and .malformed_trade_rows == 0
and .non_object_trade_markets == []
and .invalid_settlement_markets == []
and .invalid_end_time_markets == []
and .stale_trade_markets == []
and .stale_settlement_markets == []
and (.overdue_unresolved_markets | type == "array" and all(.[]; type == "string" and length > 0))
