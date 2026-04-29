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
RECEIVER_CORE="${RECEIVER_CORE:-1}"
ENGINE_CORE="${ENGINE_CORE:-2}"
SPIN_POLLS="${SPIN_POLLS:-256}"
BUSY_POLL="${BUSY_POLL:-1}"
RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native}"
RUN_ID="${RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-bitget-${SYMBOL}}"
RUN_DIR="${RUN_DIR:-target/latency-audit/${RUN_ID}}"
export RUSTFLAGS

mkdir -p "$RUN_DIR"

echo "Building Bitget latency audit with RUSTFLAGS=${RUSTFLAGS}"
cargo build -p hft-data-adapter-bitget --example latency_audit --release --locked

{
  echo "run_id=${RUN_ID}"
  echo "run_dir=${RUN_DIR}"
  echo "git_commit=$(git rev-parse HEAD 2>/dev/null || true)"
  echo "git_status=$(git status --short 2>/dev/null | wc -l | tr -d ' ') changed paths"
  echo "date_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "uname=$(uname -a)"
  echo "rustc=$(rustc -Vv | tr '\n' '; ')"
  echo "cargo=$(cargo -V)"
  echo "rustflags=${RUSTFLAGS}"
  echo "process_cores=${PROCESS_CORES}"
  echo "receiver_core=${RECEIVER_CORE}"
  echo "engine_core=${ENGINE_CORE}"
  echo "symbol=${SYMBOL}"
  echo "depth_channel=${DEPTH_CHANNEL}"
  echo "queue_capacity=${QUEUE_CAPACITY}"
  echo "max_messages=${MAX_MESSAGES}"
  echo "max_runtime_secs=${MAX_RUNTIME_SECS}"
  echo "spin_polls=${SPIN_POLLS}"
  echo "busy_poll=${BUSY_POLL}"
  if command -v lscpu >/dev/null 2>&1; then
    echo
    echo "[lscpu]"
    lscpu
  fi
  if command -v chronyc >/dev/null 2>&1; then
    echo
    echo "[chronyc tracking]"
    chronyc tracking || true
  fi
  if compgen -G "/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor" >/dev/null; then
    echo
    echo "[cpu governors]"
    cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor | sort | uniq -c
  fi
} > "${RUN_DIR}/metadata.txt"

args=(
  --symbol "$SYMBOL"
  --depth-channel "$DEPTH_CHANNEL"
  --queue-capacity "$QUEUE_CAPACITY"
  --max-messages "$MAX_MESSAGES"
  --max-runtime-secs "$MAX_RUNTIME_SECS"
  --spin-polls "$SPIN_POLLS"
  --receiver-core "$RECEIVER_CORE"
  --engine-core "$ENGINE_CORE"
  --json-out "${RUN_DIR}/summary.json"
)

if [[ "$BUSY_POLL" == "1" || "$BUSY_POLL" == "true" || "$BUSY_POLL" == "yes" ]]; then
  args+=(--busy-poll)
fi

echo "Running Bitget latency audit"
echo "  process cores: ${PROCESS_CORES}"
echo "  receiver core: ${RECEIVER_CORE}"
echo "  engine core:   ${ENGINE_CORE}"
echo "  symbol:        ${SYMBOL}"
echo "  depth channel: ${DEPTH_CHANNEL}"
echo "  busy poll:     ${BUSY_POLL}"
echo "  run dir:       ${RUN_DIR}"

if [[ "$RECEIVER_CORE" == "$ENGINE_CORE" ]]; then
  echo "WARN: RECEIVER_CORE and ENGINE_CORE are the same; p99/p999 may include self-inflicted contention" >&2
fi

set +e
taskset -c "$PROCESS_CORES" target/release/examples/latency_audit "${args[@]}" 2>&1 | tee "${RUN_DIR}/stdout.log"
status=${PIPESTATUS[0]}
set -e

echo "Artifacts written to ${RUN_DIR}"
exit "$status"
