#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workflow="$script_dir/../workflows/acr-publish.yml"
dockerfile="$script_dir/../../rust_hft/deployment/docker/Dockerfile.research"
verifier="$script_dir/verify-research-runner-binaries.sh"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

grep -Fqx '            [{repository:"research-runner",file:"rust_hft/deployment/docker/Dockerfile.research",target:"prebuilt"},' "$workflow"
grep -Fqx '  research-runner-binaries:' "$workflow"
grep -Fqx '    container: rust:1.91-bookworm' "$workflow"
grep -Fqx 'FROM debian:bookworm-slim AS runtime-base' "$dockerfile"
grep -Fqx '    needs: [selector, research-runner-binaries]' "$workflow"
grep -Fqx '      - name: Download research runner binaries' "$workflow"
grep -Fqx '          target: ${{ matrix.target }}' "$workflow"
grep -Fqx '          ../.github/scripts/verify-research-runner-binaries.sh research-bin' "$workflow"
grep -Fqx '          .github/scripts/verify-research-runner-binaries.sh rust_hft/research-bin' "$workflow"

for binary in hft-backtest alpha-harness lob-pit-materializer monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
  grep -Fq "$binary" "$workflow"
done

for binary in hft-backtest alpha-harness lob-pit-materializer monday-prediction-research monday-prediction-evaluator monday-prediction-snapshot; do
  touch "$tmp_dir/$binary"
  chmod +x "$tmp_dir/$binary"
done
"$verifier" "$tmp_dir"
rm "$tmp_dir/hft-backtest"
if "$verifier" "$tmp_dir"; then
  exit 1
fi
touch "$tmp_dir/hft-backtest"
chmod +x "$tmp_dir/hft-backtest"
touch "$tmp_dir/unexpected"
chmod +x "$tmp_dir/unexpected"
if "$verifier" "$tmp_dir"; then
  exit 1
fi

printf 'ACR research-runner prebuilt contract tests passed\n'
