#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$ROOT_DIR/scripts/tests/lib/assert.sh"

WATCHDOG_SCRIPT="$ROOT_DIR/scripts/ploy_platform_watchdog.sh"

make_stub_systemctl() {
  local stub_dir="$1"
  local log_file="$2"
  cat >"$stub_dir/systemctl" <<'SH'
#!/bin/bash
set -euo pipefail

log_file="${SYSTEMCTL_LOG:?}"
active_unit="${ACTIVE_UNIT:-}"
maintenance_active="${MAINTENANCE_ACTIVE:-0}"

echo "$*" >> "$log_file"

if [[ "$1" == "is-active" && "$2" == "--quiet" ]]; then
  unit="$3"
  if [[ "$unit" == "$active_unit" ]]; then
    exit 0
  fi
  if [[ "${MAINTENANCE_UNIT:-ploy-maintenance.service}" == "$unit" && "$maintenance_active" == "1" ]]; then
    exit 0
  fi
  exit 3
fi

if [[ "$1" == "start" ]]; then
  exit 0
fi

exit 0
SH
  chmod +x "$stub_dir/systemctl"
}

run_watchdog() {
  local scenario_name="$1"
  local active_unit="$2"
  local maintenance_active="$3"
  local lock_file="$4"

  local tmp_dir
  tmp_dir="$(mktemp -d)"
  local log_file="$tmp_dir/systemctl.log"
  : > "$log_file"
  mkdir -p "$tmp_dir/bin"
  make_stub_systemctl "$tmp_dir/bin" "$log_file"

  PATH="$tmp_dir/bin:$PATH" \
  SYSTEMCTL_LOG="$log_file" \
  ACTIVE_UNIT="$active_unit" \
  MAINTENANCE_ACTIVE="$maintenance_active" \
  MAINTENANCE_UNIT="ploy-maintenance.service" \
  PLOY_PLATFORM_WATCHDOG_UNIT="ploy-platform.service" \
  PLOY_PLATFORM_WATCHDOG_LOCK_FILE="$lock_file" \
    bash "$WATCHDOG_SCRIPT" >/dev/null

  echo "$log_file"
}

main() {
  local tmp_root
  tmp_root="$(mktemp -d)"
  trap "rm -rf '$tmp_root'" EXIT

  local log_file

  log_file="$(run_watchdog "inactive" "" "0" "$tmp_root/no-lock")"
  assert_file_contains "$log_file" "start ploy-platform.service"

  touch "$tmp_root/allow-stop.lock"
  log_file="$(run_watchdog "locked" "" "0" "$tmp_root/allow-stop.lock")"
  assert_file_not_contains "$log_file" "start ploy-platform.service"

  log_file="$(run_watchdog "maintenance" "" "1" "$tmp_root/no-lock")"
  assert_file_not_contains "$log_file" "start ploy-platform.service"

  log_file="$(run_watchdog "active" "ploy-platform.service" "0" "$tmp_root/no-lock")"
  assert_file_not_contains "$log_file" "start ploy-platform.service"

  echo "ok"
}

main "$@"
