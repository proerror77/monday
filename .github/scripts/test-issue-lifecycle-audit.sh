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

def pull_request(number, relationship, base = "main", commits = [], extra = "", title = "Fixture PR #{number}")
  { "number" => number, "base" => base,
    "title" => title,
    "body" => "## Issue relationship\n\n#{relationship}\n\n## Focused validation\n\nFixture proof.\n#{extra}\n",
    "commits" => commits }
end

def data(issues, pull_requests = [], automatic_close = true)
  { "repo" => "example/repo", "default_branch" => "main", "automatic_linked_issue_closing" => automatic_close,
    "issues" => issues, "pull_requests" => pull_requests }
end

base = issue(10, %w[enhancement ready-for-agent], issue_body, ["agent"])
runtime_control = "\n## Runtime control\n\nTarget: repository setting\nController: release-owner\nStop rule: stop on failed preflight\nRollback: restore the previous setting\n"
issue_form_runtime = <<~MARKDOWN

### Exact target identity

repository setting

### Named controller

release-owner

### Rollback identity and procedure

restore the previous setting

### Stop rules

stop on failed preflight
MARKDOWN
incomplete_commits = pull_request(32, "Refs #10", "main", [
  { "sha" => "first", "message" => "Safe fetched commit" }
])
incomplete_commits["expected_commit_count"] = 251
cases = {
  "valid_refs" => data([base], [pull_request(20, "Refs #10", "feature-stack")]),
  "valid_closes" => data([
    issue(11, %w[enhancement ready-for-agent], issue_body, ["agent"])
  ], [pull_request(22, "Closes #11", "main", [{ "sha" => "fix", "message" => "Fixes #11" }])]),
  "valid_title_closes" => data([
    issue(16, %w[enhancement ready-for-agent], issue_body, ["agent"])
  ], [pull_request(42, "Closes #16", "main", [], "", "Fixes #16")]),
  "valid_none" => data([], [pull_request(23, "<!-- Refs #1 and Closes #1 -->\nNone")], false),
  "valid_needs_info" => data([issue(13, %w[bug needs-info])]),
  "valid_wontfix" => data([issue(14, %w[enhancement wontfix])]),
  "authorized_runtime_ready" => data([
    issue(12, %w[enhancement ready-for-agent runtime],
          issue_body("None", "#99", runtime_control), [], nil,
          [{ "number" => 99, "state" => "closed" }])
  ]),
  "authorized_runtime_ready_issue_form" => data([
    issue(15, %w[enhancement ready-for-agent runtime],
          issue_body("None", "#99", issue_form_runtime), [], nil,
          [{ "number" => 99, "state" => "closed" }])
  ]),
  "missing_category" => data([issue(30, %w[ready-for-human])]),
  "conflicting_category" => data([issue(31, %w[bug enhancement ready-for-human])]),
  "missing_state" => data([issue(32, %w[enhancement])]),
  "conflicting_state" => data([issue(33, %w[enhancement needs-triage ready-for-human])]),
  "literal_escaped_newline" => data([
    issue(34, %w[enhancement ready-for-human], "## Parent\\n\\nNone\\n\\n## Blocked by\\n\\nNone\\n\\n## Details\\n\\nliteral \\n escape")
  ]),
  "valid_literal_escaped_newline" => data([
    issue(35, %w[enhancement ready-for-human], issue_body + "\n\n```json\n{\"pattern\":\"line\\\\nnext\"}\n```\n")
  ]),
  "mixed_literal_escaped_newline" => data([
    issue(60, %w[enhancement ready-for-human], "Normal preface.\n\n## Parent\\n\\nNone\\n\\n## Blocked by\\n\\nNone")
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
  "qualified_pr_fix_with_refs" => data([base], [
    pull_request(36, "Refs #10", "main", [], "Fixes proerror77/monday#10")
  ]),
  "qualified_negated_pr_closing" => data([base], [
    pull_request(39, "Refs #10", "main", [], "This does not close proerror77/monday#10.")
  ]),
  "pr_title_fix_with_refs" => data([base], [
    pull_request(34, "Refs #10", "main", [], "", "Fixes #10 in title")
  ]),
  "qualified_pr_title_fix_with_refs" => data([base], [
    pull_request(37, "Refs #10", "main", [], "", "Fixes proerror77/monday#10 in title")
  ]),
  "commit_fix_with_refs" => data([base], [
    pull_request(29, "Refs #10", "main", [
      { "sha" => "safe", "message" => "Safe first commit" },
      { "sha" => "fix-ref", "message" => "Fixes #10" }
    ])
  ]),
  "qualified_commit_fix_with_refs" => data([base], [
    pull_request(38, "Refs #10", "main", [
      { "sha" => "fix-ref", "message" => "Fixes proerror77/monday#10" }
    ])
  ]),
  "qualified_commit_with_closes" => data([base], [
    pull_request(40, "Closes #10", "main", [
      { "sha" => "qualified", "message" => "Fixes other/repo#10" }
    ])
  ]),
  "valid_qualified_commit_with_closes" => data([base], [
    pull_request(41, "Closes #10", "main", [
      { "sha" => "qualified", "message" => "Fixes example/repo#10" }
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
run_pass valid_title_closes
run_pass valid_none "automatic-linked-issue-closing: disabled (fixture)"
run_pass valid_needs_info
run_pass valid_wontfix
run_pass authorized_runtime_ready
run_pass authorized_runtime_ready_issue_form
run_pass valid_literal_escaped_newline
run_pass valid_qualified_commit_with_closes

while IFS='|' read -r name expected; do
  run_fail "$name" "$expected"
done <<'CASES'
missing_category|Issue #30: expected exactly one category label
conflicting_category|Issue #31: expected exactly one category label
missing_state|Issue #32: expected exactly one triage state label
conflicting_state|Issue #33: expected exactly one triage state label
literal_escaped_newline|Issue #34: body contains a literal escaped newline
mixed_literal_escaped_newline|Issue #60: body contains a literal escaped newline
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
qualified_pr_fix_with_refs|PR #36 body: closing keyword Fixes proerror77/monday#10 requires visible Closes #10
qualified_negated_pr_closing|PR #39 body: negated closing phrase is forbidden
pr_title_fix_with_refs|PR #34 title: closing keyword Fixes #10 requires visible Closes #10
qualified_pr_title_fix_with_refs|PR #37 title: closing keyword Fixes proerror77/monday#10 requires visible Closes #10
commit_fix_with_refs|PR #29 commit fix-ref: closing keyword Fixes #10 requires visible Closes #10
qualified_commit_fix_with_refs|PR #38 commit fix-ref: closing keyword Fixes proerror77/monday#10 requires visible Closes #10
qualified_commit_with_closes|PR #40 commit qualified: closing keyword Fixes other/repo#10 conflicts with visible Closes #10
commit_negated_resolve|PR #30 commit resolve: negated closing phrase is forbidden
commit_never_closes|PR #33 commit never: negated closing phrase is forbidden
commit_other_issue|PR #31 commit closed: closing keyword Closed #52 conflicts with visible Closes #10
commit_list_incomplete|PR #32: fetched 1 of 251 commit messages; audit cannot prove every commit safe
CASES

live_stub_dir="$tmp_dir/live-stub"
mkdir -p "$live_stub_dir"
cat > "$live_stub_dir/gh" <<'EOF'
#!/usr/bin/env bash
path="${*: -1}"
case "$path" in
  repos/example/repo)
    echo '{"default_branch":"main"}'
    ;;
  repos/example/repo/issues\?state=open\&per_page=100\&page=1)
    echo '[{"number":10,"title":"Fixture issue 10","body":"## Parent\n\nNone\n\n## Blocked by\n\nNone\n","labels":[{"name":"ready-for-agent"}],"assignees":[{"login":"agent"}],"issue_dependencies_summary":{"total_blocked_by":0}},{"number":88,"pull_request":{}}]'
    ;;
  repos/example/repo/pulls/88)
    echo '{"number":88,"base":{"ref":"main"},"title":"Fixture PR 88","body":"## Issue relationship\n\nRefs #10\n\n## Focused validation\n\nFixture proof.\n","commits":1}'
    ;;
  repos/example/repo/pulls/88/commits\?per_page=100\&page=1)
    echo '[{"sha":"safe","commit":{"message":"Safe commit"}}]'
    ;;
  repos/example/repo/issues/10)
    echo '{"number":10,"title":"Fixture issue 10","body":"## Parent\n\nNone\n\n## Blocked by\n\nNone\n","labels":[{"name":"ready-for-agent"}],"assignees":[{"login":"agent"}],"issue_dependencies_summary":{"total_blocked_by":0}}'
    ;;
  repos/example/repo/issues/10/dependencies/blocked_by\?per_page=100\&page=1)
    echo '[]'
    ;;
  *)
    echo "unexpected gh api path: $path" >&2
    exit 1
    ;;
esac
EOF
chmod +x "$live_stub_dir/gh"

set +e
live_output="$(PATH="$live_stub_dir:$PATH" ruby "$auditor" --repo example/repo --pr 88)"
live_exit=$?
set -e
test "$live_exit" -eq 1
grep -Fq "FAIL PR #88" <<<"$live_output"
grep -Fq "Issue #10: expected exactly one category label" <<<"$live_output"

echo "issue lifecycle audit fixtures: ok"
