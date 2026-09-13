#!/usr/bin/env bash
# Keep native queue validation narrow until released actionlint understands it.
set -euo pipefail
linter=${1:?expected actionlint executable}
shift
(($#)) || { echo 'expected workflow files' >&2; exit 1; }
for workflow; do
  case "$workflow" in
    .github/workflows/acr-publish.yml|*/.github/workflows/acr-publish.yml)
      # GitHub supports queue:max (100 pending), but actionlint through v1.7.12
      # parse.go:809-831 rejects that field. Check this exact usage ourselves;
      # suppress only its one known parser message, never another lint error.
      # https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency
      ruby -ryaml - "$workflow" <<'RUBY'
document = YAML.safe_load(File.read(ARGV.fetch(0)))
concurrency = document.fetch('concurrency')
abort 'ACR concurrency requires queue:max' unless concurrency.is_a?(Hash) && concurrency['queue'] == 'max'
abort 'queue:max requires cancel-in-progress:false' unless concurrency['cancel-in-progress'] == false
(document['jobs'] || {}).each_value do |job|
  next unless job.is_a?(Hash) && job['concurrency'].is_a?(Hash)
  abort 'queue exception is limited to ACR workflow concurrency' if job['concurrency'].key?('queue')
end
RUBY
      "$linter" -color -ignore '^unexpected key "queue" for "concurrency" section\. expected one of "cancel-in-progress", "group"$' "$workflow"
      ;;
    *) "$linter" -color "$workflow" ;;
  esac
done
