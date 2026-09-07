#!/usr/bin/env bash
set -euo pipefail

input=${1:?usage: read-acr-required-checks.sh CHECK_RUNS_JSON [OUTPUT]}
output=${2:-/dev/stdout}
checks_json=$(cat "$input")

check_state() {
  local name=$1 state
  state=$(jq -er --arg name "$name" '
    (if type == "array" then . else [.] end)
    | [.[].check_runs[]?
      | select(.name == $name and .app.slug == "github-actions" and .app.id == 15368)]
    | sort_by(.id)
    | if length == 0 then "missing"
      else .[-1]
        | if .status != "completed" then .status
          else (.conclusion // "missing")
          end
      end
  ' <<<"$checks_json")
  case "$state" in
    success|failure|neutral|cancelled|skipped|timed_out|action_required|stale|startup_failure|missing|queued|in_progress|waiting|pending|requested) ;;
    *) printf 'invalid check state for %s: %s\n' "$name" "$state" >&2; exit 1 ;;
  esac
  printf '%s\n' "$state"
}

monorepo_conclusion=$(check_state 'Monorepo CI gate')
prediction_conclusion=$(check_state 'Prediction Markets CI gate')
security_conclusion=$(check_state 'Security Summary Report')
printf '%s\n' \
  "monorepo_conclusion=$monorepo_conclusion" \
  "prediction_conclusion=$prediction_conclusion" \
  "security_conclusion=$security_conclusion" >"$output"
