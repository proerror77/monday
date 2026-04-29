#!/usr/bin/env bash
set -euo pipefail

# Read-only Linux latency preflight for HFT market-data staging hosts.
# This script does not change system settings.
# Set STRICT=1 to exit non-zero on warnings.

status=0

section() {
  printf '\n== %s ==\n' "$1"
}

warn() {
  printf 'WARN: %s\n' "$1" >&2
  status=1
}

ok() {
  printf 'OK: %s\n' "$1"
}

require_linux() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    printf 'ERROR: this preflight is intended for Linux hosts; current kernel is %s\n' "$(uname -s)" >&2
    exit 2
  fi
}

show_cmd() {
  local cmd="$1"
  shift
  if command -v "$cmd" >/dev/null 2>&1; then
    "$cmd" "$@" || true
  else
    warn "missing command: $cmd"
  fi
}

require_linux

section "Host"
show_cmd uname -a
show_cmd lscpu

section "NUMA"
if command -v numactl >/dev/null 2>&1; then
  numactl --hardware || true
else
  warn "numactl not installed; install numactl to verify CPU/memory node layout"
fi

section "Clock"
if command -v chronyc >/dev/null 2>&1; then
  chronyc tracking || true
else
  warn "chrony is not installed or chronyc is unavailable"
fi

section "CPU governor"
governor_files=(/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor)
if compgen -G "/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor" >/dev/null; then
  governors=$(cat "${governor_files[@]}" 2>/dev/null | sort -u | tr '\n' ' ')
  printf 'governors: %s\n' "$governors"
  if [[ "$governors" == *performance* && "$(wc -w <<<"$governors")" -eq 1 ]]; then
    ok "all visible CPU governors are performance"
  else
    warn "not all CPU governors are performance"
  fi
else
  warn "CPU governor files are not visible on this host"
fi

section "Swap"
if [[ -r /proc/swaps ]]; then
  cat /proc/swaps
  if awk 'NR > 1 { found=1 } END { exit found ? 0 : 1 }' /proc/swaps; then
    warn "swap is enabled; disable it for latency staging"
  else
    ok "swap is disabled"
  fi
fi

section "Limits"
nofile=$(ulimit -n)
printf 'ulimit -n: %s\n' "$nofile"
if [[ "$nofile" =~ ^[0-9]+$ ]] && (( nofile >= 1048576 )); then
  ok "file descriptor limit is at least 1048576"
else
  warn "file descriptor limit is below 1048576"
fi

section "Affinity tools"
for cmd in taskset chrt perf; do
  if command -v "$cmd" >/dev/null 2>&1; then
    ok "$cmd is available"
  else
    warn "$cmd is missing"
  fi
done

section "IRQ"
if command -v systemctl >/dev/null 2>&1; then
  systemctl is-active irqbalance >/dev/null 2>&1 && warn "irqbalance is active; dedicated-core staging usually disables or constrains it" || ok "irqbalance is not active"
fi
if [[ -r /proc/interrupts ]]; then
  grep -Ei 'eth|ens|enp|ena|mlx|virtio|nvme' /proc/interrupts | head -40 || true
fi

section "Recommendation"
cat <<'EOF'
For the next Bitget latency audit:
  1. Keep OS/IRQ work away from the engine core.
  2. Run the process with taskset over receiver+engine cores.
  3. Pass --engine-core for the internal engine thread.
  4. Use --busy-poll only on a dedicated core.
EOF

if [[ "${STRICT:-0}" == "1" ]]; then
  exit "$status"
fi
exit 0
