#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
RESTORE="$SCRIPT_DIR/host-rust-lob-restore.sh"
LIB="$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
[[ -x $RESTORE && -f $LIB ]]
bash -n "$RESTORE"
if MONDAY_CONTROL_PLANE_TEST=1 "$RESTORE" --controller nope >/dev/null 2>&1; then
  printf 'restore accepted a non-digest controller\n' >&2
  exit 1
fi
grep -Fq -- '--controller' "$RESTORE"
grep -Fq 'monday_active_controller_sha' "$RESTORE"
grep -Fq 'active controller link is invalid' "$RESTORE"
# shellcheck disable=SC1090
. "$LIB"
process_fixture=$(mktemp -d)
trap 'rm -rf -- "$process_fixture"' EXIT
mkdir -p "$process_fixture/proc/42"
clock_ticks=$(getconf CLK_TCK)
{
  printf '42 (fixture worker) S'
  for _ in {1..18}; do printf ' 0'; done
  printf ' %s\n' "$((clock_ticks * 25))"
} >"$process_fixture/proc/42/stat"
printf '100.000000000 0.00\n' >"$process_fixture/proc/uptime"
[[ $(monday_process_started_at_ns "$process_fixture" 42 1700000000000000000) \
  == 1699999925000000000 ]]
printf 'V2 restore contract passed\n'
