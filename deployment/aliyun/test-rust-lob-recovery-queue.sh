#!/usr/bin/env bash
# shellcheck disable=SC2016 # Contract strings must retain literal variable references.
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
projection_contract='"$ACTIVE_CONTROLLER/deployment/binance-lob-archiver-production-$MARKET.env"'
resolved_contract='secure_regular_file "$installed_env" 0'
obsolete_contract='secure_regular_file "$ENV_FILE" 0'
grep -Fq 'installed env is not the active controller projection' "$RECOVERY"
grep -Fq "$projection_contract" "$RECOVERY"
grep -Fq "$resolved_contract" "$RECOVERY"
if grep -Fq "$obsolete_contract" "$RECOVERY"; then
  printf 'recovery queue still rejects the governed environment projection symlink\n' >&2
  exit 1
fi
printf 'V2 recovery queue contract passed\n'
