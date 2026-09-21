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
pr: none
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
wrapper=$(cd "$(dirname "$gate")" && pwd)/monday-agent
[[ -x $wrapper ]]
"$wrapper" help | grep -q list

printf 'agent worktree preflight tests passed\n'
