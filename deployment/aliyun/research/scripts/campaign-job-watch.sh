#!/usr/bin/env bash
# Sourced by the canonical controller. Never submits, deletes or settles a Job.

validate_terminal_failure() {
  local dir="$1" failure="$1/terminal-failure"
  [[ -s "$failure" && -s "$dir/request.json" && -s "$dir/dispatch-identity.json" && -s "$dir/job-status.json" ]] || return 1
  jq -e --arg request "$(sha256_file "$dir/request.json")" \
    --arg identity "$(sha256_file "$dir/dispatch-identity.json")" \
    --arg job "$(sha256_file "$dir/job-status.json")" '
    .schema_version == "monday.campaign_terminal_failure.v1"
    and .reason == "job_failed" and .request_sha256 == $request
    and .dispatch_identity_sha256 == $identity and .job_status_sha256 == $job
    and .accounting_changed == false
  ' "$failure" >/dev/null
}

watch_state() {
  local reason="$1"
  jq -n --arg reason "$reason" --arg request "$request_sha256" --arg uid "$bound_job_uid" \
    --argjson deadline "$watch_deadline" \
    '{schema_version:"monday.campaign_watch_state.v1",reason:$reason,request_sha256:$request,job_uid:$uid,deadline_epoch:$deadline,accounting_changed:false}' \
    >"$generation_dir/wait-state.json.partial" || return 75
  mv -f -- "$generation_dir/wait-state.json.partial" "$generation_dir/wait-state.json"
}

read_campaign_pods() {
  if [[ -n "$campaign_pod_name" ]]; then
    "$kubectl_cli" "${kubectl_readback_args[@]}" --request-timeout=30s get "pod/$campaign_pod_name" -o json | jq '{items:[.]}'
  else
    "$kubectl_cli" "${kubectl_readback_args[@]}" --request-timeout=30s get pods -l "job-name=$job_name" -o json
  fi
}

wait_for_campaign_terminal() {
  local dispatch_control="${control:-${MONDAY_CAMPAIGN_CONTROL:-}}"
  local identity="$generation_dir/dispatch-identity.json" current="$generation_dir/dispatch-identity.current"
  "$alpha_harness" mission dispatch status --control "$dispatch_control" --submission "$submission" \
    --context "$context" --namespace "$namespace" >"$current" || return 75
  jq -e --arg request "$request_sha256" --arg job "$job_name" '
    .schema_version == "monday.campaign_dispatch_status.v1" and .request_sha256 == $request
    and .job_name == $job and (.job_uid | type == "string" and length > 0)
    and (.operation_id | type == "string" and length > 0) and .accounting_changed == false
  ' "$current" >/dev/null || die "authenticated dispatch has no exact Job binding"
  if [[ -e "$identity" ]]; then
    jq -e --slurpfile fresh "$current" '
      .operation_id == $fresh[0].operation_id and .request_sha256 == $fresh[0].request_sha256
      and .job_name == $fresh[0].job_name and .job_uid == $fresh[0].job_uid
    ' "$identity" >/dev/null || die "retained dispatch identity differs from its authenticated ledger"
    rm -f -- "$current"
  else
    mv -- "$current" "$identity" || return 75
  fi
  bound_job_uid=$(jq -er '.job_uid' "$identity") || return 65
  local authority_deadline
  authority_deadline=$(jq -er '.authority_deadline_epoch | select(type == "number" and . > 0 and floor == .)' "$identity") || return 65
  local watch="$generation_dir/job-watch.json" duration unit seconds
  [[ "$job_timeout" =~ ^([1-9][0-9]*)(s|m|h)$ ]] || die "job timeout must be an integer duration in s, m or h"
  duration="${BASH_REMATCH[1]}" unit="${BASH_REMATCH[2]}"
  ((${#duration} <= 6)) || die "job timeout is too large"
  seconds=$duration
  case "$unit" in m) seconds=$((duration * 60));; h) seconds=$((duration * 3600));; esac
  if [[ -e "$watch" ]]; then
    jq -e --arg request "$request_sha256" --arg uid "$bound_job_uid" '
      .schema_version == "monday.campaign_job_watch.v1" and .request_sha256 == $request and .job_uid == $uid
      and (.deadline_epoch | type == "number" and . > 0 and floor == .)
    ' "$watch" >/dev/null || die "saved Job watch identity or deadline is invalid"
    watch_deadline=$(jq -er '.deadline_epoch' "$watch") || return 65
  else
    watch_deadline=$(( $(date -u +%s) + seconds ))
    if [[ -n "${MONDAY_CAMPAIGN_DEADLINE_AT:-}" ]]; then
      local absolute
      absolute=$(date -u -d "$MONDAY_CAMPAIGN_DEADLINE_AT" +%s) || die "invalid workflow deadline"
      ((absolute >= watch_deadline)) || watch_deadline=$absolute
    fi
    jq -n --arg request "$request_sha256" --arg uid "$bound_job_uid" --argjson deadline "$watch_deadline" \
      '{schema_version:"monday.campaign_job_watch.v1",request_sha256:$request,job_uid:$uid,deadline_epoch:$deadline}' >"$watch.partial" || return 75
    mv -- "$watch.partial" "$watch" || return 75
  fi
  ((watch_deadline <= authority_deadline)) || watch_deadline=$authority_deadline
  local delay=2 complete failed now
  while true; do
    if ! "$kubectl_cli" "${kubectl_readback_args[@]}" --request-timeout=30s get "job/$job_name" -o json >"$job_status.partial"; then
      watch_state runtime_readback_error
      return 75
    fi
    if ! jq -e --arg request "$request_sha256" --arg uid "$bound_job_uid" --arg job "$job_name" '
      .metadata.name == $job and .metadata.uid == $uid and .metadata.annotations["research.monday/request-sha256"] == $request
    ' "$job_status.partial" >/dev/null; then
      watch_state job_identity_mismatch
      return 65
    fi
    mv -f -- "$job_status.partial" "$job_status" || return 75
    complete=$(jq -r '(.status.conditions // [] | any(.type == "Complete" and .status == "True"))' "$job_status") || return 65
    failed=$(jq -r '(.status.conditions // [] | any(.type == "Failed" and .status == "True"))' "$job_status") || return 65
    if [[ "$complete" == true && "$failed" == true ]]; then
      watch_state ambiguous_terminal_status
      return 65
    fi
    if [[ "$failed" == true ]]; then
      if read_campaign_pods >"$pod_status.partial"; then
        mv -- "$pod_status.partial" "$pod_status" || return 75
      else
        rm -f -- "$pod_status.partial"
      fi
      jq -n --arg request "$request_sha256" --arg identity "$(sha256_file "$identity")" \
        --arg job "$(sha256_file "$job_status")" --arg uid "$bound_job_uid" \
        '{schema_version:"monday.campaign_terminal_failure.v1",reason:"job_failed",request_sha256:$request,job_uid:$uid,dispatch_identity_sha256:$identity,job_status_sha256:$job,accounting_changed:false}' \
        >"$generation_dir/terminal-failure.partial" || return 75
      mv -- "$generation_dir/terminal-failure.partial" "$generation_dir/terminal-failure" || return 75
      watch_state job_failed
      return 70
    fi
    if [[ "$complete" == true ]]; then
      watch_state job_completed || return 75
      return 0
    fi
    now=$(date -u +%s)
    if ((now >= watch_deadline)); then
      watch_state deadline_exhausted
      return 124
    fi
    ((delay <= watch_deadline - now)) || delay=$((watch_deadline - now))
    sleep "$delay" || return 75
    ((delay >= 15)) || delay=$((delay * 2))
    ((delay <= 15)) || delay=15
  done
}
