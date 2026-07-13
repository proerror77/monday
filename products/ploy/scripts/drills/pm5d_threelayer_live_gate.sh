#!/usr/bin/env bash
set -euo pipefail

HOST_ROOT="/opt/ploy"
ADDR="http://127.0.0.1:8081"
MANIFEST=""
RENDERED_MANIFEST=""
DRYRUN_CONFIG=""
LIVE_CONFIG=""
DEPLOYMENT_ID=""
RUN_DRY_RUN_DRILL=1
WARNINGS=0

usage() {
  cat <<'EOF'
Usage: pm5d_threelayer_live_gate.sh [--host-root /opt/ploy] [--addr http://127.0.0.1:8081]

Stages the PM5D ThreeLayer live deployment as paused on the trade host.

Default behavior:
  - verify daemon, env, manifest, and live/dry-run config parity
  - run the existing paper live-dry-run drill when available
  - apply pm5d.threelayer.live with desired_state=paused
  - stop before placing any live orders

The script never edits /opt/ploy/.env and never changes strategy parameters.
Only the protected live-approval workflow may resume the deployment.
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

cleanup() {
  if [[ -n "$RENDERED_MANIFEST" ]]; then
    rm -f "$RENDERED_MANIFEST"
  fi
}

trap cleanup EXIT

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

ployctl_capture() {
  "$PLOYCTL" "$@" 2>&1
}

strip_blank_lines() {
  printf '%s\n' "$1" | tr -d '\r' | sed '/^[[:space:]]*$/d'
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

require_env_key() {
  local key="$1"
  env_has_key "$key" || fail "required env key missing from ${ENV_FILE}: ${key}"
}

require_any_env_key() {
  local label="$1"
  shift
  env_has_any_key "$@" || fail "required ${label} missing from ${ENV_FILE}: one of [$*]"
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

source_env_file() {
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
}

extract_manifest_field() {
  local field="$1"
  python3 - "$MANIFEST" "$field" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    value = json.load(handle)[sys.argv[2]]
print(value)
PY
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
    --dryrun-config)
      DRYRUN_CONFIG="$2"
      shift 2
      ;;
    --live-config)
      LIVE_CONFIG="$2"
      shift 2
      ;;
    --skip-dry-run-drill)
      RUN_DRY_RUN_DRILL=0
      shift
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
MANIFEST="${MANIFEST:-${HOST_ROOT}/config/deployments/pm5d.threelayer.live.json}"
DRYRUN_CONFIG="${DRYRUN_CONFIG:-${HOST_ROOT}/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml}"
LIVE_CONFIG="${LIVE_CONFIG:-${HOST_ROOT}/config/strategies/02-pm5d-threelayer.live.toml}"

require_command systemctl
require_command curl
require_command python3
[[ -x "$PLOYCTL" ]] || fail "missing ployctl binary: $PLOYCTL"
[[ -f "$ENV_FILE" ]] || fail "missing env file: $ENV_FILE"
[[ -f "$MANIFEST" ]] || fail "missing live manifest: $MANIFEST"
[[ -f "$DRYRUN_CONFIG" ]] || fail "missing dry-run config: $DRYRUN_CONFIG"
[[ -f "$LIVE_CONFIG" ]] || fail "missing live config: $LIVE_CONFIG"

DEPLOYMENT_ID="${DEPLOYMENT_ID:-$(extract_manifest_field deployment_id)}"
[[ -n "$DEPLOYMENT_ID" ]] || fail "deployment id could not be determined"

source_env_file

[[ -n "${PLOY_LIVE_ACCOUNT_ID:-}" ]] || fail "PLOY_LIVE_ACCOUNT_ID is required"
NORMALIZED_LIVE_ACCOUNT_ID="$(python3 - "$PLOY_LIVE_ACCOUNT_ID" <<'PY'
import re
import sys

raw = sys.argv[1].strip()
if not re.fullmatch(r"0[xX][0-9a-fA-F]{40}", raw):
    raise SystemExit("PLOY_LIVE_ACCOUNT_ID must be a 0x-prefixed 40-hex wallet address")
normalized = "0x" + raw[2:].lower()
if normalized == "0x" + "0" * 40:
    raise SystemExit("PLOY_LIVE_ACCOUNT_ID cannot be the all-zero address")
print(normalized)
PY
)"
EXECUTION_PRINCIPAL="$(ployctl_capture trading principal)"
[[ "$EXECUTION_PRINCIPAL" == "$NORMALIZED_LIVE_ACCOUNT_ID" ]] || fail \
  "PLOY_LIVE_ACCOUNT_ID ${NORMALIZED_LIVE_ACCOUNT_ID} does not match execution principal ${EXECUTION_PRINCIPAL}"

log_step "daemon baseline"
[[ "$(systemctl is-active ployd)" == "active" ]] || fail "ployd.service is not active"
curl -fsS "${ADDR}/health" >/dev/null
STATUS_OUTPUT="$(ployctl_capture system status)"
METRICS_OUTPUT="$(ployctl_capture system metrics)"
ALERTS_OUTPUT="$(ployctl_capture system alerts)"
TRADING_OUTPUT="$(ployctl_capture trading status)"
printf '%s\n' "$STATUS_OUTPUT"
printf '%s\n' "$METRICS_OUTPUT"
printf '%s\n' "$ALERTS_OUTPUT"
printf '%s\n' "$TRADING_OUTPUT"

ALERTS_NORMALIZED="$(strip_blank_lines "$ALERTS_OUTPUT")"
if [[ "$ALERTS_NORMALIZED" != "none" ]]; then
  if printf '%s\n' "$ALERTS_NORMALIZED" | grep -q ' critical '; then
    fail "critical alerts are active"
  fi
  warn "non-critical alerts are active"
fi

log_step "credential presence"
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
    fail "live wallet mapping must be proxy:PROXY or gnosis_safe:SAFE; got ${SIGNATURE_TYPE:-unset}:${WALLET_TYPE:-unset}"
    ;;
esac

log_step "manifest and config parity"
python3 - "$MANIFEST" "$DRYRUN_CONFIG" "$LIVE_CONFIG" <<'PY'
import json
import pathlib
import re
import sys
import tomllib

manifest_path = pathlib.Path(sys.argv[1])
dryrun_path = pathlib.Path(sys.argv[2])
live_path = pathlib.Path(sys.argv[3])

manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
if manifest.get("deployment_id") != "pm5d.threelayer.live":
    raise SystemExit("manifest deployment_id must be pm5d.threelayer.live")
if manifest.get("bundle_id") != "02-pm5d-threelayer.live":
    raise SystemExit("manifest bundle_id must be 02-pm5d-threelayer.live")
if manifest.get("runtime_mode") != "live":
    raise SystemExit("manifest runtime_mode must be live")
if manifest.get("desired_state") != "paused":
    raise SystemExit("manifest desired_state must stay paused")
if manifest.get("account_id") != "live-wallet-must-be-rendered":
    raise SystemExit("repository live manifest must retain its unrendered wallet sentinel")

dryrun = tomllib.loads(dryrun_path.read_text(encoding="utf-8"))
live = tomllib.loads(live_path.read_text(encoding="utf-8"))
if dryrun.get("runtime", {}).get("mode") != "dryrun":
    raise SystemExit("source config must use runtime.mode=dryrun")
if live.get("runtime", {}).get("mode") != "live":
    raise SystemExit("live config must use runtime.mode=live")

dryrun["runtime"]["mode"] = "live"
for key in (
    "record_market_updates_to",
    "record_market_updates_max_records",
    "record_market_updates_max_bytes",
):
    dryrun["runtime"].pop(key, None)

dry_strategy = dryrun["strategy"]
live_strategy = live["strategy"]
if not (0 < float(live_strategy["stake_usd"]) <= float(manifest["max_gross_exposure"]) <= 5):
    raise SystemExit("live stake/exposure must be positive and capped at 5 USD")
if not (0 < int(live_strategy["max_positions"]) <= int(dry_strategy["max_positions"])):
    raise SystemExit("live max_positions must be a positive dry-run risk reduction")
if not (0 < int(live_strategy["max_daily_trades"]) <= 10):
    raise SystemExit("live max_daily_trades must be bounded at 10")
if not set(live_strategy["allowed_window_secs"]).issubset(dry_strategy["allowed_window_secs"]):
    raise SystemExit("live windows must be a subset of replayed dry-run windows")
if float(live_strategy["stake_usd"]) > float(dry_strategy["stake_usd"]):
    raise SystemExit("live stake cannot exceed replayed dry-run stake")

for key in ("stake_usd", "max_positions", "max_daily_trades", "allowed_window_secs"):
    dry_strategy[key] = live_strategy[key]
if dryrun != live:
    raise SystemExit("live model/execution config differs from promoted dry-run source")

print("manifest/config gate: paused apply only")
PY

MAX_GROSS_EXPOSURE="$(extract_manifest_field max_gross_exposure)"
log_step "Polymarket account trading readiness"
if ! READINESS_OUTPUT="$(ployctl_capture trading readiness "$MAX_GROSS_EXPOSURE")"; then
  fail "Polymarket account readiness failed: $READINESS_OUTPUT"
fi
printf '%s\n' "$READINESS_OUTPUT"

RENDERED_MANIFEST="$(mktemp "${TMPDIR:-/tmp}/ploy-live-manifest.XXXXXX.json")"
python3 - "$MANIFEST" "$RENDERED_MANIFEST" "$NORMALIZED_LIVE_ACCOUNT_ID" <<'PY'
import json
import pathlib
import sys

source = pathlib.Path(sys.argv[1])
destination = pathlib.Path(sys.argv[2])
manifest = json.loads(source.read_text(encoding="utf-8"))
manifest["account_id"] = sys.argv[3]
destination.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
PY

if [[ "$RUN_DRY_RUN_DRILL" -eq 1 && -x "${HOST_ROOT}/scripts/drills/live_dry_run.sh" ]]; then
  log_step "paper live-host drill"
  "${HOST_ROOT}/scripts/drills/live_dry_run.sh"
elif [[ "$RUN_DRY_RUN_DRILL" -eq 1 ]]; then
  warn "paper drill script not found or not executable; skipping ${HOST_ROOT}/scripts/drills/live_dry_run.sh"
fi

log_step "apply paused live deployment"
APPLY_OUTPUT="$(ployctl_capture deployments apply "$RENDERED_MANIFEST")"
printf '%s\n' "$APPLY_OUTPUT"
INSPECT_OUTPUT="$(ployctl_capture deployments inspect "$DEPLOYMENT_ID")"
printf '%s\n' "$INSPECT_OUTPUT"
case "$INSPECT_OUTPUT" in
  *"desired=Paused"*) ;;
  *) fail "live deployment was not applied as paused" ;;
esac

log_step "result"
if [[ "$WARNINGS" -gt 0 ]]; then
  printf 'STAGED-WITH-WARNINGS: %s remains paused; live approval is blocked until warnings are cleared.\n' "$DEPLOYMENT_ID"
else
  printf 'STAGED: %s remains paused. Only the protected live-approval workflow may resume it.\n' "$DEPLOYMENT_ID"
fi
