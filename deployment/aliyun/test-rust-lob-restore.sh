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
printf 'V2 restore contract passed\n'
