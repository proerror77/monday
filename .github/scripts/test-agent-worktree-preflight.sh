#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
gate="$script_dir/agent-worktree-preflight.sh"

primary=$(git worktree list --porcelain | awk '$1 == "worktree" && !found { print substr($0, 10); found=1 }')
if (cd "$primary" && "$gate" check-managed) >/dev/null 2>&1; then
  echo 'primary checkout unexpectedly passed preflight' >&2
  exit 1
fi

# Invoking the helper without selecting a managed-writer check never rejects
# an otherwise legitimate primary checkout.
(cd "$primary" && "$gate") | grep -q '^usage:'
report=$($gate report)
grep -Eq 'state=(registered-clean|dirty|prunable)' <<<"$report"
grep -Eq 'checkout=(branch|detached)' <<<"$report"
grep -Eq 'head=[0-9a-f]{40}' <<<"$report"

fixture=$(mktemp -d)
fixture=$(cd "$fixture" && pwd -P)
trap 'rm -rf "$fixture"' EXIT
git -C "$fixture" init -q -b main
git -C "$fixture" config user.email test@example.invalid
git -C "$fixture" config user.name test
git -C "$fixture" commit -q --allow-empty -m initial
base=$(git -C "$fixture" rev-parse HEAD)
managed="$fixture/workspaces/owned"
git -C "$fixture" worktree add -q -b reviewed-branch "$managed" HEAD
record=$(git -C "$managed" rev-parse --git-path agent-worktree.yml)
printf '%s\n' \
  'contract: test' \
  'owner: test' \
  "worktree: $managed" \
  'branch: reviewed-branch' \
  "base_sha: $base" \
  'allowed_files: test' \
  'dependency: none' >"$record"
output=$(cd "$managed" && "$gate" check-managed)
grep -qx 'verdict=ok' <<<"$output"
git -C "$fixture" worktree add -q --detach "$fixture/detached" HEAD
fixture_report=$(cd "$fixture" && "$gate" report)
grep -Eq "worktree=$fixture/detached.*checkout=detached.*state=registered-clean" <<<"$fixture_report"

write_packet() {
  local dest=$1 writer=$2 branch=$3 files=$4
  local pr=${5:-none}
  cat >"$dest" <<EOF
from: Cursor
to: Cursor
routed_by: Monk
goal: test lease $branch
evidence_paths: none
constraints: analysis-only
done_criteria: lease identities
trading_gates: none
branch: $branch
writer: $writer
allowed_files:
  - $files
deadline: 2026-09-22T00:00:00Z
pr: $pr
EOF
}

cd "$fixture"
write_packet "$fixture/packet-a.yml" grok cursor/test-a docs/a.md
apply_a=$(cd "$fixture" && "$gate" apply --packet-file "$fixture/packet-a.yml")
grep -qx 'verdict=ok' <<<"$apply_a"
lease_a=$(sed -n 's/^lease_id=//p' <<<"$apply_a")
wt_a=$(sed -n 's/^worktree=//p' <<<"$apply_a")
[[ -d $wt_a ]]
record_a=$(git -C "$wt_a" rev-parse --git-path agent-worktree.yml)
grep -qx 'schema: monday.agent_lease.v2' "$record_a"
grep -qx "lease_id: $lease_a" "$record_a"
grep -qx 'status: active' "$record_a"
managed_a=$(cd "$wt_a" && "$gate" check-managed)
grep -qx 'verdict=ok' <<<"$managed_a"

write_packet "$fixture/packet-overlap.yml" human cursor/test-overlap docs/a.md
overlap_out=$(cd "$fixture" && "$gate" apply --packet-file "$fixture/packet-overlap.yml" 2>&1 || true)
grep -q 'reason=allowed_files_overlap' <<<"$overlap_out"
grep -qx 'status: active' "$record_a"

write_packet "$fixture/packet-same-branch.yml" human cursor/test-a docs/b.md
same_branch_out=$(cd "$fixture" && "$gate" apply --packet-file "$fixture/packet-same-branch.yml" 2>&1 || true)
grep -q 'reason=branch_already_leased' <<<"$same_branch_out"

write_packet "$fixture/packet-main.yml" human cursor/test-main docs/c.md
sed -i.bak 's|branch: cursor/test-main|branch: main|' "$fixture/packet-main.yml"
main_out=$(cd "$fixture" && "$gate" apply --packet-file "$fixture/packet-main.yml" 2>&1 || true)
grep -q 'reason=invalid_branch' <<<"$main_out"

write_packet "$fixture/packet-primary.yml" human cursor/test-primary docs/d.md
primary_out=$(cd "$fixture" && "$gate" apply --packet-file "$fixture/packet-primary.yml" --worktree "$fixture" 2>&1 || true)
grep -q 'reason=primary_checkout' <<<"$primary_out"

printf 'dirty\n' >"$wt_a/dirty.txt"
dirty_out=$(cd "$fixture" && "$gate" release "$lease_a" 2>&1 || true)
grep -q 'reason=dirty_worktree' <<<"$dirty_out"
rm -f "$wt_a/dirty.txt"

git -C "$wt_a" config user.email test@example.invalid
git -C "$wt_a" config user.name test
git -C "$wt_a" commit -q --allow-empty -m unique
unique_out=$(cd "$fixture" && "$gate" release "$lease_a" 2>&1 || true)
grep -q 'reason=unique_unpushed' <<<"$unique_out"
release_discard=$(cd "$fixture" && "$gate" release "$lease_a" --discard-unique)
grep -qx 'verdict=ok' <<<"$release_discard"
grep -qx 'status=released' <<<"$release_discard"
[[ ! -d $wt_a ]]

write_packet "$fixture/packet-clean.yml" grok cursor/test-clean docs/e.md
apply_clean=$(cd "$fixture" && "$gate" apply --packet-file "$fixture/packet-clean.yml")
grep -qx 'verdict=ok' <<<"$apply_clean"
lease_clean=$(sed -n 's/^lease_id=//p' <<<"$apply_clean")
wt_clean=$(sed -n 's/^worktree=//p' <<<"$apply_clean")
release_clean=$(cd "$fixture" && "$gate" release "$lease_clean")
grep -qx 'verdict=ok' <<<"$release_clean"
list_out=$(cd "$fixture" && "$gate" list)
grep -F "worktree=$wt_clean" <<<"$list_out" && {
  printf 'released worktree still listed: %s\n' "$wt_clean" >&2
  exit 1
}
"$gate" help | grep -q spawn

write_packet "$fixture/packet-spawn.yml" cursor-cloud cursor/test-spawn docs/spawn.md
sed -i.bak 's|deadline: 2026-09-22T00:00:00Z|deadline: 2099-01-01T00:00:00Z|' "$fixture/packet-spawn.yml"
apply_spawn=$(cd "$fixture" && "$gate" apply --packet-file "$fixture/packet-spawn.yml")
grep -qx 'verdict=ok' <<<"$apply_spawn"
lease_spawn=$(sed -n 's/^lease_id=//p' <<<"$apply_spawn")
"$gate" get "$lease_spawn" >"$fixture/before-dry-run.yml"
spawn_out=$(cd "$fixture" && "$gate" spawn "$lease_spawn")
grep -qx 'verdict=ok' <<<"$spawn_out"
grep -qx 'mode=dry-run' <<<"$spawn_out"
grep -q 'cursor-agent --print --mode plan' <<<"$spawn_out"
grep -qx 'spawn_count: 0' <<<"$(cd "$fixture" && "$gate" get "$lease_spawn")"
"$gate" get "$lease_spawn" >"$fixture/after-dry-run.yml"
cmp "$fixture/before-dry-run.yml" "$fixture/after-dry-run.yml"
[[ ! -d "$fixture/.git/agent-leases/${lease_spawn}.runs" ]]

write_packet "$fixture/packet-expired.yml" codex cursor/test-expired docs/expired.md
sed -i.bak 's|deadline: 2026-09-22T00:00:00Z|deadline: 2000-01-01T00:00:00Z|' "$fixture/packet-expired.yml"
apply_expired=$(cd "$fixture" && "$gate" apply --packet-file "$fixture/packet-expired.yml")
lease_expired=$(sed -n 's/^lease_id=//p' <<<"$apply_expired")
wt_expired=$(sed -n 's/^worktree=//p' <<<"$apply_expired")
expired_spawn=$(cd "$fixture" && "$gate" spawn "$lease_expired" 2>&1 || true)
grep -q 'reason=lease_expired' <<<"$expired_spawn"
sweep_out=$(cd "$fixture" && "$gate" sweep)
grep -qx 'verdict=ok' <<<"$sweep_out"
grep -qx 'expired=1' <<<"$sweep_out"
[[ -d $wt_expired ]]
grep -qx 'status: expired' <<<"$(cd "$fixture" && "$gate" get "$lease_expired")"

write_packet "$fixture/packet-live.yml" human cursor/test-live docs/live.md
sed -i.bak -e 's|deadline: 2026-09-22T00:00:00Z|deadline: 2099-01-01T00:00:00Z|' \
  -e 's|trading_gates: none|trading_gates: live|' "$fixture/packet-live.yml"
apply_live=$(cd "$fixture" && "$gate" apply --packet-file "$fixture/packet-live.yml")
lease_live=$(sed -n 's/^lease_id=//p' <<<"$apply_live")
live_spawn=$(cd "$fixture" && "$gate" spawn "$lease_live" 2>&1 || true)
grep -q 'reason=trading_gates_blocked' <<<"$live_spawn"

write_packet "$fixture/packet-squash.yml" grok cursor/test-squash docs/f.md 42
apply_squash=$(cd "$fixture" && "$gate" apply --packet-file "$fixture/packet-squash.yml")
grep -qx 'verdict=ok' <<<"$apply_squash"
lease_squash=$(sed -n 's/^lease_id=//p' <<<"$apply_squash")
wt_squash=$(sed -n 's/^worktree=//p' <<<"$apply_squash")
git -C "$wt_squash" config user.email test@example.invalid
git -C "$wt_squash" config user.name test
git -C "$wt_squash" commit -q --allow-empty -m unique-squash
squash_block=$(cd "$fixture" && "$gate" release "$lease_squash" 2>&1 || true)
grep -q 'reason=unique_unpushed' <<<"$squash_block"
git -C "$fixture" commit -q --allow-empty -m 'feat: squash stand-in (#42)'
release_squash=$(cd "$fixture" && "$gate" release "$lease_squash")
grep -qx 'verdict=ok' <<<"$release_squash"
[[ ! -d $wt_squash ]]

orphan="$fixture/.worktrees/cursor/orphan"
mkdir -p "$(dirname "$orphan")"
git -C "$fixture" worktree add -q -b cursor/orphan "$orphan" HEAD
release_orphan=$(cd "$fixture" && "$gate" release "$orphan")
grep -qx 'verdict=ok' <<<"$release_orphan"
[[ ! -d $orphan ]]

# Execute only fixture CLIs. The packet must remain data even when it contains
# shell syntax, multiline fields, quotes, spaces, and trailing newlines.
"$gate" release "$lease_spawn" >/dev/null
mkdir -p "$fixture/fake-bin"
cat >"$fixture/fake-bin/cursor-agent" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
before_prompt=$(($# - 1))
[[ ${!before_prompt} == -- ]] || exit 76
printf '%s' "${!#}" >"$AGENT_CAPTURE_PROMPT"
printf '%s\n' "$PWD" >"$AGENT_CAPTURE_PROMPT.cwd"
printf 'fixture stdout\n'
printf 'fixture stderr\n' >&2
touch "$AGENT_CAPTURE_PROMPT.started"
if [[ -n ${AGENT_WAIT_FILE:-} ]]; then
  for ((i=0; i<600; i++)); do
    [[ ! -e $AGENT_WAIT_FILE ]] || break
    sleep 0.05
  done
  [[ -e $AGENT_WAIT_FILE ]] || exit 99
fi
exit "${AGENT_EXIT_CODE:-0}"
EOF
chmod +x "$fixture/fake-bin/cursor-agent"
cp "$fixture/fake-bin/cursor-agent" "$fixture/fake-bin/codex"
real_date=$(command -v date)
cat >"$fixture/fake-bin/date" <<'EOF'
#!/usr/bin/env bash
if [[ $* == '-u +%Y-%m-%dT%H:%M:%SZ' && -n ${AGENT_CLOCK_FILE:-} ]]; then
  cat "$AGENT_CLOCK_FILE"
else
  exec "$AGENT_REAL_DATE" "$@"
fi
EOF
chmod +x "$fixture/fake-bin/date"
export AGENT_REAL_DATE=$real_date
export PATH="$fixture/fake-bin:$PATH"
export AGENT_CAPTURE_PROMPT="$fixture/captured-prompt"

write_packet "$fixture/full-packet.yml" cursor-cloud cursor/full-packet docs/evidence.md
sed -i.bak 's|deadline: 2026-09-22T00:00:00Z|deadline: 2099-01-01T00:00:00Z|' "$fixture/full-packet.yml"
{ printf '%s\n' '---'; cat "$fixture/full-packet.yml"; } >"$fixture/yaml-document"
mv "$fixture/yaml-document" "$fixture/full-packet.yml"
cat >>"$fixture/full-packet.yml" <<'EOF'
notes: |
  Evidence: /a path/with spaces and "quotes"/receipt.json
  Never run $(touch should-not-exist) or `touch also-should-not-exist`.
  Preserve the named SHA; read back the PR and its actual check results.

EOF
cp "$fixture/full-packet.yml" "$fixture/expected-prompt"
full_apply=$("$gate" apply --packet-file "$fixture/full-packet.yml")
full_id=$(sed -n 's/^lease_id=//p' <<<"$full_apply")
full_wt=$(sed -n 's/^worktree=//p' <<<"$full_apply")
full_source_head=$(git -C "$full_wt" rev-parse HEAD)
full_record="$fixture/.git/agent-leases/$full_id.yml"
full_snapshot="$fixture/.git/agent-leases/$full_id.packet"
printf 'changed after admission\n' >"$fixture/full-packet.yml"
full_out=$("$gate" spawn "$full_id" --execute)
cmp "$fixture/expected-prompt" "$AGENT_CAPTURE_PROMPT"
grep -Fxq "$full_wt" "$AGENT_CAPTURE_PROMPT.cwd"
[[ ! -e "$full_wt/should-not-exist" && ! -e "$full_wt/also-should-not-exist" ]]
grep -qx 'exit_code=0' <<<"$full_out"
grep -qx 'task_status=unverified' <<<"$full_out"
start_receipt=$(sed -n 's/^start_receipt=//p' <<<"$full_out")
terminal_receipt=$(sed -n 's/^terminal_receipt=//p' <<<"$full_out")
grep -qx "source_head: $full_source_head" "$start_receipt"
grep -qx "packet_sha256: $(sed -n 's/^packet_sha256=//p' <<<"$full_apply")" "$start_receipt"
grep -qx 'process_status: exited' "$terminal_receipt"
grep -qx 'task_status: unverified' "$terminal_receipt"
[[ ! -e "$fixture/.git/agent-leases/$full_id.running" ]]
grep -qx 'fixture stdout' "$(dirname "$start_receipt")/stdout.log"
grep -qx 'fixture stderr' "$(dirname "$start_receipt")/stderr.log"

printf 'corrupt\n' >>"$full_snapshot"
corrupt_out=$("$gate" spawn "$full_id" --execute 2>&1 || true)
grep -qx 'reason=packet_hash_mismatch' <<<"$corrupt_out"
cp "$fixture/expected-prompt" "$full_snapshot"
cp "$full_record" "$fixture/good-record"
sed -i.bak 's/^allowed_files:.*/allowed_files: another\/path/' "$full_record"
binding_out=$("$gate" spawn "$full_id" 2>&1 || true)
grep -qx 'reason=packet_binding_mismatch' <<<"$binding_out"
cp "$fixture/good-record" "$full_record"

git -C "$full_wt" switch -q -c cursor/drifted-branch
drift_out=$("$gate" spawn "$full_id" 2>&1 || true)
grep -qx 'reason=branch_mismatch' <<<"$drift_out"
git -C "$full_wt" switch -q cursor/full-packet
git -C "$full_wt" commit -q --allow-empty -m 'worker continuation'
if AGENT_EXIT_CODE=7 "$gate" spawn "$full_id" --execute >"$fixture/failed-run"; then
  echo 'nonzero worker exit was hidden' >&2
  exit 1
else
  [[ $? == 7 ]]
fi
grep -qx 'exit_code=7' "$fixture/failed-run"
failed_terminal=$(sed -n 's/^terminal_receipt=//p' "$fixture/failed-run")
grep -qx 'exit_code: 7' "$failed_terminal"
grep -qx 'task_status: unverified' "$failed_terminal"
[[ ! -e "$fixture/.git/agent-leases/$full_id.running" ]]

# A v1 record cannot recreate a missing packet from current operator inputs.
sed -i.bak 's/schema: monday.agent_lease.v2/schema: monday.agent_lease.v1/' "$full_record"
cp "$full_record" "$fixture/legacy-record"
legacy_out=$("$gate" spawn "$full_id" 2>&1 || true)
grep -qx 'reason=packet_snapshot_missing' <<<"$legacy_out"
cmp "$fixture/legacy-record" "$full_record"
cp "$fixture/good-record" "$full_record"
"$gate" release "$full_id" --discard-unique >/dev/null

# Reuse a released path so historical records cannot hide its current worker.
cp "$fixture/expected-prompt" "$fixture/full-packet.yml"
again_apply=$("$gate" apply --packet-file "$fixture/full-packet.yml")
again_id=$(sed -n 's/^lease_id=//p' <<<"$again_apply")
export AGENT_CLOCK_FILE="$fixture/clock"
printf '2098-01-01T00:00:00Z\n' >"$AGENT_CLOCK_FILE"
export AGENT_WAIT_FILE="$fixture/worker-can-exit"
rm -f "$AGENT_CAPTURE_PROMPT.started"
"$gate" spawn "$again_id" --execute >"$fixture/running-output" &
controller_pid=$!
wait_for_file() {
  local file=$1
  for ((i=0; i<200; i++)); do
    [[ ! -s $file ]] || return 0
    sleep 0.05
  done
  printf 'timed out waiting for %s\n' "$file" >&2
  return 1
}
wait_for_file "$fixture/.git/agent-leases/$again_id.running"
for ((i=0; i<200; i++)); do
  [[ ! -e $AGENT_CAPTURE_PROMPT.started ]] || break
  sleep 0.05
done
[[ -e $AGENT_CAPTURE_PROMPT.started ]]
duplicate=$("$gate" spawn "$again_id" --execute 2>&1 || true)
grep -qx 'reason=execution_unresolved' <<<"$duplicate"
printf '2100-01-01T00:00:00Z\n' >"$AGENT_CLOCK_FILE"
"$gate" sweep >"$fixture/sweep-running"
grep -qx 'status: expired' <<<"$("$gate" get "$again_id")"
"$gate" list >"$fixture/list-running"
grep -F "worktree=$full_wt" "$fixture/list-running" | grep -q "lease_id=$again_id.*cleanup_safety=keep"
for key in "$again_id" "$full_wt"; do
  release_running=$("$gate" release "$key" --discard-unique 2>&1 || true)
  grep -qx 'reason=execution_unresolved' <<<"$release_running"
done
write_packet "$fixture/running-overlap.yml" grok cursor/running-overlap docs/evidence.md
running_overlap=$("$gate" apply --packet-file "$fixture/running-overlap.yml" 2>&1 || true)
grep -qx 'reason=allowed_files_overlap' <<<"$running_overlap"
# The marker retains scope even if a lease record is lost or corrupted.
mv "$fixture/.git/agent-leases/$again_id.yml" "$fixture/saved-running-lease"
orphan_overlap=$("$gate" apply --packet-file "$fixture/running-overlap.yml" 2>&1 || true)
grep -qx 'reason=allowed_files_overlap' <<<"$orphan_overlap"
"$gate" list >"$fixture/list-orphan-marker"
grep -F "worktree=$full_wt" "$fixture/list-orphan-marker" | grep -q 'cleanup_safety=keep'
mv "$fixture/saved-running-lease" "$fixture/.git/agent-leases/$again_id.yml"
cp "$fixture/.git/agent-leases/$again_id.running" "$fixture/saved-marker"
printf 'damaged marker\n' >"$fixture/.git/agent-leases/$again_id.running"
damaged_overlap=$("$gate" apply --packet-file "$fixture/running-overlap.yml" 2>&1 || true)
grep -qx 'reason=execution_marker_invalid' <<<"$damaged_overlap"
cp "$fixture/saved-marker" "$fixture/.git/agent-leases/$again_id.running"
touch "$AGENT_WAIT_FILE"
wait "$controller_pid"
[[ ! -e "$fixture/.git/agent-leases/$again_id.running" ]]
"$gate" release "$again_id" --discard-unique >/dev/null

# Controller interruption leaves an unresolved marker even if its PID vanishes.
printf '2098-01-01T00:00:00Z\n' >"$AGENT_CLOCK_FILE"
rm -f "$AGENT_WAIT_FILE" "$AGENT_CAPTURE_PROMPT.started"
interrupted_apply=$("$gate" apply --packet-file "$fixture/full-packet.yml")
interrupted_id=$(sed -n 's/^lease_id=//p' <<<"$interrupted_apply")
"$gate" spawn "$interrupted_id" --execute >"$fixture/interrupted-output" &
interrupted_controller=$!
wait_for_file "$fixture/.git/agent-leases/$interrupted_id.running"
execution_id=$(sed -n 's/^execution_id: //p' "$fixture/.git/agent-leases/$interrupted_id.running")
interrupted_dir="$fixture/.git/agent-leases/$interrupted_id.runs/$execution_id"
wait_for_file "$interrupted_dir/start.yml"
interrupted_worker=$(sed -n 's/^worker_pid: //p' "$interrupted_dir/start.yml")
kill -TERM "$interrupted_controller"
if wait "$interrupted_controller"; then exit 1; else [[ $? == 143 ]]; fi
kill -0 "$interrupted_worker"
[[ -s "$fixture/.git/agent-leases/$interrupted_id.running" ]]
[[ ! -e "$interrupted_dir/terminal.yml" ]]
grep -qx 'process_status: unresolved' "$interrupted_dir/controller-exit.yml"
interrupted_release=$("$gate" release "$full_wt" --discard-unique 2>&1 || true)
grep -qx 'reason=execution_unresolved' <<<"$interrupted_release"
# Let the fixture child finish; its controller's absence never clears the marker.
touch "$AGENT_WAIT_FILE"
kill -TERM "$interrupted_worker" 2>/dev/null || true
[[ -e "$fixture/.git/agent-leases/$interrupted_id.running" ]]

repo_src="$fixture/declared-repo"
mkdir -p "$repo_src"
printf 'repo-file\n' >"$repo_src/README"
secret_file="$fixture/model.secret"
printf 'super-secret-token\n' >"$secret_file"
cat >"$fixture/agent-task.yml" <<EOF
schema: monday.agent_task.v1
agent_id: fixture-agent
contract: fixture-contract
cpu: 1
memory_mb: 256
allow_hosts: github.com,example.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: sh -c 'test -d "\$MONDAY_AGENT_WORKSPACE/repos/fixture" && test -s "\$MONDAY_AGENT_WORKSPACE/repos/fixture/README" && test -s "\$MONDAY_AGENT_WORKSPACE/tools/skill" && test -s "\$MONDAY_AGENT_WORKSPACE/.bounds" || exit 3; if [ ! -f "\$MONDAY_AGENT_WORKSPACE/marker" ]; then echo first > "\$MONDAY_AGENT_WORKSPACE/marker"; fi; echo run >> "\$MONDAY_AGENT_WORKSPACE/runs"'
EOF
pending=$("$gate" task-declare --file "$fixture/agent-task.yml")
grep -qx 'verdict=ok' <<<"$pending"
grep -qx 'phase=pending' <<<"$pending"
task_state="$fixture/.git/agent-tasks/fixture-agent"
grep -qx 'cpu: 1' "$task_state/task.yml"
grep -qx 'memory_mb: 256' "$task_state/task.yml"
! grep -q 'super-secret-token' "$task_state/task.yml"
invoke1=$("$gate" task-invoke fixture-agent)
grep -qx 'verdict=ok' <<<"$invoke1"
grep -qx 'agent_id=fixture-agent' <<<"$invoke1"
grep -qx 'phase=suspended' <<<"$invoke1"
grep -qx 'cpu=1' <<<"$invoke1"
grep -qx 'memory_mb=256' <<<"$invoke1"
grep -qx 'first' "$task_state/workspace/marker"
grep -qx 'cpu=1' "$task_state/workspace/.bounds"
grep -qx 'memory_mb=256' "$task_state/workspace/.bounds"
! grep -q 'super-secret-token' "$task_state/command.log" "$task_state/invocation.log" "$task_state/invocation.err" "$task_state/task.yml"
allowed_host=$("$gate" task-egress fixture-agent github.com)
grep -qx 'verdict=ok' <<<"$allowed_host"
denied_host=$("$gate" task-egress fixture-agent evil.example 2>&1 || true)
grep -qx 'reason=host_not_allowed' <<<"$denied_host"
suspend_out=$("$gate" task-suspend fixture-agent)
grep -qx 'phase=suspended' <<<"$suspend_out"
status_out=$("$gate" task-status fixture-agent)
grep -qx 'phase=suspended' <<<"$status_out"
grep -qx 'agent_id=fixture-agent' <<<"$status_out"
invoke2=$("$gate" task-invoke fixture-agent)
grep -qx 'agent_id=fixture-agent' <<<"$invoke2"
grep -qx 'phase=suspended' <<<"$invoke2"
grep -qx 'first' "$task_state/checkpoint/marker"
runs=$(wc -l <"$task_state/workspace/runs" | tr -d ' ')
[[ $runs == 2 ]]
mkdir -p "$fixture/.git/agent-leases"
cat >"$fixture/.git/agent-leases/occupied.yml" <<EOF
status: active
contract: occupied-contract
EOF
cat >"$fixture/occupied-task.yml" <<EOF
schema: monday.agent_task.v1
agent_id: occupied-agent
contract: occupied-contract
cpu: 1
memory_mb: 128
allow_hosts: github.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: true
EOF
"$gate" task-declare --file "$fixture/occupied-task.yml" >/dev/null
occupied_invoke=$("$gate" task-invoke occupied-agent 2>&1 || true)
grep -qx 'reason=writer_already_active' <<<"$occupied_invoke"

cat >"$fixture/edit-task.yml" <<EOF
schema: monday.agent_task.v1
agent_id: edit-agent
contract: edit-contract
cpu: 1
memory_mb: 256
allow_hosts: github.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: sh -c 'if [ ! -f "\$MONDAY_AGENT_WORKSPACE/marker" ]; then printf edited-by-agent > "\$MONDAY_AGENT_WORKSPACE/repos/fixture/README"; echo first > "\$MONDAY_AGENT_WORKSPACE/marker"; fi; echo run >> "\$MONDAY_AGENT_WORKSPACE/runs"'
EOF
"$gate" task-declare --file "$fixture/edit-task.yml" >/dev/null
"$gate" task-invoke edit-agent >/dev/null
"$gate" task-suspend edit-agent >/dev/null
"$gate" task-invoke edit-agent >/dev/null
grep -qx 'edited-by-agent' "$fixture/.git/agent-tasks/edit-agent/workspace/repos/fixture/README"

cat >"$fixture/mem-task.yml" <<EOF
schema: monday.agent_task.v1
agent_id: mem-agent
contract: mem-contract
cpu: 1
memory_mb: 32
allow_hosts: github.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: bash -c 'x=\$(printf "%80000000s" ""); printf "%s" \${#x}'
EOF
"$gate" task-declare --file "$fixture/mem-task.yml" >/dev/null
mem_out=$("$gate" task-invoke mem-agent 2>&1 || true)
grep -qx 'reason=memory_exceeded' <<<"$mem_out" || {
  printf 'memory assertion failed:\n%s\n' "$mem_out" >&2
  exit 1
}
! grep -qx 'verdict=ok' <<<"$mem_out"

cat >"$fixture/sleep-task.yml" <<EOF
schema: monday.agent_task.v1
agent_id: sleep-agent
contract: sleep-contract
cpu: 1
memory_mb: 256
allow_hosts: github.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: sleep 40
EOF
"$gate" task-declare --file "$fixture/sleep-task.yml" >/dev/null
sleep_out=$("$gate" task-invoke sleep-agent)
grep -qx 'verdict=ok' <<<"$sleep_out" || {
  printf 'in-budget sleep failed:\n%s\n' "$sleep_out" >&2
  exit 1
}
grep -qx 'phase=suspended' <<<"$sleep_out"
grep -qx 'suspended' "$fixture/.git/agent-tasks/sleep-agent/phase"

cat >"$fixture/net-task.yml" <<EOF
schema: monday.agent_task.v1
agent_id: net-agent
contract: net-contract
cpu: 1
memory_mb: 256
allow_hosts: github.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: curl -sS -o /dev/null -w '%{http_code}' https://example.com
EOF
"$gate" task-declare --file "$fixture/net-task.yml" >/dev/null
set +e
net_out=$("$gate" task-invoke net-agent 2>&1)
net_status=$?
set -e
grep -qx 'reason=host_not_allowed' <<<"$net_out" || {
  printf 'egress assertion failed status=%s:\n%s\n' "$net_status" "$net_out" >&2
  exit 1
}
! grep -qx 'verdict=ok' <<<"$net_out"
! grep -q '200' <<<"$net_out"
[[ $net_status != 77 ]]
grep -qx 'example.com' "$fixture/.git/agent-tasks/net-agent/egress-refused"
! grep -q '200' "$fixture/.git/agent-tasks/net-agent/invocation.log" "$fixture/.git/agent-tasks/net-agent/invocation.err"

if [[ -x /usr/bin/curl ]]; then
  cat >"$fixture/abs-curl.yml" <<EOF
schema: monday.agent_task.v1
agent_id: abs-curl-agent
contract: abs-curl-contract
cpu: 1
memory_mb: 256
allow_hosts: github.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: /usr/bin/curl -sS -o /dev/null -w '%{http_code}' --max-time 15 https://example.com
EOF
  "$gate" task-declare --file "$fixture/abs-curl.yml" >/dev/null
  set +e
  abs_out=$("$gate" task-invoke abs-curl-agent 2>&1)
  abs_status=$?
  set -e
  grep -qx 'reason=host_not_allowed' <<<"$abs_out" || {
    printf 'absolute curl was not refused status=%s:\n%s\n' "$abs_status" "$abs_out" >&2
    printf 'log:\n%s\n' "$(cat "$fixture/.git/agent-tasks/abs-curl-agent/invocation.log" "$fixture/.git/agent-tasks/abs-curl-agent/invocation.err" 2>/dev/null)" >&2
    exit 1
  }
  [[ $abs_status != 77 ]]
  ! grep -q '200' "$fixture/.git/agent-tasks/abs-curl-agent/invocation.log" "$fixture/.git/agent-tasks/abs-curl-agent/invocation.err"
fi

cat >"$fixture/two-url.yml" <<EOF
schema: monday.agent_task.v1
agent_id: two-url-agent
contract: two-url-contract
cpu: 1
memory_mb: 256
allow_hosts: github.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: curl -sS -o /dev/null -w '%{url_effective} %{http_code}\\n' --max-time 15 https://example.com https://github.com
EOF
"$gate" task-declare --file "$fixture/two-url.yml" >/dev/null
two_out=$("$gate" task-invoke two-url-agent 2>&1 || true)
grep -qx 'reason=host_not_allowed' <<<"$two_out" || {
  printf 'multi-url curl was not refused:\n%s\n' "$two_out" >&2
  exit 1
}
! grep -q 'example.com/ 200' "$fixture/.git/agent-tasks/two-url-agent/invocation.log" "$fixture/.git/agent-tasks/two-url-agent/invocation.err"

cat >"$fixture/leak-task.yml" <<EOF
schema: monday.agent_task.v1
agent_id: leak-agent
contract: leak-contract
cpu: 1
memory_mb: 256
allow_hosts: github.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: sh -c 'cat "\${MONDAY_AGENT_WORKSPACE%/workspace}/model.secret"; curl -sS https://example.com'
EOF
"$gate" task-declare --file "$fixture/leak-task.yml" >/dev/null
leak_out=$("$gate" task-invoke leak-agent 2>&1 || true)
grep -qx 'reason=secret_leaked' <<<"$leak_out"
! grep -qx 'reason=host_not_allowed' <<<"$leak_out"

cat >"$fixture/bg-task.yml" <<EOF
schema: monday.agent_task.v1
agent_id: bg-agent
contract: bg-contract
cpu: 1
memory_mb: 256
allow_hosts: github.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: sh -c '(sleep 0.3; curl -sS -o /dev/null https://example.com) &'
EOF
"$gate" task-declare --file "$fixture/bg-task.yml" >/dev/null
bg_out=$("$gate" task-invoke bg-agent 2>&1 || true)
grep -qx 'reason=host_not_allowed' <<<"$bg_out" || {
  printf 'background egress assertion failed:\n%s\n' "$bg_out" >&2
  exit 1
}
! grep -qx 'verdict=ok' <<<"$bg_out"

if ! command -v aria2c >/dev/null 2>&1; then
  cat >"$fixture/missing-tool.yml" <<EOF
schema: monday.agent_task.v1
agent_id: missing-tool-agent
contract: missing-tool-contract
cpu: 1
memory_mb: 256
allow_hosts: github.com
model_provider: fixture-provider
model_secret_file: $secret_file
workspace_repo_name: fixture
workspace_repo_path: $repo_src
workspace_tool_name: skill
workspace_tool_endpoint: file:///fixture-skill
command: sh -c 'aria2c https://github.com/resource || true'
EOF
  "$gate" task-declare --file "$fixture/missing-tool.yml" >/dev/null
  missing_out=$("$gate" task-invoke missing-tool-agent)
  grep -qx 'verdict=ok' <<<"$missing_out"
  [[ ! -s $fixture/.git/agent-tasks/missing-tool-agent/egress-refused ]]
fi

printf 'agent worktree preflight tests passed\n'
