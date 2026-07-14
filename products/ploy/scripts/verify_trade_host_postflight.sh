#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${PLOY_ROOT_DIR:-/opt/ploy}"
EXPECTED_SHA="${1:?usage: verify_trade_host_postflight.sh <40-char-sha> <paused|running>}"
EXPECTED_STATE="${2:?usage: verify_trade_host_postflight.sh <40-char-sha> <paused|running>}"
SYSTEMCTL="${SYSTEMCTL:-systemctl}"
PLOYCTL="${PLOYCTL:-${ROOT_DIR}/current/bin/ployctl}"
CURL="${CURL:-curl}"
PGREP="${PGREP:-pgrep}"
STAT="${STAT:-stat}"

fail() {
  printf 'trade-host postflight failed: %s\n' "$*" >&2
  exit 1
}

[[ "$EXPECTED_SHA" =~ ^[0-9a-f]{40}$ ]] || fail "expected release SHA must be 40 lowercase hex characters"
case "$EXPECTED_STATE" in
  paused|running) ;;
  *) fail "expected state must be paused or running" ;;
esac

release_path="${ROOT_DIR}/releases/${EXPECTED_SHA}"
[[ -d "$release_path" ]] || fail "immutable release directory is missing"
release_dir="$(cd "$release_path" && pwd -P)"
current_dir="$(readlink -f "${ROOT_DIR}/current")"
[[ "$current_dir" == "$release_dir" ]] || fail "current release is ${current_dir}, expected ${release_dir}"
[[ -x "${release_dir}/bin/ployd" ]] || fail "ployd missing from immutable release"
[[ -x "${release_dir}/bin/ployctl" ]] || fail "ployctl missing from immutable release"
[[ -x "${release_dir}/bin/ploy-runner" ]] || fail "ploy-runner missing from immutable release"
[[ -x "${release_dir}/bin/node" ]] || fail "bundled Node.js runtime missing from immutable release"
[[ -x "${release_dir}/tools/polymarket-account-ops/cli.js" ]] || fail "account-ops CLI missing from immutable release"
[[ -x "${release_dir}/tools/polymarket-account-ops/ploy-account-ops" ]] || fail "account-ops launcher missing from immutable release"
[[ -L "${ROOT_DIR}/bin/ploy-account-ops" ]] || fail "account-ops CLI symlink is missing"
[[ -x "${release_dir}/tools/predict-fun-account-ops/cli.js" ]] || fail "Predict account-ops CLI missing from immutable release"
[[ -x "${release_dir}/tools/predict-fun-account-ops/ploy-predict-account-ops" ]] || fail "Predict account-ops launcher missing from immutable release"
[[ -L "${ROOT_DIR}/bin/ploy-predict-account-ops" ]] || fail "Predict account-ops CLI symlink is missing"
(cd "$release_dir" && sha256sum -c FILES.sha256 >/dev/null) \
  || fail "immutable release file checksum verification failed"
grep -Fxq "PLOY_RELEASE_SHA=${EXPECTED_SHA}" "${ROOT_DIR}/.env" \
  || fail "PLOY_RELEASE_SHA is not bound to the immutable release"
grep -Fxq "PLOY_LIVE_APPROVAL_FILE=${ROOT_DIR}/data/live-approvals/pending.json" "${ROOT_DIR}/.env" \
  || fail "runtime live-approval enforcement is not configured"
grep -Fxq "PLOY_ACCOUNT_OPS_WRITE_ENABLED=false" "${ROOT_DIR}/.env" \
  || fail "account-ops must remain write-disabled after deploy"
require_unique_false() {
  local key="$1"
  [[ "$(grep -cE "^${key}=" "${ROOT_DIR}/.env")" == 1 ]] \
    || fail "${key} must appear exactly once"
  grep -Fxq "${key}=false" "${ROOT_DIR}/.env" \
    || fail "${key} must remain disabled after deploy"
}
require_unique_false PLOY_PREDICT_ACCOUNT_OPS_WRITE_ENABLED
require_unique_false PLOY_PREDICT_APPROVAL_WRITE_ENABLED
require_unique_false PLOY_PREDICT_RECONCILE_WRITE_ENABLED
[[ "$($STAT -c '%U:%G:%a' "${ROOT_DIR}/data/account-ops")" == "root:root:700" ]] \
  || fail "account-ops state directory must be root:root mode 700"
"${release_dir}/bin/node" -e "require('${release_dir}/tools/polymarket-account-ops/account_ops.js')" \
  || fail "account-ops runtime dependencies are not loadable"
"${release_dir}/bin/node" -e "require('${release_dir}/tools/predict-fun-account-ops/account_ops.js')" \
  || fail "Predict account-ops runtime dependencies are not loadable"

python3 - "$release_dir/release.json" "$EXPECTED_SHA" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
expected = sys.argv[2]
payload = json.loads(path.read_text(encoding="utf-8"))
if payload.get("git_sha") != expected:
    raise SystemExit(f"release receipt SHA mismatch: {payload.get('git_sha')} != {expected}")
if not payload.get("bundle_sha256"):
    raise SystemExit("release receipt is missing bundle_sha256")
PY

"$SYSTEMCTL" is-active --quiet ployd.service || fail "ployd.service is not active"

assert_property() {
  local property="$1"
  local expected="$2"
  local actual
  actual="$($SYSTEMCTL show -P "$property" ployd.service)"
  [[ "$actual" == "$expected" ]] || fail "ployd.service ${property}=${actual}, expected ${expected}"
}

assert_property Restart always
assert_property RestartUSec 5s
assert_property MemoryHigh 1342177280
assert_property MemoryMax 1610612736
assert_property OOMPolicy kill

if "$PGREP" -af '(^|/)cargo([[:space:]]|$)|(^|/)rustc([[:space:]]|$)' >/dev/null; then
  "$PGREP" -af '(^|/)cargo([[:space:]]|$)|(^|/)rustc([[:space:]]|$)' >&2 || true
  fail "Rust build process is running on the trade host"
fi

"$CURL" -fsS http://127.0.0.1:8081/health >/dev/null || fail "ployd health endpoint failed"
"$PLOYCTL" system status
"$PLOYCTL" system metrics
"$PLOYCTL" system alerts
"$PLOYCTL" trading status
deployments="$($PLOYCTL deployments list 2>&1)"
printf '%s\n' "$deployments"
if printf '%s\n' "$deployments" | awk \
  '$0 ~ /mode=Live/ && $1 != "pm5d.threelayer.live" && $0 !~ /lifecycle=Archived/ { found=1 } END { exit !found }'; then
  fail "unexpected non-archived live deployment exists on trade host"
fi

inspect="$($PLOYCTL deployments inspect pm5d.threelayer.live 2>&1)"
printf '%s\n' "$inspect"
case "$EXPECTED_STATE" in
  paused)
    [[ "$inspect" == *"desired=Paused"*"observed=Paused"* ]] \
      || fail "live deployment is not desired=Paused observed=Paused"
    ;;
  running)
    [[ "$inspect" == *"desired=Running"*"observed=Running"* ]] \
      || fail "live deployment is not desired=Running observed=Running"
    metrics="$($PLOYCTL system metrics 2>&1)"
    [[ "$metrics" == *"venue:venue:polymarket:healthy"* ]] \
      || fail "Polymarket venue health is not fresh"
    ;;
esac

printf 'trade-host postflight passed: sha=%s state=%s\n' "$EXPECTED_SHA" "$EXPECTED_STATE"
