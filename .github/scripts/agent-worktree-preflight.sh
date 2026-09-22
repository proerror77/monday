#!/usr/bin/env bash
set -euo pipefail
umask 077

cleanup() {
  local exit_status=$?
  trap - EXIT
  lease_lock_release
  [[ -z ${PENDING_PACKET:-} ]] || rm -f -- "$PENDING_PACKET"
  # An interrupted wait is not evidence that the worker stopped. Keep the
  # marker, including when the controller exits before writing its start receipt.
  if [[ -n ${RUN_DIR:-} && ${RUN_FINISHED:-0} != 1 ]]; then
    printf 'execution_id: %s\ncontroller_exit_code: %s\nprocess_status: unresolved\ntask_status: unverified\n' \
      "$RUN_ID" "$exit_status" >"$RUN_DIR/controller-exit.yml"
  fi
  exit "$exit_status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

fail() {
  printf 'verdict=blocked\nreason=%s\n' "$1"
  exit 1
}

value() {
  sed -n "s/^$1: *[\"']\{0,1\}\(.*[^\"']\)[\"']\{0,1\}$/\1/p" "$2" | head -1
}

check_managed() {
  local root primary branch record recorded_root recorded_branch base
  root=$(git rev-parse --show-toplevel) || fail not_a_git_worktree
  primary=$(git worktree list --porcelain | awk '$1 == "worktree" && !found { print substr($0, 10); found=1 }')
  [[ "$root" != "$primary" ]] || fail primary_checkout
  branch=$(git branch --show-current)
  [[ -n "$branch" ]] || fail detached_writer
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
  local path='' branch='' head='' prunable='' line state checkout
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ -z "$line" ]]; then
      [[ -n "$path" ]] || continue
      if [[ "$prunable" == true ]]; then state=prunable
      elif [[ -n $(git -C "$path" status --porcelain) ]]; then state=dirty
      else state=registered-clean; fi
      if [[ -n "$branch" ]]; then checkout=branch; else checkout=detached; fi
      printf 'worktree=%s\tbranch=%s\thead=%s\tcheckout=%s\tstate=%s\n' \
        "$path" "$branch" "$head" "$checkout" "$state"
      path='' branch='' head='' prunable=''
    elif [[ "$line" == worktree\ * ]]; then path=${line#worktree }
    elif [[ "$line" == branch\ * ]]; then branch=${line#branch refs/heads/}
    elif [[ "$line" == HEAD\ * ]]; then head=${line#HEAD }
    elif [[ "$line" == prunable* ]]; then prunable=true
    fi
  done < <(git worktree list --porcelain)
  if [[ -n "$path" ]]; then
    if [[ "$prunable" == true ]]; then state=prunable
    elif [[ -n $(git -C "$path" status --porcelain) ]]; then state=dirty
    else state=registered-clean; fi
    if [[ -n "$branch" ]]; then checkout=branch; else checkout=detached; fi
    printf 'worktree=%s\tbranch=%s\thead=%s\tcheckout=%s\tstate=%s\n' \
      "$path" "$branch" "$head" "$checkout" "$state"
  fi
}

# Strict ownership validation is opt-in for a managed writer. Ordinary local
# editing uses the task's ownership assessment, not a mandatory worktree gate.

# Lease helpers for monday-agent. Sourced by agent-worktree-preflight.sh.
# Lab-only: manages worktree ownership and explicit local CLI executions.

yaml_scalar() {
  local key=$1 file=$2
  local line
  line=$(grep -E "^${key}:" "$file" | head -1 || true)
  [[ -n $line ]] || return 1
  line=${line#*:}
  line=${line#"${line%%[![:space:]]*}"}
  line=${line%"${line##*[![:space:]]}"}
  if [[ $line == \"*\" || $line == \'*\' ]]; then
    line=${line:1:${#line}-2}
  fi
  printf '%s\n' "$line"
}

yaml_list() {
  local key=$1 file=$2
  local scalar in_list=0
  if scalar=$(yaml_scalar "$key" "$file" 2>/dev/null); then
    if [[ -n $scalar ]]; then
      local IFS=,
      local item
      for item in $scalar; do
        item=${item#"${item%%[![:space:]]*}"}
        item=${item%"${item##*[![:space:]]}"}
        [[ -n $item ]] && printf '%s\n' "$item"
      done
      return 0
    fi
  fi
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ $line == "$key:" ]]; then
      in_list=1
      continue
    fi
    if ((in_list)); then
      if [[ $line =~ ^[[:space:]]*-[[:space:]]*(.*)$ ]]; then
        local item=${BASH_REMATCH[1]}
        item=${item#\"}
        item=${item%\"}
        printf '%s\n' "$item"
      elif [[ $line =~ ^[^[:space:]] ]]; then
        break
      fi
    fi
  done <"$file"
}

git_primary() {
  git worktree list --porcelain | awk '$1 == "worktree" && !found { print substr($0, 10); found=1 }'
}

git_common() {
  git rev-parse --path-format=absolute --git-common-dir
}

integration_tip() {
  if git rev-parse --verify -q origin/main >/dev/null; then
    git rev-parse origin/main
  elif git rev-parse --verify -q main >/dev/null; then
    git rev-parse main
  else
    git rev-parse HEAD
  fi
}

paths_overlap() {
  local a=$1 b=$2
  [[ -z $a || -z $b ]] && return 1
  [[ $a == "$b" ]] && return 0
  case $a in
    "$b"/*) return 0 ;;
  esac
  case $b in
    "$a"/*) return 0 ;;
  esac
  # shellcheck disable=SC2053
  [[ $a == $b ]] && return 0
  # shellcheck disable=SC2053
  [[ $b == $a ]] && return 0
  return 1
}

lease_store() {
  printf '%s/agent-leases\n' "$(git_common)"
}

lease_lock_acquire() {
  local dir
  dir="$(git_common)/agent-leases.lock"
  mkdir -p "$(git_common)"
  local i=0
  while ! mkdir "$dir" 2>/dev/null; do
    sleep 0.05
    i=$((i + 1))
    if ((i >= 200)); then
      fail lock_timeout
    fi
  done
  LEASE_LOCK_DIR=$dir
}

lease_lock_release() {
  if [[ -n ${LEASE_LOCK_DIR:-} ]]; then
    rmdir "$LEASE_LOCK_DIR" 2>/dev/null || true
    LEASE_LOCK_DIR=
  fi
}

active_lease_files() {
  local store
  store=$(lease_store)
  [[ -d $store ]] || return 0
  local f
  for f in "$store"/*.yml; do
    [[ -f $f ]] || continue
    if [[ $(yaml_scalar status "$f" || true) == active ]] || lease_running "$f"; then
      printf '%s\n' "$f"
    fi
  done
  # Running ownership must survive a missing or damaged mutable lease record.
  for f in "$store"/*.running; do
    [[ ! -e $f && ! -L $f ]] || printf '%s\n' "$f"
  done
}

validate_execution_markers() {
  local file key
  for file in "$(lease_store)"/*.running; do
    [[ -e $file || -L $file ]] || continue
    [[ -f $file && ! -L $file ]] || return 1
    for key in execution_id lease_id worktree owner branch allowed_files pr; do
      [[ -n $(yaml_scalar "$key" "$file" || true) ]] || return 1
    done
  done
}

lease_running() {
  local file=$1 id
  id=$(yaml_scalar lease_id "$file") || return 1
  [[ -e $(lease_store)/${id}.running || -L $(lease_store)/${id}.running ]]
}

worktree_running() {
  local worktree=$1 file
  # Unknown marker scope cannot be treated as evidence that a path is free.
  validate_execution_markers || return 0
  for file in "$(lease_store)"/*.running; do
    [[ -f $file ]] || continue
    [[ $(yaml_scalar worktree "$file") != "$worktree" ]] || return 0
  done
  return 1
}

fail_apply() {
  lease_lock_release
  fail "$1"
}

canonical_packet_hash() {
  local file=$1
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  else
    shasum -a 256 "$file" | awk '{print $1}'
  fi
}

new_lease_id() {
  local stamp rand
  stamp=$(date -u +"%Y%m%dT%H%M%SZ")
  rand=$(openssl rand -hex 4) || fail missing_openssl
  printf '%s-%s\n' "$stamp" "$rand"
}

write_lease_record() {
  local dest=$1 temporary
  mkdir -p "$(dirname "$dest")"
  temporary=$(mktemp "${dest}.tmp.XXXXXX")
  cat >"$temporary" <<EOF
schema: ${LEASE_SCHEMA:-monday.agent_lease.v2}
lease_id: $LEASE_ID
status: $LEASE_STATUS
contract: $LEASE_CONTRACT
owner: $LEASE_OWNER
seat: $LEASE_SEAT
worktree: $LEASE_WORKTREE
branch: $LEASE_BRANCH
base_sha: $LEASE_BASE
allowed_files: $LEASE_ALLOWED
dependency: ${LEASE_DEPENDENCY:-none}
packet_sha256: $LEASE_PACKET_SHA
packet_file: ${LEASE_PACKET_FILE:-none}
pr: $LEASE_PR
deadline: $LEASE_DEADLINE
trading_gates: $LEASE_GATES
spawn_count: ${LEASE_SPAWN_COUNT:-0}
last_execution_id: ${LEASE_LAST_RUN:-none}
EOF
  if [[ $LEASE_STATUS == released ]]; then
    cat >>"$temporary" <<EOF
released_at: $(date -u +"%Y-%m-%dT%H:%M:%SZ")
recovery_sha: $LEASE_RECOVERY_SHA
recovery_pr: $LEASE_PR
EOF
  fi
  mv -f -- "$temporary" "$dest"
}

cmd_apply() {
  local packet='' worktree_override=''
  while (($#)); do
    case "$1" in
      --packet-file) packet=$2; shift 2 ;;
      --worktree) worktree_override=$2; shift 2 ;;
      *) fail "unknown_apply_argument" ;;
    esac
  done
  [[ -n $packet && -f $packet ]] || fail missing_packet_file
  mkdir -p "$(lease_store)"
  PENDING_PACKET=$(mktemp "$(lease_store)/.packet.XXXXXX")
  cp -- "$packet" "$PENDING_PACKET"
  packet=$PENDING_PACKET

  local key goal gates branch writer deadline pr
  for key in from to routed_by evidence_paths constraints done_criteria; do
    yaml_scalar "$key" "$packet" >/dev/null || fail "missing_$key"
  done
  goal=$(yaml_scalar goal "$packet") || fail missing_goal
  gates=$(yaml_scalar trading_gates "$packet") || fail missing_trading_gates
  branch=$(yaml_scalar branch "$packet") || fail missing_branch
  writer=$(yaml_scalar writer "$packet") || fail missing_writer
  deadline=$(yaml_scalar deadline "$packet") || fail missing_deadline
  pr=$(yaml_scalar pr "$packet") || fail missing_pr

  local allowed line
  allowed=
  while IFS= read -r line; do
    [[ -z $line ]] && continue
    if [[ -z $allowed ]]; then
      allowed=$line
    else
      allowed="$allowed,$line"
    fi
  done < <(yaml_list allowed_files "$packet")
  [[ -n $allowed ]] || fail missing_allowed_files

  case $writer in
    cursor-cloud | codex | grok | human) ;;
    *) fail invalid_writer ;;
  esac
  case $branch in
    cursor/* | codex/*) ;;
    *) fail invalid_branch ;;
  esac
  [[ $branch != main ]] || fail primary_checkout
  [[ $pr == none || $pr =~ ^[0-9]+$ ]] || fail invalid_pr

  local primary
  primary=$(git_primary)
  local wt
  if [[ -n $worktree_override ]]; then
    wt=$(cd "$worktree_override" 2>/dev/null && pwd -P || printf '%s\n' "$worktree_override")
  else
    wt="$primary/.worktrees/$branch"
  fi
  [[ $wt != "$primary" ]] || fail primary_checkout

  lease_lock_acquire
  mkdir -p "$(lease_store)"
  validate_execution_markers || fail_apply execution_marker_invalid
  if worktree_running "$wt"; then fail_apply execution_unresolved; fi

  local f other_owner other_branch other_pr other_files item other
  while IFS= read -r f; do
    [[ -n $f ]] || continue
    other_owner=$(yaml_scalar owner "$f")
    other_branch=$(yaml_scalar branch "$f")
    other_pr=$(yaml_scalar pr "$f")
    other_files=$(yaml_scalar allowed_files "$f")
    [[ $other_owner != "$writer" ]] || fail_apply writer_already_active
    [[ $other_branch != "$branch" ]] || fail_apply branch_already_leased
    [[ $(yaml_scalar worktree "$f") != "$wt" ]] || fail_apply worktree_already_leased
    if [[ $pr != none && $other_pr == "$pr" ]]; then
      fail_apply pr_already_leased
    fi
    IFS=',' read -r -a other_arr <<<"$other_files"
    IFS=',' read -r -a self_arr <<<"$allowed"
    for item in "${self_arr[@]}"; do
      for other in "${other_arr[@]}"; do
        if paths_overlap "$item" "$other"; then
          fail_apply allowed_files_overlap
        fi
      done
    done
  done < <(active_lease_files)

  if [[ ! -d $wt ]]; then
    mkdir -p "$(dirname "$wt")"
    if git show-ref --verify --quiet "refs/heads/$branch"; then
      git worktree add -q "$wt" "$branch"
    else
      git worktree add -q -b "$branch" "$wt" HEAD
    fi
  fi
  wt=$(cd "$wt" && pwd -P)
  [[ $wt != "$primary" ]] || fail_apply primary_checkout
  [[ $(git -C "$wt" rev-parse --show-toplevel) == "$wt" ]] || fail_apply worktree_mismatch
  [[ $(git -C "$wt" rev-parse --path-format=absolute --git-common-dir) == "$(git_common)" ]] || fail_apply repository_mismatch
  [[ $(git -C "$wt" branch --show-current) == "$branch" ]] || fail_apply branch_mismatch

  LEASE_ID=$(new_lease_id)
  LEASE_SCHEMA=monday.agent_lease.v2
  LEASE_STATUS=active
  LEASE_CONTRACT=$goal
  LEASE_OWNER=$writer
  LEASE_SEAT=$writer
  LEASE_WORKTREE=$wt
  LEASE_BRANCH=$branch
  LEASE_BASE=$(git -C "$wt" rev-parse HEAD)
  LEASE_ALLOWED=$allowed
  LEASE_DEPENDENCY=none
  LEASE_PACKET_SHA=$(canonical_packet_hash "$packet")
  LEASE_PACKET_FILE="$(lease_store)/${LEASE_ID}.packet"
  mv -- "$packet" "$LEASE_PACKET_FILE"
  PENDING_PACKET=$LEASE_PACKET_FILE
  LEASE_PR=$pr
  LEASE_DEADLINE=$deadline
  LEASE_GATES=$gates

  local record store_file
  record=$(git -C "$wt" rev-parse --git-path agent-worktree.yml)
  store_file="$(lease_store)/${LEASE_ID}.yml"
  write_lease_record "$record"
  write_lease_record "$store_file"
  PENDING_PACKET=
  lease_lock_release
  printf 'verdict=ok\nlease_id=%s\nworktree=%s\nbranch=%s\npacket_sha256=%s\n' \
    "$LEASE_ID" "$wt" "$branch" "$LEASE_PACKET_SHA"
}

unpushed_count() {
  local path=$1
  local tip
  tip=$(cd "$path" && integration_tip)
  git -C "$path" rev-list --count "$tip"..HEAD
}

head_in_tip() {
  local path=$1
  local tip
  tip=$(cd "$path" && integration_tip)
  git merge-base --is-ancestor "$(git -C "$path" rev-parse HEAD)" "$tip"
}

squash_merged_pr() {
  local pr=$1
  [[ $pr == none || -z $pr ]] && return 1
  [[ $pr =~ ^[0-9]+$ ]] || return 1
  local tip
  tip=$(integration_tip)
  git log --format=%s --grep="(#${pr})" -n 1 "$tip" | grep -Fq "(#${pr})"
}

merged_pr_for_branch() {
  local branch=$1
  [[ -n $branch ]] || return 1
  command -v gh >/dev/null 2>&1 || return 1
  local n
  n=$(gh pr list --head "$branch" --state merged --limit 1 --json number --jq '.[0].number // empty' 2>/dev/null || true)
  [[ -n $n ]] || return 1
  printf '%s\n' "$n"
}

unique_commits_recovered() {
  local path=$1 pr=$2 branch=$3
  head_in_tip "$path" && return 0
  if [[ $pr != none && -n $pr ]] && squash_merged_pr "$pr"; then
    return 0
  fi
  local found
  if found=$(merged_pr_for_branch "$branch") && squash_merged_pr "$found"; then
    return 0
  fi
  return 1
}

find_lease_file() {
  local key=$1
  local store f id wt found='' occupied=0
  store=$(lease_store)
  [[ -d $store ]] || return 1
  for f in "$store"/*.yml; do
    [[ -f $f ]] || continue
    id=$(yaml_scalar lease_id "$f" || true)
    wt=$(yaml_scalar worktree "$f" || true)
    if [[ $id == "$key" ]]; then
      printf '%s\n' "$f"
      return 0
    fi
    if [[ $wt == "$key" ]]; then
      if [[ $(yaml_scalar status "$f" || true) == active ]] || lease_running "$f"; then
        found=$f
        occupied=1
      elif ((occupied == 0)); then
        found=$f
      fi
    fi
  done
  [[ -n $found ]] || return 1
  printf '%s\n' "$found"
}

cmd_release() {
  local key='' discard=0 pr_override=none
  while (($#)); do
    case "$1" in
      --discard-unique) discard=1; shift ;;
      --pr) pr_override=$2; shift 2 ;;
      --*) fail unknown_release_argument ;;
      *) key=$1; shift ;;
    esac
  done
  [[ -n $key ]] || fail missing_lease_id
  lease_lock_acquire
  local store_file='' wt='' leased=0
  if store_file=$(find_lease_file "$key"); then
    leased=1
    local status
    status=$(yaml_scalar status "$store_file")
    [[ $status == active || $status == expired ]] || fail_apply lease_not_active
    wt=$(yaml_scalar worktree "$store_file")
  else
    [[ -d $key ]] || fail_apply lease_not_found
    wt=$(cd "$key" && pwd -P)
    [[ $wt != "$(git_primary)" ]] || fail_apply primary_checkout
  fi
  [[ -d $wt ]] || fail_apply worktree_missing
  if worktree_running "$wt"; then fail_apply execution_unresolved; fi

  if [[ -n $(git -C "$wt" status --porcelain) ]]; then
    fail_apply dirty_worktree
  fi
  local ahead=0 branch pr
  ahead=$(unpushed_count "$wt")
  if ((leased)); then
    branch=$(yaml_scalar branch "$store_file")
    pr=$(yaml_scalar pr "$store_file")
  else
    branch=$(git -C "$wt" branch --show-current || true)
    pr=$pr_override
  fi
  if [[ $pr_override != none ]]; then
    pr=$pr_override
  fi
  if ((ahead > 0)) && ! unique_commits_recovered "$wt" "$pr" "$branch"; then
    if ((discard != 1)); then
      fail_apply unique_unpushed
    fi
  fi

  if ((leased)); then
    load_lease_file "$store_file"
  else
    LEASE_ID=$(new_lease_id)
    LEASE_CONTRACT="unleased cleanup"
    LEASE_OWNER=human
    LEASE_SEAT=human
    LEASE_BASE=$(git -C "$wt" rev-parse HEAD)
    LEASE_ALLOWED=none
    LEASE_DEPENDENCY=none
    LEASE_PACKET_SHA=none
    LEASE_DEADLINE=none
    LEASE_GATES=fail-closed
    store_file="$(lease_store)/${LEASE_ID}.yml"
    mkdir -p "$(lease_store)"
  fi
  LEASE_STATUS=released
  LEASE_WORKTREE=$wt
  LEASE_BRANCH=${branch:-none}
  LEASE_PR=${pr:-none}
  LEASE_RECOVERY_SHA=$(integration_tip)

  git worktree remove "$wt"
  write_lease_record "$store_file"
  lease_lock_release
  printf 'verdict=ok\nlease_id=%s\nstatus=released\nrecovery_sha=%s\n' \
    "$LEASE_ID" "$LEASE_RECOVERY_SHA"
}

cleanup_class() {
  local path=$1 state=$2 lease_status=$3 ahead=$4 pr=$5 branch=$6
  local primary
  primary=$(git_primary)
  if [[ $path == "$primary" ]]; then
    printf 'keep\tprimary checkout\n'
    return
  fi
  if [[ $state == dirty ]]; then
    printf 'keep\tdirty worktree\n'
    return
  fi
  if worktree_running "$path"; then
    printf 'keep\tunresolved execution\n'
    return
  fi
  if [[ $lease_status == active ]]; then
    printf 'keep\tactive lease\n'
    return
  fi
  if ((ahead > 0)); then
    if unique_commits_recovered "$path" "$pr" "$branch"; then
      printf 'cleanup-safe\tsquash-merged or contained in integration tip\n'
    else
      printf 'keep\tunique unpushed commits\n'
    fi
    return
  fi
  if [[ $state == registered-clean || $state == prunable ]]; then
    printf 'cleanup-safe\tclean and not uniquely ahead\n'
    return
  fi
  printf 'unknown\tunclassified\n'
}

cmd_list() {
  local path='' branch='' head='' prunable='' line state checkout
  emit_list_row() {
    [[ -n $path ]] || return 0
    if [[ $prunable == true ]]; then state=prunable
    elif [[ -n $(git -C "$path" status --porcelain) ]]; then state=dirty
    else state=registered-clean; fi
    if [[ -n $branch ]]; then checkout=branch; else checkout=detached; fi
    local lease_id=none lease_status=none owner=none pr=none
    local f
    if f=$(find_lease_file "$path"); then
      lease_id=$(yaml_scalar lease_id "$f")
      lease_status=$(yaml_scalar status "$f")
      owner=$(yaml_scalar owner "$f")
      pr=$(yaml_scalar pr "$f")
    fi
    if [[ $pr == none && -n $branch ]]; then
      local found
      if found=$(merged_pr_for_branch "$branch"); then
        pr=$found
      fi
    fi
    local ahead=0
    if [[ -d $path ]]; then
      ahead=$(unpushed_count "$path")
    fi
    local safety reason
    IFS=$'\t' read -r safety reason < <(cleanup_class "$path" "$state" "$lease_status" "$ahead" "$pr" "$branch")
    printf 'worktree=%s\tbranch=%s\thead=%s\tcheckout=%s\tstate=%s\tlease_id=%s\tlease_status=%s\towner=%s\tpr=%s\tunpushed=%s\tcleanup_safety=%s\treason=%s\n' \
      "$path" "$branch" "$head" "$checkout" "$state" "$lease_id" "$lease_status" "$owner" "$pr" "$ahead" "$safety" "$reason"
    path='' branch='' head='' prunable=''
  }
  while IFS= read -r line || [[ -n $line ]]; do
    if [[ -z $line ]]; then
      emit_list_row
    elif [[ $line == worktree\ * ]]; then path=${line#worktree }
    elif [[ $line == branch\ * ]]; then branch=${line#branch refs/heads/}
    elif [[ $line == HEAD\ * ]]; then head=${line#HEAD }
    elif [[ $line == prunable* ]]; then prunable=true
    fi
  done < <(git worktree list --porcelain)
  emit_list_row
}

cmd_get() {
  local key=${1:-}
  [[ -n $key ]] || fail missing_lease_id
  local f
  if f=$(find_lease_file "$key"); then
    cat "$f"
    return 0
  fi
  cmd_list | grep -F "worktree=$key" || fail lease_not_found
}

deadline_passed() {
  local deadline=$1
  [[ -n $deadline && $deadline != none ]] || return 1
  local now
  now=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
  [[ $deadline < $now ]]
}

load_lease_file() {
  local f=$1
  LEASE_ID=$(yaml_scalar lease_id "$f")
  LEASE_SCHEMA=$(yaml_scalar schema "$f")
  LEASE_STATUS=$(yaml_scalar status "$f")
  LEASE_CONTRACT=$(yaml_scalar contract "$f")
  LEASE_OWNER=$(yaml_scalar owner "$f")
  LEASE_SEAT=$(yaml_scalar seat "$f")
  LEASE_WORKTREE=$(yaml_scalar worktree "$f")
  LEASE_BRANCH=$(yaml_scalar branch "$f")
  LEASE_BASE=$(yaml_scalar base_sha "$f")
  LEASE_ALLOWED=$(yaml_scalar allowed_files "$f")
  LEASE_DEPENDENCY=$(yaml_scalar dependency "$f")
  LEASE_PACKET_SHA=$(yaml_scalar packet_sha256 "$f")
  LEASE_PACKET_FILE=$(yaml_scalar packet_file "$f" 2>/dev/null || true)
  LEASE_PR=$(yaml_scalar pr "$f")
  LEASE_DEADLINE=$(yaml_scalar deadline "$f")
  LEASE_GATES=$(yaml_scalar trading_gates "$f")
  LEASE_SPAWN_COUNT=$(yaml_scalar spawn_count "$f" 2>/dev/null || true)
  LEASE_SPAWN_COUNT=${LEASE_SPAWN_COUNT:-0}
  LEASE_LAST_RUN=$(yaml_scalar last_execution_id "$f" 2>/dev/null || true)
  if [[ $LEASE_STATUS == released ]]; then
    LEASE_RECOVERY_SHA=$(yaml_scalar recovery_sha "$f" 2>/dev/null || true)
  fi
}

persist_lease() {
  local store_file=$1
  write_lease_record "$store_file"
  if [[ -d $LEASE_WORKTREE ]]; then
    local record
    record=$(git -C "$LEASE_WORKTREE" rev-parse --git-path agent-worktree.yml)
    write_lease_record "$record"
  fi
}

cmd_sweep() {
  lease_lock_acquire
  local expired=0 f
  while IFS= read -r f; do
    [[ -n $f ]] || continue
    [[ $(yaml_scalar status "$f") == active ]] || continue
    local deadline
    deadline=$(yaml_scalar deadline "$f" || true)
    if deadline_passed "$deadline"; then
      load_lease_file "$f"
      LEASE_STATUS=expired
      persist_lease "$f"
      expired=$((expired + 1))
    fi
  done < <(active_lease_files)
  lease_lock_release
  printf 'verdict=ok\nexpired=%s\n' "$expired"
}

validate_spawn_context() {
  [[ $LEASE_SCHEMA == monday.agent_lease.v2 ]] || fail_apply packet_snapshot_missing
  [[ $LEASE_PACKET_FILE == "$(lease_store)/${LEASE_ID}.packet" ]] || fail_apply packet_path_mismatch
  [[ -f $LEASE_PACKET_FILE && ! -L $LEASE_PACKET_FILE ]] || fail_apply packet_snapshot_missing
  [[ $(canonical_packet_hash "$LEASE_PACKET_FILE") == "$LEASE_PACKET_SHA" ]] || fail_apply packet_hash_mismatch
  local packet=$LEASE_PACKET_FILE allowed
  allowed=$(yaml_list allowed_files "$packet" | paste -sd ',' -)
  if [[ $(yaml_scalar goal "$packet") != "$LEASE_CONTRACT" ||
        $(yaml_scalar writer "$packet") != "$LEASE_SEAT" ||
        $LEASE_OWNER != "$LEASE_SEAT" ||
        $(yaml_scalar branch "$packet") != "$LEASE_BRANCH" ||
        $(yaml_scalar trading_gates "$packet") != "$LEASE_GATES" ||
        $(yaml_scalar deadline "$packet") != "$LEASE_DEADLINE" ||
        $(yaml_scalar pr "$packet") != "$LEASE_PR" ||
        $allowed != "$LEASE_ALLOWED" ]]; then
    fail_apply packet_binding_mismatch
  fi
  [[ -d $LEASE_WORKTREE ]] || fail_apply worktree_missing
  [[ $(git -C "$LEASE_WORKTREE" rev-parse --show-toplevel) == "$LEASE_WORKTREE" ]] || fail_apply worktree_mismatch
  [[ $(git -C "$LEASE_WORKTREE" rev-parse --path-format=absolute --git-common-dir) == "$(git_common)" ]] || fail_apply repository_mismatch
  git worktree list --porcelain | grep -Fx "worktree $LEASE_WORKTREE" >/dev/null || fail_apply unregistered_worktree
  [[ $(git -C "$LEASE_WORKTREE" branch --show-current) == "$LEASE_BRANCH" ]] || fail_apply branch_mismatch
  git -C "$LEASE_WORKTREE" merge-base --is-ancestor "$LEASE_BASE" HEAD || fail_apply base_mismatch
}

cmd_spawn() {
  local key='' execute=0
  while (($#)); do
    case "$1" in
      --execute) execute=1; shift ;;
      --*) fail unknown_spawn_argument ;;
      *) key=$1; shift ;;
    esac
  done
  [[ -n $key ]] || fail missing_lease_id
  lease_lock_acquire
  local store_file
  store_file=$(find_lease_file "$key") || fail_apply lease_not_found
  load_lease_file "$store_file"
  if worktree_running "$LEASE_WORKTREE"; then fail_apply execution_unresolved; fi
  [[ $LEASE_STATUS == active ]] || fail_apply lease_not_active
  if deadline_passed "$LEASE_DEADLINE"; then
    fail_apply lease_expired
  fi
  case $LEASE_GATES in
    fail-closed | none) ;;
    *) fail_apply trading_gates_blocked ;;
  esac
  validate_spawn_context
  local spawn_bin
  local -a spawn_args
  case $LEASE_SEAT in
    cursor-cloud)
      spawn_bin=cursor-agent
      spawn_args=(--print --mode plan)
      ;;
    codex)
      spawn_bin=codex
      spawn_args=(exec)
      ;;
    grok) fail_apply grok_seat ;;
    human) fail_apply human_seat ;;
    *) fail_apply unknown_seat ;;
  esac
  local mode=dry-run
  if ((execute)); then
    command -v "$spawn_bin" >/dev/null 2>&1 || fail_apply seat_cli_missing
    mode=execute
  fi
  printf 'verdict=ok\nmode=%s\nseat=%s\nworktree=%s\ncommand=' "$mode" "$LEASE_SEAT" "$LEASE_WORKTREE"
  printf '%q' "$spawn_bin"
  local arg
  for arg in "${spawn_args[@]}"; do
    printf ' %q' "$arg"
  done
  printf ' <verified-packet>\npacket_file=%s\npacket_sha256=%s\n' "$LEASE_PACKET_FILE" "$LEASE_PACKET_SHA"
  if ((execute == 0)); then
    lease_lock_release
    return
  fi

  local prompt source_head worker_pid exit_status marker start_sha terminal_sha prompt_sha
  # Preserve the packet's trailing newlines and hash the actual CLI argument.
  prompt=$(cat "$LEASE_PACKET_FILE" && printf '.') || fail_apply packet_snapshot_missing
  prompt=${prompt%.}
  if command -v sha256sum >/dev/null 2>&1; then
    prompt_sha=$(printf '%s' "$prompt" | sha256sum | awk '{print $1}')
  else
    prompt_sha=$(printf '%s' "$prompt" | shasum -a 256 | awk '{print $1}')
  fi
  [[ $prompt_sha == "$LEASE_PACKET_SHA" ]] || fail_apply packet_hash_mismatch
  source_head=$(git -C "$LEASE_WORKTREE" rev-parse HEAD)
  RUN_ID=$(new_lease_id)
  RUN_DIR="$(lease_store)/${LEASE_ID}.runs/$RUN_ID"
  mkdir -p "$RUN_DIR"
  marker="$(lease_store)/${LEASE_ID}.running"
  (set -o noclobber; cat >"$marker" <<EOF
execution_id: $RUN_ID
lease_id: $LEASE_ID
status: running
worktree: $LEASE_WORKTREE
owner: $LEASE_OWNER
branch: $LEASE_BRANCH
allowed_files: $LEASE_ALLOWED
pr: $LEASE_PR
EOF
  ) || fail_apply execution_unresolved
  LEASE_LAST_RUN=$RUN_ID
  LEASE_SPAWN_COUNT=$((LEASE_SPAWN_COUNT + 1))
  persist_lease "$store_file"
  (cd "$LEASE_WORKTREE" && exec "$spawn_bin" "${spawn_args[@]}" -- "$prompt") \
    </dev/null >"$RUN_DIR/stdout.log" 2>"$RUN_DIR/stderr.log" &
  worker_pid=$!
  cat >"$RUN_DIR/start.yml.partial" <<EOF
schema: monday.agent_execution_start.v1
execution_id: $RUN_ID
lease_id: $LEASE_ID
packet_sha256: $LEASE_PACKET_SHA
base_sha: $LEASE_BASE
source_head: $source_head
branch: $LEASE_BRANCH
worktree: $LEASE_WORKTREE
seat: $LEASE_SEAT
pr: $LEASE_PR
controller_pid: $$
worker_pid: $worker_pid
started_at: $(date -u +"%Y-%m-%dT%H:%M:%SZ")
stdout: $RUN_DIR/stdout.log
stderr: $RUN_DIR/stderr.log
task_status: unverified
EOF
  mv -- "$RUN_DIR/start.yml.partial" "$RUN_DIR/start.yml"
  start_sha=$(canonical_packet_hash "$RUN_DIR/start.yml")
  lease_lock_release
  printf 'execution_id=%s\nstart_receipt=%s\n' "$RUN_ID" "$RUN_DIR/start.yml"
  if wait "$worker_pid"; then exit_status=0; else exit_status=$?; fi
  local source_after stdout_sha stderr_sha
  source_after=$(git -C "$LEASE_WORKTREE" rev-parse HEAD)
  stdout_sha=$(canonical_packet_hash "$RUN_DIR/stdout.log")
  stderr_sha=$(canonical_packet_hash "$RUN_DIR/stderr.log")
  cat >"$RUN_DIR/terminal.yml.partial" <<EOF
schema: monday.agent_execution_terminal.v1
execution_id: $RUN_ID
lease_id: $LEASE_ID
start_sha256: $start_sha
packet_sha256: $LEASE_PACKET_SHA
source_head_after: $source_after
process_status: exited
exit_code: $exit_status
finished_at: $(date -u +"%Y-%m-%dT%H:%M:%SZ")
stdout_sha256: $stdout_sha
stderr_sha256: $stderr_sha
task_status: unverified
EOF
  mv -- "$RUN_DIR/terminal.yml.partial" "$RUN_DIR/terminal.yml"
  terminal_sha=$(canonical_packet_hash "$RUN_DIR/terminal.yml")
  lease_lock_acquire
  [[ $(yaml_scalar execution_id "$marker") == "$RUN_ID" ]] || fail_apply execution_marker_mismatch
  rm -- "$marker"
  RUN_FINISHED=1
  lease_lock_release
  printf 'process_status=exited\nexit_code=%s\ntask_status=unverified\nterminal_receipt=%s\nterminal_sha256=%s\n' \
    "$exit_status" "$RUN_DIR/terminal.yml" "$terminal_sha"
  return "$exit_status"
}


task_store() {
  printf '%s/agent-tasks\n' "$(git_common)"
}

task_dir() {
  printf '%s/%s\n' "$(task_store)" "$1"
}

contract_has_active_writer() {
  local contract=$1 f other
  while IFS= read -r f; do
    [[ -n $f ]] || continue
    other=$(yaml_scalar contract "$f" || true)
    [[ $other == "$contract" ]] && return 0
  done < <(active_lease_files)
  return 1
}

host_allowed() {
  local file=$1 host=$2 item
  local hosts
  hosts=$(yaml_scalar allow_hosts "$file")
  IFS=',' read -r -a items <<<"$hosts"
  for item in "${items[@]}"; do
    item=${item#"${item%%[![:space:]]*}"}
    item=${item%"${item##*[![:space:]]}"}
    [[ $item == "$host" ]] && return 0
  done
  return 1
}

materialize_task_workspace() {
  local dir=$1
  local ws=$dir/workspace
  if [[ -d $dir/checkpoint ]]; then
    rm -rf "$ws"
    mkdir -p "$ws"
    cp -a "$dir/checkpoint/." "$ws/"
  else
    mkdir -p "$ws"
  fi
  local repo_name repo_path tool_name tool_endpoint
  repo_name=$(yaml_scalar workspace_repo_name "$dir/task.yml")
  repo_path=$(yaml_scalar workspace_repo_path "$dir/task.yml")
  tool_name=$(yaml_scalar workspace_tool_name "$dir/task.yml")
  tool_endpoint=$(yaml_scalar workspace_tool_endpoint "$dir/task.yml")
  mkdir -p "$ws/repos/$repo_name" "$ws/tools"
  # A restored checkpoint already holds the agent's repo edits. Recopying the
  # declared source would wipe that state on the next invoke.
  if [[ ! -d $dir/checkpoint || ! -e $ws/repos/$repo_name ]]; then
    if [[ -d $repo_path ]]; then
      mkdir -p "$ws/repos/$repo_name"
      cp -a "$repo_path/." "$ws/repos/$repo_name/"
    fi
  fi
  printf '%s\n' "$tool_endpoint" >"$ws/tools/$tool_name"
  local cpu memory
  cpu=$(yaml_scalar cpu "$dir/task.yml")
  memory=$(yaml_scalar memory_mb "$dir/task.yml")
  printf 'cpu=%s\nmemory_mb=%s\n' "$cpu" "$memory" >"$ws/.bounds"
  printf 'ready\n' >"$ws/.ready"
}

checkpoint_task_workspace() {
  local dir=$1
  [[ -d $dir/workspace ]] || return 0
  rm -rf "$dir/checkpoint"
  mkdir -p "$dir/checkpoint"
  cp -a "$dir/workspace/." "$dir/checkpoint/"
}

cmd_task_declare() {
  local file=
  while (($#)); do
    case "$1" in
      --file) file=$2; shift 2 ;;
      *) fail unknown_task_argument ;;
    esac
  done
  [[ -n $file && -f $file ]] || fail missing_task_file
  local agent_id contract cpu memory hosts provider secret_file command
  local repo_name repo_path tool_name tool_endpoint
  agent_id=$(yaml_scalar agent_id "$file") || fail missing_agent_id
  contract=$(yaml_scalar contract "$file") || fail missing_contract
  cpu=$(yaml_scalar cpu "$file") || fail missing_cpu
  memory=$(yaml_scalar memory_mb "$file") || fail missing_memory_mb
  hosts=$(yaml_scalar allow_hosts "$file") || fail missing_allow_hosts
  provider=$(yaml_scalar model_provider "$file") || fail missing_model_provider
  command=$(yaml_scalar command "$file") || fail missing_command
  repo_name=$(yaml_scalar workspace_repo_name "$file") || fail missing_workspace_repo_name
  repo_path=$(yaml_scalar workspace_repo_path "$file") || fail missing_workspace_repo_path
  tool_name=$(yaml_scalar workspace_tool_name "$file") || fail missing_workspace_tool_name
  tool_endpoint=$(yaml_scalar workspace_tool_endpoint "$file") || fail missing_workspace_tool_endpoint
  secret_file=$(yaml_scalar model_secret_file "$file" || true)
  [[ $cpu =~ ^[0-9]+$ && $memory =~ ^[0-9]+$ ]] || fail invalid_resource_bounds
  [[ -n $hosts ]] || fail missing_allow_hosts
  if [[ -n $secret_file && -f $secret_file ]]; then
    local secret
    secret=$(cat "$secret_file")
    [[ $command != *"$secret"* ]] || fail secret_in_command
    [[ $(cat "$file") != *"$secret"* ]] || fail secret_in_task
  fi
  local dir
  dir=$(task_dir "$agent_id")
  [[ ! -e $dir ]] || fail agent_exists
  mkdir -p "$dir"
  cat >"$dir/task.yml" <<EOF
schema: monday.agent_task.v1
agent_id: $agent_id
contract: $contract
cpu: $cpu
memory_mb: $memory
allow_hosts: $hosts
model_provider: $provider
model_secret_ref: model.secret
workspace_repo_name: $repo_name
workspace_repo_path: $repo_path
workspace_tool_name: $tool_name
workspace_tool_endpoint: $tool_endpoint
command: $command
EOF
  if [[ -n $secret_file && -f $secret_file ]]; then
    cp "$secret_file" "$dir/model.secret"
    chmod 600 "$dir/model.secret"
  else
    : >"$dir/model.secret"
  fi
  printf 'pending\n' >"$dir/phase"
  printf 'verdict=ok\nagent_id=%s\nphase=pending\n' "$agent_id"
}

cmd_task_status() {
  local agent_id=${1:-}
  [[ -n $agent_id ]] || fail missing_agent_id
  local dir phase
  dir=$(task_dir "$agent_id")
  [[ -f $dir/phase ]] || fail agent_not_found
  phase=$(cat "$dir/phase")
  printf 'verdict=ok\nagent_id=%s\nphase=%s\ncpu=%s\nmemory_mb=%s\n' \
    "$agent_id" "$phase" \
    "$(yaml_scalar cpu "$dir/task.yml")" \
    "$(yaml_scalar memory_mb "$dir/task.yml")"
}

cmd_task_suspend() {
  local agent_id=${1:-}
  [[ -n $agent_id ]] || fail missing_agent_id
  local dir phase
  dir=$(task_dir "$agent_id")
  [[ -f $dir/phase ]] || fail agent_not_found
  phase=$(cat "$dir/phase")
  case $phase in
    pending | running | suspended) ;;
    *) fail agent_not_suspendable ;;
  esac
  checkpoint_task_workspace "$dir"
  printf 'suspended\n' >"$dir/phase"
  printf 'verdict=ok\nagent_id=%s\nphase=suspended\n' "$agent_id"
}

install_egress_wrappers() {
  local dir=$1
  local bindir=$dir/bin
  mkdir -p "$bindir"
  local hosts_file=$dir/allow_hosts
  : >"$hosts_file"
  local hosts item
  hosts=$(yaml_scalar allow_hosts "$dir/task.yml")
  IFS=',' read -r -a items <<<"$hosts"
  for item in "${items[@]}"; do
    item=${item#"${item%%[![:space:]]*}"}
    item=${item%"${item##*[![:space:]]}"}
    [[ -n $item ]] && printf '%s\n' "$item" >>"$hosts_file"
  done
  local tool real
  for tool in curl wget nc ncat aria2c; do
    real=$(command -v "$tool" 2>/dev/null || true)
    [[ -n $real && $real != "$bindir/$tool" ]] || real=
    cat >"$bindir/$tool" <<EOF
#!/bin/bash
host=
for arg in "\$@"; do
  case "\$arg" in
    http://*|https://*)
      host=\${arg#*://}
      host=\${host%%/*}
      host=\${host%%:*}
      host=\${host%%\\?*}
      ;;
  esac
done
refuse() {
  printf '%s\n' "\$1" > "\$MONDAY_AGENT_EGRESS_REFUSED"
  echo host_not_allowed >&2
  exit 76
}
if [[ -z \$host ]]; then
  refuse missing-host
fi
ok=0
while IFS= read -r item || [[ -n \$item ]]; do
  [[ \$item == "\$host" ]] && ok=1
done < "\$MONDAY_AGENT_ALLOW_FILE"
if [[ \$ok != 1 ]]; then
  refuse "\$host"
fi
real='$real'
if [[ -z \$real ]]; then
  echo command_not_found >&2
  exit 127
fi
exec "\$real" "\$@"
EOF
    chmod +x "$bindir/$tool"
  done
}

rss_tree_kb() {
  local pid=$1
  local self kids kid sum
  self=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || true)
  sum=${self:-0}
  [[ $sum =~ ^[0-9]+$ ]] || sum=0
  kids=$(pgrep -P "$pid" 2>/dev/null || true)
  for kid in $kids; do
    sum=$((sum + $(rss_tree_kb "$kid")))
  done
  printf '%s\n' "$sum"
}

kill_tree() {
  local pid=$1
  local kids kid
  kids=$(pgrep -P "$pid" 2>/dev/null || true)
  for kid in $kids; do
    kill_tree "$kid"
  done
  kill -KILL "$pid" 2>/dev/null || true
}

assert_resource_bounds_enforceable() {
  local cpu=$1 memory=$2
  ps -o rss= -p $$ >/dev/null 2>&1 || fail memory_bound_unenforceable
  [[ $cpu =~ ^[1-9][0-9]*$ ]] || fail cpu_bound_unenforceable
  [[ $memory =~ ^[1-9][0-9]*$ ]] || fail memory_bound_unenforceable
  ( ulimit -t $((cpu * 3600)) ) || fail cpu_bound_unenforceable
}

# GNU ps -g selects a session, not a process group. An empty session match
# would skip the RSS cap, so match the recorded pgid from a full snapshot.
run_bounded_command() {
  local cpu=$1 memory=$2 command=$3
  local max_kb=$((memory * 1024))
  local child pgid snapshot pid grp rss state total alive exceeded=0 spins=0
  set -m
  (
    ulimit -t $((cpu * 3600)) || exit 126
    eval "$command"
    wait
  ) &
  child=$!
  pgid=$(ps -o pgid= -p "$child" 2>/dev/null | tr -d ' ' || true)
  [[ $pgid =~ ^[0-9]+$ ]] || pgid=$child
  while :; do
    snapshot=$(ps -ax -o pid=,pgid=,rss=,state=) || return 125
    total=0
    alive=0
    while read -r pid grp rss state; do
      [[ $grp == "$pgid" ]] || continue
      [[ $state == Z* ]] && continue
      [[ $rss =~ ^[0-9]+$ ]] || continue
      total=$((total + rss))
      alive=1
    done <<<"$snapshot"
    if ((alive == 0)) && ! kill -0 "$child" 2>/dev/null; then
      break
    fi
    if ((alive == 1 && total > max_kb)); then
      kill -KILL -"$pgid" 2>/dev/null || kill_tree "$child"
      exceeded=1
      break
    fi
    spins=$((spins + 1))
    if ((spins > 400)); then
      kill -KILL -"$pgid" 2>/dev/null || kill_tree "$child"
      wait "$child" 2>/dev/null || true
      return 124
    fi
    sleep 0.05
  done
  if ((exceeded)); then
    wait "$child" 2>/dev/null || true
    return 137
  fi
  local exit_status=0
  wait "$child" || exit_status=$?
  return "$exit_status"
}

cmd_task_egress() {
  local agent_id=${1:-} host=${2:-}
  [[ -n $agent_id && -n $host ]] || fail missing_egress_target
  local dir
  dir=$(task_dir "$agent_id")
  [[ -f $dir/task.yml ]] || fail agent_not_found
  if host_allowed "$dir/task.yml" "$host"; then
    printf 'verdict=ok\nagent_id=%s\nhost=%s\n' "$agent_id" "$host"
  else
    fail host_not_allowed
  fi
}

cmd_task_invoke() {
  local agent_id=${1:-}
  [[ -n $agent_id ]] || fail missing_agent_id
  local dir phase contract
  dir=$(task_dir "$agent_id")
  [[ -f $dir/phase && -f $dir/task.yml ]] || fail agent_not_found
  phase=$(cat "$dir/phase")
  case $phase in
    pending | suspended) ;;
    running) fail agent_already_running ;;
    failed) fail agent_failed ;;
    *) fail agent_not_invocable ;;
  esac
  contract=$(yaml_scalar contract "$dir/task.yml")
  if contract_has_active_writer "$contract"; then
    fail writer_already_active
  fi
  printf 'running\n' >"$dir/phase"
  materialize_task_workspace "$dir"
  local ws command cpu memory
  ws=$dir/workspace
  command=$(yaml_scalar command "$dir/task.yml")
  cpu=$(yaml_scalar cpu "$dir/task.yml")
  memory=$(yaml_scalar memory_mb "$dir/task.yml")
  [[ -d $ws/repos/$(yaml_scalar workspace_repo_name "$dir/task.yml") ]] || {
    printf 'failed\n' >"$dir/phase"
    fail workspace_not_materialized
  }
  [[ -s $ws/tools/$(yaml_scalar workspace_tool_name "$dir/task.yml") ]] || {
    printf 'failed\n' >"$dir/phase"
    fail workspace_not_materialized
  }
  [[ -s $ws/.bounds ]] || {
    printf 'failed\n' >"$dir/phase"
    fail bounds_not_declared
  }
  assert_resource_bounds_enforceable "$cpu" "$memory"
  install_egress_wrappers "$dir"
  printf 'command=%s\ncpu=%s\nmemory_mb=%s\n' "$command" "$cpu" "$memory" >>"$dir/command.log"
  rm -f "$dir/egress-refused"
  local exit_status=0
  set +e
  (
    cd "$ws"
    export MONDAY_AGENT_ID=$agent_id
    export MONDAY_AGENT_WORKSPACE=$ws
    export MONDAY_AGENT_CPU=$cpu
    export MONDAY_AGENT_MEMORY_MB=$memory
    export MONDAY_AGENT_ALLOW_FILE=$dir/allow_hosts
    export MONDAY_AGENT_EGRESS_REFUSED=$dir/egress-refused
    export PATH="$dir/bin:$PATH"
    run_bounded_command "$cpu" "$memory" "$command"
  ) >>"$dir/invocation.log" 2>>"$dir/invocation.err"
  exit_status=$?
  set -e
  if [[ -f $dir/model.secret ]]; then
    local secret
    secret=$(cat "$dir/model.secret")
    if [[ -n $secret ]]; then
      if grep -F -q -- "$secret" "$dir/task.yml" "$dir/command.log" "$dir/invocation.log" "$dir/invocation.err" 2>/dev/null; then
        printf 'failed\n' >"$dir/phase"
        fail secret_leaked
      fi
    fi
  fi
  if [[ -s $dir/egress-refused ]]; then
    printf 'failed\n' >"$dir/phase"
    fail host_not_allowed
  fi
  if ((exit_status == 137)); then
    printf 'failed\n' >"$dir/phase"
    fail memory_exceeded
  fi
  if ((exit_status == 125)); then
    printf 'failed\n' >"$dir/phase"
    fail memory_bound_unenforceable
  fi
  if ((exit_status == 126)); then
    printf 'failed\n' >"$dir/phase"
    fail cpu_bound_unenforceable
  fi
  if ((exit_status != 0)); then
    printf 'failed\n' >"$dir/phase"
    printf 'verdict=blocked\nreason=command_failed\nagent_id=%s\nphase=failed\n' "$agent_id" >&2
    return "$exit_status"
  fi
  checkpoint_task_workspace "$dir"
  printf 'suspended\n' >"$dir/phase"
  printf 'verdict=ok\nagent_id=%s\nphase=suspended\ncpu=%s\nmemory_mb=%s\nworkspace=%s\n' \
    "$agent_id" "$cpu" "$memory" "$ws"
}

usage() {
  echo "usage: $0 check-managed|report|list|get|apply|release|spawn|sweep|task-declare|task-invoke|task-suspend|task-status|task-egress"
}

case "${1:-help}" in
  check-managed) check_managed ;;
  report) report ;;
  list) shift; cmd_list "$@" ;;
  get) shift; cmd_get "$@" ;;
  apply) shift; cmd_apply "$@" ;;
  release) shift; cmd_release "$@" ;;
  spawn) shift; cmd_spawn "$@" ;;
  sweep) shift; cmd_sweep "$@" ;;
  task-declare) shift; cmd_task_declare "$@" ;;
  task-invoke) shift; cmd_task_invoke "$@" ;;
  task-suspend) shift; cmd_task_suspend "$@" ;;
  task-status) shift; cmd_task_status "$@" ;;
  task-egress) shift; cmd_task_egress "$@" ;;
  help|--help|-h) usage ;;
  *) usage >&2; exit 2 ;;
esac
