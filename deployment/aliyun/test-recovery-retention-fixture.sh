#!/usr/bin/env bash
# Shared fixture constructors only; never invoked by a production entrypoint.
# shellcheck disable=SC2034,SC2317,SC2329 # Native sourced functions consume these globals and overrides.
RETENTION_FIXTURE_SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

retention_fixture_context() {
  local fixture_root=$1
  RETENTION_FIXTURE_UID=$(command id -u)
  RETENTION_FIXTURE_GID=$(command id -g)
  # shellcheck disable=SC1091
  . "$RETENTION_FIXTURE_SCRIPT_DIR/host-rust-lob-recovery-queue.sh"
  # shellcheck disable=SC1091
  . "$RETENTION_FIXTURE_SCRIPT_DIR/rust-lob-control-plane-lib.sh"
  eval "$(declare -f secure_regular_file | sed '1s/secure_regular_file/retention_fixture_regular/')"
  eval "$(declare -f secure_directory | sed '1s/secure_directory/retention_fixture_directory/')"
  secure_regular_file() { retention_fixture_regular "$1" "$RETENTION_FIXTURE_UID"; }
  secure_directory() { retention_fixture_directory "$1" "$RETENTION_FIXTURE_UID" "$RETENTION_FIXTURE_GID"; }
  ensure_root_directory() {
    if [[ -e $1 || -L $1 ]]; then secure_directory "$1" "$RETENTION_FIXTURE_UID" "$RETENTION_FIXTURE_GID"
    else mkdir -m 0750 -- "$1"; fi
  }
  id() {
    if [[ ${2:-} == hftcollector ]]; then
      case "$1" in -u) printf '%s\n' "$RETENTION_FIXTURE_UID" ;; -g) printf '%s\n' "$RETENTION_FIXTURE_GID" ;; *) return 1 ;; esac
    else command id "$@"; fi
  }
  configure_paths "$fixture_root"
  if [[ ${2:-} == health ]]; then
    CANONICAL_ROOT="$fixture_root/spool/binance-lob"
    QUEUE_ROOT="$fixture_root/spool/binance-lob-recovery"
    EVIDENCE_ROOT="$fixture_root/evidence/lob-queue"
  fi
  MARKET=spot; market_paths
}

setup_retention_fixture() {
  local fixture_root=$1 asset runtime controller deployment
  retention_fixture_context "$@"
  mkdir -p "$BIN_DIR" "$RELEASE_ROOT" "$CONTROLLER_RELEASE_ROOT" "$CONFIG_ROOT" \
    "$QUEUE_MARKET_ROOT" "$CANONICAL_SPOOL" "$EVIDENCE_ROOT" "$LOCK_ROOT" "$ROOT_PREFIX/tmp"
  printf '#!/bin/sh\nexit 99\n' >"$fixture_root/payload"
  chmod 0755 "$fixture_root/payload"
  RETENTION_FIXTURE_PAYLOAD=$(sha256sum "$fixture_root/payload" | awk '{print $1}')
  mkdir "$RELEASE_ROOT/$RETENTION_FIXTURE_PAYLOAD"
  cp "$fixture_root/payload" "$RELEASE_ROOT/$RETENTION_FIXTURE_PAYLOAD/binance-lob-archiver"
  printf '{}\n' >"$RELEASE_ROOT/$RETENTION_FIXTURE_PAYLOAD/release.json"
  deployment="$fixture_root/controller/deployment"; mkdir -p "$deployment"
  while IFS= read -r asset; do cp "$RETENTION_FIXTURE_SCRIPT_DIR/$asset" "$deployment/$asset"; done \
    < <(printf '%s\n' "$(monday_runtime_assets)" "$(monday_controller_assets)" | sort -u)
  sed "s|^SPOOL_DIR=.*|SPOOL_DIR=$CANONICAL_SPOOL|" \
    "$deployment/binance-lob-archiver-production-spot.env" >"$fixture_root/spot.env"
  mv "$fixture_root/spot.env" "$deployment/binance-lob-archiver-production-spot.env"
  runtime=$(monday_rust_lob_runtime_contract_sha256 "$deployment")
  jq -cnS --arg payload "$RETENTION_FIXTURE_PAYLOAD" --arg runtime "$runtime" \
    --arg source "$(printf '%040d' 21)" --arg bundle "$(printf '%064d' 22)" \
    '{schema:"monday.rust_lob_controller_release.v2",control_plane_version:2,topology:"stable",
      artifact_sha256:$payload,artifact_uri:("oss://bucket/payload/"+$payload),runtime_contract_sha256:$runtime,
      deployment_source_revision:$source,deployment_bundle_sha256:$bundle,
      deployment_bundle_uri:("oss://bucket/bundle/"+$bundle)}' >"$fixture_root/controller/release.json"
  controller=$(sha256sum "$fixture_root/controller/release.json" | awk '{print $1}')
  ln -s "$RELEASE_ROOT/$RETENTION_FIXTURE_PAYLOAD/binance-lob-archiver" "$fixture_root/controller/binance-lob-archiver"
  (cd "$fixture_root/controller" || exit 1
    sha256sum release.json >release.json.sha256
    for asset in deployment/*; do monday_sha256_checksum_line "$asset"; done | sort -k2 >deployment.sha256)
  chmod -R go-w "$fixture_root/controller"
  mv "$fixture_root/controller" "$CONTROLLER_RELEASE_ROOT/$controller"
  ln -s "$CONTROLLER_RELEASE_ROOT/$controller" "$ACTIVE_CONTROLLER"
  ln -s "$ACTIVE_CONTROLLER/binance-lob-archiver" "$PRODUCTION_LINK"
  ln -s "$ACTIVE_CONTROLLER/deployment/host-rust-lob-recovery-queue.sh" "$INSTALLED_RECOVERY"
  ln -s "$ACTIVE_CONTROLLER/deployment/binance-lob-archiver-production-spot.env" "$ENV_FILE"
  EXECUTING_RECOVERY_PROGRAM=$(readlink -f "$INSTALLED_RECOVERY")
  RETENTION_FIXTURE_CONTROLLER=$controller
  secure_release_identity
  systemctl() { fail 'retention must not invoke systemctl'; }
  runuser() { fail 'retention must not execute a payload/uploader'; }
}

retention_fixture_job() {
  local number=$1 payload=${2:-$(printf '%064d' 41)} version=${3:-v1} env_sha bundle source job_sha
  RETAIN_JOB_ID="20260901T000000Z-spot-${payload:0:12}-$number"
  RETENTION_FIXTURE_JOB_DIR="$QUEUE_MARKET_ROOT/$RETAIN_JOB_ID.failed"
  RETENTION_FIXTURE_SEGMENT_DIR="$RETENTION_FIXTURE_JOB_DIR/date=2026-09-01/hour=00"
  RETENTION_FIXTURE_EVIDENCE="$EVIDENCE_ROOT/$RETAIN_JOB_ID"
  mkdir -m 0750 "$RETENTION_FIXTURE_JOB_DIR" "$RETENTION_FIXTURE_EVIDENCE"
  mkdir -m 0750 "${RETENTION_FIXTURE_SEGMENT_DIR%/*}" "$RETENTION_FIXTURE_SEGMENT_DIR"
  mkdir -m 0750 "$RETENTION_FIXTURE_EVIDENCE/recovery-input"
  cp "$RELEASE_ENV_FILE" "$RETENTION_FIXTURE_JOB_DIR/recovery.env"
  env_sha=$(sha256sum "$RETENTION_FIXTURE_JOB_DIR/recovery.env" | awk '{print $1}')
  bundle=$(printf '%064d' 42); source=$(printf '%040d' 43)
  jq -cnS --arg job "$RETAIN_JOB_ID" --arg canonical "$CANONICAL_SPOOL" --arg p "$payload" \
    --arg env "$env_sha" --arg bundle "$bundle" --arg source "$source" \
    '{schema:"monday.rust_lob_recovery_queue.v1",job_id:$job,market:"spot",queued_at:"2026-09-01T00:00:00Z",
      canonical_spool:$canonical,recovery_unit:"binance-lob-archiver-recovery@spot.service",
      release_sha256:$p,deployment_bundle_sha256:$bundle,deployment_source_revision:$source,
      env_sha256:$env,release_env:"recovery.env"}' >"$RETENTION_FIXTURE_JOB_DIR/job.json"
  jq -cnS --arg job "$RETAIN_JOB_ID" --arg p "$payload" --arg env "$env_sha" --arg bundle "$bundle" --arg source "$source" \
    '{schema:"monday.rust_lob_recovery_queue_result.v1",job_id:$job,market:"spot",release_sha256:$p,
      deployment_bundle_sha256:$bundle,deployment_source_revision:$source,env_sha256:$env,
      started_at:"2026-09-01T00:00:01Z",completed_at:"2026-09-01T00:00:02Z",result:"failed",step:"drain",message:"fixture"}' \
      >"$RETENTION_FIXTURE_EVIDENCE/result.json"
  if [[ $version == v2 ]]; then
    jq --arg controller "$(printf '%064d' 44)" --arg runtime "$(printf '%064d' 45)" --arg payload "$payload" \
      '.+{controller_sha256:$controller,runtime_contract_sha256:$runtime,payload_sha256:$payload}' \
      "$RETENTION_FIXTURE_JOB_DIR/job.json" >"$RETENTION_FIXTURE_JOB_DIR/job.new"
    mv "$RETENTION_FIXTURE_JOB_DIR/job.new" "$RETENTION_FIXTURE_JOB_DIR/job.json"
    job_sha=$(sha256sum "$RETENTION_FIXTURE_JOB_DIR/job.json" | awk '{print $1}')
    jq --arg controller "$(printf '%064d' 44)" --arg runtime "$(printf '%064d' 45)" \
      --arg payload "$payload" --arg job_sha "$job_sha" --arg bundle "$bundle" --arg source "$source" \
      '.+{schema:"monday.rust_lob_recovery_queue_result.v2",payload_sha256:$payload,
        controller_sha256:$controller,runtime_contract_sha256:$runtime,job_receipt_sha256:$job_sha,
        adoption_sha256:"",request_sha256:"",executing_controller_sha256:$controller,
        executing_deployment_bundle_sha256:$bundle,executing_deployment_source_revision:$source,
        minimum_upload_success_at:.started_at,upload_triplet_readback:{}}' \
      "$RETENTION_FIXTURE_EVIDENCE/result.json" >"$RETENTION_FIXTURE_EVIDENCE/result.new"
    mv "$RETENTION_FIXTURE_EVIDENCE/result.new" "$RETENTION_FIXTURE_EVIDENCE/result.json"
  fi
  printf 'lock-evidence\n' >"$RETENTION_FIXTURE_JOB_DIR/.binance-lob-archiver.lock"
  printf 'unfinished-part\n' >"$RETENTION_FIXTURE_SEGMENT_DIR/part-one.jsonl.part"
  printf 'corrupt-part\n' >"$RETENTION_FIXTURE_SEGMENT_DIR/part-two.part.corrupt"
  printf 'sealed-but-not-delivered\n' >"$RETENTION_FIXTURE_SEGMENT_DIR/part-three.jsonl.zst"
  printf '{"readiness":"rejected","sequence_gaps":1}\n' >"$RETENTION_FIXTURE_SEGMENT_DIR/part-three.manifest.json"
  printf '{"failure_count":7,"last_error":"old failure","last_success_at":null}\n' >"$RETENTION_FIXTURE_JOB_DIR/upload-status.json"
  printf '{"original_backup":"unrecovered"}\n' >"$RETENTION_FIXTURE_EVIDENCE/recovery-input/receipt.json"
  printf 'original-backup\n' >"$RETENTION_FIXTURE_EVIDENCE/recovery-input/original.jsonl.part"
  chmod -R go-w "$RETENTION_FIXTURE_JOB_DIR" "$RETENTION_FIXTURE_EVIDENCE"
  RETAIN_JOB_SHA256=$(sha256sum "$RETENTION_FIXTURE_JOB_DIR/job.json" | awk '{print $1}')
  RETAIN_RESULT_SHA256=$(sha256sum "$RETENTION_FIXTURE_EVIDENCE/result.json" | awk '{print $1}')
  RETAIN_CONTROLLER=$RETENTION_FIXTURE_CONTROLLER
  RETAIN_REQUEST_ID="fixture-$number"
  RETAIN_REASON_CODE=retain-unrecovered-historical-evidence
}

retention_fixture_retain() { (queue_lock; retain_market); }
retention_fixture_check() { (RETENTION_DEADLINE=$((SECONDS + 25)); check_retained_market); }
