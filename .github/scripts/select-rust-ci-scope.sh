#!/usr/bin/env bash
set -euo pipefail

event=${GITHUB_EVENT_NAME:-}
base=
head=HEAD
changed_files=
metadata=
output=${GITHUB_OUTPUT:-/dev/stdout}

while (($#)); do
  case "$1" in
    --event) event=$2; shift 2 ;;
    --base) base=$2; shift 2 ;;
    --head) head=$2; shift 2 ;;
    --changed-files) changed_files=$2; shift 2 ;;
    --metadata) metadata=$2; shift 2 ;;
    --output) output=$2; shift 2 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

loop=false
handoff=false
json=false
ondo=false
collector=false
control=false
focused=false
toolchain=false

select_all() {
  loop=true
  handoff=true
  json=true
  ondo=true
  collector=true
  control=true
  focused=true
  toolchain=true
}

emit() {
  printf '%s\n' \
    "loop=$loop" \
    "handoff=$handoff" \
    "json=$json" \
    "ondo=$ondo" \
    "collector=$collector" \
    "control=$control" \
    "focused=$focused" \
    "toolchain=$toolchain" >>"$output"
}

# Branch pushes and manual runs retain the complete build/package contract.
if [[ $event != pull_request ]]; then
  select_all
  emit
  exit 0
fi

repo_root=$(git rev-parse --show-toplevel)
declare -a paths=()
if [[ -n $changed_files ]]; then
  while IFS= read -r path; do paths+=("$path"); done <"$changed_files"
else
  [[ -n $base ]] || { printf '%s\n' '--base is required for pull requests' >&2; exit 2; }
  while IFS= read -r -d '' path; do paths+=("$path"); done \
    < <(git diff --no-renames --name-only --diff-filter=ACMRD -z "$base...$head")
fi

needs_metadata=false
for path in "${paths[@]}"; do
  case "$path" in
    rust_hft/Cargo.toml|rust_hft/Cargo.lock|rust_hft/rust-toolchain*|rust_hft/.cargo/*|.cargo/*|.github/workflows/ci.yml|.github/scripts/select-rust-ci-scope.sh|.github/scripts/test-select-rust-ci-scope.sh|.github/scripts/fixtures/rust-ci-scope/*)
      select_all
      emit
      exit 0
      ;;
    deployment/aliyun/*)
      control=true
      toolchain=true
      ;;
    rust_hft/*)
      needs_metadata=true
      ;;
  esac
done

if [[ $needs_metadata == false ]]; then
  emit
  exit 0
fi

if [[ -z $metadata ]]; then
  metadata=$(mktemp)
  trap 'rm -f "$metadata"' EXIT
  (cd "$repo_root/rust_hft" && cargo metadata --format-version 1 --no-deps --locked) >"$metadata"
fi

declare -a package_names=() package_dirs=() package_dependencies=()
while IFS=$'\t' read -r name manifest dependencies; do
  if [[ $manifest == "$repo_root/"* ]]; then manifest=${manifest#"$repo_root/"}; fi
  package_names+=("$name")
  package_dirs+=("${manifest%/Cargo.toml}")
  package_dependencies+=("$dependencies")
done < <(jq -r '.packages[] | [.name, .manifest_path, ([.dependencies[] | select(.path != null) | .name] | join(","))] | @tsv' "$metadata")

affected=$'\n'
is_affected() { [[ $affected == *$'\n'"$1"$'\n'* ]]; }
mark_affected() { is_affected "$1" || affected+="$1"$'\n'; }

for path in "${paths[@]}"; do
  [[ $path == rust_hft/* ]] || continue
  owner=
  owner_length=0
  for ((index = 0; index < ${#package_names[@]}; index++)); do
    name=${package_names[$index]}
    directory=${package_dirs[$index]}
    if [[ $path == "$directory"/* && ${#directory} -gt $owner_length ]]; then
      owner=$name
      owner_length=${#directory}
    fi
  done
  if [[ -z $owner || $owner == rust-hft-workspace ]]; then
    select_all
    emit
    exit 0
  fi
  mark_affected "$owner"
done

# Cargo.toml remains the source of truth for downstream package impact.
changed=true
while [[ $changed == true ]]; do
  changed=false
  for ((index = 0; index < ${#package_names[@]}; index++)); do
    name=${package_names[$index]}
    is_affected "$name" && continue
    dependency_list=${package_dependencies[$index]}
    [[ -n $dependency_list ]] || continue
    IFS=',' read -ra dependencies <<<"$dependency_list"
    for dependency in "${dependencies[@]}"; do
      if [[ -n $dependency ]] && is_affected "$dependency"; then
        mark_affected "$name"
        changed=true
        break
      fi
    done
  done
done

select_if_affected() {
  local flag=$1
  shift
  local package
  for package in "$@"; do
    if is_affected "$package"; then
      case "$flag" in
        loop) loop=true ;;
        handoff) handoff=true ;;
        json) json=true ;;
        ondo) ondo=true ;;
        collector) collector=true ;;
        focused) focused=true ;;
      esac
      toolchain=true
      return
    fi
  done
}

select_if_affected loop alpha-domain alpha-store alpha-engine alpha-onnx-evaluator \
  alpha-harness hft-harnessctl hft-research-ml
select_if_affected handoff hft-live
select_if_affected json hft-integration hft-data-adapter-binance hft-infra-redis hft-live
select_if_affected ondo hft-data-adapter-ondo-perps hft-execution-adapter-ondo-perps hft-live
select_if_affected collector hft-collector
select_if_affected focused hft-live hft-paper hft-all-in-one

# Collector source and its host controls are one release boundary.
if [[ $collector == true ]]; then control=true; fi

emit
