#!/usr/bin/env ruby
# frozen_string_literal: true

require "digest"
require "fileutils"
require "json"
require "open3"
require "optparse"
require "time"
require "tmpdir"
require "uri"

EVIDENCE_EXCLUSIONS = ["GitHub metadata mutation", "branch protection and required checks", "deployment and runtime resources", "source code and Agent-led research PRD scope"].freeze

module Canonical
  PRESERVE_ARRAY_ORDER = %w[comments commits events].freeze
  IDENTITY_KEYS = %w[number id node_id sha filename name login context].freeze

  module_function

  def value(object, parent_key = nil)
    case object
    when Hash
      object.keys.sort.each_with_object({}) do |key, result|
        result[key] = value(object.fetch(key), key)
      end
    when Array
      values = object.map { |entry| value(entry) }
      return values if PRESERVE_ARRAY_ORDER.include?(parent_key)

      values.sort_by { |entry| array_sort_key(entry) }
    else
      object
    end
  end

  def dump(object); JSON.generate(value(object)) + "\n"; end

  def array_sort_key(entry)
    if entry.is_a?(Hash)
      key = IDENTITY_KEYS.find { |candidate| entry.key?(candidate) }
      return [key, entry.fetch(key).to_s, JSON.generate(entry)] if key
    end
    ["", "", JSON.generate(entry)]
  end
end

class GitHubReadOnly
  API_VERSION = "2026-03-10"
  REST_ACCEPT = "application/vnd.github+json"
  RELATIONSHIP_QUERY = <<~GRAPHQL.freeze
    query IssueLifecycleRelationships($owner: String!, $name: String!, $cursor: String) {
      repository(owner: $owner, name: $name) {
        id
        nameWithOwner
        defaultBranchRef { name target { ... on Commit { oid } } }
        issues(first: 50, after: $cursor, states: [OPEN, CLOSED], orderBy: {field: CREATED_AT, direction: ASC}) {
          totalCount
          nodes {
            number
            parent { number url repository { nameWithOwner } }
            subIssues(first: 100) { totalCount nodes { number url repository { nameWithOwner } } pageInfo { hasNextPage } }
            blockedBy(first: 100) { totalCount nodes { number url repository { nameWithOwner } } pageInfo { hasNextPage } }
            blocking(first: 100) { totalCount nodes { number url repository { nameWithOwner } } pageInfo { hasNextPage } }
            closedByPullRequestsReferences(first: 100) {
              totalCount
              nodes { number url repository { nameWithOwner } }
              pageInfo { hasNextPage }
            }
          }
          pageInfo { hasNextPage endCursor }
        }
      }
    }
  GRAPHQL

  attr_reader :pages

  def initialize(repo)
    unless repo.to_s.match?(/\A[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\z/)
      raise "invalid repository #{repo.inspect}; expected OWNER/REPO"
    end

    @repo = repo
    @pages = []
    @phase = "capture"
  end

  def with_phase(phase)
    previous = @phase
    @phase = phase
    yield
  ensure
    @phase = previous
  end

  def get(path)
    body, headers, status = request(
      "gh", "api", "--method", "GET", "--include",
      "-H", "Accept: #{REST_ACCEPT}",
      "-H", "X-GitHub-Api-Version: #{API_VERSION}",
      path
    )
    selected = headers["x-github-api-version-selected"]
    raise "GitHub selected API version #{selected.inspect}, expected #{API_VERSION}" if selected && selected != API_VERSION

    record_page("rest", path, body, headers, status)
    [body, headers]
  end

  def paginate(path, collection_key: nil)
    request_path = add_page(path, 1)
    seen_requests = {}
    seen_objects = {}
    items = []
    expected_total = nil
    page = 1

    loop do
      raise "pagination loop at #{request_path}" if seen_requests[request_path]

      seen_requests[request_path] = true
      body, headers = get(request_path)
      batch = collection_key ? body.fetch(collection_key) : body
      raise "GitHub API GET #{request_path} did not return an array" unless batch.is_a?(Array)

      if collection_key
        total = Integer(body.fetch("total_count"))
        expected_total ||= total
        raise "pagination total changed for #{path}: #{expected_total} to #{total}" unless total == expected_total
      end

      batch.each do |entry|
        identity = object_identity(entry)
        raise "duplicate paginated object #{identity} from #{path}" if identity && seen_objects[identity]

        seen_objects[identity] = true if identity
        items << entry
      end

      next_url = next_link(headers["link"])
      break if !next_url && (batch.length < 100 || expected_total == items.length)

      page += 1
      request_path = next_url ? api_path(next_url) : add_page(path, page)
    end

    if expected_total && items.length != expected_total
      raise "incomplete pagination for #{path}: expected #{expected_total}, fetched #{items.length}"
    end

    collection_key ? { "total_count" => expected_total, collection_key => items } : items
  end

  def relationships
    owner, name = @repo.split("/", 2)
    cursor = nil
    nodes = []
    expected_total = nil
    seen_numbers = {}
    repository_identity = nil

    loop do
      command = [
        "gh", "api", "graphql", "--include",
        "-f", "query=#{RELATIONSHIP_QUERY}",
        "-F", "owner=#{owner}", "-F", "name=#{name}"
      ]
      command.concat(["-F", "cursor=#{cursor}"]) if cursor
      body, headers, status = request(*command)
      raise "GitHub GraphQL returned errors: #{body.fetch("errors").inspect}" if body.key?("errors")

      repository = body.dig("data", "repository")
      identity = {
        "id" => repository && repository["id"],
        "full_name" => repository && repository["nameWithOwner"],
        "default_branch" => repository && repository.dig("defaultBranchRef", "name"),
        "default_branch_sha" => repository && repository.dig("defaultBranchRef", "target", "oid")
      }
      raise "GitHub GraphQL repository identity drift" if repository_identity && repository_identity != identity

      repository_identity = identity
      connection = repository && repository["issues"]
      raise "GitHub GraphQL omitted repository issue relationships" unless connection.is_a?(Hash)

      expected_total ||= connection["totalCount"] && Integer(connection["totalCount"])
      batch = connection.fetch("nodes")
      raise "GitHub GraphQL relationship nodes are not an array" unless batch.is_a?(Array)

      batch.each do |node|
        number = Integer(node.fetch("number"))
        raise "duplicate GraphQL issue ##{number}" if seen_numbers[number]

        validate_nested_connections!(node, number)
        seen_numbers[number] = true
        nodes << normalize_relationships(node)
      end

      label = "IssueLifecycleRelationships:#{cursor || "START"}"
      record_page("graphql", label, body, headers, status)
      page_info = connection.fetch("pageInfo")
      break unless page_info.fetch("hasNextPage")

      cursor = page_info["endCursor"]
      raise "GraphQL relationship pagination omitted endCursor" if cursor.to_s.empty?
    end

    if expected_total && nodes.length != expected_total
      raise "incomplete GraphQL issue pagination: expected #{expected_total}, fetched #{nodes.length}"
    end
    unless repository_identity["full_name"] == @repo &&
           repository_identity["default_branch_sha"].to_s.match?(/\A[0-9a-f]{40}\z/i)
      raise "GitHub GraphQL returned invalid repository identity"
    end
    relationships = nodes.sort_by { |node| node.fetch("number") }.to_h { |node| [node.fetch("number"), node] }
    [repository_identity, relationships]
  end

  def assert_rest_unchanged!(pages)
    pages.select { |page| page["protocol"] == "rest" }.each do |page|
      etag = page["etag"]
      raise "capture stability drift: endpoint #{page["request"]} omitted ETag" if etag.to_s.empty?

      _, headers, status = request(
        "gh", "api", "--method", "GET", "--include",
        "-H", "Accept: #{REST_ACCEPT}", "-H", "X-GitHub-Api-Version: #{API_VERSION}",
        "-H", "If-None-Match: #{etag}", page["request"], not_modified: true
      )
      unless status == 304 && headers["etag"] == etag
        raise "capture stability drift: endpoint/header #{page["request"]} changed during readback"
      end
      @pages << page.merge("phase" => @phase, "status" => status, "etag" => headers["etag"], "last_modified" => headers["last-modified"], "api_version" => headers["x-github-api-version-selected"], "media_type" => headers["x-github-media-type"])
    end
  end

  private

  def request(*command, not_modified: false)
    stdout, stderr, status = Open3.capture3(*command)
    allowed_304 = not_modified && stdout.gsub("\r\n", "\n").match?(/^HTTP\/[0-9.]+ 304 /)
    raise "#{command.first(3).join(" ")} failed: #{stderr.strip}" unless status.success? || allowed_304

    response_status, headers, body_text = parse_http(stdout, allow_empty: not_modified)
    unless response_status.between?(200, 299) || (not_modified && response_status == 304)
      raise "GitHub read returned HTTP #{response_status}: #{body_text.strip}"
    end
    [response_status == 304 ? nil : JSON.parse(body_text), headers, response_status]
  rescue JSON::ParserError => error
    raise "GitHub read returned invalid JSON: #{error.message}"
  end

  def parse_http(output, allow_empty: false)
    normalized = output.gsub("\r\n", "\n")
    start = normalized.rindex(/^HTTP\/[0-9.]+ [0-9]{3}.*$/)
    raise "GitHub read omitted HTTP response headers" unless start

    header_text, body = normalized[start..].split("\n\n", 2)
    raise "GitHub read omitted a response body" unless body || allow_empty

    lines = header_text.lines(chomp: true)
    status = Integer(lines.shift.match(/\AHTTP\/[0-9.]+ ([0-9]{3})/)[1])
    headers = lines.each_with_object({}) do |line, result|
      name, value = line.split(":", 2)
      next unless value

      result[name.downcase] = value.strip
    end
    [status, headers, body.to_s]
  end

  def record_page(protocol, request_name, body, headers, status)
    @pages << {
      "phase" => @phase,
      "protocol" => protocol,
      "request" => request_name,
      "status" => status,
      "etag" => headers["etag"],
      "last_modified" => headers["last-modified"],
      "api_version" => headers["x-github-api-version-selected"],
      "media_type" => headers["x-github-media-type"],
      "body_sha256" => Digest::SHA256.hexdigest(Canonical.dump(body))
    }
  end

  def add_page(path, page)
    separator = path.include?("?") ? "&" : "?"
    "#{path}#{separator}per_page=100&page=#{page}"
  end

  def next_link(header)
    return unless header

    header.split(",").each do |part|
      match = part.match(/<([^>]+)>;\s*rel="([^"]+)"/)
      return match[1] if match && match[2].split.include?("next")
    end
    nil
  end

  def api_path(url)
    uri = URI(url)
    raise "pagination link uses unexpected host #{uri.host.inspect}" unless uri.host == "api.github.com"

    path = uri.path.sub(%r{\A/}, "")
    uri.query ? "#{path}?#{uri.query}" : path
  end

  def object_identity(entry)
    return unless entry.is_a?(Hash)

    %w[id node_id filename sha number].each do |key|
      return "#{key}:#{entry.fetch(key)}" if entry.key?(key)
    end
    nil
  end

  def validate_nested_connections!(node, number)
    complete = ->(related) { related.key?("number") && !related["url"].to_s.empty? && !related.dig("repository", "nameWithOwner").to_s.empty? }
    raise "Issue ##{number} parent relationship identity is incomplete" if node["parent"] && !complete.call(node["parent"])
    %w[subIssues blockedBy blocking closedByPullRequestsReferences].each do |key|
      connection = node.fetch(key)
      raise "Issue ##{number} #{key} pagination is incomplete" if connection.dig("pageInfo", "hasNextPage")
      raise "Issue ##{number} #{key} relationship identity is incomplete" unless connection.fetch("nodes").all?(&complete)
      unless Integer(connection.fetch("totalCount")) == connection.fetch("nodes").length
        raise "Issue ##{number} #{key} count does not match returned nodes"
      end
    end
  end

  def normalize_relationships(node)
    {
      "number" => Integer(node.fetch("number")),
      "parent" => node["parent"],
      "sub_issues" => node.dig("subIssues", "nodes"),
      "blocked_by" => node.dig("blockedBy", "nodes"),
      "blocking" => node.dig("blocking", "nodes"),
      "closed_by_pull_requests" => node.dig("closedByPullRequestsReferences", "nodes")
    }
  end
end

def issue_number_from_url(url)
  match = url.to_s.match(%r{/issues/([0-9]+)\z})
  match && Integer(match[1])
end

def group_issue_objects(objects, known_numbers, field)
  objects.each_with_object(Hash.new { |hash, key| hash[key] = [] }) do |object, grouped|
    number = object.dig("issue", "number") || issue_number_from_url(object["issue_url"])
    raise "#{field} object #{object["id"].inspect} has no Issue number" unless number
    raise "#{field} object references unknown Issue ##{number}" unless known_numbers.include?(number)

    grouped[number] << object
  end
end

def read_graph(github, repo)
  labels = github.paginate("repos/#{repo}/labels")
  entries = github.paginate("repos/#{repo}/issues?state=all&sort=created&direction=asc")
  numbers = entries.map { |entry| Integer(entry.fetch("number")) }
  raise "duplicate Issue/PR number in repository listing" unless numbers.uniq.length == numbers.length

  comments = github.paginate("repos/#{repo}/issues/comments?sort=created&direction=asc")
  events = github.paginate("repos/#{repo}/issues/events")
  comments_by_number = group_issue_objects(comments, numbers, "comment")
  events_by_number = group_issue_objects(events, numbers, "event")
  repository, relationships = github.relationships
  default_branch = repository.fetch("default_branch")
  default_branch_sha = repository.fetch("default_branch_sha")
  issue_numbers = entries.reject { |entry| entry.key?("pull_request") }.map { |entry| Integer(entry.fetch("number")) }.sort
  unless relationships.keys.sort == issue_numbers
    raise "GraphQL issue relationship set does not match REST issues"
  end

  items = entries.sort_by { |entry| Integer(entry.fetch("number")) }.map do |entry|
    number = Integer(entry.fetch("number"))
    metadata, = github.get("repos/#{repo}/issues/#{number}")
    raise "Issue API returned mismatched number for ##{number}" unless Integer(metadata.fetch("number")) == number
    unless Integer(metadata.fetch("comments")) == comments_by_number[number].length
      raise "incomplete comments for Issue/PR ##{number}"
    end

    item = {
      "number" => number,
      "kind" => entry.key?("pull_request") ? "pull_request" : "issue",
      "issue" => metadata,
      "comments" => comments_by_number[number],
      "events" => events_by_number[number]
    }
    item["relationships"] = relationships.fetch(number) unless entry.key?("pull_request")
    item["pull_request"] = capture_pull_request(github, repo, number) if entry.key?("pull_request")
    item
  end

  graph = {
    "schema" => "monday.issue_lifecycle_preflight.v1",
    "repository" => repository,
    "label_catalog" => labels,
    "items" => items
  }
  counts = {
    "issues" => issue_numbers.length,
    "pull_requests" => entries.length - issue_numbers.length,
    "labels" => labels.length,
    "issue_comments" => comments.length,
    "issue_events" => events.length
  }
  [graph, counts, default_branch, default_branch_sha, relationships]
end

def capture_graph(repo)
  github = GitHubReadOnly.new(repo)
  graph, counts, default_branch, default_branch_sha, relationships = read_graph(github, repo)
  primary_pages = github.pages.dup
  stable_repository, stable_relationships = github.with_phase("stability_check") do
    result = github.relationships
    github.assert_rest_unchanged!(primary_pages)
    result
  end
  raise "capture stability drift: relationships changed during readback" unless Canonical.value([graph["repository"], relationships]) == Canonical.value([stable_repository, stable_relationships])
  page_key = ->(page) { [page["protocol"], page["request"]] }
  raise "capture stability drift: endpoint set changed during readback" unless primary_pages.map(&page_key).sort == github.pages.drop(primary_pages.length).map(&page_key).sort
  [graph, counts, default_branch, default_branch_sha, github.pages]
end

def capture_pull_request(github, repo, number)
  metadata, = github.get("repos/#{repo}/pulls/#{number}")
  raise "pull request API returned mismatched number for ##{number}" unless Integer(metadata.fetch("number")) == number

  commits = github.paginate("repos/#{repo}/pulls/#{number}/commits")
  files = github.paginate("repos/#{repo}/pulls/#{number}/files")
  review_comments = github.paginate("repos/#{repo}/pulls/#{number}/comments")
  if commits.length != Integer(metadata.fetch("commits"))
    raise "incomplete commits for PR ##{number}: expected #{metadata.fetch("commits")}, fetched #{commits.length}"
  end
  if files.length != Integer(metadata.fetch("changed_files"))
    raise "incomplete files for PR ##{number}: expected #{metadata.fetch("changed_files")}, fetched #{files.length}"
  end
  if review_comments.length != Integer(metadata.fetch("review_comments"))
    raise "incomplete review comments for PR ##{number}"
  end

  head_sha = metadata.dig("head", "sha")
  raise "PR ##{number} returned invalid head SHA #{head_sha.inspect}" unless head_sha.to_s.match?(/\A[0-9a-f]{40}\z/i)

  {
    "metadata" => metadata,
    "commits" => commits,
    "files" => files,
    "reviews" => github.paginate("repos/#{repo}/pulls/#{number}/reviews"),
    "review_comments" => review_comments,
    "check_runs" => github.paginate("repos/#{repo}/commits/#{head_sha}/check-runs?filter=all", collection_key: "check_runs"),
    "statuses" => github.paginate("repos/#{repo}/commits/#{head_sha}/statuses")
  }
end

def write_bundle(output, graph, manifest)
  raise "output already exists: #{output}" if File.exist?(output) || File.symlink?(output)

  parent = File.dirname(File.expand_path(output))
  raise "output parent is not a directory: #{parent}" unless File.directory?(parent)

  temporary = Dir.mktmpdir(".issue-lifecycle-preflight-", parent)
  begin
    preflight_json = Canonical.dump(graph)
    preflight_sha = Digest::SHA256.hexdigest(preflight_json)
    manifest["preflight"] = { "file" => "preflight.json", "sha256" => preflight_sha }
    manifest_json = Canonical.dump(manifest)

    File.write(File.join(temporary, "preflight.json"), preflight_json, mode: "wb")
    File.write(File.join(temporary, "preflight.json.sha256"), "#{preflight_sha}  preflight.json\n", mode: "wb")
    File.write(File.join(temporary, "manifest.json"), manifest_json, mode: "wb")
    File.write(
      File.join(temporary, "manifest.json.sha256"),
      "#{Digest::SHA256.hexdigest(manifest_json)}  manifest.json\n",
      mode: "wb"
    )
    File.rename(temporary, output)
  ensure
    FileUtils.remove_entry(temporary) if File.exist?(temporary)
  end
end

def capture(options)
  repo, controller, output = options.values_at(:repo, :controller, :output)
  raise "--repo OWNER/REPO is required" if repo.to_s.empty?
  raise "--controller NAME is required" if controller.to_s.strip.empty?
  raise "--output DIR is required" if output.to_s.empty?

  graph, counts, default_branch, default_branch_sha, pages = capture_graph(repo)
  graphql_media_types = pages.select { |page| page["protocol"] == "graphql" }.map { |page| page["media_type"] }
  unless graphql_media_types.any? && graphql_media_types.all? { |media_type| !media_type.to_s.empty? } && graphql_media_types.uniq.length == 1
    raise "capture stability drift: GraphQL media type missing or inconsistent: #{graphql_media_types.uniq.inspect}"
  end
  manifest = {
    "schema" => "monday.issue_lifecycle_manifest.v1",
    "repository" => repo,
    "captured_at" => Time.now.utc.iso8601(6),
    "controller" => controller,
    "target" => "GitHub Issue and pull request metadata in #{repo}",
    "exclusions" => EVIDENCE_EXCLUSIONS,
    "default_branch" => default_branch,
    "default_branch_sha" => default_branch_sha,
    "api" => {
      "rest_version" => GitHubReadOnly::API_VERSION,
      "rest_accept" => GitHubReadOnly::REST_ACCEPT,
      "graphql_media_type" => graphql_media_types.first
    },
    "pages" => pages,
    "counts" => counts
  }
  write_bundle(output, graph, manifest)
  puts "captured #{repo} at #{default_branch_sha} -> #{output}"
end

command = ARGV.shift
options = {}
parser = OptionParser.new do |flags|
  flags.banner = "Usage: issue-lifecycle-preflight.rb capture --repo OWNER/REPO --controller NAME --output DIR"
  flags.on("--repo OWNER/REPO", "Repository to read") { |value| options[:repo] = value }
  flags.on("--controller NAME", "Named evidence controller") { |value| options[:controller] = value }
  flags.on("--output DIR", "New evidence bundle directory") { |value| options[:output] = value }
end

begin
  parser.parse!
  raise "unexpected arguments: #{ARGV.join(" ")}" unless ARGV.empty?
  raise "unsupported operation #{command.inspect}; expected capture" unless command == "capture"

  capture(options)
rescue StandardError => error
  warn "ERROR issue lifecycle preflight: #{error.message}"
  exit 2
end
