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
changed_files="$(git diff --name-only "${range}")"

failures=()
allowed_paths=(
  "crates/ploy-research/"
  "scripts/check_event_dataset_scope.sh"
  "scripts/check_event_dataset_verification_lane.sh"
  "tasks/event_dataset_verification_matrix.md"
)
protected_prefixes=(
  "apps/ploy-backtest/"
  "apps/ploy-replay/"
  "crates/ploy-feed-loaders/"
  "crates/ploy-strategy-bundles/"
  "crates/ploy-strategy-runtime/"
)
protected_files=(
  ".github/workflows/optimize.yml"
  "Cargo.toml"
)

is_allowed() {
  local path="$1"
  local allowed
  for allowed in "${allowed_paths[@]}"; do
    if [[ "${allowed}" == */ ]]; then
      [[ "${path}" == "${allowed}"* ]] && return 0
    else
      [[ "${path}" == "${allowed}" ]] && return 0
    fi
  done
  return 1
}

while IFS= read -r path; do
  [[ -z "${path}" ]] && continue

  local_hit=0
  for prefix in "${protected_prefixes[@]}"; do
    if [[ "${path}" == "${prefix}"* ]]; then
      failures+=("protected hot-path file changed: ${path}")
      local_hit=1
      break
    fi
  done
  if [[ "${local_hit}" -eq 1 ]]; then
    continue
  fi

  for exact in "${protected_files[@]}"; do
    if [[ "${path}" == "${exact}" ]]; then
      failures+=("protected hot-path file changed: ${path}")
      local_hit=1
      break
    fi
  done
  if [[ "${local_hit}" -eq 1 ]]; then
    continue
  fi

  if ! is_allowed "${path}"; then
    failures+=("event dataset slice escaped crates/ploy-research: ${path}")
  fi
done <<< "${changed_files}"

if git diff --quiet "${range}" -- crates/ploy-research/Cargo.toml; then
  :
else
  if git diff --unified=0 "${range}" -- crates/ploy-research/Cargo.toml | grep -Eq '^\+.*polars-export *= *\[.*\]|^\+.*default *= .*polars-export|^\+.*full *= .*polars-export'; then
    failures+=("ploy-research/Cargo.toml now routes dataset work through polars-export defaults")
  fi
fi

if [[ "${#failures[@]}" -gt 0 ]]; then
  echo "FAIL: event dataset scope guard rejected hot-path changes"
  printf ' - %s\n' "${failures[@]}"
  exit 1
fi

echo "PASS: event dataset slice stays in crates/ploy-research and leaves optimize/backtest hot paths untouched"
