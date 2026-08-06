#!/usr/bin/env ruby
# frozen_string_literal: true

require "json"
require "open3"
require "optparse"

CATEGORIES = %w[bug enhancement].freeze
TRIAGE_STATES = %w[needs-triage needs-info ready-for-agent ready-for-human wontfix].freeze
RUNTIME_CONTROL_FIELDS = {
  "Target" => /\A(?:[-*]\s*)?(?:\*\*)?(?:Target|Exact target identity)(?:\*\*)?\s*:\s*(.+)\z/i,
  "Candidate" => /\A(?:[-*]\s*)?(?:\*\*)?(?:Candidate|Candidate and configuration identity)(?:\*\*)?\s*:\s*(.+)\z/i,
  "Controller" => /\A(?:[-*]\s*)?(?:\*\*)?(?:Controller|Named controller)(?:\*\*)?\s*:\s*(.+)\z/i,
  "Stop rule" => /\A(?:[-*]\s*)?(?:\*\*)?Stop rules?(?:\*\*)?\s*:\s*(.+)\z/i,
  "Rollback" => /\A(?:[-*]\s*)?(?:\*\*)?(?:Rollback|Rollback identity)(?:\*\*)?\s*:\s*(.+)\z/i
}.freeze
CLOSING_KEYWORD_SOURCE = "(?:close[sd]?|fix(?:es|ed)?|resolve[sd]?)"
QUALIFIED_ISSUE_REFERENCE = "(?:([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+))?#(\\d+)\\b"
CLOSING_PATTERN = Regexp.new("\\b(#{CLOSING_KEYWORD_SOURCE})\\s*:?\\s+#{QUALIFIED_ISSUE_REFERENCE}", Regexp::IGNORECASE)
NEGATED_CLOSING_PATTERN = Regexp.new(
  "\\b(?:(?:do(?:es)?|did|will|would|should|can|could|must)\\s+not|cannot|doesn't|don't|didn't|won't|can't|never|not)(?:\\s+[A-Za-z-]+){0,3}\\s+#{CLOSING_KEYWORD_SOURCE}\\s*:?\\s+#{QUALIFIED_ISSUE_REFERENCE}",
  Regexp::IGNORECASE
)

class GitHubReadOnly
  API_VERSION = "2026-03-10"

  def initialize(repo)
    raise "invalid repository #{repo.inspect}; expected OWNER/REPO" unless repo.to_s.match?(/\A[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\z/)

    @repo = repo
  end

  attr_reader :repo

  def get(path, allow_404 = false)
    stdout, stderr, status = Open3.capture3(
      "gh", "api", "--method", "GET",
      "-H", "Accept: application/vnd.github+json",
      "-H", "X-GitHub-Api-Version: #{API_VERSION}",
      path
    )
    return nil if allow_404 && !status.success? && stderr.include?("HTTP 404")
    raise "GitHub API GET #{path} failed: #{stderr.strip}" unless status.success?

    JSON.parse(stdout)
  end

  def paginate(path)
    page = 1
    items = []
    loop do
      separator = path.include?("?") ? "&" : "?"
      batch = get("#{path}#{separator}per_page=100&page=#{page}")
      raise "GitHub API GET #{path} did not return an array" unless batch.is_a?(Array)

      items.concat(batch)
      break if batch.length < 100

      page += 1
    end
    items
  end
end
def markdown_section(markdown, title)
  match = markdown.to_s.match(/^##(?:#)?[ \t]+#{Regexp.escape(title)}[ \t]*\r?\n(?<body>.*?)(?=^##(?:#)?[ \t]+|\z)/mi)
  match && match[:body]
end

def visible_markdown(body)
  body.to_s.gsub(/<!--.*?-->/m, "")
end

def names(values)
  Array(values).map { |value| value.is_a?(Hash) ? value["name"] || value["login"] : value }.compact
end

def native_parent_number(issue)
  parent = issue["parent"]
  parent.is_a?(Hash) ? parent["number"] : parent
end

def blockers(issue)
  Array(issue["blocked_by"]).map do |blocker|
    blocker.is_a?(Hash) ? blocker : { "number" => blocker, "state" => "open" }
  end
end

def summary_numbers(body, title)
  section = markdown_section(visible_markdown(body), title)
  section && section.scan(/#(\d+)/).flatten.map(&:to_i).uniq.sort
end

def references(numbers)
  Array(numbers).empty? ? "None" : Array(numbers).map { |number| "##{number}" }.join(", ")
end

def invalid_control_value?(value)
  value.to_s.strip.empty? || value.to_s.strip.match?(/\A(?:none|n\/a|tbd|unknown|-)\z/i)
end

def issue_form_value(body, *titles)
  titles.each do |title|
    section = markdown_section(visible_markdown(body), title)
    next unless section

    value = section.lines.map(&:strip).reject(&:empty?).join(" ")
    return value unless value.empty?
  end

  nil
end

def hydrate_live_issue!(github, repo, issue)
  return unless issue && !issue.key?("pull_request")

  parent_url = issue["parent_issue_url"].to_s
  issue["parent"] = parent_url[/\/issues\/(\d+)\z/, 1]&.to_i
  summary = issue["issue_dependencies_summary"]
  issue["blocked_by"] = if summary && summary["total_blocked_by"].to_i.zero?
                          []
                        else
                          github.paginate("repos/#{repo}/issues/#{issue.fetch("number")}/dependencies/blocked_by")
                        end
end

def missing_runtime_control(body)
  section = markdown_section(visible_markdown(body), "Runtime control")
  unless section
    field_values = {
      "Target" => issue_form_value(body, "Exact target identity", "Target"),
      "Candidate" => issue_form_value(body, "Candidate and configuration identity", "Candidate"),
      "Controller" => issue_form_value(body, "Named controller", "Controller"),
      "Stop rule" => issue_form_value(body, "Stop rules", "Stop rule"),
      "Rollback" => issue_form_value(body, "Rollback identity and procedure", "Rollback identity", "Rollback")
    }
    return field_values.each_with_object([]) { |(name, value), missing| missing << name if invalid_control_value?(value) }
  end

  lines = section.lines.map(&:strip).reject(&:empty?)
  RUNTIME_CONTROL_FIELDS.each_with_object([]) do |(name, pattern), missing|
    value = lines.map { |line| line.match(pattern) }.compact.map { |match| match[1].strip }.first
    missing << name if invalid_control_value?(value)
  end
end

def literal_escaped_newline_artifact?(body)
  text = body.to_s
  return false unless text.include?("\\n")

  !text.include?("\n") || text.match?(/(?:\A|\n)[#]{1,6}[ \t]+[^\\\r\n]+\\n(?:\\n)?/)
end

def active_owner_violation(issue)
  assignee_count = names(issue["assignees"]).length
  return if assignee_count == 1

  "Issue ##{issue.fetch("number")}: active implementation requires exactly one assignee; found #{assignee_count}"
end

def audit_issue(issue, has_open_pr)
  number = issue.fetch("number")
  labels = names(issue["labels"])
  violations = []
  categories = labels & CATEGORIES
  states = labels & TRIAGE_STATES

  violations << "Issue ##{number}: expected exactly one category label; found #{categories.empty? ? "none" : categories.join(", ")}" unless categories.length == 1
  violations << "Issue ##{number}: expected exactly one triage state label; found #{states.empty? ? "none" : states.join(", ")}" unless states.length == 1
  violations << "Issue ##{number}: body contains a literal escaped newline (\\n); publish multiline Markdown through a body file" if literal_escaped_newline_artifact?(issue["body"])
  violations << "Issue ##{number}: tracking issues cannot use ready-for-agent" if labels.include?("tracking") && labels.include?("ready-for-agent")

  if labels.include?("runtime") && labels.include?("ready-for-agent")
    missing = missing_runtime_control(issue["body"])
    violations << "Issue ##{number}: runtime ready-for-agent is missing Runtime control: #{missing.join(", ")}" unless missing.empty?
    open_blockers = blockers(issue).reject { |blocker| blocker["state"].to_s.downcase == "closed" }.map { |blocker| blocker["number"] }
    violations << "Issue ##{number}: runtime ready-for-agent has open native blocker #{references(open_blockers)}" unless open_blockers.empty?
  end

  owner_violation = active_owner_violation(issue) if has_open_pr
  violations << owner_violation if owner_violation

  parent_summary = summary_numbers(issue["body"], "Parent")
  native_parent = native_parent_number(issue)
  native_parents = native_parent ? [native_parent] : []
  if parent_summary && parent_summary != native_parents
    violations << "Issue ##{number}: Parent summary references #{references(parent_summary)}; native parent is #{references(native_parents)}"
  elsif !parent_summary && !native_parents.empty?
    violations << "Issue ##{number}: Parent summary is missing; native parent is #{references(native_parents)}"
  end

  blocker_summary = summary_numbers(issue["body"], "Blocked by")
  native_blockers = blockers(issue).map { |blocker| blocker["number"] }.compact.uniq.sort
  if blocker_summary && blocker_summary != native_blockers
    violations << "Issue ##{number}: Blocked by summary references #{references(blocker_summary)}; native blocked-by is #{references(native_blockers)}"
  elsif !blocker_summary && !native_blockers.empty?
    violations << "Issue ##{number}: Blocked by summary is missing; native blocked-by is #{references(native_blockers)}"
  end

  violations
end

def visible_relationship(body)
  section = markdown_section(visible_markdown(body), "Issue relationship")
  lines = section.to_s.lines.map(&:strip).reject(&:empty?)
  return nil unless lines.length == 1
  return { "kind" => "none" } if lines.first == "None"

  match = lines.first.match(/\A(Refs|Closes) #(\d+)\z/)
  match && { "kind" => match[1].downcase, "number" => match[2].to_i }
end

def relationship_numbers(body)
  section = markdown_section(visible_markdown(body), "Issue relationship")
  section.to_s.scan(/\b(?:Refs|Closes) #(\d+)\b/).flatten.map(&:to_i).uniq
end

def closing_keywords(text)
  text.to_s.to_enum(:scan, CLOSING_PATTERN).map do
    match = Regexp.last_match
    { "keyword" => match[1], "repository" => match[2], "number" => match[3].to_i }
  end
end

def closing_description(closing)
  target = closing["repository"] ? "#{closing["repository"]}##{closing["number"]}" : "##{closing["number"]}"
  "#{closing["keyword"]} #{target}"
end

def closing_targets_issue?(closing, number, repo)
  return false unless closing["number"] == number

  !closing["repository"] || (repo && closing["repository"].casecmp?(repo))
end

def relationship_description(relationship)
  return "None" if relationship["kind"] == "none"

  "#{relationship["kind"] == "refs" ? "Refs" : "Closes"} ##{relationship["number"]}"
end

def audit_pull_request(pull_request, issues, default_branch, repo)
  number = pull_request.fetch("number")
  relationship = visible_relationship(pull_request["body"])
  unless relationship
    return ["PR ##{number}: expected exactly one visible issue relationship (Refs #N, Closes #N, or None)"]
  end

  violations = []
  visible_body = visible_markdown(pull_request["body"])
  body_closings = closing_keywords(visible_body)
  title = pull_request["title"].to_s
  title_closings = closing_keywords(title)
  violations << "PR ##{number} body: negated closing phrase is forbidden" if visible_body.match?(NEGATED_CLOSING_PATTERN)
  violations << "PR ##{number} title: negated closing phrase is forbidden" if title.match?(NEGATED_CLOSING_PATTERN)

  if relationship["kind"] == "closes"
    expected_number = relationship["number"]
    unexpected = body_closings.reject { |closing| closing_targets_issue?(closing, expected_number, repo) }
    if unexpected.any? || body_closings.length != 1
      violations << "PR ##{number} body: visible Closes ##{expected_number} must be the only closing keyword relationship"
    end
    unexpected_title = title_closings.reject { |closing| closing_targets_issue?(closing, expected_number, repo) }
    if unexpected_title.any? || title_closings.length > 1
      violations << "PR ##{number} title: visible Closes ##{expected_number} must be the only closing keyword relationship"
    end
  else
    body_closings.each do |closing|
      violations << "PR ##{number} body: closing keyword #{closing_description(closing)} requires visible Closes ##{closing["number"]}"
    end
    title_closings.each do |closing|
      violations << "PR ##{number} title: closing keyword #{closing_description(closing)} requires visible Closes ##{closing["number"]}"
    end
  end

  target = relationship["number"] && issues[relationship["number"]]
  if relationship["number"] && !target
    violations << "PR ##{number}: #{relationship_description(relationship)} references an issue unavailable to the audit"
  end

  if relationship["kind"] == "closes"
    base = pull_request["base"].is_a?(Hash) ? pull_request.dig("base", "ref") : pull_request["base"]
    violations << "PR ##{number}: Closes ##{relationship["number"]} targets #{base}, not default branch #{default_branch}" if base != default_branch
    target_labels = target ? names(target["labels"]) : []
    violations << "PR ##{number}: runtime issue ##{relationship["number"]} cannot be closed by a pull request" if target_labels.include?("runtime")
    violations << "PR ##{number}: tracking issue ##{relationship["number"]} cannot be closed by a pull request" if target_labels.include?("tracking")
  end

  commits = Array(pull_request["commits"])
  expected_commit_count = pull_request["expected_commit_count"]
  if expected_commit_count && commits.length != expected_commit_count
    violations << "PR ##{number}: fetched #{commits.length} of #{expected_commit_count} commit messages; audit cannot prove every commit safe"
  end
  commits.each_with_index do |commit, index|
    message = commit["message"] || commit.dig("commit", "message") || ""
    identity = commit["sha"].to_s.empty? ? (index + 1).to_s : commit["sha"].to_s[0, 12]
    violations << "PR ##{number} commit #{identity}: negated closing phrase is forbidden" if message.match?(NEGATED_CLOSING_PATTERN)
    closing_keywords(message).each do |closing|
      if relationship["kind"] != "closes"
        violations << "PR ##{number} commit #{identity}: closing keyword #{closing_description(closing)} requires visible Closes ##{closing["number"]}"
      elsif !closing_targets_issue?(closing, relationship["number"], repo)
        violations << "PR ##{number} commit #{identity}: closing keyword #{closing_description(closing)} conflicts with visible Closes ##{relationship["number"]}"
      end
    end
  end

  violations
end

def cross_pr_double_close_violations(pull_requests)
  closers_by_issue = {}
  pull_requests.each do |pull_request|
    relationship = visible_relationship(pull_request["body"])
    next unless relationship && relationship["kind"] == "closes"

    number = relationship["number"]
    pr_number = pull_request.fetch("number")
    (closers_by_issue[number] ||= []) << pr_number
  end

  closers_by_issue.each_with_object([]) do |(issue_number, pr_numbers), violations|
    next unless pr_numbers.length > 1

    violations << "Issue ##{issue_number} is claimed closed by multiple open pull requests: PR ##{pr_numbers.sort.join(", #")}; exactly one may use Closes"
  end
end

EVIDENCE_SECTION_HEADINGS = [
  "Completion evidence",
  "Runtime closure evidence",
  "Runtime control"
].freeze

def evidence_section_present?(body)
  return false if body.nil? || body.empty?

  markdown = visible_markdown(body)
  EVIDENCE_SECTION_HEADINGS.any? { |heading| markdown.match?(/^#{Regexp.escape(heading)}\s*$/m) }
end

def comment_history_has_evidence?(comments)
  # GitHub's issue API exposes `comments` as an Integer count; a full
  # comment fetch is done via hydrate_live_issue! when available.
  return false if comments.nil? || comments == 0 || comments.empty?

  comments.any? do |comment|
    body = comment.is_a?(Hash) ? comment["body"] : comment.to_s
    evidence_section_present?(body) || body.to_s.include?("rollback identity") || body.to_s.include?("Named controller")
  end
end

def audit_closed_runtime_evidence(closed_runtime_issues)
  closed_runtime_issues.each_with_object([]) do |issue, violations|
    number = issue.fetch("number")
    next if evidence_section_present?(issue["body"]) || comment_history_has_evidence?(issue["comments"])

    violations << "Issue ##{number} (runtime, closed): missing completion/closure evidence (Target, Named controller, rollback identity, stop rules, terminal result) in body or comment history"
  end
end

def automatic_close_description(value, fixture)
  return "enabled (fixture)" if fixture && value == true
  return "disabled (fixture)" if fixture && value == false
  return "#{value} (fixture)" if fixture && !value.nil?

  "unknown/unavailable via documented API"
end

def load_live(repo, pr_number)
  github = GitHubReadOnly.new(repo)
  repository = github.get("repos/#{repo}")
  entries = github.paginate("repos/#{repo}/issues?state=open")
  issue_entries = entries.reject { |entry| entry.key?("pull_request") }
  pull_entries = entries.select { |entry| entry.key?("pull_request") }
  closed_runtime_entries = []
  unless pr_number
    recent_closed = github.paginate("repos/#{repo}/issues?state=closed&sort=updated&direction=desc")
    closed_issue_entries = recent_closed.reject { |entry| entry.key?("pull_request") }
    closed_runtime_entries = closed_issue_entries.select do |entry|
      names(entry["labels"]).include?("runtime")
    end
    closed_runtime_entries.each do |entry|
      hydrate_live_issue!(github, repo, entry)
      count = entry["comments"].to_i
      entry["comments"] = if count.positive?
                            github.paginate("repos/#{repo}/issues/#{entry.fetch("number")}/comments")
                          else
                            []
                          end
    end
  end
  if pr_number
    pull_entries = pull_entries.select { |entry| entry["number"] == pr_number }
    raise "open PR ##{pr_number} was not returned by /issues" if pull_entries.empty?
  end

  pull_requests = pull_entries.map do |entry|
    pull_request = github.get("repos/#{repo}/pulls/#{entry.fetch("number")}")
    pull_request["expected_commit_count"] = pull_request["commits"]
    pull_request["commits"] = github.paginate("repos/#{repo}/pulls/#{entry.fetch("number")}/commits")
    pull_request
  end

  known = issue_entries.each_with_object({}) { |issue, by_number| by_number[issue.fetch("number")] = issue }
  linked_issue_numbers = pull_requests.flat_map { |pull_request| relationship_numbers(pull_request["body"]) }.uniq
  audited_issue_numbers = pr_number ? linked_issue_numbers : issue_entries.map { |issue| issue.fetch("number") }
  linked_issue_numbers.each do |number|
    next if known.key?(number)

    issue = github.get("repos/#{repo}/issues/#{number}", true)
    known[number] = issue if issue && !issue.key?("pull_request")
  end
  audited_issue_numbers.each { |number| hydrate_live_issue!(github, repo, known[number]) }

  {
    "source" => "live",
    "repo" => repo,
    "scope" => pr_number ? "pull request ##{pr_number}" : "all open issues and pull requests",
    "default_branch" => repository.fetch("default_branch"),
    "automatic_linked_issue_closing" => nil,
    "issues" => known.values,
    "audited_issue_numbers" => audited_issue_numbers,
    "pull_requests" => pull_requests,
    "closed_runtime_issues" => closed_runtime_entries
  }
end

def render(data, label, violations, fixture)
  status = violations.empty? ? "PASS" : "FAIL"
  lines = [
    "#{status} #{label}",
    "# Issue Lifecycle Audit",
    "",
    "- Scope: #{data["scope"] || "fixture case #{label}"}",
    "- Repository: #{data["repo"] || "fixture"}",
    "- Default branch: `#{data.fetch("default_branch")}`",
    "- automatic-linked-issue-closing: #{automatic_close_description(data["automatic_linked_issue_closing"], fixture)}",
    "- Result: **#{status}** (#{violations.length} violation#{violations.length == 1 ? "" : "s"})"
  ]
  if violations.empty?
    lines.concat(["", "No lifecycle violations found."])
  else
    lines.concat(["", "## Violations", ""])
    violations.each_with_index { |violation, index| lines << "#{index + 1}. #{violation}" }
  end
  lines.join("\n") + "\n"
end

options = {}
parser = OptionParser.new do |parser|
  parser.banner = "Usage: issue-lifecycle-audit.rb [--repo OWNER/REPO [--pr N] | --fixture FILE --case NAME]"
  parser.on("--repo OWNER/REPO", "Audit live GitHub data") { |value| options[:repo] = value }
  parser.on("--pr NUMBER", Integer, "Audit only one open pull request") { |value| options[:pr] = value }
  parser.on("--fixture FILE", "Read a fixture matrix instead of GitHub") { |value| options[:fixture] = value }
  parser.on("--case NAME", "Audit one named fixture case") { |value| options[:case] = value }
  parser.on("--summary FILE", "Append Markdown output to FILE") { |value| options[:summary] = value }
  parser.on("--violations-json FILE", "Write per-issue violation mapping as JSON to FILE") { |value| options[:violations_json] = value }
end

begin
  parser.parse!
  fixture = !!options[:fixture]
  if fixture
    raise "--fixture requires --case" unless options[:case]
    raise "--fixture cannot be combined with --repo or --pr" if options[:repo] || options[:pr]
    matrix = JSON.parse(File.read(options[:fixture]))
    data = matrix.fetch("cases").fetch(options[:case])
    label = options[:case]
  else
    repo = options[:repo] || ENV["GITHUB_REPOSITORY"]
    raise "--repo OWNER/REPO is required outside GitHub Actions" unless repo
    data = load_live(repo, options[:pr])
    label = options[:pr] ? "PR ##{options[:pr]}" : "live"
  end

  violations = []
  open_pr_issue_numbers = data.fetch("pull_requests", []).flat_map { |pull_request| relationship_numbers(pull_request["body"]) }.uniq
  audited_issue_numbers = data["audited_issue_numbers"] || data.fetch("issues", []).map { |issue| issue.fetch("number") }
  data.fetch("issues", []).each do |issue|
    next unless audited_issue_numbers.include?(issue.fetch("number"))

    violations.concat(audit_issue(issue, open_pr_issue_numbers.include?(issue.fetch("number"))))
  end
  data.fetch("issues", []).each do |issue|
    number = issue.fetch("number")
    next if audited_issue_numbers.include?(number) || !open_pr_issue_numbers.include?(number)

    owner_violation = active_owner_violation(issue)
    violations << owner_violation if owner_violation
  end

  issues = data.fetch("issues", []).each_with_object({}) do |issue, by_number|
    by_number[issue.fetch("number")] = issue
  end
  data.fetch("pull_requests", []).each do |pull_request|
    violations.concat(audit_pull_request(pull_request, issues, data.fetch("default_branch"), data["repo"]))
  end

  violations.concat(cross_pr_double_close_violations(data.fetch("pull_requests", [])))
  violations.concat(audit_closed_runtime_evidence(data.fetch("closed_runtime_issues", [])))

  output = render(data, label, violations, fixture)
  File.open(options[:summary], "a") { |file| file.write(output) } if options[:summary]
  if options[:violations_json]
    per_issue = {}
    violations.each do |violation|
      issue_number = violation[/^Issue #(\d+)/, 1] || violation[/^PR #(\d+)/, 1]
      next unless issue_number

      (per_issue[issue_number.to_i] ||= []) << violation
    end
    File.write(options[:violations_json], JSON.generate(per_issue))
  end
  puts output
  exit(violations.empty? ? 0 : 1)
rescue StandardError => error
  output = "ERROR issue lifecycle audit\n# Issue Lifecycle Audit\n\n- Result: **ERROR**\n- #{error.message}\n"
  warn output
  File.open(options[:summary], "a") { |file| file.write(output) } if options[:summary] rescue nil
  exit 2
end
