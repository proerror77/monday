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
FORWARD_PLAN_SCHEMA = "monday.issue_lifecycle_forward_plan.v1"
REVERSE_PLAN_SCHEMA = "monday.issue_lifecycle_reverse_plan.v1"
RECEIPT_SCHEMA = "monday.issue_lifecycle_receipt.v1"
BUNDLE_FILES = %w[manifest.json manifest.json.sha256 preflight.json preflight.json.sha256].freeze
PAGE_KEYS = %w[api_version body_sha256 etag last_modified link media_type phase protocol request status].freeze
PLAN_STATE_KEYS = %w[assignees blocked_by body labels parent state].freeze
DERIVED_ISSUE_METADATA_KEYS = %w[assignee assignees body closed_at closed_by comments issue_dependencies_summary labels parent_issue_url state state_reason sub_issues_summary updated_at].freeze

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

def parse_canonical_json(contents, name)
  object = JSON.parse(contents)
  raise "#{name} is not canonical JSON" unless contents.b == Canonical.dump(object).b

  object
rescue JSON::ParserError => error
  raise "#{name} is invalid JSON: #{error.message}"
end

def read_canonical_document(path)
  contents = File.binread(path)
  [parse_canonical_json(contents, File.basename(path)), contents, Digest::SHA256.hexdigest(contents)]
end

def verify_sidecar(bundle, filename)
  sidecar = "#{filename}.sha256"
  sidecar_contents = File.binread(File.join(bundle, sidecar))
  match = sidecar_contents.match(/\A([0-9a-f]{64})  #{Regexp.escape(filename)}\n\z/)
  raise "#{sidecar} is invalid" unless match

  contents = File.binread(File.join(bundle, filename))
  actual = Digest::SHA256.hexdigest(contents)
  raise "#{filename} digest mismatch" unless actual == match[1]

  [contents, actual]
end

def collection_scope(request, repo)
  path, query = request.split("?", 2)
  relative = path[%r{\Arepos/#{Regexp.escape(repo)}/(.+)\z}, 1] || path[%r{\Arepositories/[1-9][0-9]*/(.+)\z}, 1]
  return unless relative

  filters = URI.decode_www_form(query.to_s).reject { |key, _| %w[after before page].include?(key) }.sort
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
      raise "legacy manifest cannot verify multi-page REST provenance" if page_count > 1
      requests = rest_pages.each_with_object([]) do |page, result|
        result << page.fetch("request") if collection_scope(page.fetch("request"), repo) == scope
      end
      raise "legacy manifest REST page inventory does not match preflight" unless requests == [start]
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

def relationship_reference?(reference, kind)
  return false unless reference.is_a?(Hash) && reference.keys.sort == %w[number repository url] &&
                      reference["number"].is_a?(Integer) && reference["number"].positive? && reference["repository"].is_a?(Hash) &&
                      reference["repository"].keys == ["nameWithOwner"]
  repo = reference.dig("repository", "nameWithOwner")
  url = URI(reference["url"].to_s)
  repo.to_s.match?(%r{\A[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\z}) && url.is_a?(URI::HTTPS) && url.host == "github.com" &&
    url.port == 443 && !url.userinfo && !url.query && !url.fragment && url.path == "/#{repo}/#{kind}/#{reference["number"]}"
rescue URI::InvalidURIError
  false
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
  uri.is_a?(URI::HTTPS) && uri.host == "api.github.com" && uri.port == 443 && !uri.userinfo && !uri.query && !uri.fragment && uri.path == expected_path
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
      relationship_lists = { "blocked_by" => "issues", "blocking" => "issues", "closed_by_pull_requests" => "pull", "sub_issues" => "issues" }
      unless item.keys.sort == %w[comments events issue kind number relationships] &&
             relationships.is_a?(Hash) &&
             relationships.keys.sort == %w[blocked_by blocking closed_by_pull_requests number parent sub_issues] &&
             relationships["number"] == number &&
             (!relationships["parent"] || relationship_reference?(relationships["parent"], "issues")) &&
             relationship_lists.all? do |key, kind|
               relationships[key].is_a?(Array) && relationships[key].all? { |reference| relationship_reference?(reference, kind) }
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
             unique_identity_fields?(pull_request["statuses"], "id") &&
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

  preflight_contents, preflight_sha = verify_sidecar(bundle, "preflight.json")
  manifest_contents, manifest_sha = verify_sidecar(bundle, "manifest.json")
  graph = parse_canonical_json(preflight_contents, "preflight.json")
  manifest = parse_canonical_json(manifest_contents, "manifest.json")
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
  [graph, manifest, link_provenance, { "manifest_sha256" => manifest_sha, "preflight_sha256" => preflight_sha }]
rescue Errno::EACCES, Errno::ENOENT => error
  raise "bundle read failed: #{error.message}"
end

def issue_plan_state(item)
  metadata = item.fetch("issue")
  relationships = item.fetch("relationships")
  Canonical.value(
    "assignees" => metadata.fetch("assignees").map { |assignee| assignee.fetch("login") },
    "blocked_by" => relationships.fetch("blocked_by").map { |reference| reference.fetch("number") },
    "body" => metadata["body"],
    "labels" => metadata.fetch("labels").map { |label| label.fetch("name") },
    "parent" => relationships["parent"] && relationships.dig("parent", "number"),
    "state" => { "reason" => metadata["state_reason"], "value" => metadata.fetch("state") }
  )
end

def github_login?(login)
  login.is_a?(String) && login.match?(/\A(?=.{1,39}\z)[A-Za-z0-9]+(?:-[A-Za-z0-9]+)*\z/)
end

def valid_utc_timestamp?(value)
  value.is_a?(String) && value.end_with?("Z") && Time.iso8601(value).utc_offset.zero?
rescue ArgumentError
  false
end

def validate_plan_relationship_scope!(items, repo, prefix)
  items.each do |number, item|
    relationships = item.fetch("relationships")
    references = [relationships["parent"], *relationships.fetch("sub_issues"),
                  *relationships.fetch("blocked_by"), *relationships.fetch("blocking")].compact
    unless references.all? { |reference| reference.dig("repository", "nameWithOwner") == repo }
      raise "#{prefix} Issue ##{number} relationship scope is unsupported"
    end
  end
end

def validate_issue_metadata_derivations!(item, states, repo, prefix)
  number = item.fetch("number")
  metadata = item.fetch("issue")
  relationships = item.fetch("relationships")
  state = states.fetch(number).dig("state", "value")

  if metadata.key?("assignee")
    assignee = metadata["assignee"]
    valid_assignee = assignee.nil? ? metadata.fetch("assignees").empty? :
      assignee.is_a?(Hash) && metadata.fetch("assignees").any? { |entry| Canonical.value(entry) == Canonical.value(assignee) }
    raise "#{prefix} Issue ##{number} assignee summary is inconsistent" unless valid_assignee
  end

  if metadata.key?("parent_issue_url")
    parent = states.fetch(number)["parent"]
    expected_parent_url = parent && "https://api.github.com/repos/#{repo}/issues/#{parent}"
    raise "#{prefix} Issue ##{number} parent summary is inconsistent" unless metadata["parent_issue_url"] == expected_parent_url
  end

  if metadata.key?("sub_issues_summary")
    sub_issues = relationships.fetch("sub_issues").map { |reference| reference.fetch("number") }
    completed = sub_issues.count { |related| states.fetch(related).dig("state", "value") == "closed" }
    expected = {
      "completed" => completed,
      "percent_completed" => sub_issues.empty? ? 0 : completed * 100 / sub_issues.length,
      "total" => sub_issues.length
    }
    raise "#{prefix} Issue ##{number} sub-issues summary is inconsistent" unless metadata["sub_issues_summary"] == expected
  end

  if metadata.key?("issue_dependencies_summary")
    blocked_by = relationships.fetch("blocked_by").map { |reference| reference.fetch("number") }
    blocking = relationships.fetch("blocking").map { |reference| reference.fetch("number") }
    expected = {
      "blocked_by" => blocked_by.count { |related| states.fetch(related).dig("state", "value") == "open" },
      "blocking" => blocking.count { |related| states.fetch(related).dig("state", "value") == "open" },
      "total_blocked_by" => blocked_by.length,
      "total_blocking" => blocking.length
    }
    raise "#{prefix} Issue ##{number} dependency summary is inconsistent" unless metadata["issue_dependencies_summary"] == expected
  end

  if metadata.key?("closed_at")
    valid_closed_at = state == "open" ? metadata["closed_at"].nil? : valid_utc_timestamp?(metadata["closed_at"])
    raise "#{prefix} Issue ##{number} closed_at is inconsistent" unless valid_closed_at
  end
  if metadata.key?("closed_by")
    closed_by = metadata["closed_by"]
    valid_closed_by = state == "open" ? closed_by.nil? : closed_by.nil? || closed_by.is_a?(Hash) && !closed_by["login"].to_s.empty?
    raise "#{prefix} Issue ##{number} closed_by is inconsistent" unless valid_closed_by
  end
  if metadata.key?("updated_at") && !valid_utc_timestamp?(metadata["updated_at"])
    raise "#{prefix} Issue ##{number} updated_at is invalid"
  end
end

def validate_plan_state!(state, number, issue_numbers, label_names, target:)
  unless state.is_a?(Hash) && state.keys.sort == PLAN_STATE_KEYS && (state["body"].nil? || state["body"].is_a?(String))
    raise "forward plan Issue ##{number} state schema is invalid"
  end

  labels = state["labels"]
  assignees = state["assignees"]
  blockers = state["blocked_by"]
  parent = state["parent"]
  issue_state = state["state"]
  unless labels.is_a?(Array) && labels.all? { |label| label.is_a?(String) && !label.empty? } &&
         labels.uniq.length == labels.length && (labels - label_names).empty? &&
         assignees.is_a?(Array) && assignees.all? { |login| github_login?(login) } &&
         assignees.uniq.length == assignees.length && blockers.is_a?(Array) &&
         blockers.all? { |blocker| issue_numbers.include?(blocker) && blocker != number } && blockers.uniq.length == blockers.length &&
         (parent.nil? || issue_numbers.include?(parent) && parent != number) &&
         issue_state.is_a?(Hash) && issue_state.keys.sort == %w[reason value]
    raise "forward plan Issue ##{number} state identity is invalid"
  end

  value = issue_state["value"]
  reason = issue_state["reason"]
  valid_reason = value == "open" ? [nil, "reopened"].include?(reason) :
    value == "closed" && [nil, "completed", "duplicate", "not_planned"].include?(reason)
  valid_reason &&= !target || value != "closed" || !reason.nil?
  raise "forward plan Issue ##{number} state transition is invalid" unless valid_reason
end

def validate_plan_edges!(states)
  edges = {
    "parent" => states.transform_values { |state| [state["parent"]].compact },
    "blocked_by" => states.transform_values { |state| state.fetch("blocked_by") }
  }
  edges.each do |field, graph|
    visited = {}
    visiting = {}
    visit = lambda do |number|
      raise "forward plan #{field} relationships contain a cycle" if visiting[number]
      return if visited[number]

      visiting[number] = true
      graph.fetch(number).each { |related| visit.call(related) }
      visiting.delete(number)
      visited[number] = true
    end
    graph.each_key { |number| visit.call(number) }
  end
end

def expected_derived_relationships(states)
  sub_issues = Hash.new { |hash, key| hash[key] = [] }
  blocking = Hash.new { |hash, key| hash[key] = [] }
  states.each do |number, state|
    sub_issues[state["parent"]] << number if state["parent"]
    state.fetch("blocked_by").each { |blocker| blocking[blocker] << number }
  end
  [sub_issues, blocking]
end

def validate_derived_relationships!(items, states, prefix)
  expected_sub_issues, expected_blocking = expected_derived_relationships(states)
  items.each do |number, item|
    actual_sub_issues = item.dig("relationships", "sub_issues").map { |reference| reference.fetch("number") }
    actual_blocking = item.dig("relationships", "blocking").map { |reference| reference.fetch("number") }
    unless actual_sub_issues.sort == expected_sub_issues[number].sort && actual_blocking.sort == expected_blocking[number].sort
      raise "#{prefix} Issue ##{number} derived relationship drift"
    end
  end
end

def reverse_plan(graph, manifest, manifest_sha, forward_path, repo, controller)
  forward, _, forward_sha = read_canonical_document(forward_path)
  expected_keys = %w[controller default_branch default_branch_sha operations preflight_manifest_sha256 preflight_sha256 repository schema target]
  unless forward.is_a?(Hash) && forward.keys.sort == expected_keys && forward["schema"] == FORWARD_PLAN_SCHEMA &&
         forward["repository"] == repo && forward["controller"] == controller && forward["target"] == manifest["target"] &&
         forward["preflight_manifest_sha256"] == manifest_sha &&
         forward["preflight_sha256"] == manifest.dig("preflight", "sha256") &&
         forward["default_branch"] == manifest["default_branch"] && forward["default_branch_sha"] == manifest["default_branch_sha"]
    raise "forward plan identity is invalid"
  end

  issues = graph.fetch("items").select { |item| item["kind"] == "issue" }.to_h { |item| [item.fetch("number"), item] }
  issue_numbers = issues.keys
  label_names = graph.fetch("label_catalog").map { |label| label.fetch("name") }
  validate_plan_relationship_scope!(issues, repo, "preflight")
  states = issues.transform_values { |item| issue_plan_state(item) }
  validate_plan_edges!(states)
  validate_derived_relationships!(issues, states, "preflight")
  issues.each_value { |item| validate_issue_metadata_derivations!(item, states, repo, "preflight") }
  targets = states.dup
  operations = forward["operations"]
  raise "forward plan operations are invalid" unless operations.is_a?(Array) && !operations.empty?

  seen = {}
  reverse_operations = operations.map do |operation|
    unless operation.is_a?(Hash) && operation.keys.sort == %w[number precondition target] && operation["number"].is_a?(Integer)
      raise "forward plan operation schema is invalid"
    end
    number = operation["number"]
    raise "forward plan references unknown Issue ##{number}" unless issues.key?(number)
    raise "forward plan contains duplicate Issue ##{number}" if seen[number]

    seen[number] = true
    validate_plan_state!(operation["precondition"], number, issue_numbers, label_names, target: false)
    validate_plan_state!(operation["target"], number, issue_numbers, label_names, target: true)
    unless Canonical.value(operation["precondition"]) == states.fetch(number)
      raise "forward plan Issue ##{number} precondition does not match preflight"
    end
    if Canonical.value(operation["target"]) == states.fetch(number)
      raise "forward plan Issue ##{number} operation is a no-op"
    end

    targets[number] = Canonical.value(operation["target"])
    { "number" => number, "precondition" => operation["target"], "target" => operation["precondition"] }
  end
  validate_plan_edges!(targets)

  reverse = {
    "controller" => controller,
    "default_branch" => manifest.fetch("default_branch"),
    "default_branch_sha" => manifest.fetch("default_branch_sha"),
    "forward_plan_sha256" => forward_sha,
    "operations" => reverse_operations,
    "preflight_manifest_sha256" => manifest_sha,
    "preflight_sha256" => manifest.dig("preflight", "sha256"),
    "repository" => repo,
    "schema" => REVERSE_PLAN_SCHEMA,
    "target" => manifest.fetch("target")
  }
  [reverse, forward, forward_sha]
end

def appended_provenance_ids(before, after, field, number)
  unless after.length >= before.length && Canonical.value(after.first(before.length), field) == Canonical.value(before, field)
    raise "post-state Issue ##{number} #{field} provenance is not append-only"
  end
  after.drop(before.length).map { |entry| entry.fetch("id") }
end

def verify_post_state!(before_graph, after_graph, forward)
  unless after_graph.fetch("repository") == before_graph.fetch("repository") &&
         Canonical.value(after_graph.fetch("label_catalog")) == Canonical.value(before_graph.fetch("label_catalog"))
    raise "post-state repository or label catalog drift"
  end

  before_items = before_graph.fetch("items").to_h { |item| [item.fetch("number"), item] }
  after_items = after_graph.fetch("items").to_h { |item| [item.fetch("number"), item] }
  raise "post-state Issue/PR inventory drift" unless after_items.keys.sort == before_items.keys.sort

  operations = forward.fetch("operations").to_h { |operation| [operation.fetch("number"), operation] }
  expected_states = before_items.select { |_, item| item["kind"] == "issue" }.transform_values { |item| issue_plan_state(item) }
  operations.each { |number, operation| expected_states[number] = Canonical.value(operation.fetch("target")) }
  after_issues = after_items.select { |_, item| item["kind"] == "issue" }
  repo = before_graph.dig("repository", "full_name")
  validate_plan_relationship_scope!(after_issues, repo, "post-state")
  validate_derived_relationships!(after_issues, expected_states, "post-state")
  after_issues.each_value { |item| validate_issue_metadata_derivations!(item, expected_states, repo, "post-state") }

  operation_receipts = []
  before_items.each do |number, before|
    after = after_items.fetch(number)
    raise "post-state item ##{number} kind drift" unless after["kind"] == before["kind"]
    if before["kind"] == "pull_request"
      raise "post-state PR ##{number} metadata drift" unless Canonical.value(after) == Canonical.value(before)
      next
    end

    unless issue_plan_state(after) == expected_states.fetch(number)
      raise "post-state Issue ##{number} does not match the forward plan"
    end
    before_metadata = before.fetch("issue").reject { |key, _| DERIVED_ISSUE_METADATA_KEYS.include?(key) }
    after_metadata = after.fetch("issue").reject { |key, _| DERIVED_ISSUE_METADATA_KEYS.include?(key) }
    unless Canonical.value(after_metadata) == Canonical.value(before_metadata)
      raise "post-state Issue ##{number} unsupported metadata drift"
    end
    before_issue = before.fetch("issue")
    after_issue = after.fetch("issue")
    unless DERIVED_ISSUE_METADATA_KEYS.all? { |key| before_issue.key?(key) == after_issue.key?(key) }
      raise "post-state Issue ##{number} derived metadata schema drift"
    end
    if before_issue["state"] == after_issue["state"] &&
       Canonical.value(before_issue.values_at("closed_at", "closed_by")) != Canonical.value(after_issue.values_at("closed_at", "closed_by"))
      raise "post-state Issue ##{number} closure metadata drift"
    end
    before_without_updated_at = before.merge("issue" => before_issue.reject { |key, _| key == "updated_at" })
    after_without_updated_at = after.merge("issue" => after_issue.reject { |key, _| key == "updated_at" })
    if before_issue["updated_at"] != after_issue["updated_at"]
      observable_change = Canonical.value(before_without_updated_at) != Canonical.value(after_without_updated_at)
      timestamps_advance = valid_utc_timestamp?(before_issue["updated_at"]) && valid_utc_timestamp?(after_issue["updated_at"]) &&
        Time.iso8601(after_issue["updated_at"]) >= Time.iso8601(before_issue["updated_at"])
      raise "post-state Issue ##{number} updated_at drift" unless observable_change && timestamps_advance
    end
    unless Canonical.value(after.dig("relationships", "closed_by_pull_requests")) ==
           Canonical.value(before.dig("relationships", "closed_by_pull_requests"))
      raise "post-state Issue ##{number} closing-reference drift"
    end
    comment_ids = appended_provenance_ids(before.fetch("comments"), after.fetch("comments"), "comments", number)
    event_ids = appended_provenance_ids(before.fetch("events"), after.fetch("events"), "events", number)
    operation = operations[number]
    if operation.nil?
      raise "post-state Issue ##{number} has unplanned provenance" unless comment_ids.empty? && event_ids.empty?
      next
    end
    operation_receipts << {
      "comment_ids" => comment_ids,
      "event_ids" => event_ids,
      "number" => number,
      "precondition_sha256" => Digest::SHA256.hexdigest(Canonical.dump(operation.fetch("precondition"))),
      "result" => "passed",
      "target_sha256" => Digest::SHA256.hexdigest(Canonical.dump(operation.fetch("target")))
    }
  end
  operation_receipts
end

def verify_receipt!(receipt, before_identity, after_identity, before_manifest, after_manifest, forward_sha, reverse_sha, operation_receipts, repo, controller)
  expected_keys = %w[api controller counts default_branch default_branch_sha forward_plan_sha256 operations pages postflight_manifest_sha256 postflight_sha256 preflight_manifest_sha256 preflight_sha256 repository reverse_plan_sha256 schema target]
  unless receipt.is_a?(Hash) && receipt.keys.sort == expected_keys && receipt["schema"] == RECEIPT_SCHEMA &&
         receipt["repository"] == repo && receipt["controller"] == controller && receipt["target"] == before_manifest["target"] &&
         receipt["preflight_sha256"] == before_manifest.dig("preflight", "sha256") &&
         receipt["postflight_sha256"] == after_manifest.dig("preflight", "sha256") &&
         receipt["preflight_manifest_sha256"] == before_identity["manifest_sha256"] &&
         receipt["postflight_manifest_sha256"] == after_identity["manifest_sha256"] &&
         receipt["forward_plan_sha256"] == forward_sha && receipt["reverse_plan_sha256"] == reverse_sha &&
         receipt["default_branch"] == after_manifest["default_branch"] && receipt["default_branch_sha"] == after_manifest["default_branch_sha"] &&
         receipt["api"] == after_manifest["api"] && receipt["pages"] == after_manifest["pages"] &&
         receipt["counts"] == after_manifest["counts"] && receipt["operations"] == Canonical.value(operation_receipts)
    raise "restoration receipt identity is invalid"
  end
end

def verify_live_bundle!(graph, manifest, repo, link_provenance, prefix)
  live_graph, live_counts, live_branch, live_branch_sha, live_pages = capture_graph(repo)
  verify_page_inventory!(live_pages, manifest.dig("api", "graphql_media_type"), live_graph, repo, link_provenance: true)
  raise "#{prefix}: Issue/PR graph changed" unless Canonical.value(live_graph) == Canonical.value(graph)
  raise "#{prefix}: counts changed" unless live_counts == manifest["counts"]
  unless live_branch == manifest["default_branch"] && live_branch_sha == manifest["default_branch_sha"]
    raise "#{prefix}: default branch changed"
  end
  comparison_pages = link_provenance ? live_pages : live_pages.map { |page| page.merge("link" => nil) }
  unless Canonical.value(comparison_pages) == Canonical.value(manifest["pages"])
    raise "#{prefix}: page/header inventory changed"
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
  verify_live_bundle!(graph, manifest, repo, link_provenance, "live verification drift") if options[:live]
  suffix = options[:live] ? " with live readback" : ""
  puts "verified #{repo} at #{manifest.fetch("default_branch_sha")}#{suffix} <- #{bundle}"
end

def plan_restore(options)
  repo, controller, bundle, forward_path = options.values_at(:repo, :controller, :bundle, :forward_plan)
  raise "--repo OWNER/REPO is required" if repo.to_s.empty?
  raise "--controller NAME is required" if controller.to_s.strip.empty?
  raise "--bundle DIR is required" if bundle.to_s.empty?
  raise "--forward-plan FILE is required" if forward_path.to_s.empty?

  post_paths = options.values_at(:reverse_plan, :receipt, :post_bundle)
  if post_paths.any? && !post_paths.all? { |path| !path.to_s.empty? }
    raise "--reverse-plan, --receipt, and --post-bundle are required together"
  end

  graph, manifest, link_provenance, identity = verify_bundle(bundle, repo, controller)
  plan, forward, forward_sha = reverse_plan(graph, manifest, identity.fetch("manifest_sha256"), forward_path, repo, controller)
  unless post_paths.any?
    verify_live_bundle!(graph, manifest, repo, link_provenance, "pre-mutation live drift")
    print Canonical.dump(plan)
    return
  end

  reverse_path, receipt_path, post_bundle = post_paths
  saved_plan, reverse_contents, reverse_sha = read_canonical_document(reverse_path)
  raise "saved reverse plan does not match the exact derived inverse" unless reverse_contents.b == Canonical.dump(plan).b && saved_plan == plan

  post_graph, post_manifest, post_link_provenance, post_identity = verify_bundle(post_bundle, repo, controller)
  unless post_manifest["default_branch"] == manifest["default_branch"] && post_manifest["default_branch_sha"] == manifest["default_branch_sha"]
    raise "post-state default branch drift"
  end
  operation_receipts = verify_post_state!(graph, post_graph, forward)
  receipt, = read_canonical_document(receipt_path)
  verify_receipt!(receipt, identity, post_identity, manifest, post_manifest, forward_sha, reverse_sha, operation_receipts, repo, controller)
  verify_live_bundle!(post_graph, post_manifest, repo, post_link_provenance, "restoration live drift")
  print reverse_contents
end

command = ARGV.shift
options = {}
parser = OptionParser.new do |flags|
  flags.banner = "Usage: issue-lifecycle-preflight.rb (capture|verify|plan-restore) --repo OWNER/REPO --controller NAME"
  flags.on("--repo OWNER/REPO", "Repository to read") { |value| options[:repo] = value }
  flags.on("--controller NAME", "Named evidence controller") { |value| options[:controller] = value }
  flags.on("--output DIR", "New evidence bundle directory") { |value| options[:output] = value }
  flags.on("--bundle DIR", "Existing evidence bundle directory") { |value| options[:bundle] = value }
  flags.on("--forward-plan FILE", "Canonical approved forward plan") { |value| options[:forward_plan] = value }
  flags.on("--reverse-plan FILE", "Exact saved reverse plan") { |value| options[:reverse_plan] = value }
  flags.on("--receipt FILE", "Canonical post-state receipt") { |value| options[:receipt] = value }
  flags.on("--post-bundle DIR", "Verified post-state evidence bundle") { |value| options[:post_bundle] = value }
  flags.on("--live", "Independently compare the bundle with live GitHub reads") { options[:live] = true }
end

begin
  parser.parse!
  raise "unexpected arguments: #{ARGV.join(" ")}" unless ARGV.empty?
  case command
  when "capture"
    raise "--bundle is only valid for verify" if options[:bundle]
    raise "--forward-plan is only valid for plan-restore" if options[:forward_plan]
    raise "restoration inputs are only valid for plan-restore" if options.values_at(:reverse_plan, :receipt, :post_bundle).any?
    raise "--live is only valid for verify" if options[:live]

    capture(options)
  when "verify"
    raise "--output is only valid for capture" if options[:output]
    raise "--forward-plan is only valid for plan-restore" if options[:forward_plan]
    raise "restoration inputs are only valid for plan-restore" if options.values_at(:reverse_plan, :receipt, :post_bundle).any?

    verify(options)
  when "plan-restore"
    raise "--output is only valid for capture" if options[:output]
    raise "--live is implicit for plan-restore" if options[:live]

    plan_restore(options)
  else
    raise "unsupported operation #{command.inspect}; expected capture, verify, or plan-restore"
  end
rescue StandardError => error
  warn "ERROR issue lifecycle preflight: #{error.message}"
  exit 2
end
