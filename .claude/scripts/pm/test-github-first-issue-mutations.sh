#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
close="$root/.claude/commands/pm/issue-close.md"
reopen="$root/.claude/commands/pm/issue-reopen.md"
sync="$root/.claude/commands/pm/issue-sync.md"

extract() { awk '/^```bash$/ { on=1; next } on && /^```$/ { exit } on' "$1"; }
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/bin" "$scratch/project"
log="$scratch/gh.log"
state="$scratch/state"

cat > "$scratch/bin/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${GH_LOG:?}"
case "$*" in
  'issue view 123 --json state --jq .state')
    if { [ "${GH_SCENARIO:-}" = close_state_read_fail ] && [ -f "${GH_STATE:?}" ]; } || [ "${GH_SCENARIO:-}" = reopen_state_read_fail ] || [ "${GH_SCENARIO:-}" = read_fail ]; then exit 1; fi
    if [ -f "${GH_STATE:?}" ]; then cat "$GH_STATE"; else echo OPEN; fi ;;
  'api --paginate repos/{owner}/{repo}/issues/123/dependencies/blocked_by --jq'*) if [ "${GH_SCENARIO:-}" = blocker ]; then echo 999; fi ;;
  'issue view 123 --json labels --jq .labels[].name')
    case "${GH_SCENARIO:-}" in no_category) echo ready-for-agent;; conflict_category) printf '%s\n' bug enhancement ready-for-agent;; no_triage) echo enhancement;; conflict_triage) printf '%s\n' enhancement needs-triage ready-for-agent;; tracking*) printf '%s\n' enhancement ready-for-agent tracking;; runtime*) printf '%s\n' enhancement ready-for-agent runtime;; *) printf '%s\n' enhancement ready-for-agent;; esac ;;
  'api --paginate repos/{owner}/{repo}/issues/123/sub_issues --jq'*) if [ "${GH_SCENARIO:-}" = tracking_child ]; then echo 999; fi ;;
  'issue view 123 --json comments --jq'*)
    if [ "${GH_SCENARIO:-}" = runtime_comments ]; then echo 'Exact target: svc; Named controller: alice; Candidate identity: sha-1; Configuration identity: cfg-1; Rollback identity: sha-0; Rollback procedure: restore; Stop rules: any failure; Terminal result: passed; Cleanup evidence: removed'; fi ;;
  'issue view 123 --json body,comments --jq'*)
    if [ "${GH_SCENARIO:-}" = runtime_body_only ]; then echo 'Exact target: svc; Named controller: alice; Candidate identity: sha-1; Configuration identity: cfg-1; Rollback identity: sha-0; Rollback procedure: restore; Stop rules: any failure; Terminal result: passed; Cleanup evidence: removed'; fi ;;
  'issue view 123 --json number,title,state,url') [ "${GH_SCENARIO:-}" != read_fail ] && echo '{"number":123}' ;;
  'issue view 123 --json state,updatedAt,url --jq'*)
    [ "${GH_SCENARIO:-}" != reopen_final_read_fail ] || exit 1
    if [ "${GH_SCENARIO:-}" = reopen_final_closed ]; then printf 'CLOSED\tnow\thttps://example/123\n'; else printf 'OPEN\tnow\thttps://example/123\n'; fi ;;
  'issue view 123 --json comments,url') echo '{}' ;;
  'issue comment 123 --body-file'*) [ "${GH_SCENARIO:-}" != comment_fail ] ;;
  'issue close 123 --reason completed') case "${GH_SCENARIO:-}" in close_fail) exit 1;; close_open) echo OPEN > "${GH_STATE:?}";; *) echo CLOSED > "${GH_STATE:?}";; esac ;;
  'issue reopen 123') [ "${GH_SCENARIO:-}" != reopen_fail ] && echo OPEN > "${GH_STATE:?}" ;;
  'issue view 123 --json state,closedAt,comments,url') [ "${GH_SCENARIO:-}" != close_final_read_fail ] && echo '{}' ;;
  *) echo "unexpected gh invocation: $*" >&2; exit 1 ;;
esac
EOF
chmod +x "$scratch/bin/gh"

make_script() {
  { printf '#!/usr/bin/env bash\nset -u\nARGUMENTS=%q\n' "$3"; extract "$2"; } > "$1"
  chmod +x "$1"
}
run_script() (
  cd "$scratch/project"
  GH_LOG="$log" GH_STATE="$state" GH_SCENARIO="${2:-}" PATH="$scratch/bin:$PATH" "$1"
)
refuse() {
  : > "$log"; rm -f "$state"; make_script "$scratch/close.sh" "$close" "123${2:+ $2}"
  if run_script "$scratch/close.sh" "$1" >/dev/null 2>&1; then echo "close accepted: $2" >&2; exit 1; fi
  ! grep -Eq 'issue (comment|close) 123' "$log"
}
allow() {
  : > "$log"; rm -f "$state"; make_script "$scratch/close.sh" "$close" "123 $2"
  run_script "$scratch/close.sh" "$1" >/dev/null
  grep -Fq 'issue comment 123 --body-file' "$log"; grep -Fq 'issue close 123 --reason completed' "$log"
}
close_fails() { : > "$log"; rm -f "$state"; make_script "$scratch/close.sh" "$close" "123 $valid"; if run_script "$scratch/close.sh" "$1" >/dev/null 2>&1; then echo "close masked $1" >&2; exit 1; fi; }
reopen_fails() { : > "$log"; echo CLOSED > "$state"; make_script "$scratch/reopen.sh" "$reopen" '123 reason'; if run_script "$scratch/reopen.sh" "$1" >/dev/null 2>&1; then echo "reopen masked $1" >&2; exit 1; fi; }

valid='Acceptance checks: focused; Result: passed'
refuse normal ''
refuse normal 'Acceptance checks: ; Result: passed'
refuse normal 'Acceptance checks:    ; Result: passed'
refuse normal 'Acceptance checks: ** TBD **; Result: passed'
# shellcheck disable=SC2016
refuse normal 'Acceptance checks: `TBD`; Result: passed'
refuse normal 'Acceptance checks: Result : passed; Result: passed'
refuse normal 'Acceptance checks: - Result: passed; Result: passed'
refuse normal 'Acceptance checks: ~~Result:~~ passed; Result: passed'
refuse normal 'Acceptance checks: focused; Result: failed'
for scenario in blocker no_category conflict_category no_triage conflict_triage; do refuse "$scenario" "$valid"; done
refuse tracking "$valid"
refuse tracking_child "$valid; Parent acceptance audit: passed"
allow tracking_box "$valid; Parent acceptance audit: passed"
runtime='Exact target: svc; Named controller: alice; Candidate identity: sha-1; Configuration identity: cfg-1; Rollback identity: sha-0; Rollback procedure: restore; Stop rules: any failure; Terminal result: passed; Cleanup evidence: removed'
refuse runtime "$valid; ${runtime/Terminal result: passed/Terminal result: failed}"
refuse runtime_body_only "$valid"
for field in "Exact target" "Named controller" "Candidate identity" "Configuration identity" "Rollback identity" "Rollback procedure" "Stop rules" "Terminal result" "Cleanup evidence"; do
  missing=$(printf '%s' "$runtime" | sed -E "s/(^|; )$field: [^;]*(; |$)/\\1/")
  placeholder=$(printf '%s' "$runtime" | sed -E "s/($field: )[^;]*/\\1TBD/")
  negative=$(printf '%s' "$runtime" | sed -E "s/($field: )[^;]*/\\1failed/")
  refuse runtime "$valid; $missing"; refuse runtime "$valid; $placeholder"; refuse runtime "$valid; $negative"
done
allow normal "$valid"
allow tracking "$valid; Parent acceptance audit: passed"
allow runtime_comments "$valid"
for scenario in comment_fail close_fail close_open close_state_read_fail close_final_read_fail; do close_fails "$scenario"; done
for scenario in read_fail reopen_fail comment_fail reopen_state_read_fail reopen_final_closed reopen_final_read_fail; do reopen_fails "$scenario"; done

: > "$log"; echo CLOSED > "$state"; make_script "$scratch/reopen.sh" "$reopen" '123 retry reason'
if run_script "$scratch/reopen.sh" comment_fail >/dev/null 2>&1; then echo 'reopen masked comment failure' >&2; exit 1; fi
run_script "$scratch/reopen.sh" normal >/dev/null
[ "$(grep -c '^issue reopen 123$' "$log")" -eq 1 ]
[ "$(grep -c '^issue comment 123 --body-file -$' "$log")" -eq 2 ]

: > "$log"; echo CLOSED > "$state"
make_script "$scratch/reopen.sh" "$reopen" '123 regression reproduced'
run_script "$scratch/reopen.sh" normal >/dev/null
grep -Fq 'issue reopen 123' "$log"
grep -Fq 'issue comment 123 --body-file -' "$log"
grep -Fq 'Local mirror: stale' "$reopen"

: > "$log"; make_script "$scratch/sync.sh" "$sync" 123
output=$(run_script "$scratch/sync.sh" normal)
case "$output" in *'No explicit new evidence; nothing to sync.'*) ;; *) exit 1;; esac
if grep -Fq 'issue comment 123' "$log"; then echo 'no-op sync posted evidence' >&2; exit 1; fi
printf 'evidence\n' > "$scratch/evidence.md"
: > "$log"; make_script "$scratch/sync.sh" "$sync" "123 $scratch/evidence.md"
run_script "$scratch/sync.sh" normal >/dev/null
grep -Fq "issue comment 123 --body-file $scratch/evidence.md" "$log"
: > "$scratch/empty.md"; : > "$log"
make_script "$scratch/sync.sh" "$sync" "123 $scratch/empty.md"
if run_script "$scratch/sync.sh" normal >/dev/null 2>&1; then echo 'sync accepted empty evidence' >&2; exit 1; fi
if grep -Fq 'issue comment 123' "$log"; then echo 'empty sync posted evidence' >&2; exit 1; fi

echo 'github-first issue mutations: ok'
