#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export RECORD_TEST_ROOT=$work GITHUB_REPOSITORY=owner/repo GITHUB_RUN_ID=42 GITHUB_RUN_ATTEMPT=2
export RECORD_SOURCE_SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa RECORD_TREE_SHA=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
head_sha=cccccccccccccccccccccccccccccccccccccccc
pr_tree=dddddddddddddddddddddddddddddddddddddddd
digest=sha256:$(printf 'e%.0s' {1..64})
mkdir "$work/bin"
cat >"$work/bin/git" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$*" in
  'rev-parse HEAD') cat "$RECORD_TEST_ROOT/checkout" ;;
  "rev-parse HEAD^{tree}") printf '%s\n' "$RECORD_TREE_SHA" ;;
  *) exit 1 ;;
esac
MOCK
cat >"$work/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
for arg; do
  case "$arg" in
    */pulls\?*) cat "$RECORD_TEST_ROOT/pulls"; exit ;;
    */git/commits/*) cat "$RECORD_TEST_ROOT/head"; exit ;;
  esac
done
exit 1
MOCK
chmod +x "$work/bin/git" "$work/bin/gh"
export PATH="$work/bin:$PATH"
printf '%s\n' "$RECORD_SOURCE_SHA" >"$work/checkout"
jq -n --arg source "$RECORD_SOURCE_SHA" --arg head "$head_sha" '[[{
  number:1367,html_url:"https://github.com/owner/repo/pull/1367",merged_at:"2026-10-08T00:00:00Z",
  merge_commit_sha:$source,base:{ref:"main",repo:{full_name:"owner/repo"}},head:{sha:$head}
}]]' >"$work/pulls"
jq -n --arg head "$head_sha" --arg tree "$pr_tree" '{sha:$head,tree:{sha:$tree}}' >"$work/head"
record() { "$script_dir/write-release-record.sh" "$RECORD_SOURCE_SHA" "$digest" "registry/image@$digest" "$work/record"; }
record
jq -e --arg source "$RECORD_SOURCE_SHA" --arg tree "$RECORD_TREE_SHA" --arg pr_tree "$pr_tree" --arg digest "$digest" '
  .main_commit==$source and .main_tree_sha==$tree and .source_tree_sha==$tree and
  .pull_requests[0].number==1367 and .pull_requests[0].tree_sha==$pr_tree and
  .base_image_digest==$digest and .base_image_digest_kind=="oci-config" and .run_attempt=="2"
' "$work/record" >/dev/null
# A direct main commit has no associated merged PR.
printf '[[]]\n' >"$work/pulls"
record
jq -e '.pull_requests==[]' "$work/record" >/dev/null
printf '%s\n' "$head_sha" >"$work/checkout"
if record >/dev/null 2>&1; then echo 'wrong checkout recorded' >&2; exit 1; fi
printf '%s\n' "$RECORD_SOURCE_SHA" >"$work/checkout"
digest=invalid
if record >/dev/null 2>&1; then echo 'invalid base digest recorded' >&2; exit 1; fi
printf 'release record contract passed\n'
ruby -ryaml - "$(cd "$script_dir/../.." && pwd)" <<'RUBY'
root = ARGV.fetch(0)
%w[docker-publish.yml acr-publish.yml].each do |name|
  doc = YAML.safe_load(File.read(File.join(root,'.github/workflows',name)))
  delivered_jobs = name=='docker-publish.yml' ? ['build-and-push'] : ['publish-research-images','publish-ordinary']
  delivered_jobs.each do |job_name|
    job = doc.fetch('jobs').fetch(job_name)
    abort 'publication cannot read PR provenance' unless job.dig('permissions','pull-requests')=='read'
    steps=job.fetch('steps')
    record=steps.find { |s| s.fetch('run','').include?('write-release-record.sh') }
    abort 'publication has no source record' unless record
    abort 'publication does not retain its record' unless steps.any? { |s| s.fetch('with',{}).fetch('path','').end_with?('release-record.json') }
    promotion=steps.find { |s| s.fetch('name','').include?('without rebuilding') || s.fetch('id','')=='promote' }
    if job_name=='publish-research-images'
      abort 'OCI delivery issues a signature' if steps.any? { |s| s.fetch('run','').match?(/cosign|oss-publish|publish-research-build-release/) }
    else
      abort 'publication has no promotion check' unless promotion
      abort 'promotion issues a signature' if promotion.fetch('run').match?(/cosign|oss-publish|publish-research-build-release/)
    end
  end
end
RUBY
