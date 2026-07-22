#!/usr/bin/env bash
set -euo pipefail

event=${GITHUB_EVENT_NAME:-}
base=
head=HEAD
changed_files=
output=${GITHUB_OUTPUT:-/dev/stdout}

while (($#)); do
  case "$1" in
    --event) event=$2; shift 2 ;;
    --base) base=$2; shift 2 ;;
    --head) head=$2; shift 2 ;;
    --changed-files) changed_files=$2; shift 2 ;;
    --output) output=$2; shift 2 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

emit() {
  printf 'research_image_smoke=%s\n' "$1" >>"$output"
}

# Main pushes and manual runs retain the complete image contract.
if [[ $event != pull_request ]]; then
  emit true
  exit 0
fi

declare -a paths=()
if [[ -n $changed_files ]]; then
  while IFS= read -r path; do paths+=("$path"); done <"$changed_files"
else
  [[ -n $base ]] || { printf '%s\n' '--base is required for pull requests' >&2; exit 2; }
  while IFS= read -r -d '' path; do paths+=("$path"); done \
    < <(git diff --no-renames --name-only --diff-filter=ACMRD -z "$base...$head")
fi

for path in "${paths[@]}"; do
  case "$path" in
    rust_hft/deployment/docker/Dockerfile.research|\
    rust_hft/.dockerignore|\
    rust_hft/Cargo.toml|rust_hft/Cargo.lock|\
    rust_hft/*/Cargo.toml|rust_hft/*/Cargo.lock|\
    rust_hft/rust-toolchain*|rust_hft/*/rust-toolchain*|\
    rust_hft/.cargo/*|.cargo/*|\
    .github/workflows/ploy-ci.yml|\
    .github/scripts/select-prediction-image-smoke.sh|\
    .github/scripts/test-select-prediction-image-smoke.sh)
      emit true
      exit 0
      ;;
  esac
done

emit false
