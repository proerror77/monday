#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
reconciler="$repo_root/.github/scripts/issue-lifecycle-status-reconcile.rb"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

if [[ ! -f "$reconciler" ]]; then
  echo "missing reconciler: $reconciler" >&2
  exit 1
fi

fake_bin="$tmp_dir/bin"
mkdir -p "$fake_bin"

cat > "$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

method=GET
paginate=0
slurp=0
path=""
while (($#)); do
  case "$1" in
    api|--silent)
      shift
      ;;
    --method|-X)
      method="$2"
      shift 2
      ;;
    -H)
      shift 2
      ;;
    --paginate)
      paginate=1
      shift
      ;;
    --slurp)
      slurp=1
      shift
      ;;
    --input)
      shift 2
      ;;
    *)
      path="$1"
      shift
      ;;
  esac
done

payload=""
if [[ "$method" == POST ]]; then
  payload="$(command cat)"
fi
printf '%s\t%s\t%s\t%s\t%s\n' "$method" "$path" "$paginate" "$slurp" "$payload" >> "$TEST_API_LOG"

sha_a=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
sha_b=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
sha_c=cccccccccccccccccccccccccccccccccccccccc
sha_d=dddddddddddddddddddddddddddddddddddddddd
sha_shared=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee

case "$path" in
  repos/example/repo/pulls\?state=open\&per_page=100)
    if [[ "$FIXTURE_MODE" == shared ]]; then
      echo "[[{\"number\":201,\"head\":{\"sha\":\"$sha_shared\"}},{\"number\":202,\"head\":{\"sha\":\"$sha_shared\"}}]]"
    elif [[ "$FIXTURE_MODE" == mapping ]]; then
      echo "[[{\"number\":101,\"head\":{\"sha\":\"$sha_a\"}}],[{\"number\":102,\"head\":{\"sha\":\"$sha_b\"}},{\"number\":103,\"head\":{\"sha\":\"$sha_c\"}}]]"
    else
      echo "[[{\"number\":101,\"head\":{\"sha\":\"$sha_d\"}}]]"
    fi
    ;;
  repos/example/repo)
    echo '{"default_branch":"main"}'
    ;;
  repos/example/repo/issues\?state=open\&per_page=100\&page=1)
    if [[ "$FIXTURE_MODE" == shared ]]; then
      echo '[{"number":10,"body":"## Parent\n\nNone\n\n## Blocked by\n\nNone\n","labels":[{"name":"ready-for-agent"}],"assignees":[{"login":"agent"}],"issue_dependencies_summary":{"total_blocked_by":0}},{"number":201,"pull_request":{}},{"number":202,"pull_request":{}}]'
    elif [[ "$FIXTURE_MODE" == mapping ]]; then
      echo '[{"number":10,"body":"## Parent\n\nNone\n\n## Blocked by\n\nNone\n","labels":[{"name":"enhancement"},{"name":"ready-for-agent"}],"assignees":[{"login":"agent"}],"issue_dependencies_summary":{"total_blocked_by":0}},{"number":20,"body":"## Parent\n\nNone\n\n## Blocked by\n\nNone\n","labels":[{"name":"ready-for-agent"}],"assignees":[{"login":"agent"}],"issue_dependencies_summary":{"total_blocked_by":0}},{"number":30,"body":"## Parent\n\nNone\n\n## Blocked by\n\nNone\n","labels":[{"name":"enhancement"},{"name":"ready-for-agent"}],"assignees":[{"login":"agent"}],"issue_dependencies_summary":{"total_blocked_by":0}},{"number":101,"pull_request":{}},{"number":102,"pull_request":{}},{"number":103,"pull_request":{}}]'
    elif [[ "$(command cat "$TEST_METADATA_STATE")" == valid ]]; then
      echo '[{"number":10,"body":"## Parent\n\nNone\n\n## Blocked by\n\nNone\n","labels":[{"name":"enhancement"},{"name":"ready-for-agent"}],"assignees":[{"login":"agent"}],"issue_dependencies_summary":{"total_blocked_by":0}},{"number":101,"pull_request":{}}]'
    else
      echo '[{"number":10,"body":"## Parent\n\nNone\n\n## Blocked by\n\nNone\n","labels":[{"name":"ready-for-agent"}],"assignees":[{"login":"agent"}],"issue_dependencies_summary":{"total_blocked_by":0}},{"number":101,"pull_request":{}}]'
    fi
    ;;
  repos/example/repo/pulls/101)
    echo '{"number":101,"base":{"ref":"main"},"title":"Fixture PR 101","body":"## Issue relationship\n\nRefs #10\n\n## Focused validation\n\nFixture proof.\n","commits":1}'
    ;;
  repos/example/repo/pulls/201)
    echo '{"number":201,"base":{"ref":"main"},"title":"Fixture PR 201","body":"## Issue relationship\n\nRefs #10\n\n## Focused validation\n\nFixture proof.\n","commits":1}'
    ;;
  repos/example/repo/pulls/202)
    echo 'fixture auditor error' >&2
    exit 1
    ;;
  repos/example/repo/pulls/102)
    echo '{"number":102,"base":{"ref":"main"},"title":"Fixture PR 102","body":"## Issue relationship\n\nRefs #20\n\n## Focused validation\n\nFixture proof.\n","commits":1}'
    ;;
  repos/example/repo/pulls/103)
    echo 'fixture auditor error' >&2
    exit 1
    ;;
  repos/example/repo/pulls/101/commits\?per_page=100\&page=1|repos/example/repo/pulls/102/commits\?per_page=100\&page=1|repos/example/repo/pulls/201/commits\?per_page=100\&page=1)
    echo '[{"sha":"safe","commit":{"message":"Safe commit"}}]'
    ;;
  repos/example/repo/commits/*/statuses\?per_page=100)
    if [[ "$FIXTURE_MODE" == dedupe ]]; then
      echo '[[{"context":"Issue Lifecycle","state":"success","description":"Issue lifecycle audit passed"},{"context":"Other","state":"failure"},{"context":"Issue Lifecycle","state":"failure","description":"Issue lifecycle policy violations found"}]]'
    elif [[ "$FIXTURE_MODE" == transition ]]; then
      state="$(command cat "$TEST_STATUS_STATE")"
      if [[ "$state" == success ]]; then
        description='Issue lifecycle audit passed'
      else
        description='Issue lifecycle policy violations found'
      fi
      printf '[[{"context":"Issue Lifecycle","state":"%s","description":"%s"}]]\n' "$state" "$description"
    else
      echo '[[]]'
    fi
    ;;
  repos/example/repo/statuses/*)
    if [[ "$method" != POST ]]; then
      echo "expected POST for $path" >&2
      exit 1
    fi
    if [[ "$FIXTURE_MODE" == transition ]]; then
      ruby -rjson -e 'print JSON.parse(ARGV.fetch(0)).fetch("state")' "$payload" > "$TEST_STATUS_STATE"
    fi
    echo "$payload"
    ;;
  *)
    echo "unexpected gh api path: $path" >&2
    exit 1
    ;;
esac
EOF
chmod +x "$fake_bin/gh"

api_log="$tmp_dir/api.log"
metadata_state="$tmp_dir/metadata-state"
status_state="$tmp_dir/status-state"
export TEST_API_LOG="$api_log" TEST_METADATA_STATE="$metadata_state" TEST_STATUS_STATE="$status_state"
printf 'valid' > "$metadata_state"
printf 'success' > "$status_state"

run_reconciler() {
  local mode="$1"
  local output="$2"
  local summary="$3"
  set +e
  FIXTURE_MODE="$mode" PATH="$fake_bin:$PATH" ruby "$reconciler" \
    --repo example/repo --summary "$summary" > "$output" 2>&1
  RUN_EXIT=$?
  set -e
}

: > "$api_log"
run_reconciler mapping "$tmp_dir/mapping.out" "$tmp_dir/mapping-summary.md"
if [[ "$RUN_EXIT" -ne 2 ]]; then
  echo "expected mapping run to exit 2, got $RUN_EXIT" >&2
  command cat "$tmp_dir/mapping.out" >&2
  exit 1
fi
grep -Fq $'GET\trepos/example/repo/pulls?state=open&per_page=100\t1\t1\t' "$api_log"
test "$(grep -c $'^POST\trepos/example/repo/statuses/' "$api_log")" -eq 3
ruby -rjson - "$api_log" <<'RUBY'
expected = {
  "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" => ["success", "Issue lifecycle audit passed"],
  "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" => ["failure", "Issue lifecycle policy violations found"],
  "cccccccccccccccccccccccccccccccccccccccc" => ["error", "Issue lifecycle audit errored"]
}
posts = File.readlines(ARGV.fetch(0), chomp: true).map do |line|
  method, path, _paginate, _slurp, payload = line.split("\t", 5)
  next unless method == "POST"
  [path.split("/").last, JSON.parse(payload)]
end.compact.to_h
abort "unexpected status heads: #{posts.keys.sort.inspect}" unless posts.keys.sort == expected.keys.sort
posts.each do |sha, payload|
  state, description = expected.fetch(sha)
  abort "wrong status payload for #{sha}: #{payload.inspect}" unless payload == {
    "context" => "Issue Lifecycle", "state" => state, "description" => description
  }
end
RUBY
grep -Fq "| #101 | \`aaaaaaaaaaaa\` | success | published |" "$tmp_dir/mapping-summary.md"
grep -Fq "| #102 | \`bbbbbbbbbbbb\` | failure | published |" "$tmp_dir/mapping-summary.md"
grep -Fq "| #103 | \`cccccccccccc\` | error | published |" "$tmp_dir/mapping-summary.md"

: > "$api_log"
run_reconciler shared "$tmp_dir/shared.out" "$tmp_dir/shared-summary.md"
if [[ "$RUN_EXIT" -ne 2 ]]; then
  echo "expected shared-head run to exit 2, got $RUN_EXIT" >&2
  command cat "$tmp_dir/shared.out" >&2
  exit 1
fi
grep -Fq "Issue #10: expected exactly one category label" "$tmp_dir/shared.out"
grep -Fq "ERROR issue lifecycle audit" "$tmp_dir/shared.out"
if grep -q $'^GET\trepos/example/repo/commits/' "$api_log"; then
  echo "status lookup GET was issued before publishing" >&2
  exit 1
fi
test "$(grep -c $'^POST\trepos/example/repo/statuses/' "$api_log")" -eq 1
ruby -rjson - "$api_log" <<'RUBY'
posts = File.readlines(ARGV.fetch(0), chomp: true).map do |line|
  method, path, _paginate, _slurp, payload = line.split("\t", 5)
  [path.split("/").last, JSON.parse(payload)] if method == "POST"
end.compact
abort "expected one aggregate status, got #{posts.inspect}" unless posts.length == 1
sha, payload = posts.fetch(0)
abort "unexpected aggregate head #{sha.inspect}" unless sha == "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
abort "shared-head status was not worst-case error: #{payload.inspect}" unless payload == {
  "context" => "Issue Lifecycle", "state" => "error", "description" => "Issue lifecycle audit errored"
}
RUBY

: > "$api_log"
run_reconciler dedupe "$tmp_dir/dedupe.out" "$tmp_dir/dedupe-summary.md"
if [[ "$RUN_EXIT" -ne 0 ]]; then
  echo "expected dedupe run to exit 0, got $RUN_EXIT" >&2
  command cat "$tmp_dir/dedupe.out" >&2
  exit 1
fi
test "$(grep -c $'^POST\trepos/example/repo/statuses/' "$api_log")" -eq 1
if grep -q $'^GET\trepos/example/repo/commits/' "$api_log"; then
  echo "status lookup GET was issued before publishing" >&2
  exit 1
fi
grep -Fq "| #101 | \`dddddddddddd\` | success | published |" "$tmp_dir/dedupe-summary.md"

: > "$api_log"
printf 'invalid' > "$metadata_state"
printf 'success' > "$status_state"
run_reconciler transition "$tmp_dir/invalid.out" "$tmp_dir/invalid-summary.md"
test "$RUN_EXIT" -eq 1
test "$(command cat "$status_state")" = failure
printf 'valid' > "$metadata_state"
run_reconciler transition "$tmp_dir/repaired.out" "$tmp_dir/repaired-summary.md"
test "$RUN_EXIT" -eq 0
test "$(command cat "$status_state")" = success
ruby -rjson - "$api_log" <<'RUBY'
states = File.readlines(ARGV.fetch(0), chomp: true).map do |line|
  method, _path, _paginate, _slurp, payload = line.split("\t", 5)
  JSON.parse(payload).fetch("state") if method == "POST"
end.compact
abort "expected green -> red -> green writes, got #{states.inspect}" unless states == %w[failure success]
RUBY

echo "issue lifecycle status reconciliation: ok"
