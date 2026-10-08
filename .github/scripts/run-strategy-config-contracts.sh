#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
event=${GITHUB_EVENT_NAME:-}
base=
head=HEAD
changed_files=
while (($#)); do
  case "$1" in
    --event) event=$2; shift 2 ;;
    --base) base=$2; shift 2 ;;
    --head) head=$2; shift 2 ;;
    --changed-files) changed_files=$2; shift 2 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT
declare -a paths=()
if [[ -n $changed_files ]]; then
  while IFS= read -r path || [[ -n $path ]]; do paths+=("$path"); done <"$changed_files"
elif [[ $event != workflow_dispatch ]]; then
  [[ -n $base && -n $head ]] || { echo 'configuration validation requires base and head' >&2; exit 2; }
  # A failed diff must stop validation. Deleted optional files have no new
  # contents; the named configuration-family tests still check required files.
  git -C "$repo_root" diff --no-renames --name-only --diff-filter=ACMRT -z "$base...$head" >"$tmp_dir/changed"
  while IFS= read -r -d '' path; do paths+=("$path"); done <"$tmp_dir/changed"
fi

selected='[]'
for path in "${paths[@]}"; do
  case "$path" in
    rust_hft/prediction-markets/config/default.toml) ;;
    rust_hft/prediction-markets/config/strategies/*.toml)
      name=${path#rust_hft/prediction-markets/config/strategies/}
      [[ $name =~ ^[A-Za-z0-9][A-Za-z0-9._-]*\.toml$ ]] || {
        printf 'unsupported strategy configuration path: %s\n' "$path" >&2; exit 2;
      }
      ;;
    *) continue ;;
  esac
  [[ -f $repo_root/$path && ! -L $repo_root/$path ]] || {
    printf 'configuration is missing or is not a regular file: %s\n' "$path" >&2; exit 2;
  }
  relative=${path#rust_hft/prediction-markets/}
  selected=$(jq -cn --argjson prior "$selected" --arg path "$relative" '$prior + [$path] | unique')
done

export MONDAY_STRATEGY_CONFIG_FILES_JSON=$selected
cd "$repo_root/rust_hft/prediction-markets"
printf 'Configuration files: %s\n' "$selected"
# This filter runs parsing and configuration invariants. It starts no runtime,
# feed, database, training, or order execution process.
cargo test --locked -p ploy-strategy-bundles --no-default-features --lib 'config::tests::'
