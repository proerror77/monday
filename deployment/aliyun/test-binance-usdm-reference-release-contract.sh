#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(git -C "$script_dir" rev-parse --show-toplevel)
dockerfile="$repo_root/rust_hft/deployment/docker/Dockerfile.binance-lob-archiver"
workflow="$repo_root/.github/workflows/acr-publish.yml"

grep -F -- '--bin binance-usdm-reference-collector' "$dockerfile" >/dev/null
grep -F -- '/out/bin/binance-usdm-reference-collector' "$dockerfile" >/dev/null
grep -F -- '/usr/local/bin/binance-usdm-reference-collector' "$dockerfile" >/dev/null
grep -F -- 'artifact/binance-usdm-reference-collector' "$workflow" >/dev/null
grep -F -- 'binance-usdm-reference-collector.sha256' "$workflow" >/dev/null
grep -F -- 'monday.binance_usdm_reference_release.v1' "$workflow" >/dev/null
grep -F -- 'binance-usdm-reference-release.json.sha256' "$workflow" >/dev/null
grep -F -- '--bin binance-usdm-reference-artifact-verifier' "$dockerfile" >/dev/null
grep -F -- '/usr/local/bin/binance-usdm-reference-artifact-verifier' "$dockerfile" >/dev/null
grep -F -- 'artifact/binance-usdm-reference-artifact-verifier' "$workflow" >/dev/null
grep -F -- 'binance-usdm-reference-artifact-verifier.sha256' "$workflow" >/dev/null
grep -F -- 'verifier:{file:"binance-usdm-reference-artifact-verifier"' "$workflow" >/dev/null
grep -F -- 'binance-usdm-reference-shadow-gate.sh' "$workflow" >/dev/null
grep -F -- '--bin binance-usdm-reference-upload' "$dockerfile" >/dev/null
grep -F -- '/out/bin/binance-usdm-reference-upload' "$dockerfile" >/dev/null
grep -F -- '/usr/local/bin/binance-usdm-reference-upload' "$dockerfile" >/dev/null
grep -F -- 'artifact/binance-usdm-reference-upload' "$workflow" >/dev/null
grep -F -- 'binance-usdm-reference-upload.sha256' "$workflow" >/dev/null
grep -F -- 'binance-usdm-reference-production-control-assets.sha256' "$workflow" >/dev/null
grep -F -- 'binance-usdm-reference-production-control.tar.gz' "$workflow" >/dev/null
grep -F -- 'binance-usdm-reference-production-control.tar.gz.sha256' "$workflow" >/dev/null

printf '%s\n' 'Binance USD-M reference release contract tests passed'
