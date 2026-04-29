#!/usr/bin/env bash
set -euo pipefail

# Build and run the Bitget latency audit on a Linux staging host.
# Defaults assume core 1 for WebSocket/runtime and core 2 for the engine thread.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "ERROR: this runner is intended for Linux hosts; current kernel is $(uname -s)" >&2
  exit 2
fi
if ! command -v taskset >/dev/null 2>&1; then
  echo "ERROR: taskset is required; install util-linux" >&2
  exit 2
fi

SYMBOL="${SYMBOL:-BTCUSDT}"
DEPTH_CHANNEL="${DEPTH_CHANNEL:-books1}"
QUEUE_CAPACITY="${QUEUE_CAPACITY:-1024}"
MAX_MESSAGES="${MAX_MESSAGES:-5000}"
MAX_RUNTIME_SECS="${MAX_RUNTIME_SECS:-300}"
PROCESS_CORES="${PROCESS_CORES:-1,2}"
ENGINE_CORE="${ENGINE_CORE:-2}"
SPIN_POLLS="${SPIN_POLLS:-256}"
BUSY_POLL="${BUSY_POLL:-1}"
RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native}"
export RUSTFLAGS

echo "Building Bitget latency audit with RUSTFLAGS=${RUSTFLAGS}"
cargo build -p hft-data-adapter-bitget --example latency_audit --release --locked

args=(
  --symbol "$SYMBOL"
  --depth-channel "$DEPTH_CHANNEL"
  --queue-capacity "$QUEUE_CAPACITY"
  --max-messages "$MAX_MESSAGES"
  --max-runtime-secs "$MAX_RUNTIME_SECS"
  --spin-polls "$SPIN_POLLS"
  --engine-core "$ENGINE_CORE"
)

if [[ "$BUSY_POLL" == "1" || "$BUSY_POLL" == "true" || "$BUSY_POLL" == "yes" ]]; then
  args+=(--busy-poll)
fi

echo "Running Bitget latency audit"
echo "  process cores: ${PROCESS_CORES}"
echo "  engine core:   ${ENGINE_CORE}"
echo "  symbol:        ${SYMBOL}"
echo "  depth channel: ${DEPTH_CHANNEL}"
echo "  busy poll:     ${BUSY_POLL}"

exec taskset -c "$PROCESS_CORES" target/release/examples/latency_audit "${args[@]}"
