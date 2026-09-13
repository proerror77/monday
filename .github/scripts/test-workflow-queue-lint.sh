#!/usr/bin/env bash
set -euo pipefail
linter=${1:?expected actionlint executable}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/.github/workflows"
workflow="$work/.github/workflows/acr-publish.yml"
# Keep this fixture focused: the wrapper must allow valid queue syntax without
# losing ordinary actionlint failures elsewhere in the same workflow.
cat > "$work/base.yml" <<'YAML'
name: queue contract
on: workflow_dispatch
concurrency:
  group: publication
  queue: max
  cancel-in-progress: false
jobs:
  verify:
    runs-on: ubuntu-latest
    steps:
      - run: echo verified
YAML
cp "$work/base.yml" "$workflow"
"$script_dir/lint-workflow-files.sh" "$linter" "$workflow"
reject() {
  if "$script_dir/lint-workflow-files.sh" "$linter" "$1" > "$work/error" 2>&1; then
    printf 'invalid queue/workflow unexpectedly passed: %s\n' "$2" >&2
    exit 1
  fi
}
sed 's/queue: max/queue: typo/' "$work/base.yml" > "$workflow"
reject "$workflow" invalid-queue
sed 's/cancel-in-progress: false/cancel-in-progress: true/' "$work/base.yml" > "$workflow"
reject "$workflow" conflicting-cancellation
sed '/  queue: max/a\
  unrelated-field: true
' "$work/base.yml" > "$workflow"
reject "$workflow" unrelated-concurrency-field
cp "$work/base.yml" "$workflow"
printf 'unrelated-workflow-field: true\n' >> "$workflow"
reject "$workflow" unrelated-workflow-field
sed '/    runs-on: ubuntu-latest/a\
    concurrency:\
      group: nested\
      queue: typo
' "$work/base.yml" > "$workflow"
reject "$workflow" nested-queue
cp "$work/base.yml" "$work/.github/workflows/other.yml"
reject "$work/.github/workflows/other.yml" exception-in-unrelated-workflow
# Ordinary workflows remain supported without a queue exception.
sed '/  queue: max/d' "$work/base.yml" > "$work/.github/workflows/other.yml"
"$script_dir/lint-workflow-files.sh" "$linter" "$work/.github/workflows/other.yml"
printf 'native queue field and retained workflow lint tests passed\n'
