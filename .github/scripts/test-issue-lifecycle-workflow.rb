#!/usr/bin/env ruby
# frozen_string_literal: true

require "yaml"

repo_root = File.expand_path("../..", __dir__)
workflow = YAML.load_file(File.join(repo_root, ".github/workflows/issue-lifecycle.yml"))
events = workflow.fetch("on")

raise "incorrect workflow events" unless events.keys.sort == %w[pull_request schedule workflow_dispatch]
raise "incomplete pull request events" unless events.fetch("pull_request").fetch("types").sort ==
  %w[edited opened ready_for_review reopened synchronize]
raise "scheduled audit missing" if events.fetch("schedule").empty?
raise "permissions are not minimal and read-only" unless workflow.fetch("permissions") == {
  "contents" => "read", "issues" => "read", "pull-requests" => "read"
}

jobs = workflow.fetch("jobs")
raise "expected one audit job" unless jobs.length == 1
job = jobs.values.first
raise "unstable check name" unless job.fetch("name") == "Issue Lifecycle"
raise "GitHub token is not wired" unless job.fetch("env").fetch("GH_TOKEN") == "${{ github.token }}"

steps = job.fetch("steps")
remote_actions = steps.map { |step| step["uses"] }.compact.reject { |uses| uses.start_with?("./") }
raise "checkout action missing" unless remote_actions.any? { |uses| uses.start_with?("actions/checkout@") }
raise "third-party action is not pinned" unless remote_actions.all? { |uses| uses.match?(/@[0-9a-f]{40}\z/) }

contract_step = steps.find { |step| step["name"] == "Verify workflow contract" }
raise "workflow contract check is not enforced" unless contract_step&.fetch("run") ==
  "ruby .github/scripts/test-issue-lifecycle-workflow.rb"
raise "unexpected runnable workflow step" unless steps.count { |step| step["run"] } == 3

audit_steps = steps.select { |step| step["run"]&.include?("issue-lifecycle-audit.rb") }
raise "expected only PR and repository audits" unless audit_steps.length == 2
raise "workflow duplicates audit policy" unless audit_steps.all? do |step|
  run = step.fetch("run")
  run.include?("ruby .github/scripts/issue-lifecycle-audit.rb") && run.include?("--summary \"$GITHUB_STEP_SUMMARY\"")
end
pr_audit = audit_steps.find { |step| step["if"] == "github.event_name == 'pull_request'" }
live_audit = audit_steps.find { |step| step["if"] == "github.event_name != 'pull_request'" }
raise "PR audit does not target one PR" unless pr_audit&.fetch("run")&.include?("--pr \"$PR_NUMBER\"")
raise "scheduled audit is not repository-wide" unless live_audit && !live_audit.fetch("run").include?("--pr")

puts "issue lifecycle workflow: ok"
