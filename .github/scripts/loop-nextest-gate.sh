#!/usr/bin/env bash
# The Monorepo CI gate already fails when a needed job fails. This check also
# rejects a skipped archive or shard when loop tests were selected, which is
# the same condition that used to run `cargo test` inside Rust Workspace.
set -euo pipefail

loop=${LOOP:-}
jobs=${SELECTED_JOBS:-}
archive=${ARCHIVE_RESULT:-missing}
shard=${SHARD_RESULT:-missing}

if [[ $jobs == *",ci/rust,"* && $loop == true ]]; then
  if [[ $archive != success || $shard != success ]]; then
    printf 'loop nextest gate rejected: archive=%s shard=%s\n' "$archive" "$shard" >&2
    exit 1
  fi
else
  case $archive in
    success|skipped) ;;
    *) printf 'loop nextest archive %s\n' "$archive" >&2; exit 1 ;;
  esac
  case $shard in
    success|skipped) ;;
    *) printf 'loop nextest shard %s\n' "$shard" >&2; exit 1 ;;
  esac
fi
