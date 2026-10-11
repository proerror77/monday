#!/usr/bin/env bash
# A successful no-op publisher is not a baseline. Only an authenticated marker
# after image readback proves publication. This reader does not dispatch work.
set -euo pipefail
head=${1:?current source required} output=${2:?baseline output required} mode=${3:-images}
[[ $mode == images || $mode == builds ]] || exit 2
: "${GITHUB_REPOSITORY:?}"
[[ $head =~ ^[0-9a-f]{40}$ ]]
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# Bound the complete history read, including pagination and retries. Never
# print gh stderr: debug output can contain authentication headers.
deadline=$(ruby -e 'puts Process.clock_gettime(Process::CLOCK_MONOTONIC) + 180')
read_get() {
  ruby - "$1" "$2" "$3" "$deadline" "$work" <<'RUBY'
require 'json'
require 'timeout'
endpoint, output, collection, deadline, work = ARGV
deadline = Float(deadline)
3.times do |index|
  remaining = deadline - Process.clock_gettime(Process::CLOCK_MONOTONIC)
  abort 'research baseline GET budget exhausted' unless remaining.positive?
  response = File.join(work, 'response.json')
  errors = File.join(work, 'response.stderr')
  pid = Process.spawn('gh', 'api', '--method', 'GET', '--paginate', '--slurp', endpoint,
                      out: response, err: errors, pgroup: true)
  begin
    _, status = Timeout.timeout([45, remaining].min) { Process.wait2(pid) }
    code = status.exitstatus
  rescue Timeout::Error
    begin
      Process.kill('KILL', -pid)
    rescue Errno::ESRCH
      # The process may exit between the deadline and cancellation.
    end
    Process.wait(pid) rescue Errno::ECHILD
    code = 124
  end
  if code == 0
    begin
      pages = JSON.parse(File.read(response))
      valid = pages.is_a?(Array) && !pages.empty? && pages.all? do |page|
        page.is_a?(Hash) && page[collection].is_a?(Array) &&
          page['total_count'].is_a?(Integer) && page['total_count'] >= 0
      end
      raise 'invalid pages' unless valid
      entries = pages.flat_map { |page| page.fetch(collection) }
      ids = entries.map { |entry| entry.is_a?(Hash) && entry['id'] }
      raise 'incomplete pages' unless pages.all? { |page| page['total_count'] == entries.length }
      raise 'invalid or duplicate ids' unless ids.all? { |id| id.is_a?(Integer) && id.positive? } && ids.uniq == ids
    rescue JSON::ParserError, RuntimeError
      abort "research baseline GET invalid response: resource=#{collection}"
    end
    abort 'research baseline GET budget exhausted' unless Process.clock_gettime(Process::CLOCK_MONOTONIC) < deadline
    File.rename(response, output)
    exit 0
  end
  http = File.read(errors).scan(/\(HTTP (\d{3})\)/).flatten.last
  retryable = code == 124 || %w[500 502 503 504].include?(http)
  retrying = retryable && index < 2
  warn "research baseline GET failed: resource=#{collection} attempt=#{index + 1} status=#{code == 124 ? 'timeout' : http || 'unknown'} retry=#{retrying}"
  exit 1 unless retrying
  delay = index + 1
  abort 'research baseline GET budget exhausted' unless deadline - Process.clock_gettime(Process::CLOCK_MONOTONIC) > delay
  sleep delay
end
RUBY
}
# The product catalog starts a new release contract. Older bundle markers do
# not prove publication of these products. Query its complete publication era.
migration=$(git log -1 --format=%H -G 'monday.research-products.v2' -- .github/scripts/research-release-products.json)
[[ $migration =~ ^[0-9a-f]{40}$ ]] || { echo 'research product migration is missing' >&2; exit 1; }
since=$(git show -s --format=%ct "$migration")
since=$(ruby -e 'puts Time.at(Integer(ARGV[0])).utc.strftime("%Y-%m-%dT%H:%M:%SZ")' "$since")
read_get "repos/$GITHUB_REPOSITORY/actions/workflows/acr-publish.yml/runs?branch=main&status=completed&created=%3E%3D$since&per_page=100" "$work/runs.json" workflow_runs
jq -e 'all(.[].workflow_runs[];
  (.run_attempt|type=="number" and .>0 and floor==.) and
  (.head_sha|type=="string" and test("^[0-9a-f]{40}$")) and
  (.head_branch|type=="string") and (.path|type=="string") and (.event|type=="string") and
  (.head_repository.full_name|type=="string") and (.status|type=="string") and
  (.conclusion|type=="string"))' "$work/runs.json" >/dev/null
jq -r --arg repo "$GITHUB_REPOSITORY" '
  [.[].workflow_runs[]? | select(.head_branch=="main" and .head_repository.full_name==$repo and
    .path==".github/workflows/acr-publish.yml" and (.event=="workflow_run" or .event=="workflow_dispatch") and
    .status=="completed")] | sort_by(.id) | reverse |
  .[] | [.id,.run_attempt,.head_sha,.conclusion] | @tsv' "$work/runs.json" >"$work/runs.tsv"
baseline='{"cex-runner":"BOOTSTRAP","controller":"BOOTSTRAP","prediction-runner":"BOOTSTRAP"}'
while IFS=$'\t' read -r run attempt source conclusion; do
  [[ $run =~ ^[1-9][0-9]*$ && $attempt =~ ^[1-9][0-9]*$ && $source =~ ^[0-9a-f]{40}$ ]] || exit 1
  git merge-base --is-ancestor "$migration" "$source" || continue
  for ((actual_attempt=attempt; actual_attempt>=1; actual_attempt--)); do
    read_get "repos/$GITHUB_REPOSITORY/actions/runs/$run/attempts/$actual_attempt/jobs?per_page=100" "$work/jobs.json" jobs
    jq -e --argjson run "$run" --argjson attempt "$actual_attempt" --arg source "$source" '
      all(.[].jobs[]; .run_id==$run and .run_attempt==$attempt and .head_sha==$source and
        (.name|type=="string") and (.status|type=="string"))' "$work/jobs.json" >/dev/null
    marker=none
    # Compatibility: a completed legacy full Build run also proved its images.
    # A newer image-only job cannot produce this full Build baseline.
    if [[ $conclusion == success && $actual_attempt == "$attempt" ]]; then
      marker=$(jq -er --arg source "$source" '
        [.[].jobs[]? | select(.status=="completed" and .conclusion=="success" and
          (.name | startswith("Research products published")))] |
        if length==0 then "none" elif length!=1 then error("ambiguous publication marker") else .[0].name |
          capture("^Research products published \\[(?<products>[a-z,-]+)\\] \\((?<source>[0-9a-f]{40})\\)$") |
          if .source == $source then .products else error("publication marker source mismatch") end end' "$work/jobs.json")
    fi
    if [[ $mode == images ]]; then
      delivered=$(jq -er '
        [.[].jobs[] | select(.name|startswith("Publish OCI "))] as $jobs |
        if ([$jobs[].name]|unique|length)!=($jobs|length) then error("ambiguous OCI job") else
          [$jobs[] | select(.status=="completed" and .conclusion=="success") | .name |
            if .=="Publish OCI research-runner" then "cex-runner"
            elif .=="Publish OCI campaign-cycle-controller" then "controller"
            elif .=="Publish OCI prediction-research-runner" then "prediction-runner"
            else error("unknown OCI product") end] | sort | join(",") end' "$work/jobs.json")
      [[ -z $delivered ]] || marker=$(bash "$(dirname "$0")/research-release-products.sh" merge "$marker" "$delivered")
    fi
    if [[ $marker != none ]]; then
      git cat-file -e "$source^{commit}"
      git merge-base --is-ancestor "$source" "$head" || { echo 'published research source is outside current history' >&2; exit 1; }
      normalized=$(bash "$(dirname "$0")/research-release-products.sh" normalize "$marker")
      [[ $normalized == "$marker" ]] || { echo 'noncanonical publication marker' >&2; exit 1; }
      baseline=$(jq -c --arg products "$marker" --arg source "$source" '
        reduce ($products|split(",")[]) as $product (.;
          if .[$product] == "BOOTSTRAP" then .[$product]=$source else . end)' <<<"$baseline")
      [[ $baseline == *BOOTSTRAP* ]] || break 2
    fi
  done
done <"$work/runs.tsv"
# Each product bootstraps independently until its own readback succeeds. Old
# mixed runner markers cannot prove that the split products were published.
ruby -e 'abort "research baseline GET budget exhausted" unless Process.clock_gettime(Process::CLOCK_MONOTONIC) < Float(ARGV[0])' "$deadline"
printf '%s\n' "$baseline" >"$output"
