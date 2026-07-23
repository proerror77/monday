#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workflow="$script_dir/../workflows/acr-publish.yml"

grep -Fqx '            [{repository:"research-runner",file:"rust_hft/deployment/docker/Dockerfile.research",target:"prebuilt"},' "$workflow"
grep -Fqx '          target: ${{ matrix.target }}' "$workflow"
grep -Fqx '            test -x "research-bin/$binary"' "$workflow"

for binary in hft-backtest alpha-harness lob-pit-materializer monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
  grep -Fq "$binary" "$workflow"
done

printf 'ACR research-runner prebuilt contract tests passed\n'
