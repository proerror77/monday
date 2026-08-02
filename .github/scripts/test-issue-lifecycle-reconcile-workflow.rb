#!/usr/bin/env ruby
# frozen_string_literal: true

require "yaml"

repo_root = File.expand_path("../..", __dir__)
workflow_path = ARGV.fetch(0, File.join(repo_root, ".github/workflows/issue-lifecycle-reconcile.yml"))
source = File.read(workflow_path)
workflow = YAML.safe_load(source, permitted_classes: [], permitted_symbols: [], aliases: false)
events = workflow.fetch("on")

raise "incorrect workflow events" unless events.keys.sort == %w[issues pull_request_target schedule workflow_dispatch]
raise "issue activity is filtered" unless events.fetch("issues").nil?
raise "incomplete pull request events" unless events.fetch("pull_request_target").fetch("types").sort ==
  %w[edited opened ready_for_review reopened synchronize]
raise "scheduled reconciliation missing" if events.fetch("schedule").empty?
raise "reconciliation runs are not serialized" unless workflow.fetch("concurrency") == {
  "group" => "issue-lifecycle-status-reconciliation", "cancel-in-progress" => true
}

read_permissions = { "contents" => "read", "issues" => "read", "pull-requests" => "read" }
raise "top-level permissions are not read-only" unless workflow.fetch("permissions") == read_permissions

jobs = workflow.fetch("jobs")
raise "expected one status-writer job" unless jobs.length == 1
job = jobs.values.first
raise "job collides with commit status context" if job.fetch("name") == "Issue Lifecycle"
raise "status permission is not isolated to the writer" unless job.fetch("permissions") ==
  read_permissions.merge("statuses" => "write")
raise "GitHub token is not wired" unless job.fetch("env").fetch("GH_TOKEN") == "${{ github.token }}"

steps = job.fetch("steps")
remote_actions = steps.map { |step| step["uses"] }.compact.reject { |uses| uses.start_with?("./") }
checkout = steps.find { |step| step["uses"]&.start_with?("actions/checkout@") }
raise "checkout action missing" unless checkout
raise "third-party action is not pinned" unless remote_actions.all? { |uses| uses.match?(/@[0-9a-f]{40}\z/) }
raise "checkout does not use the trusted default branch" unless checkout.dig("with", "ref") ==
  "${{ github.event.repository.default_branch }}"
raise "pull-request head code is referenced" if source.include?("github.event.pull_request.head")

runnable = steps.select { |step| step["run"] }
raise "expected one reconciliation command" unless runnable.length == 1
run = runnable.first.fetch("run")
raise "reconciler is not invoked" unless run.include?("ruby .github/scripts/issue-lifecycle-status-reconcile.rb")
raise "workflow summary is not wired" unless run.include?('--summary "$GITHUB_STEP_SUMMARY"')
raise "workflow duplicates auditor policy" if run.include?("issue-lifecycle-audit.rb")

puts "issue lifecycle reconciliation workflow: ok"
