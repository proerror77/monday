#!/usr/bin/env bash
set -euo pipefail

# Pin a running process by name to a specific CPU core list
# Usage: ./scripts/pin_cores.sh <process_name> <core_list>
# Example: ./scripts/pin_cores.sh hft-live 2,3

if [[ $# -lt 2 ]]; then
  echo "Usage: $0 <process_name> <core_list>" >&2
  exit 1
fi

NAME=$1
CORES=$2

PIDS=$(pgrep -f "$NAME" || true)
if [[ -z "$PIDS" ]]; then
  echo "No process found matching: $NAME" >&2
  exit 1
fi

for pid in $PIDS; do
  echo "Pinning PID $pid to cores $CORES"
  taskset -pc "$CORES" "$pid"
done

echo "Done."
