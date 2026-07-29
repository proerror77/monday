#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
auditor="$repo_root/.github/scripts/issue-lifecycle-audit.rb"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

ruby -rjson - "$tmp_dir/cases.json" <<'RUBY'
def issue_body(parent = "None", blockers = "None", extra = "")
  "## Parent\n\n#{parent}\n\n## Blocked by\n\n#{blockers}\n#{extra}"
end

def issue(number, labels, body = issue_body, assignees = [], parent = nil, blocked_by = [])
  { "number" => number, "title" => "Fixture issue #{number}", "body" => body,
    "labels" => labels, "assignees" => assignees, "parent" => parent,
    "blocked_by" => blocked_by }
end

def pull_request(number, relationship, base = "main", commits = [], extra = "")
  { "number" => number, "base" => base,
    "body" => "## Issue relationship\n\n#{relationship}\n\n## Focused validation\n\nFixture proof.\n#{extra}\n",
    "commits" => commits }
end

def data(issues, pull_requests = [], automatic_close = true)
  { "default_branch" => "main", "automatic_linked_issue_closing" => automatic_close,
    "issues" => issues, "pull_requests" => pull_requests }
end

base = issue(10, %w[enhancement ready-for-agent], issue_body, ["agent"])
runtime_control = "\n## Runtime control\n\nTarget: repository setting\nController: release-owner\nStop rule: stop on failed preflight\nRollback: restore the previous setting\n"
incomplete_commits = pull_request(32, "Refs #10", "main", [
  { "sha" => "first", "message" => "Safe fetched commit" }
])
incomplete_commits["expected_commit_count"] = 251
cases = {
  "valid_refs" => data([base], [pull_request(20, "Refs #10", "feature-stack")]),
  "valid_closes" => data([
    issue(11, %w[enhancement ready-for-agent], issue_body, ["agent"])
  ], [pull_request(22, "Closes #11", "main", [{ "sha" => "fix", "message" => "Fixes #11" }])]),
  "valid_none" => data([], [pull_request(23, "<!-- Refs #1 and Closes #1 -->\nNone")], false),
  "valid_needs_info" => data([issue(13, %w[bug needs-info])]),
  "valid_wontfix" => data([issue(14, %w[enhancement wontfix])]),
  "authorized_runtime_ready" => data([
    issue(12, %w[enhancement ready-for-agent runtime],
          issue_body("None", "#99", runtime_control), [], nil,
          [{ "number" => 99, "state" => "closed" }])
  ]),
  "missing_category" => data([issue(30, %w[ready-for-human])]),
  "conflicting_category" => data([issue(31, %w[bug enhancement ready-for-human])]),
  "missing_state" => data([issue(32, %w[enhancement])]),
  "conflicting_state" => data([issue(33, %w[enhancement needs-triage ready-for-human])]),
  "literal_escaped_newline" => data([
    issue(34, %w[enhancement ready-for-human], issue_body + "literal \\n escape")
  ]),
  "tracking_agent_queue" => data([issue(35, %w[enhancement ready-for-agent tracking])]),
  "runtime_missing_control" => data([issue(36, %w[enhancement ready-for-agent runtime])]),
  "runtime_open_blocker" => data([
    issue(37, %w[enhancement ready-for-agent runtime],
          issue_body("None", "#99", runtime_control), [], nil,
          [{ "number" => 99, "state" => "open" }])
  ]),
  "active_missing_owner" => data([
    issue(40, %w[enhancement ready-for-agent])
  ], [pull_request(140, "Refs #40")]).merge("audited_issue_numbers" => []),
  "active_multiple_owners" => data([
    issue(41, %w[enhancement ready-for-agent], issue_body, %w[first second])
  ], [pull_request(141, "Refs #41")]).merge("audited_issue_numbers" => []),
  "parent_mismatch" => data([
    issue(42, %w[enhancement ready-for-human], issue_body("#456"), [], 455)
  ]),
  "blocker_mismatch" => data([
    issue(43, %w[enhancement ready-for-human], issue_body("None", "#78"), [], nil,
          [{ "number" => 77, "state" => "open" }])
  ]),
  "invalid_multiple_relationships" => data([], [
    pull_request(21, "Refs #10\nCloses #10\nNone")
  ]),
  "negated_pr_closing" => data([base], [
    pull_request(24, "Refs #10", "main", [], "This does not close #10.")
  ]),
  "non_default_closes" => data([base], [pull_request(25, "Closes #10", "stack")]),
  "runtime_closes" => data([
    issue(50, %w[enhancement ready-for-human runtime], issue_body, ["operator"])
  ], [pull_request(26, "Closes #50")]),
  "tracking_closes" => data([
    issue(51, %w[enhancement ready-for-human tracking], issue_body, ["maintainer"])
  ], [pull_request(27, "Closes #51")]),
  "pr_fix_with_refs" => data([base], [
    pull_request(28, "Refs #10", "main", [], "Fixes #10")
  ]),
  "commit_fix_with_refs" => data([base], [
    pull_request(29, "Refs #10", "main", [
      { "sha" => "safe", "message" => "Safe first commit" },
      { "sha" => "fix-ref", "message" => "Fixes #10" }
    ])
  ]),
  "commit_negated_resolve" => data([base], [
    pull_request(30, "Closes #10", "main", [
      { "sha" => "resolve", "message" => "This does not resolve #10" }
    ])
  ]),
  "commit_never_closes" => data([base], [
    pull_request(33, "Closes #10", "main", [
      { "sha" => "never", "message" => "This never closes #10" }
    ])
  ]),
  "commit_other_issue" => data([
    base, issue(52, %w[bug ready-for-human])
  ], [pull_request(31, "Closes #10", "main", [
    { "sha" => "closed", "message" => "Closed #52" }
  ])]),
  "commit_list_incomplete" => data([base], [incomplete_commits])
}
File.write(ARGV.fetch(0), JSON.generate("cases" => cases))
RUBY

run_pass() {
  local name="$1"
  local expected="${2-}"
  local output
  output="$(ruby "$auditor" --fixture "$tmp_dir/cases.json" --case "$name")"
  grep -Fq "PASS $name" <<<"$output"
  if [[ -n "$expected" ]]; then
    grep -Fq "$expected" <<<"$output"
  fi
}

run_fail() {
  local name="$1"
  local expected="$2"
  local output audit_exit summary
  summary="$tmp_dir/$name-summary.md"
  set +e
  output="$(ruby "$auditor" --fixture "$tmp_dir/cases.json" --case "$name" --summary "$summary" 2>&1)"
  audit_exit=$?
  set -e
  test "$audit_exit" -eq 1
  grep -Fq "$expected" <<<"$output"
  grep -Fq "$expected" "$summary"
}

run_pass valid_refs
run_pass valid_closes
run_pass valid_none "automatic-linked-issue-closing: disabled (fixture)"
run_pass valid_needs_info
run_pass valid_wontfix
run_pass authorized_runtime_ready

while IFS='|' read -r name expected; do
  run_fail "$name" "$expected"
done <<'CASES'
missing_category|Issue #30: expected exactly one category label
conflicting_category|Issue #31: expected exactly one category label
missing_state|Issue #32: expected exactly one triage state label
conflicting_state|Issue #33: expected exactly one triage state label
literal_escaped_newline|Issue #34: body contains a literal escaped newline
tracking_agent_queue|Issue #35: tracking issues cannot use ready-for-agent
runtime_missing_control|Issue #36: runtime ready-for-agent is missing Runtime control
runtime_open_blocker|Issue #37: runtime ready-for-agent has open native blocker #99
active_missing_owner|Issue #40: active implementation requires exactly one assignee
active_multiple_owners|Issue #41: active implementation requires exactly one assignee
parent_mismatch|Issue #42: Parent summary references #456; native parent is #455
blocker_mismatch|Issue #43: Blocked by summary references #78; native blocked-by is #77
invalid_multiple_relationships|PR #21: expected exactly one visible issue relationship
negated_pr_closing|PR #24 body: negated closing phrase is forbidden
non_default_closes|PR #25: Closes #10 targets stack, not default branch main
runtime_closes|PR #26: runtime issue #50 cannot be closed by a pull request
tracking_closes|PR #27: tracking issue #51 cannot be closed by a pull request
pr_fix_with_refs|PR #28 body: closing keyword Fixes #10 requires visible Closes #10
commit_fix_with_refs|PR #29 commit fix-ref: closing keyword Fixes #10 requires visible Closes #10
commit_negated_resolve|PR #30 commit resolve: negated closing phrase is forbidden
commit_never_closes|PR #33 commit never: negated closing phrase is forbidden
commit_other_issue|PR #31 commit closed: closing keyword Closed #52 conflicts with visible Closes #10
commit_list_incomplete|PR #32: fetched 1 of 251 commit messages; audit cannot prove every commit safe
CASES

ruby -ryaml -e '
  workflow = YAML.load_file(ARGV.fetch(0))
  events = workflow.fetch("on")
  abort "incorrect workflow events" unless events.keys.sort == %w[pull_request schedule workflow_dispatch]
  expected_pr_types = %w[edited opened ready_for_review reopened synchronize]
  actual_pr_types = events.fetch("pull_request").fetch("types").sort
  abort "incomplete pull_request activity types" unless actual_pr_types == expected_pr_types
  abort "permissions are not read-only and minimal" unless workflow.fetch("permissions") == {
    "contents" => "read", "issues" => "read", "pull-requests" => "read"
  }
  jobs = workflow.fetch("jobs")
  abort "expected one workflow job" unless jobs.length == 1
  job = jobs.values.first
  abort "unstable check name" unless job["name"] == "Issue Lifecycle"
  checkout = job.fetch("steps").find { |step| step["uses"].to_s.start_with?("actions/checkout@") }
  abort "checkout action is not pinned" unless checkout && checkout["uses"].match?(/\Aactions\/checkout@[0-9a-f]{40}\z/)
  runs = job.fetch("steps").map { |step| step["run"] }.compact
  abort "PR event does not target one PR" unless runs.any? { |run| run.include?("--pr") }
  abort "audit does not write step summary" unless runs.all? { |run| run.include?("--summary") }
' "$repo_root/.github/workflows/issue-lifecycle.yml"

echo "issue lifecycle audit fixtures: ok"
