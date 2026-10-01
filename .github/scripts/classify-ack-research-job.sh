#!/usr/bin/env bash
# Metadata only. The private ACK executor must independently admit the public
# workflow run, exact source SHA and this fixed profile. This grants no authority.
set -euo pipefail

detect=false base= head= loop=false
job= selected=true research=false event= repository= head_repository=
while (($#)); do
  case "$1" in
    --detect-scope) detect=true; shift ;;
    --base) base=${2:?}; shift 2 ;;
    --head) head=${2:?}; shift 2 ;;
    --loop) loop=${2:?}; shift 2 ;;
    --job) job=${2:?}; shift 2 ;;
    --selected) selected=${2:?}; shift 2 ;;
    --research) research=${2:?}; shift 2 ;;
    --event) event=${2:?}; shift 2 ;;
    --repository) repository=${2:?}; shift 2 ;;
    --head-repository) head_repository=${2:?}; shift 2 ;;
    *) printf 'unknown ACK classification argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done
[[ $selected =~ ^(true|false)$ && $research =~ ^(true|false)$ ]] || {
  echo 'ACK job flags must be explicit true/false values' >&2; exit 2;
}
[[ $repository =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || {
  echo 'ACK classification requires the expected source repository' >&2; exit 2;
}
case "$event" in
  push|pull_request|workflow_dispatch|workflow_run|schedule) ;;
  *) echo 'unsupported ACK source event' >&2; exit 2 ;;
esac

if [[ $detect == true ]]; then
  [[ $loop =~ ^(true|false)$ ]] || exit 2
  research=$loop
  if [[ $event == schedule || $event == workflow_dispatch ]] ||
     ! git cat-file -e "${base}^{commit}" 2>/dev/null ||
     ! git cat-file -e "${head}^{commit}" 2>/dev/null; then
    research=true
  else
    changed_paths=$(mktemp)
    trap 'rm -f "$changed_paths"' EXIT
    git diff --no-renames --name-only "$base...$head" > "$changed_paths" || exit 1
    while IFS= read -r path; do
      case "$path" in
        rust_hft/alpha-harness/*|rust_hft/research-core/*|deployment/aliyun/research/*|rust_hft/tools/collector/*research*|rust_hft/tools/collector/*materializ*|rust_hft/tools/collector/*replay*|rust_hft/tools/collector/*tape-slic*|rust_hft/tools/collector/*market_tape*) research=true ;;
      esac
    done < "$changed_paths"
  fi
  if [[ -n ${GITHUB_OUTPUT:-} ]]; then
    printf 'ack_research=%s\n' "$research" >> "$GITHUB_OUTPUT"
  else
    printf 'ack_research=%s\n' "$research"
  fi
  exit 0
fi

backend=github profile=
case "$job" in
  ci/research-preflight) [[ $research == false ]] || { backend=ack; profile=ci-research-preflight; } ;;
  ci/rust) [[ $research == false ]] || { backend=ack; profile=ci-rust; } ;;
  ci/rust-fast-gates) [[ $research == false ]] || { backend=ack; profile=ci-rust-fast-gates; } ;;
  security/clippy-strict) [[ $research == false ]] || { backend=ack; profile=security-clippy-research; } ;;
  ploy/research-image-binaries) backend=ack; profile=research-image-binaries ;;
  ploy/research-image-smoke) backend=ack; profile=research-image-smoke ;;
  ploy/rust-format) backend=ack; profile=prediction-research-format ;;
  ploy/rust-research-heavy) backend=ack; profile=prediction-research-heavy ;;
  acr/research-runner-binaries) backend=ack; profile=research-release-binaries ;;
  acr/research-publish) backend=ack; profile=research-release-publish ;;
  acr/research-source-test) backend=ack; profile=research-source-test ;;
  *) printf 'unknown ACK research job: %s\n' "$job" >&2; exit 2 ;;
esac

if [[ $selected == false ]]; then
  backend=unselected profile=
elif [[ $backend == ack && $event == pull_request && $head_repository != "$repository" ]]; then
  echo 'Selected research work rejects fork or missing source-repository identity; it cannot fall back to a GitHub-hosted compiler.' >&2
  exit 1
fi
printf 'backend=%s\nprofile=%s\n' "$backend" "$profile"
