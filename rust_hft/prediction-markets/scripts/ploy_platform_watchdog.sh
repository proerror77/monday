#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLOY_PLATFORM_WATCHDOG_ROOT="${PLOY_PLATFORM_WATCHDOG_ROOT:-$(cd "$SCRIPT_DIR/.." && pwd)}"
LOCK_FILE="${PLOY_PLATFORM_WATCHDOG_LOCK_FILE:-$PLOY_PLATFORM_WATCHDOG_ROOT/run/allow-platform-stop.lock}"
MAINTENANCE_UNIT="${PLOY_PLATFORM_WATCHDOG_MAINTENANCE_UNIT:-ploy-maintenance.service}"

log_watchdog() {
  local message="$1"
  echo "ploy_platform_watchdog: $message"
  if command -v logger >/dev/null 2>&1; then
    logger -t ploy-platform-watchdog "$message" || true
  fi
}

resolve_platform_unit() {
  if [[ -n "${PLOY_PLATFORM_WATCHDOG_UNIT:-}" ]]; then
    printf '%s\n' "$PLOY_PLATFORM_WATCHDOG_UNIT"
    return 0
  fi

  local candidates=(
    "ploy-platform.service"
    "ploy-platform-live.service"
    "ploy-crypto-live.service"
  )

  local unit
  for unit in "${candidates[@]}"; do
    if systemctl cat "$unit" >/dev/null 2>&1; then
      printf '%s\n' "$unit"
      return 0
    fi
  done

  return 1
}

main() {
  local platform_unit
  if ! platform_unit="$(resolve_platform_unit)"; then
    log_watchdog "no known platform unit installed; skipping"
    exit 0
  fi

  if [[ -f "$LOCK_FILE" ]]; then
    log_watchdog "lock file present at $LOCK_FILE; skipping restart for $platform_unit"
    exit 0
  fi

  if systemctl is-active --quiet "$MAINTENANCE_UNIT"; then
    log_watchdog "maintenance unit $MAINTENANCE_UNIT is active; skipping restart for $platform_unit"
    exit 0
  fi

  if systemctl is-active --quiet "$platform_unit"; then
    exit 0
  fi

  log_watchdog "unit $platform_unit inactive; starting"
  systemctl start "$platform_unit"
}

main "$@"
