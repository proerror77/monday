#!/usr/bin/env bash
set -euo pipefail

# Example: start N shards of the live app and pin each to a core via taskset
# Usage: ./scripts/start_shards_example.sh <num_shards> <config_path>

NUM_SHARDS=${1:-2}
CONFIG=${2:-config/dev/system.yaml}

echo "Starting ${NUM_SHARDS} shards using config: ${CONFIG}"

for i in $(seq 0 $((NUM_SHARDS-1))); do
  CORE=$i
  LOG="shard_${i}.log"
  echo "Launching shard ${i} on core ${CORE} -> ${LOG}"
  # NOTE: requires taskset on Linux. On macOS, consider 'cpulimit' or 'psrset'.
  taskset -c ${CORE} cargo run -p live --features "bitget,trend-strategy,metrics,infra-ipc" -- \
    --config "${CONFIG}" --exit-after-ms 0 > "${LOG}" 2>&1 &
  sleep 0.2
done

echo "Use 'pkill -f hft-live' to stop all shards."
