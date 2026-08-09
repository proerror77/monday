#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
monitor="$script_dir/polymarket-market-tape-canary-monitor.sh"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

# Loading the monitor exposes its pure sampling and stop-rule functions without
# starting an observation or touching systemd.
# shellcheck disable=SC1090 # Resolved from this checkout at runtime.
source "$monitor"

rollback_now() {
  printf 'ROLLBACK=%s\n' "$1"
  return 42
}

set +e
cpu_output=$(evaluate_cpu_stop 1 98)
cpu_rc=$?
set -e
[[ $cpu_rc -eq 42 ]]
[[ $cpu_output == 'ROLLBACK=host_cpu_98_gt_95' ]]
[[ $(pending_growth_stop_reason 1 2) == 'pending_continuous_growth_1_to_2' ]]

fake_bin="$tmp_dir/bin"
mkdir -p "$fake_bin"
cat >"$fake_bin/systemctl" <<'SYSTEMCTL'
#!/usr/bin/env bash
set -euo pipefail
property=
for argument in "$@"; do
  case "$argument" in
    --property=*) property=${argument#--property=} ;;
  esac
done
case "$property" in
  ActiveState) printf '%s\n' "${FAKE_ACTIVE_STATE:?}" ;;
  MainPID) printf '%s\n' "${FAKE_MAIN_PID:-0}" ;;
  Result) printf '%s\n' "${FAKE_RESULT:-success}" ;;
  *) exit 64 ;;
esac
SYSTEMCTL
cat >"$fake_bin/ps" <<'PS'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${FAKE_PS_LOG:?}"
printf '37.9\n'
PS
chmod 0755 "$fake_bin/systemctl" "$fake_bin/ps"

export FAKE_PS_LOG="$tmp_dir/ps.log"
inactive_cpu=$(PATH="$fake_bin:$PATH" FAKE_ACTIVE_STATE=inactive FAKE_RESULT=success \
  uploader_process_cpu polymarket-market-tape-upload.service)
[[ $inactive_cpu == 0 ]]
[[ ! -e $FAKE_PS_LOG ]]

active_cpu=$(PATH="$fake_bin:$PATH" FAKE_ACTIVE_STATE=activating FAKE_MAIN_PID=4242 \
  uploader_process_cpu polymarket-market-tape-upload.service)
[[ $active_cpu == 37 ]]
grep -Fxq -- '-p 4242 -o %cpu=' "$FAKE_PS_LOG"

if PATH="$fake_bin:$PATH" FAKE_ACTIVE_STATE=active FAKE_MAIN_PID=0 \
  uploader_process_cpu polymarket-market-tape-upload.service >/dev/null 2>&1; then
  printf 'active uploader without a MainPID must fail closed\n' >&2
  exit 1
fi
