#!/usr/bin/env bash
# The host functions are loaded below and consume these globals and fakes.
# ShellCheck versions report those indirect fake calls as SC2317 or SC2329.
# Each subshell independently initializes its state; no state crosses cases.
# shellcheck disable=SC2034,SC2317,SC2329,SC2030,SC2031
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
cutover="$script_dir/host-bybit-options-cutover.sh"
RUNTIME_HEALTH_POLICY="$script_dir/bybit-options-runtime-health-policy.jq"
tmp_dir=$(mktemp -d)
tmp_dir=$(cd "$tmp_dir" && pwd -P)
trap 'rm -rf "$tmp_dir"' EXIT
sha256sum_bin=$(command -v gsha256sum || command -v sha256sum)
sha256sum() { "$sha256sum_bin" "$@"; }
# shellcheck disable=SC1090,SC1091
. "$script_dir/bybit-options-control-plane-lib.sh"
for function in runtime_matches_release capture_rollback_runtime_identity \
  rollback_runtime_identity_matches health_ready_for_release health_ready_for_rollback \
  wait_for_rollback_health rollback_after_failure copy_health_evidence \
  clear_health_before_restart production_is_fail_closed segment_artifacts \
  require_empty_segment_spool run_candidate_drain bootstrap_remaining_seconds \
  capture_bootstrap_segments verify_bootstrap_upload complete_new_host_bootstrap; do
  body=$(sed -n "/^$function() {/,/^}/p" "$cutover")
  [[ -n $body ]]
  eval "$body"
done

reject() {
  local label=$1
  shift
  if "$@" >/dev/null 2>&1; then
    printf 'Bybit cutover accepted %s\n' "$label" >&2
    exit 1
  fi
}

# These bounded fakes model only external process/service effects. The real
# host rollback function, identity checks and JSON predicates run unchanged.
(
  CANONICAL_SPOOL="$tmp_dir/rollback-spool"
  EVIDENCE_DIR="$tmp_dir/rollback-evidence"
  RELEASE_ROOT="$tmp_dir/releases"
  mkdir -p "$CANONICAL_SPOOL" "$EVIDENCE_DIR"
  printf 'old binary bytes\n' >"$tmp_dir/old-binary"
  OLD_SHA256=$(sha256sum "$tmp_dir/old-binary" | awk '{print $1}')
  OLD_BINARY="$RELEASE_ROOT/$OLD_SHA256/bybit-options-archiver"
  mkdir -p "${OLD_BINARY%/*}"
  cp "$tmp_dir/old-binary" "$OLD_BINARY"
  PRODUCTION_LINK="$tmp_dir/production-link"
  ln -s "$OLD_BINARY" "$PRODUCTION_LINK"
  OLD_DEPLOYMENT="$tmp_dir/old-deployment"
  mkdir "$OLD_DEPLOYMENT"
  printf 'bound rollback asset\n' >"$OLD_DEPLOYMENT/asset"
  (cd "$OLD_DEPLOYMENT" && sha256sum asset) >"$EVIDENCE_DIR/rollback-deployment.sha256"
  ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=$(sha256sum "$EVIDENCE_DIR/rollback-deployment.sha256" | awk '{print $1}')
  OLD_MODE=upgrade
  UNIT=bybit-options-archiver.service
  TIMER=bybit-options-upload.timer
  UPLOAD_UNIT=bybit-options-upload.service
  PRODUCTION_UNITS=("$UNIT")
  UPLOAD_UNITS=("$UPLOAD_UNIT" "$TIMER")
  TRANSITION_MASK_UNITS=("$UNIT" "$UPLOAD_UNIT" "$TIMER")
  CANDIDATE_DEPLOYMENT="$tmp_dir/candidate-deployment"
  UPLOAD_FAILURE_BASELINE=28
  MINIMUM_SYMBOLS=500
  HEALTH_TIMEOUT_SECONDS=1
  ROLLBACK_MAIN_PID=
  ROLLBACK_INVOCATION_ID=
  ROLLBACK_UPLOAD_WARNING=null
  ROLLBACK_WARNING_INTERPRETED=false
  active=true
  enabled=true
  observed_pid=123
  observed_invocation=$(printf 'a%.0s' {1..32})
  observed_image=$OLD_BINARY
  jq -n '{failure_count:28,last_success_at:1750000000001,last_error_at:null,last_error:null}' \
    >"$tmp_dir/rollback-status.json"
  cp "$tmp_dir/rollback-status.json" "$CANONICAL_SPOOL/upload-status.json"
  jq -n '{schema:"monday.bybit_options_quote.v1",venue:"bybit",category:"option",
    disk_warning:false,spool_warning:false,upload_failure_count:28,upload_warning:true,
    last_upload_error_at:null,connected_workers:1,symbols_expected:1500,symbols_seen:1450,
    active_segment_bytes:1,last_event_at_ms:1750000000001,updated_at_ms:1750000000002}' \
    >"$tmp_dir/rollback-health.json"
  cp "$tmp_dir/rollback-health.json" "$CANONICAL_SPOOL/health.json"
  canonical_spool_paths_safe() { return 0; }
  date() { if [[ $1 == +%s ]]; then printf '1750000000\n'; else command date "$@"; fi; }
  readlink() {
    if [[ $2 == /proc/*/exe ]]; then printf '%s\n' "$observed_image";
    else command readlink "$@"; fi
  }
  systemctl() {
    case "$1" in
      disable) active=false; enabled=false ;;
      is-active) [[ $3 == "$UNIT" && $active == true ]] ;;
      is-enabled)
        if [[ $2 == --quiet ]]; then [[ $enabled == true ]]; else printf 'masked\n'; fi ;;
      show)
        case "$3" in
          --property=MainPID) printf '%s\n' "$observed_pid" ;;
          --property=InvocationID) printf '%s\n' "$observed_invocation" ;;
          --property=NRestarts) printf '0\n' ;;
          *) return 1 ;;
        esac ;;
      start)
        if [[ $2 == "$UNIT" ]]; then
          active=true
          cp "$tmp_dir/rollback-health.json" "$CANONICAL_SPOOL/health.json"
        fi ;;
      enable) [[ $2 != "$UNIT" ]] || enabled=true ;;
      mask|unmask|daemon-reload|reset-failed) return 0 ;;
      *) return 1 ;;
    esac
  }
  run_candidate_drain() {
    bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" "$UPLOAD_FAILURE_BASELINE"
  }
  install_deployment() { return 0; }
  atomic_symlink() { ln -sf "$1" "$2"; }

  rollback_after_failure
  [[ $ROLLBACK_RESULT == previous-release-health-verified && $active == true && $enabled == true ]]
  [[ $ROLLBACK_UPLOAD_WARNING == true && $ROLLBACK_WARNING_INTERPRETED == true ]]
  jq -e '.upload_warning == true and .upload_failure_count == 28' \
    "$EVIDENCE_DIR/rollback-verified-health.json" >/dev/null
  reject 'historical warning on the ordinary candidate path' health_ready_for_release 500 1750000000000
  for filter in '.failure_count=29' '.failure_count=27' '.last_error="active"' '.last_error_at=1750000000001'; do
    jq "$filter" "$tmp_dir/rollback-status.json" >"$CANONICAL_SPOOL/upload-status.json"
    reject "rollback status $filter" health_ready_for_rollback 1750000000000
  done
  cp "$tmp_dir/rollback-status.json" "$CANONICAL_SPOOL/upload-status.json"
  for filter in '.upload_failure_count=29' '.last_upload_error_at=1750000000001' \
    '.last_event_at_ms=1750000000000' '.updated_at_ms=1750000000000' \
    '.disk_warning=true' '.connected_workers=0'; do
    jq "$filter" "$tmp_dir/rollback-health.json" >"$CANONICAL_SPOOL/health.json"
    reject "rollback health $filter" health_ready_for_rollback 1750000000000
  done
  cp "$tmp_dir/rollback-health.json" "$CANONICAL_SPOOL/health.json"
  observed_pid=124
  reject 'rollback PID drift' health_ready_for_rollback 1750000000000
  observed_pid=123
  observed_invocation=$(printf 'b%.0s' {1..32})
  reject 'rollback invocation drift' health_ready_for_rollback 1750000000000
  observed_invocation=$(printf 'a%.0s' {1..32})
  observed_image=/candidate/bybit-options-archiver
  reject 'rollback process image drift' health_ready_for_rollback 1750000000000
  observed_image=$OLD_BINARY
  printf 'tampered\n' >>"$OLD_BINARY"
  reject 'rollback binary digest drift' health_ready_for_rollback 1750000000000
  cp "$tmp_dir/old-binary" "$OLD_BINARY"
  UPLOAD_FAILURE_BASELINE=0
  jq '.failure_count=0' "$tmp_dir/rollback-status.json" >"$CANONICAL_SPOOL/upload-status.json"
  jq '.upload_failure_count=0' "$tmp_dir/rollback-health.json" >"$CANONICAL_SPOOL/health.json"
  reject 'warning with no historical failures' health_ready_for_rollback 1750000000000
)

# Exercise the real bootstrap sequence using local synthetic test segments and
# a fake uploader that creates (or corrupts) the actual expected receipt shape.
(
  OLD_MODE=new-host
  UNIT=bybit-options-archiver.service
  TIMER=bybit-options-upload.timer
  UPLOAD_UNIT=bybit-options-upload.service
  CANDIDATE_BINARY=/candidate/bybit-options-archiver
  CANDIDATE_DEPLOYMENT="$tmp_dir/candidate-deployment"
  SAFE_PATH=/usr/bin:/bin
  DRAIN_ENV_KEYS=()
  UPLOAD_FAILURE_BASELINE=0
  canonical_spool_paths_safe() { return 0; }
  timeout() {
    [[ $1 == --signal=TERM && $2 == --kill-after=10s && $3 =~ ^[1-9][0-9]*$ ]] || return 1
    shift 3
    "$@"
  }
  systemctl() {
    case "$1" in
      is-active) [[ $3 == "$UNIT" && $active == true ]] ;;
      stop) [[ $bootstrap_case == stop-stuck ]] || active=false ;;
      start) active=true ;;
      *) return 1 ;;
    esac
  }
  runtime_matches_release() { [[ $1 == "$CANDIDATE_BINARY" && $active == true ]]; }
  wait_for_release_health() { [[ $bootstrap_case != restart-unhealthy ]]; }
  runuser() {
    local data="$CANONICAL_SPOOL/$segment" compressed_sha
    printf 'compressed test bytes\n' >"$data.zst"
    compressed_sha=$(sha256sum "$data.zst" | awk '{print $1}')
    jq -n --arg source "$source_sha" --arg compressed "$compressed_sha" --arg file "$segment" \
      --argjson uploaded "$success_ms" '{schema:"monday.bybit_options_upload.v1",
      source_sha256:$source,compressed_sha256:$compressed,uploaded_at_ms:$uploaded,
      object:("oss://monday-lob-apne1-1045353359/lake/raw/venue=bybit/market=option/dataset=options_quotes/date=2026-01-01/hour=00/sha256=" + $compressed + "/" + $file + ".zst")}' \
      >"$data.uploaded.json"
    rm "$data"
    case "$bootstrap_case" in
      no-success) success_json=null ;;
      old-success) success_json=$BOOTSTRAP_PREVIOUS_SUCCESS_MS ;;
      future-success) success_json=$((success_ms + 60000)) ;;
      *) success_json=$success_ms ;;
    esac
    jq -n --argjson success "$success_json" '{failure_count:0,last_success_at:$success,last_error_at:null,last_error:null}' \
      >"$CANONICAL_SPOOL/upload-status.json"
    case "$bootstrap_case" in
      active-error) jq '.last_error="active"' "$CANONICAL_SPOOL/upload-status.json" >"$tmp_dir/changed.json"
        mv "$tmp_dir/changed.json" "$CANONICAL_SPOOL/upload-status.json" ;;
      new-failure) jq '.failure_count=1' "$CANONICAL_SPOOL/upload-status.json" >"$tmp_dir/changed.json"
        mv "$tmp_dir/changed.json" "$CANONICAL_SPOOL/upload-status.json" ;;
      wrong-marker) jq '.source_sha256=("0"*64)' "$data.uploaded.json" >"$tmp_dir/changed.json"
        mv "$tmp_dir/changed.json" "$data.uploaded.json" ;;
    esac
  }
  for bootstrap_case in valid no-events stop-stuck no-success old-success future-success \
    active-error new-failure wrong-marker restart-unhealthy empty-spool deadline-expired; do
    CANONICAL_SPOOL="$tmp_dir/bootstrap-$bootstrap_case"
    EVIDENCE_DIR="$tmp_dir/evidence-$bootstrap_case"
    mkdir "$CANONICAL_SPOOL" "$EVIDENCE_DIR"
    BOOTSTRAP_STARTED_MS=$(( $(date +%s) * 1000 - 1000 ))
    BOOTSTRAP_PREVIOUS_SUCCESS_MS=$((BOOTSTRAP_STARTED_MS - 1))
    success_ms=$((BOOTSTRAP_STARTED_MS + 3))
    BOOTSTRAP_DEADLINE=$((SECONDS + 30))
    BOOTSTRAP_UPLOAD_VERIFIED=false
    active=true
    segment="bybit-options.$BOOTSTRAP_STARTED_MS.ndjson"
    printf '{"kind":"options_catalog"}\n{"kind":"ticker"}\n' >"$CANONICAL_SPOOL/$segment"
    source_sha=$(sha256sum "$CANONICAL_SPOOL/$segment" | awk '{print $1}')
    source_bytes=$(wc -c <"$CANONICAL_SPOOL/$segment")
    jq -n --arg file "$segment" --arg sha "$source_sha" --argjson bytes "$source_bytes" \
      --argjson started "$BOOTSTRAP_STARTED_MS" '{schema:"monday.bybit_options_quote.v1",file:$file,
      sha256:$sha,bytes:$bytes,start_received_at_ms:$started,end_received_at_ms:($started+1),
      events:2,event_types:{options_catalog:1,ticker:1}}' >"$CANONICAL_SPOOL/$segment.manifest.json"
    printf '{}\n' >"$CANONICAL_SPOOL/$segment._SUCCESS"
    case "$bootstrap_case" in
      no-events) jq '.event_types={options_catalog:1}' "$CANONICAL_SPOOL/$segment.manifest.json" >"$tmp_dir/changed.json"
        mv "$tmp_dir/changed.json" "$CANONICAL_SPOOL/$segment.manifest.json" ;;
      empty-spool) rm "$CANONICAL_SPOOL/$segment" ;;
      deadline-expired) BOOTSTRAP_DEADLINE=$SECONDS ;;
    esac
    if [[ $bootstrap_case == valid ]]; then
      complete_new_host_bootstrap
      [[ $BOOTSTRAP_UPLOAD_VERIFIED == true && $active == true ]]
      [[ -s $EVIDENCE_DIR/bootstrap/$segment.uploaded.json ]]
    else
      reject "bootstrap $bootstrap_case" complete_new_host_bootstrap
    fi
  done
)

printf '%s\n' 'Bybit Options cutover behavior tests passed'
