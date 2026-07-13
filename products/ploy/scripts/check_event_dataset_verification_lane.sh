#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

base_ref="${1:-}"
if [[ -z "${base_ref}" ]]; then
  for candidate in origin/fix/backtest-realism origin/main; do
    if git rev-parse --verify --quiet "${candidate}" >/dev/null; then
      base_ref="${candidate}"
      break
    fi
  done
fi

if [[ -z "${base_ref}" ]]; then
  echo "FAIL: could not resolve a default base ref; pass one explicitly" >&2
  exit 1
fi

if ! git rev-parse --verify --quiet "${base_ref}" >/dev/null; then
  echo "FAIL: base ref '${base_ref}' does not exist" >&2
  exit 1
fi

range="${base_ref}...HEAD"
changed_file_list="$(mktemp)"
trap 'rm -f "${changed_file_list}"' EXIT

git diff --name-only "${range}" > "${changed_file_list}"

failures=0
while IFS= read -r path; do
  [[ -z "${path}" ]] && continue

  case "${path}" in
    apps/ploy-backtest/*|apps/ploy-replay/*|crates/ploy-feed-loaders/*|crates/ploy-strategy-bundles/*|crates/ploy-strategy-runtime/*|.github/workflows/optimize.yml|Cargo.toml)
      echo "FAIL: protected hot-path file changed: ${path}" >&2
      failures=1
      continue
      ;;
    crates/ploy-research/*|tasks/event_dataset_verification_matrix.md|scripts/check_event_dataset_verification_lane.sh|scripts/check_event_dataset_scope.sh)
      ;;
    *)
      echo "FAIL: verification lane escaped allowed scope: ${path}" >&2
      failures=1
      ;;
  esac
done < "${changed_file_list}"

if [[ "${failures}" -ne 0 ]]; then
  echo "FAIL: event dataset verification lane detected forbidden changes" >&2
  exit 1
fi

echo "PASS: verification lane stayed within event dataset scope and left optimize/backtest hot paths untouched"
