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
grep -Fxq 'IOAccounting=true' "$service"
grep -Fxq 'IOReadBandwidthMax=/data 67108864' "$service"
grep -Fxq 'OnBootSec=5min' "$timer"
grep -Fxq 'OnUnitInactiveSec=1min' "$timer"
grep -Fxq 'AccuracySec=1s' "$timer"
if grep -Eq '^On(Calendar=|ActiveSec=|UnitActiveSec=)' "$timer"; then
  printf 'market uploader timer must schedule from the prior inactive state\n' >&2
  exit 1
fi

# Capacity model, not a physical-I/O benchmark or production acceptance.
# Cold raw passes: sealed single-hour=1; fallback=2; split=3; fallback+split=4.
# Also budget three compressed-data reads at a 10% compression ratio, and
# 120s for CPU/network/other work per 4 GiB archive. Production must verify
# these bounds and actual backlog drain; no validation path is skipped.
read_bytes_per_second=$(awk '$1 == "IOReadBandwidthMax=/data" { print $2 }' "$service")
inactive_minutes=$(sed -n 's/^OnUnitInactiveSec=\([0-9][0-9]*\)min$/\1/p' "$timer")
accuracy_seconds=$(sed -n 's/^AccuracySec=\([0-9][0-9]*\)s$/\1/p' "$timer")
awk -v cap="$read_bytes_per_second" -v idle_minutes="$inactive_minutes" -v accuracy="$accuracy_seconds" '
  BEGIN {
    mib = 1048576
    archive_mib = 4096
    incoming_mibps = 7.84
    if (cap <= 0 || idle_minutes <= 0 || accuracy <= 0) exit 1
    for (raw_passes = 1; raw_passes <= 4; raw_passes++) {
      seconds = idle_minutes * 60 + accuracy + 120 + archive_mib * (raw_passes + 0.3) * mib / cap
      drain = archive_mib / seconds
      printf "archive_capacity_model raw_passes=%d drain_MiBps=%.3f incoming_MiBps=%.2f assumed_extra_s=120 assumed_compressed_ratio=0.1\n", raw_passes, drain, incoming_mibps
      if (drain < incoming_mibps * 1.10) exit 1
    }
    # Negative control: even two raw passes and zero other work cannot drain
    # the observed generation rate at the superseded 32 MiB/s + 5min policy.
    if (archive_mib / (300 + archive_mib * 2 / 32) >= incoming_mibps) exit 1
  }
'

"$script_dir/test-polymarket-market-tape-canary-monitor.sh"
