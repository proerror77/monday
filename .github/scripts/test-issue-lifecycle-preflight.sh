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
    body="${body//\{\"number\":3\}/\{\"number\":3,\"url\":\"https:\/\/github.com\/example\/repo\/issues\/3\",\"repository\":\{\"nameWithOwner\":\"example\/repo\"\}\}}"
    body="${body//\{\"number\":1\}/\{\"number\":1,\"url\":\"https:\/\/github.com\/example\/repo\/issues\/1\",\"repository\":\{\"nameWithOwner\":\"example\/repo\"\}\}}"
    body="${body//\"number\":3,\"parent\":null/\"number\":3,\"parent\":\{\"number\":1,\"url\":\"https:\/\/github.com\/example\/repo\/issues\/1\",\"repository\":\{\"nameWithOwner\":\"example\/repo\"\}\}}"
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
alice='{"id":19,"node_id":"U_alice","login":"alice"}'
assignees='[{"id":20,"node_id":"U_bob","login":"bob"},{"id":19,"node_id":"U_alice","login":"alice"}]'
if [[ "${FIXTURE_MODE:-normal}" == reordered ]]; then
  labels='[{"color":"0e8a16","name":"ready-for-agent","node_id":"LA_ready","id":11},{"name":"enhancement","id":10,"color":"a2eeef","node_id":"LA_enhancement"}]'
  assignees='[{"login":"alice","node_id":"U_alice","id":19},{"node_id":"U_bob","id":20,"login":"bob"}]'
elif [[ "${FIXTURE_MODE:-normal}" == post-state* ]]; then
  assignees='[{"id":19,"node_id":"U_alice","login":"alice"}]'
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
    respond '[{"id":101,"number":1},{"id":103,"number":3},{"id":102,"number":2,"pull_request":{"url":"https://api.github.com/repos/example/repo/pulls/2"}}]'
    ;;
  repos/example/repo/issues/comments*)
    call="$(bump comments)"
    comments='[{"id":1001,"issue_url":"https://api.github.com/repos/example/repo/issues/1","body":"evidence"},{"id":1002,"issue_url":"https://api.github.com/repos/example/repo/issues/2","body":"review context"}]'
    if [[ "${FIXTURE_MODE:-normal}" == live-drift || "${FIXTURE_MODE:-normal}" == capture-race && "$call" -gt 1 ]]; then
      comments='[{"id":1001,"issue_url":"https://api.github.com/repos/example/repo/issues/1","body":"evidence changed concurrently"},{"id":1002,"issue_url":"https://api.github.com/repos/example/repo/issues/2","body":"review context"}]'
    elif [[ "${FIXTURE_MODE:-normal}" == post-state-provenance ]]; then
      comments='[{"id":1001,"issue_url":"https://api.github.com/repos/example/repo/issues/1","body":"evidence"},{"id":1002,"issue_url":"https://api.github.com/repos/example/repo/issues/2","body":"review context"},{"body":"completion evidence","id":1003,"issue_url":"https://api.github.com/repos/example/repo/issues/1"}]'
    fi
    etag=fixture
    [[ "$call" -le 1 || "${FIXTURE_MODE:-normal}" != header-race && "${FIXTURE_MODE:-normal}" != capture-race ]] || etag=changed
    respond "$comments" "$etag"
    ;;
  repos/example/repo/issues/events*)
    events='[{"id":2001,"event":"labeled","issue":{"number":1,"url":"https://api.github.com/repos/example/repo/issues/1"}},{"id":2002,"event":"cross-referenced","issue":{"number":2,"url":"https://api.github.com/repos/example/repo/issues/2"}}]'
    [[ "${FIXTURE_MODE:-normal}" != post-state-provenance ]] || events='[{"id":2001,"event":"labeled","issue":{"number":1,"url":"https://api.github.com/repos/example/repo/issues/1"}},{"id":2002,"event":"cross-referenced","issue":{"number":2,"url":"https://api.github.com/repos/example/repo/issues/2"}},{"event":"closed","id":2003,"issue":{"number":1,"url":"https://api.github.com/repos/example/repo/issues/1"}}]'
    respond "$events"
    ;;
  repos/example/repo/issues/1)
    issue_body='Issue one 研究'; issue_state=open; state_reason=null; issue_comments=1
    closed_at=null; closed_by=null; updated_at='"2026-08-02T00:00:00Z"'
    dependencies='{"blocked_by":0,"blocking":0,"total_blocked_by":1,"total_blocking":0}'
    if [[ "${FIXTURE_MODE:-normal}" == post-state* ]]; then
      issue_body='Issue one repaired'; issue_state=closed; state_reason='"completed"'
      closed_at='"2026-08-02T01:00:00Z"'; closed_by="$alice"; updated_at='"2026-08-02T01:00:00Z"'
      dependencies='{"blocked_by":0,"blocking":0,"total_blocked_by":0,"total_blocking":0}'
      [[ "${FIXTURE_MODE:-normal}" != post-state-provenance ]] || issue_comments=2
      [[ "${FIXTURE_MODE:-normal}" != post-state-provenance ]] || updated_at='"2026-08-02T02:00:00Z"'
    fi
    respond "{\"id\":101,\"node_id\":\"I_one\",\"url\":\"https://api.github.com/repos/example/repo/issues/1\",\"number\":1,\"comments\":$issue_comments,\"state\":\"$issue_state\",\"state_reason\":$state_reason,\"body\":\"$issue_body\",\"labels\":$labels,\"assignee\":$alice,\"assignees\":$assignees,\"closed_at\":$closed_at,\"closed_by\":$closed_by,\"parent_issue_url\":null,\"sub_issues_summary\":{\"completed\":1,\"percent_completed\":100,\"total\":1},\"issue_dependencies_summary\":$dependencies,\"updated_at\":$updated_at}"
    ;;
  repos/example/repo/issues/3)
    call="$(bump issue3)"
    [[ "$call" -le 1 || -n "$if_none_match" ]] || { echo "stability GET missing If-None-Match" >&2; exit 1; }
    body=Blocker etag=fixture updated_at='"2026-08-01T00:00:00Z"'
    dependencies='{"blocked_by":0,"blocking":1,"total_blocked_by":0,"total_blocking":1}'
    if [[ "${FIXTURE_MODE:-normal}" == post-state* ]]; then
      updated_at='"2026-08-02T01:00:00Z"'
      dependencies='{"blocked_by":0,"blocking":0,"total_blocked_by":0,"total_blocking":0}'
    fi
    [[ "${FIXTURE_MODE:-normal}" != issue-detail-race || "$call" -le 1 ]] || { body='Blocker changed concurrently'; etag=changed; }
    respond "{\"id\":103,\"node_id\":\"I_three\",\"url\":\"https://api.github.com/repos/example/repo/issues/3\",\"number\":3,\"comments\":0,\"state\":\"closed\",\"state_reason\":\"completed\",\"body\":\"$body\",\"labels\":[],\"assignee\":null,\"assignees\":[],\"closed_at\":\"2026-08-01T00:00:00Z\",\"closed_by\":$alice,\"parent_issue_url\":\"https://api.github.com/repos/example/repo/issues/1\",\"sub_issues_summary\":{\"completed\":0,\"percent_completed\":0,\"total\":0},\"issue_dependencies_summary\":$dependencies,\"updated_at\":$updated_at}" "$etag"
    ;;
  repos/example/repo/issues/2)
    respond "{\"id\":102,\"node_id\":\"PR_two\",\"url\":\"https://api.github.com/repos/example/repo/issues/2\",\"number\":2,\"comments\":1,\"state\":\"open\",\"body\":\"Pull request conversation\",\"labels\":$labels,\"assignees\":[],\"pull_request\":{\"url\":\"https://api.github.com/repos/example/repo/pulls/2\"},\"updated_at\":\"2026-08-02T00:00:00Z\"}"
    ;;
  repos/example/repo/pulls/2)
    call="$(bump pull2)"
    state=open title=Stable
    if [[ "${FIXTURE_MODE:-normal}" == closed-pr-race ]]; then
      state=closed; [[ "$call" -le 1 ]] || { title='Changed concurrently'; etag=changed; }
    fi
    respond "{\"id\":202,\"node_id\":\"PR_two\",\"url\":\"https://api.github.com/repos/example/repo/pulls/2\",\"number\":2,\"state\":\"$state\",\"title\":\"$title\",\"head\":{\"ref\":\"feature\",\"sha\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"},\"base\":{\"ref\":\"main\",\"sha\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"},\"commits\":1,\"changed_files\":2,\"review_comments\":1}" "${etag:-fixture}"
    ;;
  repos/example/repo/pulls/2/commits*) respond '[{"sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","commit":{"message":"Refs #1"}}]' ;;
  repos/example/repo/pulls/2/files*) respond '[{"sha":"cccccccccccccccccccccccccccccccccccccccc","filename":"safe-copy.txt","status":"added"},{"sha":"cccccccccccccccccccccccccccccccccccccccc","filename":"safe.txt","status":"added"}]' ;;
  repos/example/repo/pulls/2/reviews*) respond '[{"id":3001,"node_id":"PRR_review","state":"APPROVED","_links":{"pull_request":{"href":"https://api.github.com/repos/example/repo/pulls/2"}}}]' ;;
  repos/example/repo/pulls/2/comments*) respond '[{"id":4001,"node_id":"PRRC_comment","pull_request_url":"https://api.github.com/repos/example/repo/pulls/2","body":"looks good"}]' ;;
  repos/example/repo/commits/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/check-runs*)
    respond '{"total_count":1,"check_runs":[{"id":5001,"node_id":"CR_check","head_sha":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","name":"CI","status":"completed","conclusion":"success"}]}'
    ;;
  repos/example/repo/commits/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/statuses*)
    respond '[{"id":6001,"node_id":"SC_status","context":"legacy","state":"success"}]'
    ;;
  graphql)
    test "$(grep -o 'number url repository { nameWithOwner }' <<<"$query" | wc -l | tr -d ' ')" -eq 5
    call="$(bump graphql)"
    media_type='github.v4; format=json'
    [[ "${FIXTURE_MODE:-normal}" != graphql-media-race || "$call" -le 1 ]] || media_type='github.v4; format=json; drift=1'
    [[ "${FIXTURE_MODE:-normal}" != graphql-media-missing || "$call" -le 1 ]] || media_type=
    if [[ "${FIXTURE_MODE:-normal}" == post-state* ]]; then
      respond '{"data":{"repository":{"id":"R_repo","nameWithOwner":"example/repo","defaultBranchRef":{"name":"main","target":{"oid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}},"issues":{"totalCount":2,"pageInfo":{"endCursor":null,"hasNextPage":false},"nodes":[{"number":1,"parent":null,"subIssues":{"nodes":[{"number":3}],"totalCount":1,"pageInfo":{"hasNextPage":false}},"blockedBy":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"blocking":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"closedByPullRequestsReferences":{"nodes":[{"repository":{"nameWithOwner":"example/repo"},"url":"https://github.com/example/repo/pull/2","number":2}],"totalCount":1,"pageInfo":{"hasNextPage":false}}},{"number":3,"parent":{"number":1},"subIssues":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"blockedBy":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"blocking":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"closedByPullRequestsReferences":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}}}]}}}}' fixture "$media_type"
    elif [[ "${FIXTURE_MODE:-normal}" == reordered ]]; then
      respond '{"data":{"repository":{"id":"R_repo","nameWithOwner":"example/repo","defaultBranchRef":{"name":"main","target":{"oid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}},"issues":{"totalCount":2,"nodes":[{"number":3,"parent":null,"subIssues":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false}},"blockedBy":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false}},"blocking":{"totalCount":1,"nodes":[{"number":1}],"pageInfo":{"hasNextPage":false}},"closedByPullRequestsReferences":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false}}},{"number":1,"parent":null,"subIssues":{"totalCount":1,"nodes":[{"number":3}],"pageInfo":{"hasNextPage":false}},"blockedBy":{"totalCount":1,"nodes":[{"number":3}],"pageInfo":{"hasNextPage":false}},"blocking":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false}},"closedByPullRequestsReferences":{"totalCount":1,"nodes":[{"number":2,"url":"https://github.com/example/repo/pull/2","repository":{"nameWithOwner":"example/repo"}}],"pageInfo":{"hasNextPage":false}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}' fixture "$media_type"
    else
      respond '{"data":{"repository":{"id":"R_repo","nameWithOwner":"example/repo","defaultBranchRef":{"name":"main","target":{"oid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}},"issues":{"totalCount":2,"pageInfo":{"endCursor":null,"hasNextPage":false},"nodes":[{"number":1,"parent":null,"subIssues":{"nodes":[{"number":3}],"totalCount":1,"pageInfo":{"hasNextPage":false}},"blockedBy":{"nodes":[{"number":3}],"totalCount":1,"pageInfo":{"hasNextPage":false}},"blocking":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"closedByPullRequestsReferences":{"nodes":[{"repository":{"nameWithOwner":"example/repo"},"url":"https://github.com/example/repo/pull/2","number":2}],"totalCount":1,"pageInfo":{"hasNextPage":false}}},{"number":3,"parent":null,"subIssues":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"blockedBy":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}},"blocking":{"nodes":[{"number":1}],"totalCount":1,"pageInfo":{"hasNextPage":false}},"closedByPullRequestsReferences":{"nodes":[],"totalCount":0,"pageInfo":{"hasNextPage":false}}}]}}}}' fixture "$media_type"
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

plan_restore() {
  local mode="$1" bundle="$2" forward_plan="$3"
  rm -f "$TEST_FAKE_STATE"/*
  FIXTURE_MODE="$mode" PATH="$fake_bin:$PATH" ruby "$preflight" plan-restore \
    --repo example/repo --controller "Codex /root" --bundle "$bundle" --forward-plan "$forward_plan"
}

plan_restore_dry() {
  local mode="$1" bundle="$2" forward_plan="$3" reverse_plan="$4" receipt="$5" post_bundle="$6"
  rm -f "$TEST_FAKE_STATE"/*
  FIXTURE_MODE="$mode" PATH="$fake_bin:$PATH" ruby "$preflight" plan-restore \
    --repo example/repo --controller "Codex /root" --bundle "$bundle" --forward-plan "$forward_plan" \
    --reverse-plan "$reverse_plan" --receipt "$receipt" --post-bundle "$post_bundle"
}

plan_restore_fails() {
  local mode="$1" expected="$2" output exit_code
  shift 2
  rm -f "$TEST_FAKE_STATE"/*
  set +e
  output="$(FIXTURE_MODE="$mode" PATH="$fake_bin:$PATH" ruby "$preflight" plan-restore \
    --repo example/repo --controller "Codex /root" "$@" 2>"$tmp_dir/plan-error.log")"
  exit_code=$?
  set -e
  test "$exit_code" -eq 2
  test -z "$output"
  if ! grep -Fq "ERROR issue lifecycle preflight: $expected" "$tmp_dir/plan-error.log"; then
    echo "plan-restore did not report the expected error: $(<"$tmp_dir/plan-error.log")" >&2
    exit 1
  fi
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
remove_link_provenance() { ruby -rjson -e 'path = ARGV.fetch(0); object = JSON.parse(File.binread(path)); object.fetch("pages").each { |page| page.delete("link") }; File.binwrite(path, JSON.generate(object) + "\n")' "$1"; }
verify_fails() {
  local name="$1" bundle="$2" expected="$3"
  shift 3
  set +e
  output="$(verify "$bundle" "$@" 2>&1)"
  exit_code=$?
  set -e
  test "$exit_code" -eq 2
  if ! grep -Fq "ERROR issue lifecycle preflight:" <<<"$output" || ! grep -Fq "$expected" <<<"$output"; then
    echo "$name did not report the expected verifier error: $output" >&2
    exit 1
  fi
}
verify_live_fails() {
  local mode="$1" bundle="$2"
  local before output exit_code expected
  case "$mode" in
    live-drift) expected="live verification drift: Issue/PR graph changed" ;;
    live-link-drift) expected="live verification drift: page/header inventory changed" ;;
    incomplete) expected="simulated missing pagination page" ;;
    graphql-media-*) expected="manifest GraphQL media type does not match page inventory" ;;
    *) expected="capture stability drift:" ;;
  esac
  before="$(find "$bundle" -mindepth 1 -maxdepth 1 -type f -exec sha256sum {} \; | sort)"
  set +e
  output="$(verify_live "$mode" "$bundle" 2>&1)"
  exit_code=$?
  set -e
  test "$exit_code" -eq 2
  grep -Fq "ERROR issue lifecycle preflight:" <<<"$output" && grep -Fq "$expected" <<<"$output"
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

ruby -rjson -rdigest - "$tmp_dir/bundle-a" "$tmp_dir/forward-plan.json" <<'RUBY'
bundle, output = ARGV
preflight_sha = File.read(File.join(bundle, "preflight.json.sha256")).split.first
before = {
  "assignees" => %w[alice bob],
  "blocked_by" => [3],
  "body" => "Issue one 研究",
  "labels" => %w[enhancement ready-for-agent],
  "parent" => nil,
  "state" => {"reason" => nil, "value" => "open"}
}
after = before.merge(
  "assignees" => ["alice"],
  "blocked_by" => [],
  "body" => "Issue one repaired",
  "state" => {"reason" => "completed", "value" => "closed"}
)
plan = {
  "controller" => "Codex /root",
  "default_branch" => "main",
  "default_branch_sha" => "a" * 40,
  "operations" => [{"number" => 1, "precondition" => before, "target" => after}],
  "preflight_manifest_sha256" => Digest::SHA256.file(File.join(bundle, "manifest.json")).hexdigest,
  "preflight_sha256" => preflight_sha,
  "repository" => "example/repo",
  "schema" => "monday.issue_lifecycle_forward_plan.v1",
  "target" => "GitHub Issue and pull request metadata in example/repo"
}
File.binwrite(output, JSON.generate(plan) + "\n")
RUBY

plan_tree_before="$(find "$tmp_dir/bundle-a" -type f -exec sha256sum {} \; | sort; sha256sum "$tmp_dir/forward-plan.json")"
reverse_plan="$(plan_restore normal "$tmp_dir/bundle-a" "$tmp_dir/forward-plan.json")"
test "$(find "$tmp_dir/bundle-a" -type f -exec sha256sum {} \; | sort; sha256sum "$tmp_dir/forward-plan.json")" = "$plan_tree_before"
ruby -rjson -rdigest - "$tmp_dir/forward-plan.json" "$reverse_plan" <<'RUBY'
forward_path, reverse_json = ARGV
forward = JSON.parse(File.binread(forward_path))
reverse = JSON.parse(reverse_json)
operation = forward.fetch("operations").fetch(0)
expected = {
  "controller" => forward.fetch("controller"),
  "default_branch" => forward.fetch("default_branch"),
  "default_branch_sha" => forward.fetch("default_branch_sha"),
  "forward_plan_sha256" => Digest::SHA256.file(forward_path).hexdigest,
  "operations" => [{"number" => 1, "precondition" => operation.fetch("target"), "target" => operation.fetch("precondition")}],
  "preflight_manifest_sha256" => forward.fetch("preflight_manifest_sha256"),
  "preflight_sha256" => forward.fetch("preflight_sha256"),
  "repository" => forward.fetch("repository"),
  "schema" => "monday.issue_lifecycle_reverse_plan.v1",
  "target" => forward.fetch("target")
}
abort "wrong reverse plan" unless reverse == expected
abort "reverse plan is not canonical" unless reverse_json + "\n" == JSON.generate(reverse) + "\n"
RUBY
printf '%s\n' "$reverse_plan" >"$tmp_dir/reverse-plan.json"
cp "$tmp_dir/forward-plan.json" "$tmp_dir/forward-duplicate-reason.json"
ruby -rjson -e 'path = ARGV.fetch(0); plan = JSON.parse(File.binread(path)); plan.dig("operations", 0, "target", "state")["reason"] = "duplicate"; File.binwrite(path, JSON.generate(plan) + "\n")' "$tmp_dir/forward-duplicate-reason.json"
test -n "$(plan_restore normal "$tmp_dir/bundle-a" "$tmp_dir/forward-duplicate-reason.json")"
capture post-state "$tmp_dir/post-bundle"
ruby -rjson -rdigest - "$tmp_dir/bundle-a" "$tmp_dir/forward-plan.json" "$tmp_dir/reverse-plan.json" "$tmp_dir/post-bundle" "$tmp_dir/receipt.json" <<'RUBY'
bundle, forward_path, reverse_path, post_bundle, output = ARGV
forward = JSON.parse(File.binread(forward_path))
operation = forward.fetch("operations").fetch(0)
post_manifest = JSON.parse(File.binread(File.join(post_bundle, "manifest.json")))
digest = ->(object) { Digest::SHA256.hexdigest(JSON.generate(object) + "\n") }
receipt = {
  "api" => post_manifest.fetch("api"),
  "controller" => forward.fetch("controller"),
  "counts" => post_manifest.fetch("counts"),
  "default_branch" => post_manifest.fetch("default_branch"),
  "default_branch_sha" => post_manifest.fetch("default_branch_sha"),
  "forward_plan_sha256" => Digest::SHA256.file(forward_path).hexdigest,
  "operations" => [{
    "comment_ids" => [],
    "event_ids" => [],
    "number" => operation.fetch("number"),
    "precondition_sha256" => digest.call(operation.fetch("precondition")),
    "result" => "passed",
    "target_sha256" => digest.call(operation.fetch("target"))
  }],
  "pages" => post_manifest.fetch("pages"),
  "postflight_manifest_sha256" => Digest::SHA256.file(File.join(post_bundle, "manifest.json")).hexdigest,
  "postflight_sha256" => post_manifest.dig("preflight", "sha256"),
  "preflight_manifest_sha256" => Digest::SHA256.file(File.join(bundle, "manifest.json")).hexdigest,
  "preflight_sha256" => forward.fetch("preflight_sha256"),
  "repository" => forward.fetch("repository"),
  "reverse_plan_sha256" => Digest::SHA256.file(reverse_path).hexdigest,
  "schema" => "monday.issue_lifecycle_receipt.v1",
  "target" => forward.fetch("target")
}
File.binwrite(output, JSON.generate(receipt) + "\n")
RUBY
dry_inputs_before="$(find "$tmp_dir/bundle-a" "$tmp_dir/post-bundle" -type f -exec sha256sum {} \; | sort; sha256sum "$tmp_dir/forward-plan.json" "$tmp_dir/reverse-plan.json" "$tmp_dir/receipt.json")"
dry_reverse="$(plan_restore_dry post-state "$tmp_dir/bundle-a" "$tmp_dir/forward-plan.json" "$tmp_dir/reverse-plan.json" "$tmp_dir/receipt.json" "$tmp_dir/post-bundle")"
test "$dry_reverse" = "$reverse_plan"
test "$(find "$tmp_dir/bundle-a" "$tmp_dir/post-bundle" -type f -exec sha256sum {} \; | sort; sha256sum "$tmp_dir/forward-plan.json" "$tmp_dir/reverse-plan.json" "$tmp_dir/receipt.json")" = "$dry_inputs_before"

capture post-state-provenance "$tmp_dir/post-provenance"
ruby -rjson -rdigest - "$tmp_dir/receipt.json" "$tmp_dir/post-provenance" "$tmp_dir/receipt-provenance.json" <<'RUBY'
source, post_bundle, output = ARGV
receipt = JSON.parse(File.binread(source))
manifest = JSON.parse(File.binread(File.join(post_bundle, "manifest.json")))
receipt["counts"] = manifest.fetch("counts")
receipt.fetch("operations").first["comment_ids"] = [1003]
receipt.fetch("operations").first["event_ids"] = [2003]
receipt["pages"] = manifest.fetch("pages")
receipt["postflight_manifest_sha256"] = Digest::SHA256.file(File.join(post_bundle, "manifest.json")).hexdigest
receipt["postflight_sha256"] = manifest.dig("preflight", "sha256")
File.binwrite(output, JSON.generate(receipt) + "\n")
RUBY
test "$(plan_restore_dry post-state-provenance "$tmp_dir/bundle-a" "$tmp_dir/forward-plan.json" "$tmp_dir/reverse-plan.json" "$tmp_dir/receipt-provenance.json" "$tmp_dir/post-provenance")" = "$reverse_plan"

for mutation in stale-precondition unsupported-field no-op duplicate unknown-issue relationship-cycle unknown-label trailing-hyphen-login consecutive-hyphen-login numeric-login stale-manifest-identity stale-identity; do
  cp "$tmp_dir/forward-plan.json" "$tmp_dir/forward-$mutation.json"
  ruby -rjson -e '
    path, mutation = ARGV
    plan = JSON.parse(File.binread(path))
    operation = plan.fetch("operations").first
    case mutation
    when "stale-precondition" then operation.fetch("precondition")["body"] = "stale"
    when "unsupported-field" then operation.fetch("target")["title"] = "not supported"
    when "no-op" then operation["target"] = operation.fetch("precondition")
    when "duplicate" then plan.fetch("operations") << JSON.parse(JSON.generate(operation))
    when "unknown-issue" then operation["number"] = 2
    when "relationship-cycle" then operation.fetch("target")["parent"] = 3
    when "unknown-label" then operation.fetch("target")["labels"] = ["missing-label"]
    when "trailing-hyphen-login" then operation.fetch("target")["assignees"] = ["alice-"]
    when "consecutive-hyphen-login" then operation.fetch("target")["assignees"] = ["alice--bob"]
    when "numeric-login" then operation.fetch("target")["assignees"] = [123]
    when "stale-manifest-identity" then plan["preflight_manifest_sha256"] = "0" * 64
    when "stale-identity" then plan["default_branch_sha"] = "0" * 40
    end
    File.binwrite(path, JSON.generate(plan) + "\n")
  ' "$tmp_dir/forward-$mutation.json" "$mutation"
  case "$mutation" in
    stale-precondition) expected='forward plan Issue #1 precondition does not match preflight' ;;
    unsupported-field) expected='forward plan Issue #1 state schema is invalid' ;;
    no-op) expected='forward plan Issue #1 operation is a no-op' ;;
    duplicate) expected='forward plan contains duplicate Issue #1' ;;
    unknown-issue) expected='forward plan references unknown Issue #2' ;;
    relationship-cycle) expected='forward plan parent relationships contain a cycle' ;;
    unknown-label|trailing-hyphen-login|consecutive-hyphen-login|numeric-login) expected='forward plan Issue #1 state identity is invalid' ;;
    stale-manifest-identity|stale-identity) expected='forward plan identity is invalid' ;;
  esac
  plan_restore_fails normal "$expected" --bundle "$tmp_dir/bundle-a" --forward-plan "$tmp_dir/forward-$mutation.json"
done
copy_bundle cross-repo
ruby -rjson -e '
  path = ARGV.fetch(0)
  graph = JSON.parse(File.binread(path))
  reference = graph.fetch("items").find { |item| item["number"] == 1 }.dig("relationships", "blocked_by", 0)
  reference.fetch("repository")["nameWithOwner"] = "other/repo"
  reference["url"] = "https://github.com/other/repo/issues/3"
  File.binwrite(path, JSON.generate(graph) + "\n")
' "$tmp_dir/cross-repo/preflight.json"
resign_preflight "$tmp_dir/cross-repo"
cp "$tmp_dir/forward-plan.json" "$tmp_dir/forward-cross-repo.json"
ruby -rjson -rdigest -e '
  path, bundle = ARGV
  plan = JSON.parse(File.binread(path))
  plan["preflight_manifest_sha256"] = Digest::SHA256.file(File.join(bundle, "manifest.json")).hexdigest
  plan["preflight_sha256"] = File.read(File.join(bundle, "preflight.json.sha256")).split.first
  File.binwrite(path, JSON.generate(plan) + "\n")
' "$tmp_dir/forward-cross-repo.json" "$tmp_dir/cross-repo"
plan_restore_fails normal 'preflight Issue #1 relationship scope is unsupported' \
  --bundle "$tmp_dir/cross-repo" --forward-plan "$tmp_dir/forward-cross-repo.json"
plan_restore_fails live-drift 'pre-mutation live drift: Issue/PR graph changed' \
  --bundle "$tmp_dir/bundle-a" --forward-plan "$tmp_dir/forward-plan.json"

cp "$tmp_dir/reverse-plan.json" "$tmp_dir/reverse-tampered.json"
ruby -rjson -e 'path = ARGV.fetch(0); plan = JSON.parse(File.binread(path)); plan.fetch("operations").first.fetch("target")["body"] = "tampered"; File.binwrite(path, JSON.generate(plan) + "\n")' "$tmp_dir/reverse-tampered.json"
plan_restore_fails post-state 'saved reverse plan does not match the exact derived inverse' \
  --bundle "$tmp_dir/bundle-a" --forward-plan "$tmp_dir/forward-plan.json" --reverse-plan "$tmp_dir/reverse-tampered.json" \
  --receipt "$tmp_dir/receipt.json" --post-bundle "$tmp_dir/post-bundle"

cp "$tmp_dir/receipt.json" "$tmp_dir/receipt-tampered.json"
ruby -rjson -e 'path = ARGV.fetch(0); receipt = JSON.parse(File.binread(path)); receipt.fetch("operations").first["result"] = "failed"; File.binwrite(path, JSON.generate(receipt) + "\n")' "$tmp_dir/receipt-tampered.json"
plan_restore_fails post-state 'restoration receipt identity is invalid' \
  --bundle "$tmp_dir/bundle-a" --forward-plan "$tmp_dir/forward-plan.json" --reverse-plan "$tmp_dir/reverse-plan.json" \
  --receipt "$tmp_dir/receipt-tampered.json" --post-bundle "$tmp_dir/post-bundle"

for mutation in unsupported-metadata pr-metadata closing-reference derived-relationship assignee-summary parent-summary sub-issues-summary dependencies-summary closure-metadata updated-at comment-rewrite; do
  cp -R "$tmp_dir/post-bundle" "$tmp_dir/post-$mutation"
  ruby -rjson -e '
    path, mutation = ARGV
    graph = JSON.parse(File.binread(path))
    issue = graph.fetch("items").find { |item| item["number"] == 1 }
    case mutation
    when "unsupported-metadata" then issue.fetch("issue")["node_id"] = "I_concurrent"
    when "pr-metadata" then graph.fetch("items").find { |item| item["kind"] == "pull_request" }.dig("pull_request", "metadata")["title"] = "concurrent"
    when "closing-reference" then issue.fetch("relationships")["closed_by_pull_requests"] = []
    when "derived-relationship" then issue.fetch("relationships")["sub_issues"] = []
    when "assignee-summary" then graph.fetch("items").find { |item| item["number"] == 3 }.fetch("issue")["assignee"] = {"id" => 99, "login" => "mallory"}
    when "parent-summary" then graph.fetch("items").find { |item| item["number"] == 3 }.fetch("issue")["parent_issue_url"] = "https://api.github.com/repos/example/repo/issues/99"
    when "sub-issues-summary" then issue.fetch("issue").fetch("sub_issues_summary")["total"] = 99
    when "dependencies-summary" then graph.fetch("items").find { |item| item["number"] == 3 }.fetch("issue").fetch("issue_dependencies_summary")["total_blocking"] = 99
    when "closure-metadata" then graph.fetch("items").find { |item| item["number"] == 3 }.fetch("issue")["closed_at"] = "2026-08-02T03:00:00Z"
    when "updated-at" then issue.fetch("issue")["updated_at"] = "not-a-timestamp"
    when "comment-rewrite" then issue.fetch("comments").first["body"] = "rewritten"
    end
    File.binwrite(path, JSON.generate(graph) + "\n")
  ' "$tmp_dir/post-$mutation/preflight.json" "$mutation"
  resign_preflight "$tmp_dir/post-$mutation"
  case "$mutation" in
    unsupported-metadata) expected='post-state Issue #1 unsupported metadata drift' ;;
    pr-metadata) expected='post-state PR #2 metadata drift' ;;
    closing-reference) expected='post-state Issue #1 closing-reference drift' ;;
    derived-relationship) expected='post-state Issue #1 derived relationship drift' ;;
    assignee-summary) expected='post-state Issue #3 assignee summary is inconsistent' ;;
    parent-summary) expected='post-state Issue #3 parent summary is inconsistent' ;;
    sub-issues-summary) expected='post-state Issue #1 sub-issues summary is inconsistent' ;;
    dependencies-summary) expected='post-state Issue #3 dependency summary is inconsistent' ;;
    closure-metadata) expected='post-state Issue #3 closure metadata drift' ;;
    updated-at) expected='post-state Issue #1 updated_at is invalid' ;;
    comment-rewrite) expected='post-state Issue #1 comments provenance is not append-only' ;;
  esac
  plan_restore_fails post-state "$expected" \
    --bundle "$tmp_dir/bundle-a" --forward-plan "$tmp_dir/forward-plan.json" --reverse-plan "$tmp_dir/reverse-plan.json" \
    --receipt "$tmp_dir/receipt.json" --post-bundle "$tmp_dir/post-$mutation"
done
plan_restore_fails post-state 'post-state Issue #3 derived relationship drift' \
  --bundle "$tmp_dir/bundle-a" --forward-plan "$tmp_dir/forward-plan.json" --reverse-plan "$tmp_dir/reverse-plan.json" \
  --receipt "$tmp_dir/receipt.json" --post-bundle "$tmp_dir/bundle-a"
plan_restore_fails normal 'restoration live drift: Issue/PR graph changed' \
  --bundle "$tmp_dir/bundle-a" --forward-plan "$tmp_dir/forward-plan.json" --reverse-plan "$tmp_dir/reverse-plan.json" \
  --receipt "$tmp_dir/receipt.json" --post-bundle "$tmp_dir/post-bundle"

set +e
apply_stdout="$(PATH="$fake_bin:$PATH" ruby "$preflight" apply --repo example/repo --controller "Codex /root" 2>"$tmp_dir/apply-error.log")"
apply_exit=$?
set -e
test "$apply_exit" -eq 2 && test -z "$apply_stdout"
grep -Fq 'unsupported operation "apply"' "$tmp_dir/apply-error.log"

ruby -rjson - "$tmp_dir/bundle-a" <<'RUBY'
bundle = ARGV.fetch(0)
graph = JSON.parse(File.read(File.join(bundle, "preflight.json")))
manifest = JSON.parse(File.read(File.join(bundle, "manifest.json")))
abort "wrong graph schema" unless graph.fetch("schema") == "monday.issue_lifecycle_preflight.v1"
abort "missing graph" unless graph.fetch("label_catalog").length == 2 && graph.fetch("items").map { |item| item.fetch("number") } == [1, 2, 3]
issue = graph.fetch("items").find { |item| item["number"] == 1 }
pull = graph.fetch("items").find { |item| item["number"] == 2 }
blocker = graph.fetch("items").find { |item| item["number"] == 3 }
abort "incomplete Issue metadata" unless issue.dig("issue", "body") == "Issue one 研究" && %w[body labels assignees state].all? { |field| issue.fetch("issue").key?(field) } && issue["comments"].length == 1 && issue["events"].length == 1 && issue.dig("relationships", "blocked_by").map { |entry| entry["number"] } == [3]
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
remove_link_provenance "$tmp_dir/verify-v1-pages/manifest.json"
rehash "$tmp_dir/verify-v1-pages" manifest.json
verify "$tmp_dir/verify-v1-pages"
cp -R "$tmp_dir/bundle-canonical-link" "$tmp_dir/verify-v1-multipage"
remove_link_provenance "$tmp_dir/verify-v1-multipage/manifest.json"
rehash "$tmp_dir/verify-v1-multipage" manifest.json
verify_fails v1-multipage "$tmp_dir/verify-v1-multipage" "legacy manifest cannot verify multi-page REST provenance"

copy_bundle verify-missing
rm "$tmp_dir/verify-missing/manifest.json.sha256"
verify_fails missing "$tmp_dir/verify-missing" "bundle file set is invalid"

copy_bundle verify-extra
touch "$tmp_dir/verify-extra/unexpected"
verify_fails extra "$tmp_dir/verify-extra" "bundle file set is invalid"

copy_bundle verify-symlink
rm "$tmp_dir/verify-symlink/preflight.json"
ln -s "$tmp_dir/bundle-a/preflight.json" "$tmp_dir/verify-symlink/preflight.json"
verify_fails symlink "$tmp_dir/verify-symlink" "bundle entry preflight.json is not a regular file"

copy_bundle verify-tampered
printf ' ' >>"$tmp_dir/verify-tampered/preflight.json"
verify_fails tampered "$tmp_dir/verify-tampered" "preflight.json digest mismatch"

copy_bundle verify-digest
printf '%064d  preflight.json\n' 0 >"$tmp_dir/verify-digest/preflight.json.sha256"
verify_fails digest "$tmp_dir/verify-digest" "preflight.json digest mismatch"

copy_bundle verify-noncanonical
ruby -rjson -e 'path = ARGV.fetch(0); File.binwrite(path, JSON.pretty_generate(JSON.parse(File.binread(path))) + "\n")' "$tmp_dir/verify-noncanonical/preflight.json"
rehash "$tmp_dir/verify-noncanonical" preflight.json
verify_fails noncanonical "$tmp_dir/verify-noncanonical" "preflight.json is not canonical JSON"

copy_bundle verify-schema
set_json_field "$tmp_dir/verify-schema/manifest.json" schema '"monday.issue_lifecycle_manifest.v2"'
rehash "$tmp_dir/verify-schema" manifest.json
verify_fails schema "$tmp_dir/verify-schema" "manifest schema is invalid"

verify_fails scope "$tmp_dir/bundle-a" "manifest scope is invalid" --repo other/repo

copy_bundle verify-api
set_json_field "$tmp_dir/verify-api/manifest.json" api '{"rest_version":"wrong"}'
rehash "$tmp_dir/verify-api" manifest.json
verify_fails api "$tmp_dir/verify-api" "manifest API provenance is invalid"

copy_bundle verify-pages
set_json_field "$tmp_dir/verify-pages/manifest.json" pages '[]'
rehash "$tmp_dir/verify-pages" manifest.json
verify_fails pages "$tmp_dir/verify-pages" "manifest page inventory is empty"

copy_bundle verify-counts
set_json_field "$tmp_dir/verify-counts/manifest.json" counts '{"issues":999}'
rehash "$tmp_dir/verify-counts" manifest.json
verify_fails counts "$tmp_dir/verify-counts" "manifest counts do not match preflight"

copy_bundle verify-branch
set_json_field "$tmp_dir/verify-branch/manifest.json" default_branch_sha '"cccccccccccccccccccccccccccccccccccccccc"'
rehash "$tmp_dir/verify-branch" manifest.json
verify_fails branch "$tmp_dir/verify-branch" "manifest preflight identity is invalid"

copy_bundle verify-item-schema
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("items").first["unexpected"] = true
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-item-schema/preflight.json"
resign_preflight "$tmp_dir/verify-item-schema"
verify_fails item-schema "$tmp_dir/verify-item-schema" "preflight Issue #1 schema is invalid"

copy_bundle verify-pr-schema
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("items").find { |item| item["kind"] == "pull_request" }.fetch("pull_request").delete("commits")
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-pr-schema/preflight.json"
resign_preflight "$tmp_dir/verify-pr-schema"
verify_fails pr-schema "$tmp_dir/verify-pr-schema" "preflight PR #2 schema is invalid"

copy_bundle verify-relationship-schema
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("items").find { |item| item["kind"] == "issue" }.fetch("relationships").delete("blocked_by")
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-relationship-schema/preflight.json"
resign_preflight "$tmp_dir/verify-relationship-schema"
verify_fails relationship-schema "$tmp_dir/verify-relationship-schema" "preflight Issue #1 schema is invalid"

copy_bundle verify-rest-etag
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("pages").select { |page| page["protocol"] == "rest" }.each { |page| page["etag"] = nil }
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-rest-etag/manifest.json"
rehash "$tmp_dir/verify-rest-etag" manifest.json
verify_fails rest-etag "$tmp_dir/verify-rest-etag" "manifest REST ETag provenance is invalid"

copy_bundle verify-page-omission
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("pages").reject! { |page| page["request"] == "repos/example/repo/issues/1" }
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-page-omission/manifest.json"
rehash "$tmp_dir/verify-page-omission" manifest.json
verify_fails page-omission "$tmp_dir/verify-page-omission" "manifest REST page inventory does not match preflight"

copy_bundle verify-link-target
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  page = object.fetch("pages").find { |entry| entry["phase"] == "capture" && entry["request"].include?("/labels?") }
  page["link"] = %q(<https://api.github.com/repos/example/repo/labels?per_page=100&page=2>; rel="next")
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-link-target/manifest.json"
rehash "$tmp_dir/verify-link-target" manifest.json
verify_fails link-target "$tmp_dir/verify-link-target" "manifest REST pagination chain has an extra page"

cp -R "$tmp_dir/bundle-canonical-link" "$tmp_dir/verify-page-size"
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("pages").each do |page|
    page["request"].sub!("per_page=100", "per_page=1") if page["request"].include?("repositories/123456/labels")
    page["link"].sub!("per_page=100", "per_page=1") if page["link"]
  end
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-page-size/manifest.json"
rehash "$tmp_dir/verify-page-size" manifest.json
verify_fails page-size "$tmp_dir/verify-page-size" "manifest REST Link scope is invalid"

copy_bundle verify-entry-identity
ruby -rjson -e '
  path = ARGV.fetch(0)
  object = JSON.parse(File.binread(path))
  object.fetch("items").first.fetch("comments").first.clear
  File.binwrite(path, JSON.generate(object) + "\n")
' "$tmp_dir/verify-entry-identity/preflight.json"
resign_preflight "$tmp_dir/verify-entry-identity"
verify_fails entry-identity "$tmp_dir/verify-entry-identity" "preflight item #1 metadata is invalid"

for field in comment comment-host relationship-host relationship-repo relationship-kind relationship-number label commit review review-comment check-run status; do
  expected="preflight PR #2 schema is invalid"
  [[ "$field" != comment && "$field" != comment-host && "$field" != label ]] || expected="preflight item #1 metadata is invalid"
  [[ "$field" != relationship-* ]] || expected="preflight Issue #1 schema is invalid"
  copy_bundle "verify-scope-$field"
  ruby -rjson -e '
    path, field = ARGV
    object = JSON.parse(File.binread(path))
    case field
    when "comment" then object.fetch("items").first.fetch("comments").first["issue_url"].sub!("example/repo", "other/repo")
    when "comment-host" then object.fetch("items").first.fetch("comments").first["issue_url"].sub!("api.github.com", "evil.example")
    when "relationship-host" then object.fetch("items").first.fetch("relationships").fetch("blocked_by").first["url"].sub!("github.com", "evil.example")
    when "relationship-repo" then object.fetch("items").first.fetch("relationships").fetch("blocked_by").first.dig("repository")["nameWithOwner"] = "other/repo"
    when "relationship-kind" then object.fetch("items").first.fetch("relationships").fetch("blocked_by").first["url"].sub!("/issues/", "/pull/")
    when "relationship-number" then object.fetch("items").first.fetch("relationships").fetch("blocked_by").first["url"].sub!("/3", "/999")
    when "label" then labels = object.fetch("items").first.dig("issue", "labels"); labels.first.merge!("id" => 999, "name" => "outside-catalog"); labels.sort_by! { |label| label["id"] }
    else
      pull = object.fetch("items").find { |item| item["kind"] == "pull_request" }.fetch("pull_request")
      case field
      when "commit" then pull.fetch("commits").first["sha"] = "d" * 40
      when "review" then pull.fetch("reviews").first.dig("_links", "pull_request")["href"].sub!("example/repo", "other/repo")
      when "review-comment" then pull.fetch("review_comments").first["pull_request_url"].sub!("example/repo", "other/repo")
      when "check-run" then pull.dig("check_runs", "check_runs").first["head_sha"] = "d" * 40
      when "status" then pull.fetch("statuses").first.delete("id")
      end
    end
    File.binwrite(path, JSON.generate(object) + "\n")
  ' "$tmp_dir/verify-scope-$field/preflight.json" "$field"
  resign_preflight "$tmp_dir/verify-scope-$field"
  verify_fails "scope-$field" "$tmp_dir/verify-scope-$field" "$expected"
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
