#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)

assert_service_setting() {
  local unit=$1 key=$2 expected=$3
  awk -F= -v key="$key" -v expected="$expected" '
    /^\[/ { in_service = ($0 == "[Service]") }
    in_service && $1 == key {
      count++
      if ($2 != expected) invalid = 1
    }
    END {
      if (count != 1 || invalid) {
        printf "%s: expected one [Service] %s=%s\n", FILENAME, key, expected > "/dev/stderr"
        exit 1
      }
    }
  ' "$script_dir/$unit"
}

upload_units=(
  polymarket-market-tape-upload.service
  polymarket-reference-upload.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-rust-upload@.service
  binance-fee-upload.service
  bybit-options-upload.service
)

for unit in "${upload_units[@]}"; do
  assert_service_setting "$unit" CPUQuota 50%
  assert_service_setting "$unit" Nice 10
done

assert_service_setting binance-usdm-reference-upload.service CPUQuota 20%
assert_service_setting binance-usdm-reference-upload.service Nice 10

# Preserve the collector quota that exists on the base branch.
assert_service_setting polymarket-market-tape.service CPUQuota 100%

printf 'PASS: six 50%% upload limits, one 20%% reference upload limit, and existing market collector quota\n'
