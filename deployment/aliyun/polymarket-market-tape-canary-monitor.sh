#!/usr/bin/env bash
set -euo pipefail

systemctl_value() {
  systemctl show "$1" --property="$2" --value
}

uploader_process_cpu() {
  local unit=$1
  local state=${2:-}
  local result
  local main_pid

  if [[ -z $state ]]; then
    state=$(systemctl_value "$unit" ActiveState) || return 1
  fi
  case "$state" in
    inactive)
      result=$(systemctl_value "$unit" Result) || return 1
      [[ $result == success ]] || return 1
      printf '0\n'
      ;;
    active | activating | deactivating)
      main_pid=$(systemctl_value "$unit" MainPID) || return 1
      [[ $main_pid =~ ^[1-9][0-9]*$ ]] || return 1
      ps -p "$main_pid" -o %cpu= | awk '{sum += $1} END {printf "%d\n", sum + 0}'
      ;;
    *)
      return 1
      ;;
  esac
}

host_cpu_stop_reason() {
  printf 'host_cpu_%s_gt_95\n' "$1"
}

pending_growth_stop_reason() {
  printf 'pending_continuous_growth_%s_to_%s\n' "$1" "$2"
}

evaluate_cpu_stop() {
  local busy=$1
  local cpu=$2
  if [[ $busy -eq 1 && $cpu -gt 95 ]]; then
    rollback_now "$(host_cpu_stop_reason "$cpu")"
  fi
}

run_monitor() {
  : "${CANARY_CANDIDATE_PATH:?CANARY_CANDIDATE_PATH is required}"
  : "${CANARY_OLD_TARGET:?CANARY_OLD_TARGET is required}"
  : "${CANARY_NEW_SHA256:?CANARY_NEW_SHA256 is required}"
  : "${CANARY_OLD_SHA256:?CANARY_OLD_SHA256 is required}"

  local svc=polymarket-market-tape-upload.service
  local timer=polymarket-market-tape-upload.timer
  local watchtimer=polymarket-market-tape-upload-watchdog.timer
  local collector=polymarket-market-tape.service
  local refcollector=polymarket-reference-collector.service
  local active=/opt/monday/bin/polymarket-raw-ops
  local rollback_tmp=/opt/monday/bin/.polymarket-raw-ops.monitor-rollback-symlink-716
  local status=/data/monday/spool/polymarket/upload-status.json
  local spool=/data/monday/spool/polymarket
  local staging=/data/monday/spool/polymarket/.upload-staging
  local duration=${CANARY_DURATION_SECONDS:-3540}
  local interval=${CANARY_SAMPLE_INTERVAL_SECONDS:-5}
  local minimum_objects=${CANARY_MIN_OBJECT_CHANGES:-2}
  local start_epoch
  local end_epoch
  local max_cpu=0
  local max_proc_cpu=0
  local max_pending=0
  local growth_count=0
  local pending_since=0
  local prev_pending
  local prev_object
  local object_changes=0
  local last_report=0
  local prev_state
  local base_free
  local total_kib

  start_epoch=$(date +%s)
  end_epoch=$((start_epoch + duration))
  prev_pending=$(find "$spool" -maxdepth 1 -type f -name 'market-updates.*.ndjson' | wc -l)
  prev_object=$(jq -r '.last_uploaded_object // ""' "$status")
  prev_state=$(systemctl_value "$svc" ActiveState)
  base_free=$(df -Pk /data | awk 'NR == 2 {print $4}')
  total_kib=$(df -Pk /data | awk 'NR == 2 {print $2}')

  rollback_now() {
    local reason=$1
    trap - ERR
    printf 'STOP_RULE=%s TIME_UTC=%s\n' "$reason" "$(date -u +%FT%TZ)"
    systemctl stop "$timer" || true
    systemctl stop "$svc" || true
    rm -f "$rollback_tmp"
    ln -s "$CANARY_OLD_TARGET" "$rollback_tmp"
    mv -Tf "$rollback_tmp" "$active"
    [[ $(sha256sum "$active" | awk '{print $1}') == "$CANARY_OLD_SHA256" ]]
    systemctl start "$timer"
    printf 'ROLLED_BACK_TARGET='; readlink -f "$active"
    printf 'ROLLED_BACK_SHA256='; sha256sum "$active" | awk '{print $1}'
    printf 'MARKET_TIMER=%s WATCHDOG_TIMER=%s REFERENCE_COLLECTOR=%s\n' \
      "$(systemctl is-active "$timer" || true)" \
      "$(systemctl is-active "$watchtimer" || true)" \
      "$(systemctl is-active "$refcollector" || true)"
    exit 42
  }

  on_error() {
    local rc=$?
    rollback_now "monitor_error_rc_${rc}"
  }
  trap on_error ERR

  rm -f "$rollback_tmp"
  [[ $(readlink -f "$active") == "$CANARY_CANDIDATE_PATH" ]]
  [[ $(sha256sum "$active" | awk '{print $1}') == "$CANARY_NEW_SHA256" ]]
  [[ $(systemctl is-active "$collector" || true) == active ]]
  [[ $(systemctl is-active "$timer" || true) == active ]]
  [[ $(systemctl is-active "$watchtimer" || true) == active ]]
  [[ $(systemctl is-active "$refcollector" || true) == inactive ]]
  [[ ! -e /run/monday/polymarket-upload-watchdog.suppress ]]
  printf 'MONITOR_START_UTC=%s DURATION_SECONDS=%s BASE_FREE_KIB=%s BASE_PENDING=%s BASE_OBJECT=%s\n' \
    "$(date -u +%FT%TZ)" "$duration" "$base_free" "$prev_pending" "$prev_object"

  while [[ $(date +%s) -lt $end_epoch ]]; do
    local u1 n1 s1 i1 w1 q1 z1 t1
    local u2 n2 s2 i2 w2 q2 z2 t2
    local total1 idle1 total2 idle2 dt di cpu
    local state timer_state watch_state collector_state ref_state busy proc_cpu
    local failed pending now free free_pct obj stage_files

    read -r _ u1 n1 s1 i1 w1 q1 z1 t1 _ </proc/stat
    total1=$((u1 + n1 + s1 + i1 + w1 + q1 + z1 + t1))
    idle1=$((i1 + w1))
    sleep "$interval"
    read -r _ u2 n2 s2 i2 w2 q2 z2 t2 _ </proc/stat
    total2=$((u2 + n2 + s2 + i2 + w2 + q2 + z2 + t2))
    idle2=$((i2 + w2))
    dt=$((total2 - total1))
    di=$((idle2 - idle1))
    cpu=0
    [[ $dt -le 0 ]] || cpu=$((100 * (dt - di) / dt))

    state=$(systemctl_value "$svc" ActiveState)
    timer_state=$(systemctl_value "$timer" ActiveState)
    watch_state=$(systemctl_value "$watchtimer" ActiveState)
    collector_state=$(systemctl_value "$collector" ActiveState)
    ref_state=$(systemctl_value "$refcollector" ActiveState)
    [[ $timer_state == active ]] || rollback_now "market_timer_${timer_state}"
    [[ $watch_state == active ]] || rollback_now "shared_watchdog_timer_${watch_state}"
    [[ $collector_state == active ]] || rollback_now "market_collector_${collector_state}"
    [[ $ref_state == inactive ]] || rollback_now "reference_ownership_changed_${ref_state}"
    [[ ! -e /run/monday/polymarket-upload-watchdog.suppress ]] \
      || rollback_now suppression_reappeared
    [[ $state != failed ]] || rollback_now uploader_service_failed

    busy=0
    case "$state" in
      active | activating | deactivating) busy=1 ;;
    esac
    if [[ $busy -eq 1 && $cpu -gt $max_cpu ]]; then
      max_cpu=$cpu
    fi
    proc_cpu=$(uploader_process_cpu "$svc" "$state")
    if [[ $proc_cpu -gt $max_proc_cpu ]]; then
      max_proc_cpu=$proc_cpu
    fi
    evaluate_cpu_stop "$busy" "$cpu"

    failed=$(jq -c '.failed_segments' "$status")
    [[ $failed == '[]' ]] || rollback_now failed_segments_nonempty
    pending=$(find "$spool" -maxdepth 1 -type f -name 'market-updates.*.ndjson' | wc -l)
    if [[ $pending -gt $max_pending ]]; then
      max_pending=$pending
    fi
    if [[ $pending -gt $prev_pending ]]; then
      growth_count=$((growth_count + 1))
    elif [[ $pending -lt $prev_pending ]]; then
      growth_count=0
    fi
    [[ $growth_count -lt 3 ]] \
      || rollback_now "$(pending_growth_stop_reason "$prev_pending" "$pending")"

    now=$(date +%s)
    if [[ $pending -gt 0 ]]; then
      [[ $pending_since -ne 0 ]] || pending_since=$now
      [[ $((now - pending_since)) -lt 900 ]] || rollback_now pending_stalled_900s
    else
      pending_since=0
      growth_count=0
    fi

    free=$(df -Pk /data | awk 'NR == 2 {print $4}')
    free_pct=$((100 * free / total_kib))
    [[ $free_pct -ge 25 ]] || rollback_now "data_free_pct_${free_pct}_below_25"
    obj=$(jq -r '.last_uploaded_object // ""' "$status")
    if [[ $obj != "$prev_object" ]]; then
      object_changes=$((object_changes + 1))
      printf 'OBJECT_CHANGE_%s TIME_UTC=%s OBJECT=%s\n' \
        "$object_changes" "$(date -u +%FT%TZ)" "$obj"
      prev_object=$obj
    fi
    if [[ $state != "$prev_state" ]]; then
      printf 'SERVICE_TRANSITION TIME_UTC=%s FROM=%s TO=%s CPU=%s PROC_CPU=%s PENDING=%s\n' \
        "$(date -u +%FT%TZ)" "$prev_state" "$state" "$cpu" "$proc_cpu" "$pending"
      prev_state=$state
    fi
    if [[ $((now - last_report)) -ge 60 ]]; then
      stage_files=$(find "$staging" -maxdepth 1 -type f 2>/dev/null | wc -l)
      printf 'SAMPLE TIME_UTC=%s CPU=%s MAX_CPU=%s PROC_CPU=%s MAX_PROC_CPU=%s STATE=%s PENDING=%s MAX_PENDING=%s STAGING=%s FREE_KIB=%s FREE_PCT=%s OBJECT_CHANGES=%s\n' \
        "$(date -u +%FT%TZ)" "$cpu" "$max_cpu" "$proc_cpu" "$max_proc_cpu" \
        "$state" "$pending" "$max_pending" "$stage_files" "$free" "$free_pct" "$object_changes"
      last_report=$now
    fi
    prev_pending=$pending
  done

  [[ $(readlink -f "$active") == "$CANARY_CANDIDATE_PATH" ]]
  [[ $(sha256sum "$active" | awk '{print $1}') == "$CANARY_NEW_SHA256" ]]
  [[ $(systemctl is-active "$timer" || true) == active ]]
  [[ $(systemctl is-active "$watchtimer" || true) == active ]]
  [[ $(systemctl is-active "$collector" || true) == active ]]
  [[ $(systemctl is-active "$refcollector" || true) == inactive ]]
  [[ ! -e /run/monday/polymarket-upload-watchdog.suppress ]]
  [[ $(systemctl_value "$svc" ActiveState) == inactive ]]
  [[ $(systemctl_value "$svc" Result) == success ]]
  [[ $(jq -c '.failed_segments' "$status") == '[]' ]]

  local final_pending
  final_pending=$(find "$spool" -maxdepth 1 -type f -name 'market-updates.*.ndjson' | wc -l)
  [[ $final_pending -eq 0 ]] || rollback_now "final_pending_${final_pending}"
  [[ $object_changes -ge $minimum_objects ]] \
    || rollback_now "insufficient_rotations_${object_changes}"
  printf 'MONITOR_END_UTC=%s MAX_ACTIVE_HOST_CPU=%s MAX_PROCESS_CPU=%s MAX_PENDING=%s OBJECT_CHANGES=%s\n' \
    "$(date -u +%FT%TZ)" "$max_cpu" "$max_proc_cpu" "$max_pending" "$object_changes"
  printf 'FINAL_STATUS='; jq -c . "$status"
  printf 'FINAL_FREE_KIB='; df -Pk /data | awk 'NR == 2 {print $4}'
  printf 'FINAL_ACTIVE_SHA256='; sha256sum "$active" | awk '{print $1}'
  trap - ERR
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  run_monitor
fi
