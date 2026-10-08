#!/usr/bin/env bash
# Invoke only explicitly selected owners. Feature matrices use their own manifest.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
command=${1:?Cargo subcommand required}; shift
layout=${MONDAY_CARGO_TARGET_LAYOUT:-default}
[[ $layout == default || $layout == owning-workspace-v1 ]] || {
  echo 'unsupported scoped Cargo target layout' >&2; exit 2;
}
case "$command" in build|check|test|clippy|fmt) ;; *) echo 'unsupported scoped Cargo subcommand' >&2; exit 2 ;; esac
packages=() args=() features=false targets=false
while (($#)); do
  case "$1" in
    -p|--package) packages+=("${2:?package required}"); shift 2 ;;
    --package=*) packages+=("${1#*=}"); shift ;;
    --manifest-path|--manifest-path=*|--workspace) echo 'scoped Cargo requires explicit package owners' >&2; exit 2 ;;
    --features|--features=*|--all-features) features=true; args+=("$1"); shift ;;
    --bin|--bin=*|--test|--test=*|--example|--example=*|--bench|--bench=*) targets=true; args+=("$1"); shift ;;
    --) args+=("$@"); break ;;
    *) args+=("$1"); shift ;;
  esac
done
((${#packages[@]})) || { echo 'no packages selected' >&2; exit 2; }
for package in "${packages[@]}"; do
  [[ $package =~ ^[A-Za-z0-9_-]+$ ]] || { echo 'invalid package name' >&2; exit 2; }
done
metadata=$("$root/scripts/workspace-metadata.sh")
selected=$(printf '%s\n' "${packages[@]}" | jq -Rsc 'split("\n") | map(select(length>0)) | unique')
plans=$(jq -ce --argjson selected "$selected" '
  . as $metadata |
  if any($selected[]; . as $name | [$metadata.packages[] | select(.name==$name)] | length!=1)
  then error("selected package has no unique registered owner") else
    [.packages[] | select(.name as $name | $selected | index($name))] |
    group_by(.workspace_manifest) | map({manifest:.[0].workspace_manifest,packages:map(.name)})
  end' <<<"$metadata")
if [[ $(jq length <<<"$plans") -gt 1 && ( $features == true || $targets == true ) ]]; then
  echo 'cross-workspace feature or named-target matrix requires explicit owning manifests' >&2; exit 2
fi
while IFS= read -r plan; do
  manifest=$(jq -r .manifest <<<"$plan")
  [[ $manifest =~ ^([a-z-]+/)+Cargo\.toml$ ]] || exit 2
  owned=()
  while IFS= read -r package; do owned+=(-p "$package"); done < <(jq -r '.packages[]' <<<"$plan")
  if [[ ${MONDAY_CARGO_DRY_RUN:-0} == 1 ]]; then
    jq -cn --args '$ARGS.positional' -- cargo "$command" --manifest-path "$root/$manifest" "${owned[@]}" "${args[@]}"
  else
    if [[ $layout == owning-workspace-v1 ]]; then
      # The collector contract also builds directly into the root target.
      # Other owners use disjoint directories, with one cache cleaner each.
      target="$root/${manifest%/Cargo.toml}/target"
      [[ $manifest != data-pipelines/Cargo.toml ]] || target="$root/target"
      CARGO_TARGET_DIR="$target" cargo "$command" --manifest-path "$root/$manifest" "${owned[@]}" "${args[@]}"
    else
      cargo "$command" --manifest-path "$root/$manifest" "${owned[@]}" "${args[@]}"
    fi
  fi
done < <(jq -c '.[]' <<<"$plans")
