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
PREFLIGHT_SCHEMA = "monday.issue_lifecycle_preflight.v1"
MANIFEST_SCHEMA = "monday.issue_lifecycle_manifest.v1"
BUNDLE_FILES = %w[manifest.json manifest.json.sha256 preflight.json preflight.json.sha256].freeze
PAGE_KEYS = %w[api_version body_sha256 etag last_modified link media_type phase protocol request status].freeze

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

  def self.page_path(path, page)
    separator = path.include?("?") ? "&" : "?"
    "#{path}#{separator}per_page=100&page=#{page}"
  end

  def self.link_relations(header)
    return {} unless header

    header.split(",").each_with_object({}) do |part, relations|
      match = part.match(/\A\s*<([^>]+)>;\s*rel="([^"]+)"\s*\z/)
      raise "invalid pagination Link header" unless match

      match[2].split.each do |relation|
        raise "duplicate pagination Link relation #{relation}" if relations.key?(relation)

        relations[relation] = api_path(match[1])
      end
    end
  end

  def self.api_path(url)
    uri = URI(url)
    raise "pagination link uses unexpected host #{uri.host.inspect}" unless uri.host == "api.github.com"

    path = uri.path.sub(%r{\A/}, "")
    uri.query ? "#{path}?#{uri.query}" : path
  end

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
    request_path = self.class.page_path(path, 1)
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

      next_path = self.class.link_relations(headers["link"])["next"]
      break if !next_path && (batch.length < 100 || expected_total == items.length)

      page += 1
      request_path = next_path || self.class.page_path(path, page)
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
      @pages << page.merge("phase" => @phase, "status" => status, "etag" => headers["etag"], "last_modified" => headers["last-modified"], "link" => headers["link"], "api_version" => headers["x-github-api-version-selected"], "media_type" => headers["x-github-media-type"])
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
      "link" => headers["link"],
      "api_version" => headers["x-github-api-version-selected"],
      "media_type" => headers["x-github-media-type"],
      "body_sha256" => Digest::SHA256.hexdigest(Canonical.dump(body))
    }
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
    "schema" => PREFLIGHT_SCHEMA,
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

def read_canonical_json(path)
  contents = File.binread(path)
  object = JSON.parse(contents)
  raise "#{File.basename(path)} is not canonical JSON" unless contents == Canonical.dump(object)

  object
rescue JSON::ParserError => error
  raise "#{File.basename(path)} is invalid JSON: #{error.message}"
end

def verify_sidecar(bundle, filename)
  sidecar = "#{filename}.sha256"
  contents = File.binread(File.join(bundle, sidecar))
  match = contents.match(/\A([0-9a-f]{64})  #{Regexp.escape(filename)}\n\z/)
  raise "#{sidecar} is invalid" unless match

  actual = Digest::SHA256.file(File.join(bundle, filename)).hexdigest
  raise "#{filename} digest mismatch" unless actual == match[1]

  actual
end

def collection_scope(request, repo)
  path, query = request.split("?", 2)
  relative = path[%r{\Arepos/#{Regexp.escape(repo)}/(.+)\z}, 1] || path[%r{\Arepositories/[1-9][0-9]*/(.+)\z}, 1]
  return unless relative

  filters = URI.decode_www_form(query.to_s).reject { |key, _| %w[after before page per_page].include?(key) }.sort
  [relative, filters]
rescue ArgumentError
  nil
end

def verify_page_inventory!(pages, graphql_media_type, graph, repo, link_provenance:)
  raise "manifest page inventory is empty" unless pages.is_a?(Array) && !pages.empty?

  pages.each do |page|
    unless page.is_a?(Hash) && page.keys.sort == PAGE_KEYS &&
           %w[capture stability_check].include?(page["phase"]) &&
           %w[graphql rest].include?(page["protocol"]) &&
           !page["request"].to_s.empty? && page["body_sha256"].to_s.match?(/\A[0-9a-f]{64}\z/) &&
           (page["link"].nil? || page["link"].is_a?(String))
      raise "manifest page inventory entry is invalid"
    end
    expected_status = page["phase"] == "stability_check" && page["protocol"] == "rest" ? 304 : 200..299
    raise "manifest page status is invalid" unless expected_status === page["status"]
  end

  endpoint_set = lambda do |phase|
    pages.select { |page| page["phase"] == phase }.map { |page| [page["protocol"], page["request"]] }
  end
  captured = endpoint_set.call("capture")
  stable = endpoint_set.call("stability_check")
  unless captured.any? && captured.uniq.length == captured.length && stable.uniq.length == stable.length && captured.sort == stable.sort
    raise "manifest page inventory phases do not match"
  end
  captured_pages = pages.select { |page| page["phase"] == "capture" }.to_h { |page| [[page["protocol"], page["request"]], page] }
  pages.select { |page| page["phase"] == "stability_check" }.each do |page|
    original = captured_pages.fetch([page["protocol"], page["request"]])
    raise "manifest page body identity changed" unless page["body_sha256"] == original["body_sha256"]
    next unless page["protocol"] == "rest"

    if original["etag"].to_s.empty? || page["etag"] != original["etag"]
      raise "manifest REST ETag provenance is invalid"
    end
  end

  graphql_media_types = pages.select { |page| page["protocol"] == "graphql" }.map { |page| page["media_type"] }
  unless graphql_media_types.any? && graphql_media_types.all? { |media_type| !media_type.to_s.empty? } && graphql_media_types.uniq == [graphql_media_type]
    raise "manifest GraphQL media type does not match page inventory"
  end
  rest_pages = pages.select { |page| page["phase"] == "capture" && page["protocol"] == "rest" }
  unless rest_pages.any? && rest_pages.all? { |page| page["api_version"] == GitHubReadOnly::API_VERSION && !page["media_type"].to_s.empty? }
    raise "manifest REST provenance is incomplete"
  end

  collections = []
  paginated = lambda do |path, count, known_total = false|
    page_count = known_total ? [1, (count + 99) / 100].max : count / 100 + 1
    start = GitHubReadOnly.page_path(path, 1)
    collections << [path, start, page_count, collection_scope(start, repo)]
  end
  items = graph.fetch("items")
  expected_rest = []
  paginated.call("repos/#{repo}/labels", graph.fetch("label_catalog").length)
  paginated.call("repos/#{repo}/issues?state=all&sort=created&direction=asc", items.length)
  paginated.call("repos/#{repo}/issues/comments?sort=created&direction=asc", items.sum { |item| item.fetch("comments").length })
  paginated.call("repos/#{repo}/issues/events", items.sum { |item| item.fetch("events").length })
  items.each do |item|
    number = item.fetch("number")
    expected_rest << "repos/#{repo}/issues/#{number}"
    next unless item.fetch("kind") == "pull_request"

    pull_request = item.fetch("pull_request")
    head_sha = pull_request.dig("metadata", "head", "sha")
    expected_rest << "repos/#{repo}/pulls/#{number}"
    %w[commits files reviews].each do |field|
      paginated.call("repos/#{repo}/pulls/#{number}/#{field}", pull_request.fetch(field).length)
    end
    paginated.call("repos/#{repo}/pulls/#{number}/comments", pull_request.fetch("review_comments").length)
    paginated.call("repos/#{repo}/commits/#{head_sha}/check-runs?filter=all", pull_request.dig("check_runs", "total_count"), true)
    paginated.call("repos/#{repo}/commits/#{head_sha}/statuses", pull_request.fetch("statuses").length)
  end
  rest_by_request = rest_pages.to_h { |page| [page.fetch("request"), page] }
  collection_requests = collections.flat_map do |path, start, page_count, scope|
    if link_provenance
      current = start
      (0...page_count).map do |index|
        page = rest_by_request.fetch(current) { raise "manifest REST pagination chain is incomplete" }
        relations = GitHubReadOnly.link_relations(page["link"])
        raise "manifest REST Link scope is invalid" unless relations.values.all? { |target| collection_scope(target, repo) == scope }

        if index + 1 == page_count
          raise "manifest REST pagination chain has an extra page" if relations["next"]
        else
          current = relations["next"] || GitHubReadOnly.page_path(path, index + 2)
        end
        page.fetch("request")
      end
    else
      requests = rest_pages.each_with_object([]) do |page, result|
        result << page.fetch("request") if collection_scope(page.fetch("request"), repo) == scope
      end
      unless requests.length == page_count && requests.include?(start)
        raise "legacy manifest REST page inventory does not match preflight"
      end
      requests
    end
  end
  raise "manifest REST page inventory overlaps collections" unless collection_requests.uniq.length == collection_requests.length
  expected_rest.concat(collection_requests)
  actual_rest = rest_pages.map { |page| page.fetch("request") }
  raise "manifest REST page inventory does not match preflight" unless actual_rest.sort == expected_rest.sort

  if link_provenance
    rest_pages.reject { |page| collection_requests.include?(page.fetch("request")) }.each do |page|
      raise "manifest detail page unexpectedly has Link provenance" if page["link"]
    end
  end

  graphql_requests = pages.select { |page| page["phase"] == "capture" && page["protocol"] == "graphql" }.map { |page| page.fetch("request") }
  issue_count = items.count { |item| item.fetch("kind") == "issue" }
  graphql_page_count = [1, (issue_count + 49) / 50].max
  graphql_prefix = "IssueLifecycleRelationships:"
  unless graphql_requests.length == graphql_page_count && graphql_requests.count("#{graphql_prefix}START") == 1 &&
         graphql_requests.all? { |request| request.start_with?(graphql_prefix) && request.length > graphql_prefix.length }
    raise "manifest GraphQL page inventory does not match preflight"
  end
end

def relationship_reference?(reference)
  reference.is_a?(Hash) && reference.keys.sort == %w[number repository url] &&
    reference["number"].is_a?(Integer) && reference["number"].positive? && !reference["url"].to_s.empty? &&
    reference["repository"].is_a?(Hash) && reference["repository"].keys == ["nameWithOwner"] &&
    !reference.dig("repository", "nameWithOwner").to_s.empty?
end

def unique_identity_fields?(entries, *fields)
  return false unless entries.is_a?(Array) && entries.all? { |entry| entry.is_a?(Hash) }

  fields.all? do |field|
    identities = entries.map { |entry| entry[field] }
    identities.all? { |identity| identity && !identity.to_s.empty? } && identities.uniq.length == identities.length
  end
end

def api_url_path?(url, expected_path)
  uri = URI(url.to_s)
  uri.is_a?(URI::HTTPS) && !uri.host.to_s.empty? && uri.path == expected_path
rescue URI::InvalidURIError
  false
end

def issue_object_scope?(object, repo, number)
  url = object["issue_url"] || object.dig("issue", "url")
  api_url_path?(url, "/repos/#{repo}/issues/#{number}")
end

def pull_request_object_scope?(object, repo, number)
  url = object["pull_request_url"] || object.dig("_links", "pull_request", "href")
  api_url_path?(url, "/repos/#{repo}/pulls/#{number}")
end

def validate_graph_and_count(graph, repo)
  unless graph.is_a?(Hash) && graph.keys.sort == %w[items label_catalog repository schema] && graph["schema"] == PREFLIGHT_SCHEMA
    raise "preflight schema is invalid"
  end
  repository = graph.fetch("repository")
  unless repository.is_a?(Hash) && repository.keys.sort == %w[default_branch default_branch_sha full_name id] &&
         repository["full_name"] == repo && !repository["id"].to_s.empty? &&
         !repository["default_branch"].to_s.empty? && repository["default_branch_sha"].to_s.match?(/\A[0-9a-f]{40}\z/i)
    raise "preflight repository identity is invalid"
  end

  labels = graph.fetch("label_catalog")
  items = graph.fetch("items")
  raise "preflight collections are invalid" unless labels.is_a?(Array) && items.is_a?(Array)
  unless labels.all? { |label| label.is_a?(Hash) && label["id"].is_a?(Integer) && !label["name"].to_s.empty? } &&
         labels.map { |label| label["id"] }.uniq.length == labels.length &&
         labels.map { |label| label["name"] }.uniq.length == labels.length
    raise "preflight label catalog is invalid"
  end
  label_catalog = labels.to_h { |label| [label.fetch("id"), label.fetch("name")] }

  numbers = []
  issue_count = 0
  pull_request_count = 0
  comment_count = 0
  event_count = 0
  comment_ids = []
  event_ids = []
  items.each do |item|
    raise "preflight item is invalid" unless item.is_a?(Hash) && item["number"].is_a?(Integer) && item["number"].positive?

    number = item["number"]
    metadata = item["issue"]
    comments = item["comments"]
    events = item["events"]
    unless metadata.is_a?(Hash) && metadata["number"] == number && metadata.key?("body") &&
           api_url_path?(metadata["url"], "/repos/#{repo}/issues/#{number}") &&
           %w[open closed].include?(metadata["state"]) && metadata["labels"].is_a?(Array) &&
           metadata["assignees"].is_a?(Array) && metadata["comments"] == comments&.length &&
           unique_identity_fields?(metadata["labels"], "id", "name") &&
           metadata["labels"].all? { |label| label_catalog[label["id"]] == label["name"] } &&
           unique_identity_fields?(metadata["assignees"], "id", "login") &&
           unique_identity_fields?(comments, "id") && comments.all? { |comment| issue_object_scope?(comment, repo, number) } &&
           unique_identity_fields?(events, "id") && events.all? { |event| issue_object_scope?(event, repo, number) }
      raise "preflight item ##{number} metadata is invalid"
    end
    case item["kind"]
    when "issue"
      relationships = item["relationships"]
      relationship_lists = %w[blocked_by blocking closed_by_pull_requests sub_issues]
      unless item.keys.sort == %w[comments events issue kind number relationships] &&
             relationships.is_a?(Hash) &&
             relationships.keys.sort == %w[blocked_by blocking closed_by_pull_requests number parent sub_issues] &&
             relationships["number"] == number &&
             (!relationships["parent"] || relationship_reference?(relationships["parent"])) &&
             relationship_lists.all? do |key|
               relationships[key].is_a?(Array) && relationships[key].all? { |reference| relationship_reference?(reference) }
             end
        raise "preflight Issue ##{number} schema is invalid"
      end

      issue_count += 1
    when "pull_request"
      pull_request = item["pull_request"]
      unless item.keys.sort == %w[comments events issue kind number pull_request] && pull_request.is_a?(Hash) &&
             pull_request.keys.sort == %w[check_runs commits files metadata review_comments reviews statuses] &&
             pull_request["metadata"].is_a?(Hash) && pull_request.dig("metadata", "number") == number &&
             api_url_path?(pull_request.dig("metadata", "url"), "/repos/#{repo}/pulls/#{number}") &&
             pull_request.dig("metadata", "head", "sha").to_s.match?(/\A[0-9a-f]{40}\z/i) &&
             pull_request.dig("metadata", "base", "sha").to_s.match?(/\A[0-9a-f]{40}\z/i) &&
             unique_identity_fields?(pull_request["commits"], "sha") && pull_request["commits"].all? { |commit| commit["sha"].match?(/\A[0-9a-f]{40}\z/i) } &&
             pull_request["commits"].any? { |commit| commit["sha"] == pull_request.dig("metadata", "head", "sha") } &&
             unique_identity_fields?(pull_request["files"], "filename") &&
             unique_identity_fields?(pull_request["reviews"], "id") && pull_request["reviews"].all? { |review| pull_request_object_scope?(review, repo, number) } &&
             unique_identity_fields?(pull_request["review_comments"], "id") && pull_request["review_comments"].all? { |comment| pull_request_object_scope?(comment, repo, number) } &&
             unique_identity_fields?(pull_request["statuses"], "id") && pull_request["statuses"].all? { |status| status["sha"] == pull_request.dig("metadata", "head", "sha") } &&
             pull_request["commits"].length == pull_request.dig("metadata", "commits") &&
             pull_request["files"].length == pull_request.dig("metadata", "changed_files") &&
             pull_request["review_comments"].length == pull_request.dig("metadata", "review_comments") &&
             pull_request["check_runs"].is_a?(Hash) && pull_request["check_runs"].keys.sort == %w[check_runs total_count] &&
             unique_identity_fields?(pull_request.dig("check_runs", "check_runs"), "id") &&
             pull_request.dig("check_runs", "check_runs").all? { |run| run["head_sha"] == pull_request.dig("metadata", "head", "sha") } &&
             pull_request.dig("check_runs", "total_count") == pull_request.dig("check_runs", "check_runs").length
        raise "preflight PR ##{number} schema is invalid"
      end

      pull_request_count += 1
    else
      raise "preflight item ##{number} kind is invalid"
    end
    numbers << number
    comment_ids.concat(comments.map { |comment| comment.fetch("id") })
    event_ids.concat(events.map { |event| event.fetch("id") })
    comment_count += comments.length
    event_count += events.length
  end
  raise "preflight contains duplicate item numbers" unless numbers.uniq.length == numbers.length
  raise "preflight contains duplicate comment or event identities" unless comment_ids.uniq.length == comment_ids.length && event_ids.uniq.length == event_ids.length

  {
    "issues" => issue_count,
    "pull_requests" => pull_request_count,
    "labels" => labels.length,
    "issue_comments" => comment_count,
    "issue_events" => event_count
  }
end

def verify_bundle(bundle, repo, controller)
  unless File.directory?(bundle) && !File.symlink?(bundle) && Dir.children(bundle).sort == BUNDLE_FILES
    raise "bundle file set is invalid"
  end
  BUNDLE_FILES.each do |filename|
    path = File.join(bundle, filename)
    raise "bundle entry #{filename} is not a regular file" unless File.file?(path) && !File.symlink?(path)
  end

  preflight_sha = verify_sidecar(bundle, "preflight.json")
  verify_sidecar(bundle, "manifest.json")
  graph = read_canonical_json(File.join(bundle, "preflight.json"))
  manifest = read_canonical_json(File.join(bundle, "manifest.json"))
  expected_manifest_keys = %w[api captured_at controller counts default_branch default_branch_sha exclusions pages preflight repository schema target]
  unless manifest.is_a?(Hash) && manifest.keys.sort == expected_manifest_keys && manifest["schema"] == MANIFEST_SCHEMA
    raise "manifest schema is invalid"
  end
  unless manifest["repository"] == repo && manifest["controller"] == controller &&
         manifest["target"] == "GitHub Issue and pull request metadata in #{repo}" && manifest["exclusions"] == EVIDENCE_EXCLUSIONS
    raise "manifest scope is invalid"
  end
  begin
    captured_at = manifest.fetch("captured_at")
    raise ArgumentError unless captured_at.is_a?(String) && captured_at.end_with?("Z") && Time.iso8601(captured_at).utc_offset.zero?
  rescue ArgumentError
    raise "manifest capture time is not UTC"
  end

  api = manifest.fetch("api")
  unless api.is_a?(Hash) && api.keys.sort == %w[graphql_media_type rest_accept rest_version] &&
         api["rest_version"] == GitHubReadOnly::API_VERSION && api["rest_accept"] == GitHubReadOnly::REST_ACCEPT &&
         !api["graphql_media_type"].to_s.empty?
    raise "manifest API provenance is invalid"
  end
  pages = manifest.fetch("pages")
  link_provenance = pages.is_a?(Array) && pages.all? { |page| page.is_a?(Hash) && page.key?("link") }
  if pages.is_a?(Array)
    has_link = pages.any? { |page| page.is_a?(Hash) && page.key?("link") }
    raise "manifest page Link provenance is inconsistent" if has_link && !link_provenance

    manifest["pages"] = pages.map { |page| page.is_a?(Hash) && !page.key?("link") ? page.merge("link" => nil) : page }
  end
  counts = validate_graph_and_count(graph, repo)
  verify_page_inventory!(manifest.fetch("pages"), api.fetch("graphql_media_type"), graph, repo, link_provenance: link_provenance)
  raise "manifest counts do not match preflight" unless manifest["counts"] == counts

  repository = graph.fetch("repository")
  unless manifest["default_branch"] == repository["default_branch"] &&
         manifest["default_branch_sha"] == repository["default_branch_sha"] &&
         manifest["preflight"] == { "file" => "preflight.json", "sha256" => preflight_sha }
    raise "manifest preflight identity is invalid"
  end
  [graph, manifest, link_provenance]
rescue Errno::EACCES, Errno::ENOENT => error
  raise "bundle read failed: #{error.message}"
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
    "schema" => MANIFEST_SCHEMA,
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

def verify(options)
  repo, controller, bundle = options.values_at(:repo, :controller, :bundle)
  raise "--repo OWNER/REPO is required" if repo.to_s.empty?
  raise "--controller NAME is required" if controller.to_s.strip.empty?
  raise "--bundle DIR is required" if bundle.to_s.empty?

  graph, manifest, link_provenance = verify_bundle(bundle, repo, controller)
  if options[:live]
    live_graph, live_counts, live_branch, live_branch_sha, live_pages = capture_graph(repo)
    verify_page_inventory!(live_pages, manifest.dig("api", "graphql_media_type"), live_graph, repo, link_provenance: true)
    raise "live verification drift: Issue/PR graph changed" unless Canonical.value(live_graph) == Canonical.value(graph)
    raise "live verification drift: counts changed" unless live_counts == manifest["counts"]
    unless live_branch == manifest["default_branch"] && live_branch_sha == manifest["default_branch_sha"]
      raise "live verification drift: default branch changed"
    end
    comparison_pages = link_provenance ? live_pages : live_pages.map { |page| page.merge("link" => nil) }
    unless Canonical.value(comparison_pages) == Canonical.value(manifest["pages"])
      raise "live verification drift: page/header inventory changed"
    end
  end
  suffix = options[:live] ? " with live readback" : ""
  puts "verified #{repo} at #{manifest.fetch("default_branch_sha")}#{suffix} <- #{bundle}"
end

command = ARGV.shift
options = {}
parser = OptionParser.new do |flags|
  flags.banner = "Usage: issue-lifecycle-preflight.rb (capture|verify) --repo OWNER/REPO --controller NAME (--output|--bundle) DIR"
  flags.on("--repo OWNER/REPO", "Repository to read") { |value| options[:repo] = value }
  flags.on("--controller NAME", "Named evidence controller") { |value| options[:controller] = value }
  flags.on("--output DIR", "New evidence bundle directory") { |value| options[:output] = value }
  flags.on("--bundle DIR", "Existing evidence bundle directory") { |value| options[:bundle] = value }
  flags.on("--live", "Independently compare the bundle with live GitHub reads") { options[:live] = true }
end

begin
  parser.parse!
  raise "unexpected arguments: #{ARGV.join(" ")}" unless ARGV.empty?
  case command
  when "capture"
    raise "--bundle is only valid for verify" if options[:bundle]
    raise "--live is only valid for verify" if options[:live]

    capture(options)
  when "verify"
    raise "--output is only valid for capture" if options[:output]

    verify(options)
  else
    raise "unsupported operation #{command.inspect}; expected capture or verify"
  end
rescue StandardError => error
  warn "ERROR issue lifecycle preflight: #{error.message}"
  exit 2
end
