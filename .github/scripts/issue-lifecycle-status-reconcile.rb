#!/usr/bin/env ruby
# frozen_string_literal: true

require "json"
require "open3"
require "optparse"
require "rbconfig"

CONTEXT = "Issue Lifecycle"
STATUS_BY_EXIT = {
  0 => ["success", "Issue lifecycle audit passed"],
  1 => ["failure", "Issue lifecycle policy violations found"],
  2 => ["error", "Issue lifecycle audit errored"]
}.freeze

class GitHubStatuses
  API_VERSION = "2026-03-10"

  def initialize(repo)
    raise "invalid repository #{repo.inspect}; expected OWNER/REPO" unless repo.to_s.match?(/\A[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\z/)

    @repo = repo
  end

  def open_pull_requests
    paginate("repos/#{@repo}/pulls?state=open&per_page=100")
  end

  def create_status(sha, payload)
    run(
      "gh", "api", "--method", "POST", *headers,
      "--input", "-", "--silent", "repos/#{@repo}/statuses/#{sha}",
      stdin_data: JSON.generate(payload)
    )
  end

  private

  def paginate(path)
    pages = JSON.parse(run("gh", "api", "--method", "GET", *headers, "--paginate", "--slurp", path))
    raise "GitHub API GET #{path} did not return pages of arrays" unless pages.is_a?(Array) && pages.all? { |page| page.is_a?(Array) }

    pages.flatten(1)
  end

  def headers
    [
      "-H", "Accept: application/vnd.github+json",
      "-H", "X-GitHub-Api-Version: #{API_VERSION}"
    ]
  end

  def run(*command, stdin_data: "")
    stdout, stderr, status = Open3.capture3(*command, stdin_data: stdin_data)
    raise "#{command.first(3).join(' ')} failed: #{stderr.strip}" unless status.success?

    stdout
  end
end

def audit(repo, pr_number, summary)
  command = [
    RbConfig.ruby, File.join(__dir__, "issue-lifecycle-audit.rb"),
    "--repo", repo, "--pr", pr_number.to_s
  ]
  command.concat(["--summary", summary]) if summary
  stdout, stderr, status = Open3.capture3(*command)
  $stdout.write(stdout)
  $stderr.write(stderr)
  STATUS_BY_EXIT.key?(status.exitstatus) ? status.exitstatus : 2
end

def append_summary(path, rows)
  return unless path

  File.open(path, "a") do |summary|
    summary.puts "\n# Issue Lifecycle Status Reconciliation\n\n"
    if rows.empty?
      summary.puts "No open pull requests found."
      next
    end

    summary.puts "| Pull request | Head | State | Status |"
    summary.puts "| --- | --- | --- | --- |"
    rows.each do |number, sha, state, action|
      summary.puts "| ##{number} | `#{sha[0, 12]}` | #{state} | #{action} |"
    end
  end
end

options = {}
OptionParser.new do |parser|
  parser.banner = "Usage: issue-lifecycle-status-reconcile.rb [--repo OWNER/REPO] [--summary FILE]"
  parser.on("--repo OWNER/REPO", "Reconcile one repository") { |value| options[:repo] = value }
  parser.on("--summary FILE", "Append Markdown output to FILE") { |value| options[:summary] = value }
end.parse!

begin
  raise "unexpected arguments: #{ARGV.join(' ')}" unless ARGV.empty?

  repo = options[:repo] || ENV["GITHUB_REPOSITORY"]
  raise "--repo OWNER/REPO is required outside GitHub Actions" unless repo

  github = GitHubStatuses.new(repo)
  rows = []
  audits_by_sha = {}
  result = 0
  # ponytail: #480 intentionally keeps a per-PR merged-auditor call; add a shared snapshot only after measured API/runtime limits justify the protocol change.
  github.open_pull_requests.each do |pull_request|
    number = Integer(pull_request.fetch("number"))
    sha = pull_request.dig("head", "sha").to_s
    raise "PR ##{number} has invalid head SHA #{sha.inspect}" unless sha.match?(/\A[0-9a-f]{40}\z/i)

    audit_exit = audit(repo, number, options[:summary])
    state = STATUS_BY_EXIT.fetch(audit_exit).first
    aggregate = audits_by_sha[sha] ||= { exit: 0 }
    aggregate[:exit] = [aggregate[:exit], audit_exit].max
    rows << [number, sha, state, "pending"]
    result = [result, audit_exit].max
  end

  audits_by_sha.each do |sha, aggregate|
    state, description = STATUS_BY_EXIT.fetch(aggregate[:exit])
    payload = { "context" => CONTEXT, "state" => state, "description" => description }
    github.create_status(sha, payload)
  end
  rows.each { |row| row[3] = "published" }

  append_summary(options[:summary], rows)
  exit result
rescue StandardError => error
  warn "ERROR issue lifecycle status reconciliation: #{error.message}"
  begin
    File.open(options[:summary], "a") do |summary|
      summary.puts "\n# Issue Lifecycle Status Reconciliation\n\n- Result: **ERROR**\n- #{error.message}"
    end if options[:summary]
  rescue StandardError
    nil
  end
  exit 2
end
