#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
service="$script_dir/polymarket-market-tape-upload.service"
timer="$script_dir/polymarket-market-tape-upload.timer"
cutover="$script_dir/polymarket-raw-ops-cutover.sh"
dollar='$'

grep -Fxq 'ExecStart=/usr/bin/env ZSTD_THREADS=1 /opt/monday/bin/polymarket-raw-ops upload --quote-depth-levels 0 --quote-sample-ms 0 --upload-concurrency 2' "$service"
grep -Fxq "readonly MARKET_UPLOAD_EXEC=\"/usr/bin/env ZSTD_THREADS=1 ${dollar}ACTIVE_BINARY upload --quote-depth-levels 0 --quote-sample-ms 0 --upload-concurrency 2\"" "$cutover"
grep -Fxq 'MemoryHigh=2G' "$service"
grep -Fxq 'MemoryMax=3G' "$service"
grep -Fxq 'OnBootSec=5min' "$timer"
grep -Fxq 'OnUnitInactiveSec=5min' "$timer"
if grep -Eq '^On(Calendar=|ActiveSec=|UnitActiveSec=)' "$timer"; then
  printf 'market uploader timer must schedule from the prior inactive state\n' >&2
  exit 1
fi

"$script_dir/test-polymarket-market-tape-canary-monitor.sh"
