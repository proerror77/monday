#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'verdict=blocked\nreason=%s\n' "$1"
  exit 1
}

value() {
  sed -n "s/^$1: *[\"']\{0,1\}\(.*[^\"']\)[\"']\{0,1\}$/\1/p" "$2" | head -1
}

check() {
  local root primary branch record recorded_root recorded_branch base
  root=$(git rev-parse --show-toplevel) || fail not_a_git_worktree
  primary=$(git worktree list --porcelain | awk '$1 == "worktree" && !found { print substr($0, 10); found=1 }')
  [[ "$root" != "$primary" ]] || fail primary_checkout
  [[ "$root" == "$primary/.worktrees/codex/"* ]] || fail unmanaged_worktree
  branch=$(git branch --show-current)
  [[ "$branch" == codex/* ]] || fail unmanaged_branch
  record=$(git rev-parse --git-path agent-worktree.yml)
  [[ -f "$record" ]] || fail missing_ownership_record
  for key in contract owner worktree branch base_sha allowed_files dependency; do
    grep -q "^$key:" "$record" || fail "missing_$key"
  done
  recorded_root=$(value worktree "$record")
  recorded_branch=$(value branch "$record")
  base=$(value base_sha "$record")
  [[ "$recorded_root" == "$root" ]] || fail worktree_mismatch
  [[ "$recorded_branch" == "$branch" ]] || fail branch_mismatch
  git rev-parse --verify -q "$base^{commit}" >/dev/null || fail invalid_base_sha
  printf 'verdict=ok\nworktree=%s\nbranch=%s\nbase_sha=%s\n' "$root" "$branch" "$base"
}

report() {
  local path= branch= prunable= line state
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ -z "$line" ]]; then
      [[ -n "$path" ]] || continue
      if [[ "$prunable" == true ]]; then state=prunable
      elif [[ -n $(git -C "$path" status --porcelain) ]]; then state=dirty
      else state=registered-clean; fi
      printf 'worktree=%s\tbranch=%s\tstate=%s\n' "$path" "$branch" "$state"
      path= branch= prunable=
    elif [[ "$line" == worktree\ * ]]; then path=${line#worktree }
    elif [[ "$line" == branch\ * ]]; then branch=${line#branch refs/heads/}
    elif [[ "$line" == prunable* ]]; then prunable=true
    fi
  done < <(git worktree list --porcelain)
  if [[ -n "$path" ]]; then
    if [[ "$prunable" == true ]]; then state=prunable
    elif [[ -n $(git -C "$path" status --porcelain) ]]; then state=dirty
    else state=registered-clean; fi
    printf 'worktree=%s\tbranch=%s\tstate=%s\n' "$path" "$branch" "$state"
  fi
}

case "${1:-check}" in
  check) check ;;
  report) report ;;
  *) echo "usage: $0 [check|report]" >&2; exit 2 ;;
esac
