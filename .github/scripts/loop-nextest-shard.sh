#!/usr/bin/env bash
# Run one balanced partition from the loop archive. A compile line means this
# shard did not use the archive, so the shard fails.
set -euo pipefail

repo=${GITHUB_WORKSPACE:-$(git rev-parse --show-toplevel)}
cd "$repo/rust_hft"
# shellcheck source-path=SCRIPTDIR
# shellcheck source=loop-nextest-counts.sh
source "$repo/.github/scripts/loop-nextest-counts.sh"

shard=${LOOP_SHARD:?}
archive=${LOOP_NEXTEST_ARCHIVE:?}
work=${LOOP_NEXTEST_WORK:?}
[[ $shard =~ ^[1-4]$ ]] || { printf 'invalid shard: %s\n' "$shard" >&2; exit 1; }
[[ -f $archive ]] || { printf 'missing archive: %s\n' "$archive" >&2; exit 1; }
mkdir -p "$work"
plan=${LOOP_NEXTEST_PLAN:?}
filter=$(cat "$plan/filter-$shard.txt")
config=$repo/rust_hft/research-core/.config/nextest.toml
[[ -f $config ]]

if ! CARGO_TERM_COLOR=never cargo nextest list \
  --archive-file "$archive" \
  --filterset "$filter" \
  --config-file "$config" \
  --user-config-file none \
  --message-format json >"$work/list.json" 2>"$work/list-stderr.txt"; then
  cat "$work/list-stderr.txt" >&2
  exit 1
fi
cat "$work/list-stderr.txt" >&2
if loop_nextest_stderr_recompiled "$work/list-stderr.txt"; then
  printf 'shard %s recompiled while listing the archive\n' "$shard" >&2
  exit 1
fi

start=$(date +%s)
set +e
NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 \
  CARGO_TERM_COLOR=never \
  cargo nextest run \
  --archive-file "$archive" \
  --filterset "$filter" \
  --config-file "$config" \
  --user-config-file none \
  --retries 0 \
  --no-fail-fast \
  --no-tests=pass \
  --message-format libtest-json-plus \
  >"$work/events.jsonl" 2>"$work/stderr.txt"
status=$?
set -e
seconds=$(( $(date +%s) - start ))
cat "$work/stderr.txt" >&2
recompiled=false
if loop_nextest_stderr_recompiled "$work/stderr.txt"; then
  recompiled=true
fi
loop_nextest_write_shard "$work/events.jsonl" "$work/list.json" "$work/shard-${shard}.json" \
  "$shard" "$seconds" "$status" "$recompiled"
if [[ $recompiled == true ]]; then
  printf 'shard %s recompiled from source\n' "$shard" >&2
  exit 1
fi
if [[ $status -ne 0 ]]; then
  exit "$status"
fi
