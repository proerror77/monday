#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
preflight="$repo_root/.github/scripts/issue-lifecycle-preflight.rb"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
test -f "$preflight"

fake_bin="$tmp_dir/bin"
mkdir -p "$fake_bin" "$tmp_dir/state"
cat >"$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

method=GET
path=
query=
if_none_match=
args=("$@")
for ((index = 0; index < ${#args[@]}; index++)); do
  case "${args[$index]}" in
    --method|-X) method="${args[$((index + 1))]}" ;;
    graphql) path=graphql; method=POST ;;
    If-None-Match:*) if_none_match="${args[$index]#If-None-Match: }" ;;
    query=*) query="${args[$index]#query=}" ;;
    repos/*|repositories/*) path="${args[$index]}" ;;
  esac
done
operation="${query%%[[:space:]]*}"
printf '%s\t%s\t%s\n' "$method" "$path" "$operation" >>"$TEST_API_LOG"
if [[ "$path" == graphql && "$operation" == mutation ]]; then
  echo "GraphQL mutation is forbidden" >&2
  exit 1
fi

bump() {
  local file="$TEST_FAKE_STATE/$1" count=0
  [[ ! -f "$file" ]] || count="$(<"$file")"
  printf '%s' "$((count + 1))" | tee "$file"
}

respond() {
  local body="$1" etag="${2:-fixture}" media_type="${3:-github.v3; format=json}"
  if [[ -n "$if_none_match" && "$if_none_match" == "W/\"$etag\"" ]]; then
    printf 'HTTP/2.0 304 Not Modified\r\nEtag: W/"%s"\r\n\r\n' "$etag"; exit 1
  fi
  if [[ "$path" == graphql && "${FIXTURE_MODE:-normal}" != missing-relationship ]]; then
    body="${body//\{\"number\":3\}/\{\"number\":3,\"url\":\"https:\/\/github.test\/example\/repo\/issues\/3\",\"repository\":\{\"nameWithOwner\":\"example\/repo\"\}\}}"
    body="${body//\{\"number\":1\}/\{\"number\":1,\"url\":\"https:\/\/github.test\/example\/repo\/issues\/1\",\"repository\":\{\"nameWithOwner\":\"example\/repo\"\}\}}"
    body="${body//\"number\":3,\"parent\":null/\"number\":3,\"parent\":\{\"number\":1,\"url\":\"https:\/\/github.test\/example\/repo\/issues\/1\",\"repository\":\{\"nameWithOwner\":\"example\/repo\"\}\}}"
  fi
  local media_header= link_header=
  [[ -z "$media_type" ]] || media_header="X-GitHub-Media-Type: ${media_type}"$'\r\n'
  if [[ "${FIXTURE_MODE:-normal}" == live-link-drift && "$path" == repos/example/repo/labels* ]]; then
    link_header=$'Link: <https://api.github.com/repos/example/repo/labels?per_page=100&page=1>; rel="prev"\r\n'
  fi
  printf 'HTTP/2.0 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nEtag: W/"%s"\r\nLast-Modified: Sat, 02 Aug 2026 00:00:00 GMT\r\nX-GitHub-Api-Version-Selected: 2026-03-10\r\n%s%s\r\n%s\n' "$etag" "$media_header" "$link_header" "$body"
}

respond_with_next() {
  if [[ -n "$if_none_match" && "$if_none_match" == 'W/"fixture"' ]]; then
    printf 'HTTP/2.0 304 Not Modified\r\nEtag: W/"fixture"\r\n\r\n'; exit 1
  fi
  printf 'HTTP/2.0 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nEtag: W/"fixture"\r\nLast-Modified: Sat, 02 Aug 2026 00:00:00 GMT\r\nLink: <%s>; rel="next"\r\nX-GitHub-Api-Version-Selected: 2026-03-10\r\nX-GitHub-Media-Type: github.v3; format=json\r\n\r\n%s\n' "$1" "$2"
}

labels='[{"id":10,"node_id":"LA_enhancement","name":"enhancement","color":"a2eeef"},{"id":11,"node_id":"LA_ready","name":"ready-for-agent","color":"0e8a16"}]'
assignees='[{"id":20,"node_id":"U_bob","login":"bob"},{"id":19,"node_id":"U_alice","login":"alice"}]'
if [[ "${FIXTURE_MODE:-normal}" == reordered ]]; then
  labels='[{"color":"0e8a16","name":"ready-for-agent","node_id":"LA_ready","id":11},{"name":"enhancement","id":10,"color":"a2eeef","node_id":"LA_enhancement"}]'
  assignees='[{"login":"alice","node_id":"U_alice","id":19},{"node_id":"U_bob","id":20,"login":"bob"}]'
fi

case "$path" in
  repos/example/repo/labels*)
    if [[ "${FIXTURE_MODE:-normal}" == canonical-link ]]; then
      body="$(ruby -rjson -e 'puts JSON.generate([{"id" => 10, "name" => "enhancement"}, {"id" => 11, "name" => "ready-for-agent"}] + (100..197).map { |id| {"id" => id, "name" => "label-#{id}"} })')"
      respond_with_next 'https://api.github.com/repositories/123456/labels?per_page=100&page=2&after=cursor' "$body"
    elif [[ "${FIXTURE_MODE:-normal}" == missing-link ]]; then
      if [[ "$path" == *"&page=1"* ]]; then
        respond "$(ruby -rjson -e 'puts JSON.generate(100.times.map { |i| {"id" => i, "name" => "label-#{i}"} })')"
      else
        call="$(bump labels2)"; body='[]'; etag=fixture
        [[ "$call" -le 1 ]] || { body='[{"id":100,"name":"concurrent-label"}]'; etag=changed; }
        respond "$body" "$etag"
      fi
    elif [[ "${FIXTURE_MODE:-normal}" == incomplete ]]; then
      if [[ "$path" == *"&page=1"* ]]; then
        respond_with_next 'https://api.github.com/repos/example/repo/labels?per_page=100&page=2' "$labels"
      else
        echo "simulated missing pagination page" >&2
        exit 1
      fi
    else
      respond "$labels"
    fi
    ;;
  repositories/123456/labels*)
    respond '[{"id":198,"name":"label-198"}]'
    ;;
  repos/example/repo/issues\?state=all*)
    respond '[{"id":101,"number":1},{"id":103,"number":3},{"id":102,"number":2,"pull_request":{"url":"https://api.github.test/repos/example/repo/pulls/2"}}]'
    ;;
  repos/example/repo/issues/comments*)
    call="$(bump comments)"
    comments='[{"id":1001,"issue_url":"https://api.github.test/repos/example/repo/issues/1","body":"evidence"},{"id":1002,"issue_url":"https://api.github.test/repos/example/repo/issues/2","body":"review context"}]'
    if [[ "${FIXTURE_MODE:-normal}" == live-drift || "${FIXTURE_MODE:-normal}" == capture-race && "$call" -gt 1 ]]; then
      comments='[{"id":1001,"issue_url":"https://api.github.test/repos/example/repo/issues/1","body":"evidence changed concurrently"},{"id":1002,"issue_url":"https://api.github.test/repos/example/repo/issues/2","body":"review context"}]'
    fi
    etag=fixture
    [[ "$call" -le 1 || "${FIXTURE_MODE:-normal}" != header-race && "${FIXTURE_MODE:-normal}" != capture-race ]] || etag=changed
    respond "$comments" "$etag"
    ;;
  repos/example/repo/issues/events*)
    respond '[{"id":2001,"event":"labeled","issue":{"number":1,"url":"https://api.github.test/repos/example/repo/issues/1"}},{"id":2002,"event":"cross-referenced","issue":{"number":2,"url":"https://api.github.test/repos/example/repo/issues/2"}}]'
    ;;
  repos/example/repo/issues/1)
    respond "{\"id\":101,\"node_id\":\"I_one\",\"url\":\"https://api.github.test/repos/example/repo/issues/1\",\"number\":1,\"comments\":1,\"state\":\"open\",\"body\":\"Issue one\",\"labels\":$labels,\"assignees\":$assignees,\"updated_at\":\"2026-08-02T00:00:00Z\"}"
    ;;
  repos/example/repo/issues/3)
    call="$(bump issue3)"
    [[ "$call" -le 1 || -n "$if_none_match" ]] || { echo "stability GET missing If-None-Match" >&2; exit 1; }
    body=Blocker etag=fixture
    [[ "${FIXTURE_MODE:-normal}" != issue-detail-race || "$call" -le 1 ]] || { body='Blocker changed concurrently'; etag=changed; }
    respond "{\"id\":103,\"node_id\":\"I_three\",\"url\":\"https://api.github.test/repos/example/repo/issues/3\",\"number\":3,\"comments\":0,\"state\":\"closed\",\"body\":\"$body\",\"labels\":[],\"assignees\":[],\"updated_at\":\"2026-08-01T00:00:00Z\"}" "$etag"
    ;;
  repos/example/repo/issues/2)
    respond "{\"id\":102,\"node_id\":\"PR_two\",\"url\":\"https://api.github.test/repos/example/repo/issues/2\",\"number\":2,\"comments\":1,\"state\":\"open\",\"body\":\"Pull request conversation\",\"labels\":$labels,\"assignees\":[],\"pull_request\":{\"url\":\"https://api.github.test/repos/example/repo/pulls/2\"},\"updated_at\":\"2026-08-02T00:00:00Z\"}"
    ;;
  repos/example/repo/pulls/2)
    call="$(bump pull2)"
    state=open title=Stable
    if [[ "${FIXTURE_MODE:-normal}" == closed-pr-race ]]; then
      state=closed; [[ "$call" -le 1 ]] || { title='Changed concurrently'; etag=changed; }
    fi
    respond "{\"id\":202,\"node_id\":\"PR_two\",\"url\":\"https://api.github.test/repos/example/repo/pulls/2\",\"number\":2,\"state\":\"$state\",\"title\":\"$title\",\"head\":{\"ref\":\"feature\",\"sha\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"},\"base\":{\"ref\":\"main\",\"sha\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"},\"commits\":1,\"changed_files\":2,\"review_comments\":1}" "${etag:-fixture}"
    ;;
  repos/example/repo/pulls/2/commits*) respond '[{"sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","commit":{"message":"Refs #1"}}]' ;;
  repos/example/repo/pulls/2/files*) respond '[{"sha":"cccccccccccccccccccccccccccccccccccccccc","filename":"safe-copy.txt","status":"added"},{"sha":"cccccccccccccccccccccccccccccccccccccccc","filename":"safe.txt","status":"added"}]' ;;
  repos/example/repo/pulls/2/reviews*) respond '[{"id":3001,"node_id":"PRR_review","state":"APPROVED","_links":{"pull_request":{"href":"https://api.github.test/repos/example/repo/pulls/2"}}}]' ;;
  repos/example/repo/pulls/2/comments*) respond '[{"id":4001,"node_id":"PRRC_comment","pull_request_url":"https://api.github.test/repos/example/repo/pulls/2","body":"looks good"}]' ;;
  repos/example/repo/commits/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/check-runs*)
    respond '{"total_count":1,"check_runs":[{"id":5001,"node_id":"CR_check","head_sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","name":"CI","status":"completed","conclusion":"success"}]}'
    ;;
  repos/example/repo/commits/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/statuses*)
    respond '[{"id":6001,"node_id":"SC_status","sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","context":"legacy","state":"success"}]'
    ;;
  graphql)
    test "$(grep -o 'number url repository { nameWithOwner }' <<<"$query" | wc -l | tr -d ' ')" -eq 5
    call="$(bump graphql)"
    media_type='github.v4; format=json'
    [[ "${FIXTURE_MODE:-normal}" != graphql-media-race || "$call" -le 1 ]] || media_type='github.v4; format=json; drift=1'
    [[ "${FIXTURE_MODE:-normal}" != graphql-media-missing || "$call" -le 1 ]] || media_type=
    if [[ "${FIXTURE_MODE:-normal}" == reordered ]]; then
      respond '{"data":{"repository":{"id":"R_repo","nameWithOwner":"example/repo","defaultBranchRef":{"name":"main","target":{"oid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}},"issues":{"totalCount":2,"nodes":[{"number":3,"parent":null,"subIssues":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false}},"blockedBy":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false}},"blocking":{"totalCount":1,"nodes":[{"number":1}],"pageInfo":{"hasNextPage":false}},"closedByPullRequestsReferences":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false}}},{"number":1,"parent":null,"subIssues":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false}},"blockedBy":{"totalCount":1,"nodes":[{"number":3}],"pageInfo":{"hasNextPage":false}},"blocking":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false}},"closedByPullRequestsReferences":{"totalCount":1,"nodes":[{"number":2,"url":"https://github.test/example/repo/pull/2","repository":{"nameWithOwner":"example/repo"}}],"pageInfo":{"hasNextPage":false}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}' fixture "$media_type"
    else
      respond '{"data":{"repository":{"id":"R_repo","nameWithOwner":"example/repo","defaultBranchRef":{"name":"main","target":{"oid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}},"issues":{"totalCount":2,"pageInfo":{"endCursor":null,"hasNextPage":false},"nodes":[{"number":1,"parent":null,"subIssues":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"blockedBy":{"nodes":[{"number":3}],"totalCount":1,"pageInfo":{"hasNextPage":false}},"blocking":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"closedByPullRequestsReferences":{"nodes":[{"repository":{"nameWithOwner":"example/repo"},"url":"https://github.test/example/repo/pull/2","number":2}],"totalCount":1,"pageInfo":{"hasNextPage":false}}},{"number":3,"parent":null,"subIssues":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"blockedBy":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"blocking":{"nodes":[{"number":1}],"totalCount":1,"pageInfo":{"hasNextPage":false}},"closedByPullRequestsReferences":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}}}]}}}}' fixture "$media_type"
    fi
    ;;
  *) echo "unexpected GitHub API path: $path" >&2; exit 1 ;;
esac
EOF
chmod +x "$fake_bin/gh"

api_log="$tmp_dir/api.log"
export TEST_API_LOG="$api_log" TEST_FAKE_STATE="$tmp_dir/state"
capture() {
  local mode="$1" output="$2"
  rm -f "$TEST_FAKE_STATE"/*
  FIXTURE_MODE="$mode" PATH="$fake_bin:$PATH" ruby "$preflight" capture \
    --repo example/repo --controller "Codex /root" --output "$output"
}

verify() {
  local bundle="$1"
  shift
  PATH="$fake_bin:$PATH" ruby "$preflight" verify \
    --repo example/repo --controller "Codex /root" --bundle "$bundle" "$@"
}

verify_live() {
  local mode="$1" bundle="$2"
  rm -f "$TEST_FAKE_STATE"/*
  FIXTURE_MODE="$mode" PATH="$fake_bin:$PATH" ruby "$preflight" verify \
    --repo example/repo --controller "Codex /root" --bundle "$bundle" --live
}

copy_bundle() {
  cp -R "$tmp_dir/bundle-a" "$tmp_dir/$1"
}

rehash() {
  local bundle="$1" filename="$2"
  (cd "$bundle" && sha256sum "$filename" >"$filename.sha256")
}

resign_preflight() {
  local bundle="$1"
  rehash "$bundle" preflight.json
  ruby -rdigest -rjson -e '
    bundle = ARGV.fetch(0)
    path = File.join(bundle, "manifest.json")
    object = JSON.parse(File.binread(path))
    object.fetch("preflight")["sha256"] = Digest::SHA256.file(File.join(bundle, "preflight.json")).hexdigest
    File.binwrite(path, JSON.generate(object) + "\n")
  ' "$bundle"
  rehash "$bundle" manifest.json
}

set_json_field() {
  local path="$1" key="$2" value="$3"
  ruby -rjson -e '
    path, key, value = ARGV
    object = JSON.parse(File.binread(path))
    object[key] = JSON.parse(value)
    File.binwrite(path, JSON.generate(object) + "\n")
  ' "$path" "$key" "$value"
}

verify_fails() {
  local name="$1" bundle="$2"
  shift 2
  set +e
  output="$(verify "$bundle" "$@" 2>&1)"
  exit_code=$?
  set -e
  test "$exit_code" -eq 2
  grep -Fq "ERROR issue lifecycle preflight:" <<<"$output" || {
    echo "$name did not report a verifier error" >&2
    exit 1
  }
}

verify_live_fails() {
  local mode="$1" bundle="$2"
  local before output exit_code
  before="$(find "$bundle" -mindepth 1 -maxdepth 1 -type f -exec sha256sum {} \; | sort)"
  set +e
  output="$(verify_live "$mode" "$bundle" 2>&1)"
  exit_code=$?
  set -e
  test "$exit_code" -eq 2
  grep -Fq "ERROR issue lifecycle preflight:" <<<"$output"
  test "$(find "$bundle" -mindepth 1 -maxdepth 1 -type f -exec sha256sum {} \; | sort)" = "$before"
}

: >"$api_log"
capture normal "$tmp_dir/bundle-a"
capture reordered "$tmp_dir/bundle-b"
capture live-link-drift "$tmp_dir/bundle-link-header"
capture canonical-link "$tmp_dir/bundle-canonical-link"
verify "$tmp_dir/bundle-canonical-link"
bundle_before="$(find "$tmp_dir/bundle-a" -mindepth 1 -maxdepth 1 -type f -exec sha256sum {} \; | sort)"
api_calls_before="$(wc -l <"$api_log" | tr -d ' ')"
verify "$tmp_dir/bundle-a"
test "$(find "$tmp_dir/bundle-a" -mindepth 1 -maxdepth 1 -type f -exec sha256sum {} \; | sort)" = "$bundle_before"
test "$(wc -l <"$api_log" | tr -d ' ')" = "$api_calls_before"
expected_files=$'manifest.json\nmanifest.json.sha256\npreflight.json\npreflight.json.sha256'
actual_files="$(find "$tmp_dir/bundle-a" -mindepth 1 -maxdepth 1 -type f -print | sed 's|.*/||' | sort)"
test "$actual_files" = "$expected_files"
(
  cd "$tmp_dir/bundle-a"
  sha256sum --check --strict preflight.json.sha256 manifest.json.sha256 >/dev/null
)
test "$(awk '{print $1}' "$tmp_dir/bundle-a/preflight.json.sha256")" = \
  "$(awk '{print $1}' "$tmp_dir/bundle-b/preflight.json.sha256")"

ruby -rjson - "$tmp_dir/bundle-a" <<'RUBY'
bundle = ARGV.fetch(0)
graph = JSON.parse(File.read(File.join(bundle, "preflight.json")))
manifest = JSON.parse(File.read(File.join(bundle, "manifest.json")))
abort "wrong graph schema" unless graph.fetch("schema") == "monday.issue_lifecycle_preflight.v1"
abort "missing graph" unless graph.fetch("label_catalog").length == 2 && graph.fetch("items").map { |item| item.fetch("number") } == [1, 2, 3]
issue = graph.fetch("items").find { |item| item["number"] == 1 }
pull = graph.fetch("items").find { |item| item["number"] == 2 }
blocker = graph.fetch("items").find { |item| item["number"] == 3 }
abort "incomplete Issue metadata" unless %w[body labels assignees state].all? { |field| issue.fetch("issue").key?(field) } && issue["comments"].length == 1 && issue["events"].length == 1 && issue.dig("relationships", "blocked_by").map { |entry| entry["number"] } == [3]
abort "ambiguous blocker" unless issue.dig("relationships", "blocked_by", 0, "repository", "nameWithOwner") == "example/repo"
abort "ambiguous parent" unless blocker.dig("relationships", "parent", "repository", "nameWithOwner") == "example/repo"
abort "missing close ref" unless issue.dig("relationships", "closed_by_pull_requests").map { |entry| entry["number"] } == [2]
abort "missing PR evidence" unless pull.dig("pull_request", "metadata", "head", "sha") == "b" * 40 && pull.dig("pull_request", "metadata", "base", "sha") == "a" * 40 && %w[commits files reviews review_comments statuses].all? { |key| pull.dig("pull_request", key).is_a?(Array) } && pull.dig("pull_request", "check_runs", "total_count") == 1
abort "missing same-blob files" unless pull.dig("pull_request", "files").map { |file| file["filename"] } == %w[safe-copy.txt safe.txt]
abort "wrong manifest" unless manifest.fetch("schema") == "monday.issue_lifecycle_manifest.v1" && manifest.dig("api", "rest_version") == "2026-03-10" && manifest.fetch("target").include?("example/repo") && manifest.fetch("exclusions").include?("GitHub metadata mutation")
abort "wrong GraphQL media type" unless manifest.dig("api", "graphql_media_type") == "github.v4; format=json"
abort "wrong controller" unless manifest["controller"] == "Codex /root"
abort "wrong main" unless manifest["default_branch_sha"] == "a" * 40
abort "missing conditional stability pages" unless manifest.fetch("pages").any? { |page| page["phase"] == "stability_check" && page["protocol"] == "rest" } && manifest.fetch("pages").select { |page| page["phase"] == "stability_check" && page["protocol"] == "rest" }.all? { |page| page["status"] == 304 }
abort "wrong counts" unless manifest.fetch("counts") == {"issues" => 2, "pull_requests" => 1, "labels" => 2, "issue_comments" => 2, "issue_events" => 2}
RUBY

ruby -rjson - "$tmp_dir/bundle-link-header/manifest.json" <<'RUBY'
pages = JSON.parse(File.read(ARGV.fetch(0))).fetch("pages").select { |page| page["protocol"] == "rest" && page["request"].include?("/labels?") }
abort "capture Link was not recorded" unless pages.find { |page| page["phase"] == "capture" }.fetch("link").include?('rel="prev"')
abort "304 stability incorrectly copied capture Link" unless pages.find { |page| page["phase"] == "stability_check" }.fetch("link").nil?
RUBY

local_api_calls_before="$(wc -l <"$api_log" | tr -d ' ')"

copy_bundle verify-v1-pages
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("pages").each { |page| page.delete("link") }
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-v1-pages/manifest.json"
rehash "$tmp_dir/verify-v1-pages" manifest.json
verify "$tmp_dir/verify-v1-pages"

copy_bundle verify-missing
rm "$tmp_dir/verify-missing/manifest.json.sha256"
verify_fails missing "$tmp_dir/verify-missing"

copy_bundle verify-extra
touch "$tmp_dir/verify-extra/unexpected"
verify_fails extra "$tmp_dir/verify-extra"

copy_bundle verify-symlink
rm "$tmp_dir/verify-symlink/preflight.json"
ln -s "$tmp_dir/bundle-a/preflight.json" "$tmp_dir/verify-symlink/preflight.json"
verify_fails symlink "$tmp_dir/verify-symlink"

copy_bundle verify-tampered
printf ' ' >>"$tmp_dir/verify-tampered/preflight.json"
verify_fails tampered "$tmp_dir/verify-tampered"

copy_bundle verify-digest
printf '%064d  preflight.json\n' 0 >"$tmp_dir/verify-digest/preflight.json.sha256"
verify_fails digest "$tmp_dir/verify-digest"

copy_bundle verify-noncanonical
ruby -rjson -e 'path = ARGV.fetch(0); File.binwrite(path, JSON.pretty_generate(JSON.parse(File.binread(path))) + "\n")' "$tmp_dir/verify-noncanonical/preflight.json"
rehash "$tmp_dir/verify-noncanonical" preflight.json
verify_fails noncanonical "$tmp_dir/verify-noncanonical"

copy_bundle verify-schema
set_json_field "$tmp_dir/verify-schema/manifest.json" schema '"monday.issue_lifecycle_manifest.v2"'
rehash "$tmp_dir/verify-schema" manifest.json
verify_fails schema "$tmp_dir/verify-schema"

verify_fails scope "$tmp_dir/bundle-a" --repo other/repo

copy_bundle verify-api
set_json_field "$tmp_dir/verify-api/manifest.json" api '{"rest_version":"wrong"}'
rehash "$tmp_dir/verify-api" manifest.json
verify_fails api "$tmp_dir/verify-api"

copy_bundle verify-pages
set_json_field "$tmp_dir/verify-pages/manifest.json" pages '[]'
rehash "$tmp_dir/verify-pages" manifest.json
verify_fails pages "$tmp_dir/verify-pages"

copy_bundle verify-counts
set_json_field "$tmp_dir/verify-counts/manifest.json" counts '{"issues":999}'
rehash "$tmp_dir/verify-counts" manifest.json
verify_fails counts "$tmp_dir/verify-counts"

copy_bundle verify-branch
set_json_field "$tmp_dir/verify-branch/manifest.json" default_branch_sha '"cccccccccccccccccccccccccccccccccccccccc"'
rehash "$tmp_dir/verify-branch" manifest.json
verify_fails branch "$tmp_dir/verify-branch"

copy_bundle verify-item-schema
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("items").first["unexpected"] = true
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-item-schema/preflight.json"
resign_preflight "$tmp_dir/verify-item-schema"
verify_fails item-schema "$tmp_dir/verify-item-schema"

copy_bundle verify-pr-schema
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("items").find { |item| item["kind"] == "pull_request" }.fetch("pull_request").delete("commits")
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-pr-schema/preflight.json"
resign_preflight "$tmp_dir/verify-pr-schema"
verify_fails pr-schema "$tmp_dir/verify-pr-schema"

copy_bundle verify-relationship-schema
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("items").find { |item| item["kind"] == "issue" }.fetch("relationships").delete("blocked_by")
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-relationship-schema/preflight.json"
resign_preflight "$tmp_dir/verify-relationship-schema"
verify_fails relationship-schema "$tmp_dir/verify-relationship-schema"

copy_bundle verify-rest-etag
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("pages").select { |page| page["protocol"] == "rest" }.each { |page| page["etag"] = nil }
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-rest-etag/manifest.json"
rehash "$tmp_dir/verify-rest-etag" manifest.json
verify_fails rest-etag "$tmp_dir/verify-rest-etag"

copy_bundle verify-page-omission
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("pages").reject! { |page| page["request"] == "repos/example/repo/issues/1" }
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-page-omission/manifest.json"
rehash "$tmp_dir/verify-page-omission" manifest.json
verify_fails page-omission "$tmp_dir/verify-page-omission"

copy_bundle verify-link-target
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  page = object.fetch("pages").find { |entry| entry["phase"] == "capture" && entry["request"].include?("/labels?") }
  page["link"] = %q(<https://api.github.com/repos/example/repo/labels?per_page=100&page=2>; rel="next")
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-link-target/manifest.json"
rehash "$tmp_dir/verify-link-target" manifest.json
verify_fails link-target "$tmp_dir/verify-link-target"

copy_bundle verify-entry-identity
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("items").first.fetch("comments").first.clear
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-entry-identity/preflight.json"
resign_preflight "$tmp_dir/verify-entry-identity"
verify_fails entry-identity "$tmp_dir/verify-entry-identity"

for field in comment label commit review review-comment check-run status; do
  copy_bundle "verify-scope-$field"
  ruby -rjson -e '
    path, field = ARGV
    object = JSON.parse(File.binread(path))
    case field
    when "comment" then object.fetch("items").first.fetch("comments").first["issue_url"].sub!("example/repo", "other/repo")
    when "label" then object.fetch("items").first.dig("issue", "labels").first.merge!("id" => 999, "name" => "outside-catalog")
    else
      pull = object.fetch("items").find { |item| item["kind"] == "pull_request" }.fetch("pull_request")
      case field
      when "commit" then pull.fetch("commits").first["sha"] = "d" * 40
      when "review" then pull.fetch("reviews").first.dig("_links", "pull_request")["href"].sub!("example/repo", "other/repo")
      when "review-comment" then pull.fetch("review_comments").first["pull_request_url"].sub!("example/repo", "other/repo")
      when "check-run" then pull.dig("check_runs", "check_runs").first["head_sha"] = "d" * 40
      when "status" then pull.fetch("statuses").first["sha"] = "d" * 40
      end
    end
    File.binwrite(path, JSON.generate(object) + "\n")
  ' "$tmp_dir/verify-scope-$field/preflight.json" "$field"
  resign_preflight "$tmp_dir/verify-scope-$field"
  verify_fails "scope-$field" "$tmp_dir/verify-scope-$field"
done

test "$(wc -l <"$api_log" | tr -d ' ')" = "$local_api_calls_before"

live_bundle_before="$(find "$tmp_dir/bundle-a" -mindepth 1 -maxdepth 1 -type f -exec sha256sum {} \; | sort)"
live_api_calls_before="$(wc -l <"$api_log" | tr -d ' ')"
verify_live normal "$tmp_dir/bundle-a"
test "$(wc -l <"$api_log" | tr -d ' ')" -gt "$live_api_calls_before"
test "$(find "$tmp_dir/bundle-a" -mindepth 1 -maxdepth 1 -type f -exec sha256sum {} \; | sort)" = "$live_bundle_before"
for mode in live-drift live-link-drift incomplete missing-link capture-race header-race issue-detail-race closed-pr-race graphql-media-race graphql-media-missing; do
  verify_live_fails "$mode" "$tmp_dir/bundle-a"
done

for mode in incomplete missing-link missing-relationship capture-race header-race issue-detail-race closed-pr-race graphql-media-race graphql-media-missing; do
  set +e
  output="$(capture "$mode" "$tmp_dir/$mode" 2>&1)"
  exit_code=$?
  set -e
  test "$exit_code" -eq 2
  test ! -e "$tmp_dir/$mode"
  if [[ "$mode" == incomplete ]]; then
    grep -Fq "simulated missing pagination page" <<<"$output"
  elif [[ "$mode" == missing-relationship ]]; then
    grep -Fq "relationship identity is incomplete" <<<"$output"
  else
    grep -Fq "capture stability drift" <<<"$output"
  fi
done

if awk -F '\t' '$1 != "GET" && !($1 == "POST" && $2 == "graphql" && $3 == "query") { found = 1 } END { exit found ? 0 : 1 }' "$api_log"; then
  echo "capture used a mutating GitHub API route" >&2
  exit 1
fi

echo "issue lifecycle preflight capture: ok"
