#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
service="$script_dir/polymarket-market-tape-upload.service"
timer="$script_dir/polymarket-market-tape-upload.timer"
cutover="$script_dir/polymarket-raw-ops-cutover.sh"
dollar='$'
calendar='*-*-* *:05,10,15,20,25,30,35,40,45,50,55:00 UTC'

grep -Fxq 'ExecStart=/usr/bin/env ZSTD_THREADS=1 /opt/monday/bin/polymarket-raw-ops upload --quote-depth-levels 0 --quote-sample-ms 0 --upload-concurrency 1' "$service"
grep -Fxq "readonly MARKET_UPLOAD_EXEC=\"/usr/bin/env ZSTD_THREADS=1 ${dollar}ACTIVE_BINARY upload --quote-depth-levels 0 --quote-sample-ms 0 --upload-concurrency 1\"" "$cutover"
grep -Fxq "OnCalendar=$calendar" "$timer"
if grep -Eq '^On(Unit)?ActiveSec=' "$timer"; then
  printf 'market uploader timer must not use a relative activation schedule\n' >&2
  exit 1
fi
grep -Fxq 'Persistent=true' "$timer"

boundary=$(systemd-analyze calendar --base-time='2026-08-09 10:59:59 UTC' "$calendar")
grep -Fq '2026-08-09 11:05:00 UTC' <<<"$boundary"
ordinary=$(systemd-analyze calendar --base-time='2026-08-09 10:06:00 UTC' "$calendar")
grep -Fq '2026-08-09 10:10:00 UTC' <<<"$ordinary"

"$script_dir/test-polymarket-market-tape-canary-monitor.sh"
