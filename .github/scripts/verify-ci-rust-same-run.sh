#!/usr/bin/env bash
# `needs.rust` is the same workflow's successful producer, never a polled run.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/verify-ci-rust-evidence.sh"
receipt=${1:?receipt file required}
selected=${2:?selected scope required}
ci_verify_stages "$receipt" "$selected"
command_sha=$(ci_rust_command_sha)
checks=$(jq -c --slurpfile event "$GITHUB_EVENT_PATH" --arg repository "$GITHUB_REPOSITORY" \
  --arg checkout "$(git rev-parse HEAD)" --arg run "$GITHUB_RUN_ID" \
  --argjson attempt "$GITHUB_RUN_ATTEMPT" --arg command "$command_sha" '
  $event[0] as $e | {
    repository:(.repository==$repository),
    source:(.source_repository==($e.pull_request.head.repo.full_name // $e.repository.full_name)),
    head:(.head_sha==($e.pull_request.head.sha // $e.after // env.GITHUB_SHA)),
    base:(.base_sha==($e.pull_request.base.sha // $e.before // env.GITHUB_SHA)),
    event:(.event==env.GITHUB_EVENT_NAME),checkout:(.checkout_sha==$checkout),
    run:(.run_id==$run),attempt:(.run_attempt==$attempt),
    producer_job:(.job_id|type=="number" and .>0 and floor==.),
    command:(.command_sha256==$command)
  }' "$receipt")
ci_assert_checks "$checks" 'same-run Clippy producer'
[[ ${RUST_JOB_RESULT:?} == success ]] || { echo 'same-run Rust producer did not pass' >&2; exit 1; }
