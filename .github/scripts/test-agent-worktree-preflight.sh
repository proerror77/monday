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
grep -qx 'schema: monday.agent_lease.v1' "$record_a"
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
spawn_out=$(cd "$fixture" && "$gate" spawn "$lease_spawn")
grep -qx 'verdict=ok' <<<"$spawn_out"
grep -qx 'mode=dry-run' <<<"$spawn_out"
grep -q 'cursor-agent --print --mode plan' <<<"$spawn_out"
grep -qx 'spawn_count: 1' <<<"$(cd "$fixture" && "$gate" get "$lease_spawn")"

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

write_packet "$fixture/packet-live.yml" cursor-cloud cursor/test-live docs/live.md
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

printf 'agent worktree preflight tests passed\n'
