#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"
show_command="$repo_root/.claude/commands/pm/issue-show.md"
status_command="$repo_root/.claude/commands/pm/issue-status.md"
help_script="$repo_root/.claude/scripts/pm/help.sh"
dashboard_script="$repo_root/.claude/scripts/pm/status.sh"

extract_bash_block() {
  awk '
    /^```bash$/ { in_block=1; next }
    in_block && /^```$/ { exit }
    in_block { print }
  ' "$1"
}

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/project"
gh_log="$scratch/gh.log"

cat > "$scratch/bin/gh" <<'EOF'
#!/usr/bin/env bash
: "${GH_LOG:?}"
printf '%s\n' "$*" >> "$GH_LOG"
case "$*" in
  "issue view 123 --json number,title,state,stateReason,body,labels,assignees,parent,subIssues,blockedBy,blocking,closedByPullRequestsReferences,comments"*)
    echo '{"number":123,"state":"OPEN","parent":{"number":455},"subIssues":[{"number":472}],"blockedBy":[{"number":456}],"blocking":[],"closedByPullRequestsReferences":[{"url":"https://github.com/example/repo/pull/10"}]}'
    ;;
  "issue view 123 --json closedByPullRequestsReferences --jq"*)
    echo 'https://github.com/example/repo/pull/10'
    ;;
  "repo view --json nameWithOwner -q .nameWithOwner") echo example/repo ;;
  "api --paginate repos/example/repo/issues/123/timeline --jq"*)
    echo 'https://github.com/example/repo/pull/11'
    ;;
  "pr view https://github.com/example/repo/pull/10"*)
    echo '{"number":10,"state":"OPEN","isDraft":false}'
    ;;
  "pr view https://github.com/example/repo/pull/11"*)
    echo '{"number":11,"state":"MERGED","isDraft":false}'
    ;;
  "issue list --state open --limit 1000 --json number --jq length") echo 4 ;;
  "issue list --state closed --limit 1000 --json number --jq length") echo 7 ;;
  "issue list --state open --label tracking --limit 1000 --json number --jq length") echo 1 ;;
  "issue list --state open --label runtime --limit 1000 --json number --jq length") echo 2 ;;
  *) echo "unexpected gh invocation: $*" >&2; exit 1 ;;
esac
EOF
chmod +x "$scratch/bin/gh"

make_command_script() {
  local target="$1"
  local command="$2"

  {
    printf '#!/usr/bin/env bash\nset -u\nARGUMENTS=123\n'
    extract_bash_block "$command"
  } > "$target"
  chmod +x "$target"
}

for command in "$show_command" "$status_command"; do
  for field in labels assignees parent subIssues blockedBy blocking closedByPullRequestsReferences; do
    grep -Fq "$field" "$command"
  done

  name="$(basename "$command" .md)"
  : > "$gh_log"
  make_command_script "$scratch/$name.sh" "$command"
  command_output=$(
    cd "$scratch/project"
    GH_LOG="$gh_log" PATH="$scratch/bin:$PATH" "$scratch/$name.sh"
  )
  case "$command_output" in
    *'"parent":{"number":455}'*'"blockedBy":[{"number":456}]'*'"number":10,"state":"OPEN"'*'"number":11,"state":"MERGED"'*) ;;
    *) echo "$name did not display native relationships and linked PR state" >&2; exit 1 ;;
  esac
  grep -Fq 'pr view https://github.com/example/repo/pull/10' "$gh_log"
  grep -Fq 'pr view https://github.com/example/repo/pull/11' "$gh_log"
  if grep -Eq 'issue (create|edit|comment|close|reopen)|pr (create|edit|close|merge)' "$gh_log"; then
    echo "$name mutated GitHub" >&2
    exit 1
  fi
done

grep -Fq 'GitHub Issues are authoritative' "$help_script"
grep -Fq 'Local mirrors are optional' "$help_script"
if grep -Fq 'gh-sub-issue' "$help_script" "$dashboard_script"; then
  echo 'read entrypoint requires legacy gh-sub-issue' >&2
  exit 1
fi

: > "$gh_log"
dashboard_output=$(
  cd "$scratch/project"
  GH_LOG="$gh_log" PATH="$scratch/bin:$PATH" "$dashboard_script"
)
case "$dashboard_output" in
  *'GitHub is authoritative'*'Open: 4'*'Closed: 7'*'Tracking: 1'*'Runtime: 2'*) ;;
  *) echo 'dashboard did not report live GitHub counts' >&2; exit 1 ;;
esac

echo 'github-first issue reads: ok'
