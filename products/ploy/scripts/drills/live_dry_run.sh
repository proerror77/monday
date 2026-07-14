#!/usr/bin/env bash
set -euo pipefail

HOST_ROOT="/opt/ploy"
ADDR="http://127.0.0.1:8081"
MANIFEST=""
DEPLOYMENT_ID=""
PLOYCTL=""
DRILL_APPLIED=0
WARNINGS=0

usage() {
  cat <<'EOF'
Usage: live_dry_run.sh [--host-root /opt/ploy] [--addr http://127.0.0.1:8081] [--manifest /opt/ploy/config/deployments/example.live.dry-run.json] [--deployment-id example.live.dry-run]

Remote-host dry-run acceptance drill for the ploy workspace control plane.
This script validates host readiness with ployctl and a paper deployment only.
It does not place real live orders or trigger real redeem actions.
EOF
}

log_step() {
  printf '\n[%s] %s\n' "step" "$1"
}

note() {
  printf '[info] %s\n' "$1"
}

warn() {
  printf '[warn] %s\n' "$1"
  WARNINGS=$((WARNINGS + 1))
}

fail() {
  printf '[fail] %s\n' "$1" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

ployctl_capture() {
  "$PLOYCTL" "$@" 2>&1
}

ployctl_quiet() {
  "$PLOYCTL" "$@" >/dev/null 2>&1
}

extract_value() {
  local line="$1"
  local key="$2"
  printf '%s\n' "$line" | tr ' ' '\n' | awk -F= -v k="$key" '$1 == k { print $2 }'
}

strip_blank_lines() {
  printf '%s\n' "$1" | tr -d '\r' | sed '/^[[:space:]]*$/d'
}

normalize_status_value() {
  local value="$1"
  value="${value%%@*}"
  printf '%s\n' "$value"
}

env_has_key() {
  local key="$1"
  grep -Eq "^[[:space:]]*${key}=" "$ENV_FILE"
}

env_has_any_key() {
  local key
  for key in "$@"; do
    if env_has_key "$key"; then
      return 0
    fi
  done
  return 1
}

first_env_value() {
  local key
  for key in "$@"; do
    if env_has_key "$key"; then
      grep -E "^[[:space:]]*${key}=" "$ENV_FILE" | tail -n 1 | cut -d= -f2- | tr -d '[:space:]'
      return 0
    fi
  done
  return 1
}

require_env_key() {
  local key="$1"
  env_has_key "$key" || fail "required env key missing from ${ENV_FILE}: ${key}"
}

require_any_env_key() {
  local label="$1"
  shift
  env_has_any_key "$@" || fail "required ${label} missing from ${ENV_FILE}: one of [$*]"
}

source_env_file() {
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
}

cleanup() {
  if [[ "$DRILL_APPLIED" -eq 1 ]]; then
    note "stopping drill deployment ${DEPLOYMENT_ID}"
    ployctl_quiet deployments stop "$DEPLOYMENT_ID" || true
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --host-root)
      HOST_ROOT="$2"
      shift 2
      ;;
    --addr)
      ADDR="$2"
      shift 2
      ;;
    --manifest)
      MANIFEST="$2"
      shift 2
      ;;
    --deployment-id)
      DEPLOYMENT_ID="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

PLOYCTL="${HOST_ROOT}/bin/ployctl"
ENV_FILE="${HOST_ROOT}/.env"
RUNTIME_ROOT="${HOST_ROOT}/run/platform"
STATE_ROOT="${HOST_ROOT}/data/state"
MANIFEST="${MANIFEST:-${HOST_ROOT}/config/deployments/example.live.dry-run.json}"

if [[ -z "${DEPLOYMENT_ID}" && -f "${MANIFEST}" ]]; then
  DEPLOYMENT_ID="$(python3 - "$MANIFEST" <<'PY'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    print(json.load(handle)["deployment_id"])
PY
)"
fi

[[ -n "${DEPLOYMENT_ID}" ]] || fail "deployment id could not be determined"

trap cleanup EXIT

require_command systemctl
require_command curl
require_command python3
[[ -x "${PLOYCTL}" ]] || fail "missing ployctl binary: ${PLOYCTL}"
[[ -f "${ENV_FILE}" ]] || fail "missing env file: ${ENV_FILE}"
[[ -f "${MANIFEST}" ]] || fail "missing drill manifest: ${MANIFEST}"

source_env_file

log_step "daemon baseline"
[[ "$(systemctl is-active ployd)" == "active" ]] || fail "ployd.service is not active"
curl -fsS "${ADDR}/health" >/dev/null
STATUS_OUTPUT="$(ployctl_capture system status)"
METRICS_OUTPUT="$(ployctl_capture system metrics)"
ALERTS_OUTPUT="$(ployctl_capture system alerts)"
AUDIT_OUTPUT="$(ployctl_capture system audit)"
printf '%s\n' "${STATUS_OUTPUT}"
printf '%s\n' "${METRICS_OUTPUT}"
printf '%s\n' "${ALERTS_OUTPUT}"
printf '%s\n' "${AUDIT_OUTPUT}"

SYSTEM_STATE="$(normalize_status_value "$(extract_value "${STATUS_OUTPUT}" "status")")"
case "${SYSTEM_STATE}" in
  running)
    ;;
  degraded|recovering|starting)
    warn "system status is ${SYSTEM_STATE}"
    ;;
  *)
    fail "unexpected system status: ${SYSTEM_STATE}"
    ;;
esac

ALERTS_NORMALIZED="$(strip_blank_lines "${ALERTS_OUTPUT}")"
if [[ "${ALERTS_NORMALIZED}" != "none" ]]; then
  if printf '%s\n' "${ALERTS_NORMALIZED}" | grep -q ' critical '; then
    fail "critical alerts are active"
  fi
  warn "non-critical alerts are active"
fi

STALE_SOURCES="$(extract_value "${METRICS_OUTPUT}" "stale_sources")"
if [[ -n "${STALE_SOURCES}" && "${STALE_SOURCES}" != "0" ]]; then
  warn "stale sources reported: ${STALE_SOURCES}"
fi

log_step "config and credential presence"
if env_has_any_key \
  "PLOY_ADMIN_TOKEN" \
  "PLOY_API_ADMIN_TOKEN" \
  "PLOY_API_KEY" \
  "PLOY_OPERATOR_TOKEN" \
  "PLOY_API_OPERATOR_TOKEN"; then
  if ! env_has_key "PLOY_OPERATOR_TOKEN" && ! env_has_key "PLOY_API_OPERATOR_TOKEN"; then
    warn "operator token not configured; dry-run will rely on admin credentials"
  fi
else
  warn "operator credential not configured; dry-run will rely on local ployctl access"
fi
require_env_key "POLYMARKET_PRIVATE_KEY"
SIGNATURE_TYPE="$(first_env_value "POLY_SIGNATURE_TYPE" "POLYMARKET_SIGNATURE_TYPE" || true)"
WALLET_TYPE="$(first_env_value "POLY_WALLET_TYPE" || true)"
WALLET_TYPE="${WALLET_TYPE^^}"
case "${SIGNATURE_TYPE}:${WALLET_TYPE}" in
  proxy:PROXY|gnosis_safe:SAFE)
    require_any_env_key "non-EOA funder" "POLY_FUNDER" "POLYMARKET_FUNDER" "POLYMARKET_FUNDER_ADDRESS"
    ;;
  poly1271:*)
    fail "POLY_SIGNATURE_TYPE=poly1271 is not supported by the current custody/redemption relayer"
    ;;
  *)
    fail "wallet mapping must be proxy:PROXY or gnosis_safe:SAFE; got ${SIGNATURE_TYPE:-unset}:${WALLET_TYPE:-unset}"
    ;;
esac
[[ -f "${STATE_ROOT}/deployments.json" ]] || fail "missing deployments state file: ${STATE_ROOT}/deployments.json"
[[ -f "${RUNTIME_ROOT}/system-status.json" ]] || fail "missing system status snapshot: ${RUNTIME_ROOT}/system-status.json"
[[ -f "${RUNTIME_ROOT}/deployments.json" ]] || fail "missing deployment snapshot: ${RUNTIME_ROOT}/deployments.json"
[[ -f "${RUNTIME_ROOT}/trading-state.json" ]] || fail "missing trading state snapshot: ${RUNTIME_ROOT}/trading-state.json"
[[ -f "${RUNTIME_ROOT}/audit-log.jsonl" ]] || fail "missing audit log: ${RUNTIME_ROOT}/audit-log.jsonl"

log_step "paper deployment drill"
APPLY_OUTPUT="$(ployctl_capture deployments apply "${MANIFEST}")"
DRILL_APPLIED=1
printf '%s\n' "${APPLY_OUTPUT}"
INSPECT_OUTPUT="$(ployctl_capture deployments inspect "${DEPLOYMENT_ID}")"
printf '%s\n' "${INSPECT_OUTPUT}"
ployctl_quiet deployments pause "${DEPLOYMENT_ID}"
ployctl_quiet deployments resume "${DEPLOYMENT_ID}"
ployctl_quiet deployments stop "${DEPLOYMENT_ID}"
DRILL_APPLIED=0

FINAL_INSPECT="$(ployctl_capture deployments inspect "${DEPLOYMENT_ID}")"
printf '%s\n' "${FINAL_INSPECT}"

log_step "trading readiness"
TRADING_OUTPUT="$(ployctl_capture trading status)"
printf '%s\n' "${TRADING_OUTPUT}"

LIVE_RECONCILE_FAILURES="$(extract_value "${STATUS_OUTPUT}" "live_reconcile_failures")"
if [[ -n "${LIVE_RECONCILE_FAILURES}" && "${LIVE_RECONCILE_FAILURES}" != "0" ]]; then
  warn "live reconcile failures reported: ${LIVE_RECONCILE_FAILURES}"
fi

log_step "result"
if [[ "${WARNINGS}" -gt 0 ]]; then
  printf 'WARN: remote host passed the dry-run drill with %s warning(s); review alerts before enabling real live trading.\n' "${WARNINGS}"
else
  printf 'PASS: remote host passed the dry-run drill and is ready for manual live go/no-go review.\n'
fi
