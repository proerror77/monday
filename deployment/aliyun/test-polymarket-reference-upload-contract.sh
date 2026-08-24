#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
service="$script_dir/polymarket-reference-upload.service"
cutover="$script_dir/polymarket-raw-ops-cutover.sh"
dollar='$'

grep -Fxq 'ExecStart=/usr/bin/env ZSTD_THREADS=1 /opt/monday/bin/polymarket-raw-ops upload --spool-dir /data/monday/spool/polymarket-reference --dataset crypto_expiry_reference --quote-depth-levels 0 --quote-sample-ms 0 --upload-concurrency 1' "$service"
grep -Fxq "readonly REFERENCE_UPLOAD_EXEC=\"/usr/bin/env ZSTD_THREADS=1 ${dollar}ACTIVE_BINARY upload --spool-dir /data/monday/spool/polymarket-reference --dataset crypto_expiry_reference --quote-depth-levels 0 --quote-sample-ms 0 --upload-concurrency 1\"" "$cutover"
grep -Fxq 'CPUQuota=100%' "$service"
grep -Fxq 'Nice=10' "$service"
grep -Fxq 'MemoryHigh=1G' "$service"
grep -Fxq 'MemoryMax=1536M' "$service"
grep -Fxq 'TimeoutStartSec=0' "$service"
