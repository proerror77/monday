#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
RECOVERY="$SCRIPT_DIR/host-rust-lob-recovery-queue.sh"
[[ -x $RECOVERY ]]
bash -n "$RECOVERY"
if "$RECOVERY" obsolete spot >/dev/null 2>&1; then
  printf 'recovery queue retained an unknown action\n' >&2
  exit 1
fi
grep -Fq "active V2 controller is required" "$RECOVERY"
grep -Fq 'monday.rust_lob_controller_release.v2' "$RECOVERY"
grep -Fq 'monday.rust_lob_controller_release.v2' "$RECOVERY"
printf 'V2 recovery queue contract passed\n'
