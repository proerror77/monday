#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "== Polymarket API Usage Audit =="

audit_matches="$(rg -n "https://(clob|gamma-api|data-api)\\.polymarket\\.com|wss://ws-subscriptions-clob\\.polymarket\\.com" src -S || true)"

if [[ -z "$audit_matches" ]]; then
  echo "No explicit Polymarket URLs found in src/."
  exit 0
fi

echo "$audit_matches"

legacy_paging="$(printf '%s\n' "$audit_matches" | rg "_limit=|_offset=" -n || true)"
if [[ -n "$legacy_paging" ]]; then
  echo
  echo "ERROR: Legacy Gamma pagination params found (_limit/_offset)."
  echo "$legacy_paging"
  exit 1
fi

raw_data_trades="$(printf '%s\n' "$audit_matches" | rg "data-api\\.polymarket\\.com/trades" -n || true)"
if [[ -n "$raw_data_trades" ]]; then
  echo
  echo "ERROR: Raw data-api /trades URL usage found. Use SDK DataClient::trades instead."
  echo "$raw_data_trades"
  exit 1
fi

raw_gamma_targeted="$(printf '%s\n' "$audit_matches" | rg "src/(strategy/event_edge/mod\\.rs|strategy/live_arbitrage\\.rs|agent/sports_analyst\\.rs|agent/sports_analyst_enhanced\\.rs|agent/polymarket_sports\\.rs|agent/polymarket_politics\\.rs|agent/nba_moneyline_analyzer\\.rs|main\\.rs):.*gamma-api\\.polymarket\\.com/(events|series|markets)" -n || true)"
if [[ -n "$raw_gamma_targeted" ]]; then
  echo
  echo "ERROR: Raw Gamma /events|/series|/markets URL usage found in SDK-migrated modules."
  echo "Use typed GammaClient calls (SearchRequest/SeriesByIdRequest/EventByIdRequest/MarketsRequest) instead."
  echo "$raw_gamma_targeted"
  exit 1
fi

echo

echo "Files with direct Polymarket URL usage (candidate wrapper targets):"
printf '%s\n' "$audit_matches" | cut -d: -f1 | sort -u

echo

echo "OK: no legacy _limit/_offset params detected."
