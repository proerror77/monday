#!/usr/bin/env bash
set -euo pipefail

event=
conclusion=
source_event=
head_branch=
head_sha=
run_id=
binaries_conclusion=
smoke_conclusion=
target=
rebuild=false
current_sha=
current_run_id=
output=${GITHUB_OUTPUT:-/dev/stdout}

while (($#)); do
  case "$1" in
    --event) event=$2; shift 2 ;;
    --conclusion) conclusion=$2; shift 2 ;;
    --source-event) source_event=$2; shift 2 ;;
    --head-branch) head_branch=$2; shift 2 ;;
    --head-sha) head_sha=$2; shift 2 ;;
    --run-id) run_id=$2; shift 2 ;;
    --binaries-conclusion) binaries_conclusion=$2; shift 2 ;;
    --smoke-conclusion) smoke_conclusion=$2; shift 2 ;;
    --target) target=$2; shift 2 ;;
    --rebuild) rebuild=$2; shift 2 ;;
    --current-sha) current_sha=$2; shift 2 ;;
    --current-run-id) current_run_id=$2; shift 2 ;;
    --output) output=$2; shift 2 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
  esac
done

case "$event" in
  workflow_run)
    [[ $conclusion == success && $source_event == push && $head_branch == main ]] || {
      printf 'automated ACR publication requires a successful main push run\n' >&2
      exit 1
    }
    case "$binaries_conclusion/$smoke_conclusion" in
      success/success) publish_target=research-runner; research_mode=artifact ;;
      skipped/skipped) publish_target=none; research_mode=none ;;
      *) printf 'incomplete research image validation: binaries=%s smoke=%s\n' \
           "$binaries_conclusion" "$smoke_conclusion" >&2; exit 1 ;;
    esac
    source_sha=$head_sha
    artifact_run_id=$run_id
    ;;
  workflow_dispatch)
    case "$target" in
      all|research-runner|hft-trading|binance-lob-archiver|polymarket-evidence-compiler|polymarket-market-recorder) ;;
      *) printf 'unsupported publish target: %s\n' "$target" >&2; exit 1 ;;
    esac
    if [[ $target == all || $target == research-runner ]]; then
      [[ $rebuild == true ]] || {
        printf 'manual research publication requires rebuild_research_runner=true\n' >&2
        exit 1
      }
      research_mode=rebuild
    else
      research_mode=none
    fi
    publish_target=$target
    source_sha=$current_sha
    artifact_run_id=$current_run_id
    ;;
  *) printf 'unsupported event: %s\n' "$event" >&2; exit 1 ;;
esac

[[ $source_sha =~ ^[0-9a-f]{40}$ ]] || { printf 'invalid source SHA: %s\n' "$source_sha" >&2; exit 1; }
[[ $artifact_run_id =~ ^[1-9][0-9]*$ ]] || { printf 'invalid artifact run id: %s\n' "$artifact_run_id" >&2; exit 1; }
printf '%s\n' \
  "publish_target=$publish_target" \
  "research_mode=$research_mode" \
  "source_sha=$source_sha" \
  "artifact_run_id=$artifact_run_id" >>"$output"
