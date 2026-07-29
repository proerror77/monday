#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"

ruby -ryaml -e '
  required = %w[prd engineering-change bug-report runtime-rollout]
  categories = %w[bug enhancement]
  states = %w[needs-triage needs-info ready-for-agent ready-for-human wontfix]
  expected_labels = {
    "prd" => %w[enhancement needs-triage tracking],
    "engineering-change" => %w[enhancement needs-triage],
    "bug-report" => %w[bug needs-triage],
    "runtime-rollout" => %w[enhancement needs-triage runtime]
  }
  expected_ids = {
    "prd" => %w[problem solution stories decisions testing out_of_scope],
    "engineering-change" => %w[contract acceptance dependencies out_of_scope rollout],
    "bug-report" => %w[current_behavior reproduction expected_behavior boundary],
    "runtime-rollout" => %w[target controller candidate rollback stop_rules success out_of_scope]
  }
  required.each do |name|
    file = File.join(ARGV.fetch(0), ".github/ISSUE_TEMPLATE/#{name}.yml")
    form = YAML.load_file(file)
    abort "#{file}: missing name" unless form["name"].is_a?(String) && form["name"].length > 3
    abort "#{file}: missing description" unless form["description"].is_a?(String)
    abort "#{file}: missing body" unless form["body"].is_a?(Array) && !form["body"].empty?
    ids = form["body"].map { |field| field["id"] }.compact
    abort "#{file}: duplicate field id" unless ids.uniq == ids
    labels = form.fetch("labels", [])
    abort "#{file}: expected one category" unless (labels & categories).length == 1
    abort "#{file}: expected one state" unless (labels & states).length == 1
    abort "#{file}: incorrect labels" unless labels.sort == expected_labels.fetch(name).sort
    abort "#{file}: missing contract fields" unless ids.sort == expected_ids.fetch(name).sort
    form["body"].each do |field|
      next if field["type"] == "markdown"
      abort "#{file}: missing field id" unless field["id"]
      abort "#{file}: missing field label" unless field.dig("attributes", "label")
      abort "#{file}: field must be required" unless field.dig("validations", "required") == true
    end
  end

  config = YAML.load_file(File.join(ARGV.fetch(0), ".github/ISSUE_TEMPLATE/config.yml"))
  abort "blank issues must be disabled" unless config["blank_issues_enabled"] == false

  def visible_relationships(template)
    section = template.match(/^## Issue relationship\n(?<body>.*?)(?=^## )/m)
    abort "missing issue relationship section" unless section
    section[:body].gsub(/<!--.*?-->/m, "").lines.map(&:strip).reject(&:empty?)
  end

  template = File.read(File.join(ARGV.fetch(0), ".github/pull_request_template.md"))
  section = template.match(/^## Issue relationship\n(?<body>.*?)(?=^## )/m)
  visible = visible_relationships(template)
  abort "unsafe default issue relationship: #{visible.inspect}" unless visible == ["None"]
  abort "missing Refs example" unless section[:body].include?("Refs #123")
  abort "missing Closes example" unless section[:body].include?("Closes #123")

  unsafe = "## Issue relationship\n\nRefs #1\nCloses #1\nNone\n\n## Next\n"
  abort "unsafe relationship counterexample passed" if visible_relationships(unsafe) == ["None"]
' "$repo_root"

grep -Fq 'native parent' "$repo_root/docs/agents/issue-tracker.md"
grep -Fq 'gh issue create --parent' "$repo_root/docs/agents/issue-tracker.md"
grep -Fq 'named controller' "$repo_root/docs/agents/issue-tracker.md"
# Backticks are literal Markdown.
# shellcheck disable=SC2016
grep -Fq 'close`, `fix`, and `resolve`' "$repo_root/docs/agents/issue-tracker.md"
test -f "$repo_root/docs/agents/triage-labels.md"
# Backticks are literal Markdown.
# shellcheck disable=SC2016
grep -Fq '`tracking`' "$repo_root/docs/agents/triage-labels.md"
# Backticks are literal Markdown.
# shellcheck disable=SC2016
grep -Fq '`runtime`' "$repo_root/docs/agents/triage-labels.md"

echo "issue lifecycle contract: ok"
