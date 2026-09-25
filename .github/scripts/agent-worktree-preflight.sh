#!/usr/bin/env bash
set -euo pipefail
umask 077

cleanup() {
  local exit_status=$?
  trap - EXIT
  lease_lock_release
  [[ -z ${PENDING_PACKET:-} ]] || rm -f -- "$PENDING_PACKET"
  if [[ -n ${EGRESS_PROXY_PID:-} ]]; then
    kill "$EGRESS_PROXY_PID" 2>/dev/null || true
  fi
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
  if [[ -n ${TASK_RECEIPT_DIR:-} && -f ${TASK_RECEIPT_DIR}/task.yml ]]; then
    write_task_receipt "$TASK_RECEIPT_DIR" blocked "$1"
  fi
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
    other_owner=$(yaml_scalar owner "$f" || true)
    other_branch=$(yaml_scalar branch "$f" || true)
    other_pr=$(yaml_scalar pr "$f" || true)
    other_files=$(yaml_scalar allowed_files "$f" || true)
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
  if task_contract_busy "$goal"; then
    fail_apply writer_already_active
  fi
  local task_block
  if task_block=$(running_task_conflict "" "$goal" "$branch" "$pr" "$allowed"); then
    fail_apply "$task_block"
  fi

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

# Running and suspending tasks occupy the same contract as an active lease.
# except is the task directory allowed to pass its own admission check.
task_contract_busy() {
  local contract=$1 except=${2:-} dir phase other
  local store
  store=$(task_store)
  [[ -d $store ]] || return 1
  for dir in "$store"/*; do
    [[ -d $dir && -f $dir/task.yml && -f $dir/phase ]] || continue
    [[ -z $except || $dir != "$except" ]] || continue
    phase=$(tr -d '[:space:]' <"$dir/phase")
    case $phase in
      running | suspending) ;;
      *) continue ;;
    esac
    other=$(yaml_scalar contract "$dir/task.yml" || true)
    [[ $other == "$contract" ]] && return 0
  done
  return 1
}

comma_overlaps() {
  local left=$1 right=$2 item other
  [[ -n $left && -n $right ]] || return 1
  local IFS=,
  local -a left_arr right_arr
  read -r -a left_arr <<<"$left"
  read -r -a right_arr <<<"$right"
  for item in "${left_arr[@]}"; do
    item=${item#"${item%%[![:space:]]*}"}
    item=${item%"${item##*[![:space:]]}"}
    [[ -n $item ]] || continue
    for other in "${right_arr[@]}"; do
      other=${other#"${other%%[![:space:]]*}"}
      other=${other%"${other##*[![:space:]]}"}
      [[ -n $other ]] || continue
      if paths_overlap "$item" "$other"; then
        return 0
      fi
    done
  done
  return 1
}

scope_conflict_reason() {
  local self_contract=$1 self_branch=$2 self_pr=$3 self_files=$4
  local other_contract=$5 other_branch=$6 other_pr=$7 other_files=$8
  [[ -z $self_branch ]] && self_branch=none
  [[ -z $other_branch ]] && other_branch=none
  [[ -z $self_pr ]] && self_pr=none
  [[ -z $other_pr ]] && other_pr=none
  if [[ $self_contract == "$other_contract" ]]; then
    printf 'writer_already_active\n'
    return 0
  fi
  if [[ $self_branch != none && $self_branch == "$other_branch" ]]; then
    printf 'branch_already_active\n'
    return 0
  fi
  if [[ $self_pr != none && $self_pr == "$other_pr" ]]; then
    printf 'pr_already_active\n'
    return 0
  fi
  if comma_overlaps "$self_files" "$other_files"; then
    printf 'allowed_files_overlap\n'
    return 0
  fi
  return 1
}

# except_dir skips the task being admitted. contract/branch/pr/files describe
# the candidate, which may be a task or a lease packet.
running_task_conflict() {
  local except_dir=$1 contract=$2 branch=$3 pr=$4 files=$5
  local store dir phase other_contract other_branch other_pr other_files reason
  store=$(task_store)
  [[ -d $store ]] || return 1
  for dir in "$store"/*; do
    [[ -d $dir && -f $dir/task.yml && -f $dir/phase ]] || continue
    [[ -z $except_dir || $dir != "$except_dir" ]] || continue
    phase=$(tr -d '[:space:]' <"$dir/phase")
    case $phase in
      running | suspending) ;;
      *) continue ;;
    esac
    other_contract=$(yaml_scalar contract "$dir/task.yml" || true)
    other_branch=$(yaml_scalar branch "$dir/task.yml" || true)
    other_pr=$(yaml_scalar pr "$dir/task.yml" || true)
    other_files=$(yaml_scalar allowed_files "$dir/task.yml" || true)
    if reason=$(scope_conflict_reason "$contract" "$branch" "$pr" "$files" \
      "$other_contract" "$other_branch" "$other_pr" "$other_files"); then
      printf '%s\n' "$reason"
      return 0
    fi
  done
  return 1
}

task_consumed() {
  local dir=$1
  if [[ -f $dir/consumed ]]; then
    tr -d '[:space:]' <"$dir/consumed"
  else
    printf '0\n'
  fi
}

charge_task() {
  local dir=$1 budget consumed
  budget=$(yaml_scalar budget "$dir/task.yml" || true)
  [[ $budget =~ ^[0-9]+$ ]] || return 0
  consumed=$(task_consumed "$dir")
  printf '%s\n' "$((consumed + 1))" >"$dir/consumed"
}

write_task_receipt() {
  local dir=$1 verdict=$2 reason=${3:-}
  [[ -f $dir/task.yml ]] || return 0
  local agent contract deadline budget consumed phase invocation branch pr files
  agent=$(yaml_scalar agent_id "$dir/task.yml" || true)
  contract=$(yaml_scalar contract "$dir/task.yml" || true)
  deadline=$(yaml_scalar deadline "$dir/task.yml" || true)
  budget=$(yaml_scalar budget "$dir/task.yml" || true)
  branch=$(yaml_scalar branch "$dir/task.yml" || true)
  pr=$(yaml_scalar pr "$dir/task.yml" || true)
  files=$(yaml_scalar allowed_files "$dir/task.yml" || true)
  consumed=$(task_consumed "$dir")
  phase=$(tr -d '[:space:]' <"$dir/phase" || true)
  invocation=$( [[ -f $dir/invocation.id ]] && tr -d '[:space:]' <"$dir/invocation.id" || true )
  {
    printf 'verdict=%s\n' "$verdict"
    [[ -n $reason ]] && printf 'reason=%s\n' "$reason"
    printf 'agent_id=%s\ncontract=%s\ndeadline=%s\nbudget=%s\nconsumed=%s\nbranch=%s\npr=%s\nallowed_files=%s\ninvocation=%s\nphase=%s\n' \
      "$agent" "$contract" "${deadline:-none}" "${budget:-none}" "$consumed" \
      "${branch:-none}" "${pr:-none}" "$files" "$invocation" "$phase"
  } >"$dir/receipt"
}

worker_alive() {
  local dir=$1 pid
  [[ -f $dir/worker.pid ]] || return 1
  pid=$(tr -d '[:space:]' <"$dir/worker.pid")
  [[ $pid =~ ^[0-9]+$ ]] || return 1
  kill -0 "$pid" 2>/dev/null
}

stop_worker() {
  local pid=$1 pgid self_pgid i
  [[ $pid =~ ^[0-9]+$ && $pid != "$$" && $pid != 0 ]] || return 1
  self_pgid=$(ps -o pgid= -p $$ 2>/dev/null | tr -d ' ' || true)
  pgid=$(ps -o pgid= -p "$pid" 2>/dev/null | tr -d ' ' || true)
  if [[ $pgid =~ ^[0-9]+$ && $pgid != 0 && $pgid != "$self_pgid" ]]; then
    kill -TERM "-$pgid" 2>/dev/null || kill_tree "$pid"
  else
    kill -TERM "$pid" 2>/dev/null || true
  fi
  i=0
  while kill -0 "$pid" 2>/dev/null && ((i < 40)); do
    sleep 0.05
    i=$((i + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    if [[ $pgid =~ ^[0-9]+$ && $pgid != 0 && $pgid != "$self_pgid" ]]; then
      kill -KILL "-$pgid" 2>/dev/null || kill_tree "$pid"
    else
      kill_tree "$pid"
    fi
  fi
  i=0
  while kill -0 "$pid" 2>/dev/null && ((i < 20)); do
    sleep 0.05
    i=$((i + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    return 1
  fi
  return 0
}

# Returns 0 when this invoke still owns the phase and the write landed.
# Returns 1 when pause already took the task. The lock is released either way.
commit_task_phase() {
  local dir=$1 next=$2 current
  lease_lock_acquire
  current=$(tr -d '[:space:]' <"$dir/phase")
  case $current in
    suspending | suspended)
      lease_lock_release
      return 1
      ;;
  esac
  if [[ $next == suspended ]]; then
    checkpoint_task_workspace "$dir"
  fi
  printf '%s\n' "$next" >"$dir/phase"
  lease_lock_release
  return 0
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
  local deadline budget branch pr files
  deadline=$(yaml_scalar deadline "$file" || true)
  budget=$(yaml_scalar budget "$file" || true)
  branch=$(yaml_scalar branch "$file" || true)
  pr=$(yaml_scalar pr "$file" || true)
  files=$(yaml_list allowed_files "$file" | paste -sd ',' - || true)
  [[ -n $deadline ]] || deadline=none
  [[ -n $budget ]] || budget=none
  [[ -n $branch ]] || branch=none
  [[ -n $pr ]] || pr=none
  [[ $cpu =~ ^[0-9]+$ && $memory =~ ^[0-9]+$ ]] || fail invalid_resource_bounds
  [[ $budget == none || $budget =~ ^[0-9]+$ ]] || fail invalid_budget
  [[ $pr == none || $pr =~ ^[0-9]+$ ]] || fail invalid_pr
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
deadline: $deadline
budget: $budget
branch: $branch
pr: $pr
allowed_files: $files
command: $command
EOF
  if [[ -n $secret_file && -f $secret_file ]]; then
    cp "$secret_file" "$dir/model.secret"
    chmod 600 "$dir/model.secret"
  else
    : >"$dir/model.secret"
  fi
  printf 'pending\n' >"$dir/phase"
  printf '0\n' >"$dir/consumed"
  printf 'verdict=ok\nagent_id=%s\nphase=pending\n' "$agent_id"
}

cmd_task_status() {
  local agent_id=${1:-}
  [[ -n $agent_id ]] || fail missing_agent_id
  local dir phase
  dir=$(task_dir "$agent_id")
  [[ -f $dir/phase ]] || fail agent_not_found
  phase=$(cat "$dir/phase")
  printf 'verdict=ok\nagent_id=%s\nphase=%s\ncpu=%s\nmemory_mb=%s\ndeadline=%s\nbudget=%s\nconsumed=%s\n' \
    "$agent_id" "$phase" \
    "$(yaml_scalar cpu "$dir/task.yml")" \
    "$(yaml_scalar memory_mb "$dir/task.yml")" \
    "$(yaml_scalar deadline "$dir/task.yml" || echo none)" \
    "$(yaml_scalar budget "$dir/task.yml" || echo none)" \
    "$(task_consumed "$dir")"
}

cmd_task_suspend() {
  local agent_id=${1:-}
  [[ -n $agent_id ]] || fail missing_agent_id
  local dir phase pid
  dir=$(task_dir "$agent_id")
  [[ -f $dir/phase ]] || fail agent_not_found
  lease_lock_acquire
  phase=$(tr -d '[:space:]' <"$dir/phase")
  case $phase in
    running)
      printf 'suspending\n' >"$dir/phase"
      pid=
      if [[ -f $dir/worker.pid ]]; then
        pid=$(tr -d '[:space:]' <"$dir/worker.pid")
      fi
      lease_lock_release
      if [[ ! $pid =~ ^[0-9]+$ ]]; then
        lease_lock_acquire
        printf 'running\n' >"$dir/phase"
        lease_lock_release
        fail pause_unconfirmed
      fi
      if kill -0 "$pid" 2>/dev/null; then
        if ! stop_worker "$pid"; then
          lease_lock_acquire
          printf 'running\n' >"$dir/phase"
          lease_lock_release
          fail pause_unconfirmed
        fi
      fi
      lease_lock_acquire
      checkpoint_task_workspace "$dir"
      printf 'suspended\n' >"$dir/phase"
      lease_lock_release
      printf 'verdict=ok\nagent_id=%s\nphase=suspended\n' "$agent_id"
      return
      ;;
    suspending)
      lease_lock_release
      fail pause_unconfirmed
      ;;
    pending | suspended) ;;
    *)
      lease_lock_release
      fail agent_not_suspendable
      ;;
  esac
  checkpoint_task_workspace "$dir"
  printf 'suspended\n' >"$dir/phase"
  lease_lock_release
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
refuse() {
  printf '%s\n' "\$1" > "\$MONDAY_AGENT_EGRESS_REFUSED"
  echo host_not_allowed >&2
  exit 76
}
saw=0
for arg in "\$@"; do
  case "\$arg" in
    http://*|https://*)
      saw=1
      host=\${arg#*://}
      host=\${host%%/*}
      host=\${host%%\\?*}
      host=\${host##*@}
      host=\${host%%:*}
      ok=0
      while IFS= read -r item || [[ -n \$item ]]; do
        [[ \$item == "\$host" ]] && ok=1
      done < "\$MONDAY_AGENT_ALLOW_FILE"
      [[ \$ok == 1 ]] || refuse "\$host"
      ;;
  esac
done
[[ \$saw == 1 ]] || refuse missing-host
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

command_disallowed_host() {
  local command=$1 file=$2 rest token host next
  rest=$command
  while [[ $rest =~ https?://([^[:space:]\'\"/?#]+) ]]; do
    token=${BASH_REMATCH[1]}
    host=${token##*@}
    host=${host%%:*}
    if ! grep -qx -F -- "$host" "$file"; then
      printf '%s\n' "$host"
      return 0
    fi
    next=${rest#*"${BASH_REMATCH[0]}"}
    [[ $next == "$rest" ]] && break
    rest=$next
  done
  rest=$command
  while [[ $rest =~ [\'\"]([A-Za-z0-9.-]+\.[A-Za-z]{2,})[\'\"] ]]; do
    host=${BASH_REMATCH[1]}
    if ! grep -qx -F -- "$host" "$file"; then
      printf '%s\n' "$host"
      return 0
    fi
    next=${rest#*"${BASH_REMATCH[0]}"}
    [[ $next == "$rest" ]] && break
    rest=$next
  done
  return 1
}

prepare_task_enforcement() {
  local dir=$1
  local cc lib
  cc=$(command -v cc 2>/dev/null || command -v gcc 2>/dev/null || true)
  [[ -n $cc ]] || return 1
  lib=$dir/lib
  mkdir -p "$lib"
  cat >"$lib/measure.c" <<'EOF'
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>
int main(int argc, char **argv) {
  if (argc < 2) return 125;
  pid_t pid = fork();
  if (pid < 0) return 125;
  if (pid == 0) {
    execvp(argv[1], argv + 1);
    _exit(127);
  }
  int status = 0;
  struct rusage ru;
  if (wait4(pid, &status, 0, &ru) < 0) return 125;
  const char *path = getenv("MONDAY_PEAK_RSS");
  if (!path) return 125;
  FILE *f = fopen(path, "w");
  if (!f) return 125;
#ifdef __APPLE__
  fprintf(f, "bytes %ld\n", ru.ru_maxrss);
#else
  fprintf(f, "kb %ld\n", ru.ru_maxrss);
#endif
  fclose(f);
  if (WIFEXITED(status)) return WEXITSTATUS(status);
  if (WIFSIGNALED(status)) return 128 + WTERMSIG(status);
  return 1;
}
EOF
  cat >"$lib/hook.c" <<'EOF'
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dlfcn.h>
#include <errno.h>
#include <netdb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
static int (*real_getaddrinfo)(const char *, const char *, const struct addrinfo *, struct addrinfo **) = 0;
static int loading = 0;
static void bind_real(void) {
  if (!real_getaddrinfo) real_getaddrinfo = dlsym(RTLD_NEXT, "getaddrinfo");
}
static int name_allowed(const char *host) {
  if (!host || !host[0]) return 1;
  if (strcmp(host, "localhost") == 0 || strcmp(host, "127.0.0.1") == 0 || strcmp(host, "::1") == 0) return 1;
  const char *path = getenv("MONDAY_AGENT_ALLOW_FILE");
  if (!path) return 0;
  FILE *f = fopen(path, "r");
  if (!f) return 0;
  char line[256];
  int ok = 0;
  while (fgets(line, sizeof line, f)) {
    line[strcspn(line, "\r\n")] = 0;
    if (strcmp(line, host) == 0) ok = 1;
  }
  fclose(f);
  return ok;
}
static void refuse(const char *host) {
  const char *path = getenv("MONDAY_AGENT_EGRESS_REFUSED");
  if (!path || !host) return;
  FILE *f = fopen(path, "w");
  if (!f) return;
  fputs(host, f);
  fputc('\n', f);
  fclose(f);
}
static int my_getaddrinfo(const char *node, const char *service, const struct addrinfo *hints, struct addrinfo **res) {
  bind_real();
  if (!real_getaddrinfo) return EAI_FAIL;
  if (loading) return real_getaddrinfo(node, service, hints, res);
  if (node && !name_allowed(node)) {
    refuse(node);
    return EAI_NONAME;
  }
  return real_getaddrinfo(node, service, hints, res);
}
#ifdef __APPLE__
#define DYLD_INTERPOSE(_repl, _orig) \
  __attribute__((used)) static struct { const void *repl; const void *orig; } \
  _interpose_##_orig __attribute__((section("__DATA,__interpose"))) = { \
    (const void *)(unsigned long)&_repl, (const void *)(unsigned long)&_orig };
DYLD_INTERPOSE(my_getaddrinfo, getaddrinfo)
#else
int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints, struct addrinfo **res) {
  return my_getaddrinfo(node, service, hints, res);
}
#endif
EOF
  "$cc" -o "$lib/measure" "$lib/measure.c" || return 1
  if [[ $(uname -s) == Darwin ]]; then
    "$cc" -dynamiclib -o "$lib/hook.dylib" "$lib/hook.c" || return 1
    export MONDAY_INTERPOSE=$lib/hook.dylib
    unset MONDAY_PRELOAD
  else
    "$cc" -shared -fPIC -o "$lib/hook.so" "$lib/hook.c" -ldl || return 1
    export MONDAY_PRELOAD=$lib/hook.so
    unset MONDAY_INTERPOSE
  fi
  cat >"$lib/run-task.sh" <<'EOF'
#!/bin/bash
if [[ -n ${MONDAY_INTERPOSE:-} ]]; then
  export DYLD_INSERT_LIBRARIES="$MONDAY_INTERPOSE"
fi
if [[ -n ${MONDAY_PRELOAD:-} ]]; then
  export LD_PRELOAD="$MONDAY_PRELOAD${LD_PRELOAD:+:$LD_PRELOAD}"
fi
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY no_proxy NO_PROXY WS_PROXY WSS_PROXY
if [[ -n ${MONDAY_PROXY_PORT:-} ]]; then
  proxy="http://127.0.0.1:${MONDAY_PROXY_PORT}"
  export http_proxy="$proxy" https_proxy="$proxy" HTTP_PROXY="$proxy" HTTPS_PROXY="$proxy" ALL_PROXY="$proxy" all_proxy="$proxy"
fi
eval "$MONDAY_COMMAND"
EOF
  chmod +x "$lib/run-task.sh" "$lib/measure"
  export MONDAY_MEASURE=$lib/measure
  export MONDAY_RUNNER=$lib/run-task.sh
  export MONDAY_PEAK_RSS=$dir/peak-rss
  unset MONDAY_SANDBOX_PROFILE MONDAY_PROXY_PORT
  if ! command -v sandbox-exec >/dev/null 2>&1; then
    return 0
  fi
  cat >"$lib/proxy.c" <<'EOF'
#include <arpa/inet.h>
#include <netdb.h>
#include <signal.h>
#include <sys/select.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
static int allowed(const char *host) {
  const char *path = getenv("MONDAY_AGENT_ALLOW_FILE");
  if (!path || !host || !host[0]) return 0;
  FILE *f = fopen(path, "r");
  if (!f) return 0;
  char line[256];
  int ok = 0;
  while (fgets(line, sizeof line, f)) {
    line[strcspn(line, "\r\n")] = 0;
    if (strcmp(line, host) == 0) ok = 1;
  }
  fclose(f);
  return ok;
}
static void refuse(const char *host) {
  const char *path = getenv("MONDAY_AGENT_EGRESS_REFUSED");
  if (!path) return;
  FILE *f = fopen(path, "w");
  if (!f) return;
  fputs(host && host[0] ? host : "blocked", f);
  fputc('\n', f);
  fclose(f);
}
static void splice(int a, int b) {
  char buf[65536];
  for (;;) {
    fd_set fds;
    FD_ZERO(&fds);
    FD_SET(a, &fds);
    FD_SET(b, &fds);
    int m = a > b ? a : b;
    if (select(m + 1, &fds, 0, 0, 0) < 0) break;
    int from = FD_ISSET(a, &fds) ? a : b;
    int to = from == a ? b : a;
    ssize_t n = read(from, buf, sizeof buf);
    if (n <= 0) break;
    ssize_t off = 0;
    while (off < n) {
      ssize_t w = write(to, buf + off, (size_t)(n - off));
      if (w <= 0) return;
      off += w;
    }
  }
}
int main(void) {
  signal(SIGCHLD, SIG_IGN);
  signal(SIGPIPE, SIG_IGN);
  int s = socket(AF_INET, SOCK_STREAM, 0);
  if (s < 0) return 1;
  int one = 1;
  setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
  struct sockaddr_in addr;
  memset(&addr, 0, sizeof addr);
  addr.sin_family = AF_INET;
  addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  if (bind(s, (struct sockaddr *)&addr, sizeof addr) != 0) return 1;
  socklen_t len = sizeof addr;
  if (getsockname(s, (struct sockaddr *)&addr, &len) != 0) return 1;
  if (listen(s, 16) != 0) return 1;
  printf("%d\n", ntohs(addr.sin_port));
  fflush(stdout);
  for (;;) {
    int c = accept(s, 0, 0);
    if (c < 0) continue;
    if (fork() != 0) { close(c); continue; }
    close(s);
    char req[8192];
    size_t n = 0;
    req[0] = 0;
    while (n + 1 < sizeof req) {
      ssize_t r = read(c, req + n, sizeof req - 1 - n);
      if (r <= 0) break;
      n += (size_t)r;
      req[n] = 0;
      if (strstr(req, "\r\n\r\n")) break;
    }
    char host[256];
    host[0] = 0;
    int port = 443;
    if (!strncmp(req, "CONNECT ", 8)) {
      const char *p = req + 8;
      const char *sp = strchr(p, ' ');
      if (sp && (size_t)(sp - p) < sizeof host) {
        memcpy(host, p, (size_t)(sp - p));
        host[sp - p] = 0;
        char *colon = strrchr(host, ':');
        if (colon) { *colon = 0; port = atoi(colon + 1); }
      }
    }
    if (!allowed(host)) {
      refuse(host);
      const char *msg = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n";
      (void)write(c, msg, strlen(msg));
      _exit(0);
    }
    char portstr[16];
    snprintf(portstr, sizeof portstr, "%d", port);
    struct addrinfo hints, *res = 0;
    memset(&hints, 0, sizeof hints);
    hints.ai_socktype = SOCK_STREAM;
    int up = -1;
    if (getaddrinfo(host, portstr, &hints, &res) == 0) {
      for (struct addrinfo *ai = res; ai; ai = ai->ai_next) {
        up = socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
        if (up < 0) continue;
        if (connect(up, ai->ai_addr, ai->ai_addrlen) == 0) break;
        close(up);
        up = -1;
      }
      freeaddrinfo(res);
    }
    if (up < 0) {
      const char *msg = "HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n";
      (void)write(c, msg, strlen(msg));
      _exit(0);
    }
    const char *ok = "HTTP/1.1 200 Connection Established\r\n\r\n";
    (void)write(c, ok, strlen(ok));
    splice(c, up);
    _exit(0);
  }
}
EOF
  "$cc" -o "$lib/proxy" "$lib/proxy.c" || return 1
  if [[ -n ${EGRESS_PROXY_PID:-} ]]; then
    kill "$EGRESS_PROXY_PID" 2>/dev/null || true
  fi
  # The previous invoke leaves a port file. A background redirect truncates it
  # after the shell has already continued, so delete it before waiting.
  rm -f "$lib/proxy.port"
  "$lib/proxy" >"$lib/proxy.port" &
  EGRESS_PROXY_PID=$!
  local spins=0 port=
  while [[ $spins -lt 50 ]]; do
    if ! kill -0 "$EGRESS_PROXY_PID" 2>/dev/null; then
      return 1
    fi
    if [[ -s $lib/proxy.port ]]; then
      port=$(tr -d '[:space:]' <"$lib/proxy.port" || true)
      if [[ $port =~ ^[0-9]+$ ]]; then
        break
      fi
    fi
    sleep 0.05
    spins=$((spins + 1))
  done
  [[ ${port:-} =~ ^[0-9]+$ ]] || return 1
  cat >"$lib/sandbox.sb" <<EOF
(version 1)
(allow default)
(deny network-outbound)
(allow network-outbound (remote tcp "localhost:${port}"))
(allow network-outbound (remote udp "localhost:${port}"))
EOF
  export MONDAY_PROXY_PORT=$port
  export MONDAY_SANDBOX_PROFILE=$lib/sandbox.sb
}

# GNU ps -g selects a session, not a process group. Peak RSS comes from wait4
# so a fast allocation is still capped after it exits. There is no wall-clock
# kill: cpu is CPU time, and an in-budget sleep must be allowed to finish.
run_bounded_command() {
  local cpu=$1 memory=$2 command=$3
  local max_kb=$((memory * 1024))
  local child pgid snapshot pid grp rss state total alive exceeded=0
  [[ -n ${MONDAY_MEASURE:-} && -x ${MONDAY_MEASURE} && -n ${MONDAY_RUNNER:-} ]] || return 125
  export MONDAY_COMMAND=$command
  rm -f "${MONDAY_PEAK_RSS:-}"
  set -m
  (
    ulimit -t $((cpu * 3600)) || exit 126
    if [[ -n ${MONDAY_SANDBOX_PROFILE:-} ]]; then
      exec sandbox-exec -f "$MONDAY_SANDBOX_PROFILE" "$MONDAY_MEASURE" /bin/bash "$MONDAY_RUNNER"
    fi
    exec "$MONDAY_MEASURE" /bin/bash "$MONDAY_RUNNER"
  ) &
  child=$!
  if [[ -n ${MONDAY_WORKER_PID_FILE:-} ]]; then
    printf '%s\n' "$child" >"$MONDAY_WORKER_PID_FILE"
  fi
  pgid=$(ps -o pgid= -p "$child" 2>/dev/null | tr -d ' ' || true)
  [[ $pgid =~ ^[0-9]+$ ]] || pgid=$child
  while :; do
    # ps can exit non-zero when a process disappears mid-scan. That is not
    # proof the cap is unenforceable; the wait4 peak file still accounts.
    snapshot=$(ps -ax -o pid=,pgid=,rss=,state= 2>/dev/null || true)
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
    sleep 0.2
  done
  if ((exceeded)); then
    wait "$child" 2>/dev/null || true
    return 137
  fi
  local exit_status=0
  wait "$child" || exit_status=$?
  if ((exit_status == 137)); then
    return 137
  fi
  if ((exit_status == 126)); then
    return 126
  fi
  [[ -n ${MONDAY_PEAK_RSS:-} && -s $MONDAY_PEAK_RSS ]] || return 125
  local kind value
  read -r kind value <"$MONDAY_PEAK_RSS" || return 125
  if [[ $kind == bytes && $value =~ ^[0-9]+$ ]]; then
    if (( value > max_kb * 1024 )); then
      return 137
    fi
  elif [[ $kind == kb && $value =~ ^[0-9]+$ ]]; then
    if (( value > max_kb )); then
      return 137
    fi
  else
    return 125
  fi
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
  TASK_RECEIPT_DIR=$dir
  contract=$(yaml_scalar contract "$dir/task.yml")
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
  prepare_task_enforcement "$dir" || {
    printf 'failed\n' >"$dir/phase"
    fail egress_unenforceable
  }
  lease_lock_acquire
  phase=$(tr -d '[:space:]' <"$dir/phase")
  case $phase in
    pending | suspended) ;;
    running | suspending)
      if worker_alive "$dir"; then
        lease_lock_release
        fail agent_already_running
      fi
      lease_lock_release
      fail execution_unresolved
      ;;
    failed)
      lease_lock_release
      fail agent_failed
      ;;
    *)
      lease_lock_release
      fail agent_not_invocable
      ;;
  esac
  local task_deadline task_budget task_branch task_pr task_files admission
  task_deadline=$(yaml_scalar deadline "$dir/task.yml" || echo none)
  task_budget=$(yaml_scalar budget "$dir/task.yml" || echo none)
  task_branch=$(yaml_scalar branch "$dir/task.yml" || echo none)
  task_pr=$(yaml_scalar pr "$dir/task.yml" || echo none)
  task_files=$(yaml_scalar allowed_files "$dir/task.yml" || true)
  if deadline_passed "$task_deadline"; then
    lease_lock_release
    fail deadline_passed
  fi
  if [[ $task_budget =~ ^[0-9]+$ ]] && (( $(task_consumed "$dir") >= task_budget )); then
    lease_lock_release
    fail budget_exhausted
  fi
  if admission=$(running_task_conflict "$dir" "$contract" "$task_branch" "$task_pr" "$task_files"); then
    lease_lock_release
    fail "$admission"
  fi
  local lease_file lease_contract lease_branch lease_pr lease_files lease_reason
  while IFS= read -r lease_file; do
    [[ -n $lease_file ]] || continue
    lease_contract=$(yaml_scalar contract "$lease_file" || true)
    lease_branch=$(yaml_scalar branch "$lease_file" || true)
    lease_pr=$(yaml_scalar pr "$lease_file" || true)
    lease_files=$(yaml_scalar allowed_files "$lease_file" || true)
    if lease_reason=$(scope_conflict_reason "$contract" "$task_branch" "$task_pr" "$task_files" \
      "$lease_contract" "$lease_branch" "$lease_pr" "$lease_files"); then
      lease_lock_release
      fail "$lease_reason"
    fi
  done < <(active_lease_files)
  printf '%s-%s\n' "$(date -u +"%Y%m%dT%H%M%SZ")" "$$" >"$dir/invocation.id"
  rm -f "$dir/worker.pid"
  printf 'running\n' >"$dir/phase"
  charge_task "$dir"
  lease_lock_release
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
    export MONDAY_WORKER_PID_FILE=$dir/worker.pid
    run_bounded_command "$cpu" "$memory" "$command"
  ) >>"$dir/invocation.log" 2>>"$dir/invocation.err"
  exit_status=$?
  set -e
  if [[ -f $dir/model.secret ]]; then
    local secret
    secret=$(cat "$dir/model.secret")
    if [[ -n $secret ]]; then
      if grep -F -q -- "$secret" "$dir/task.yml" "$dir/command.log" "$dir/invocation.log" "$dir/invocation.err" 2>/dev/null; then
        commit_task_phase "$dir" failed || {
          printf 'verdict=ok\nagent_id=%s\nphase=%s\n' "$agent_id" "$(tr -d '[:space:]' <"$dir/phase")"
          return 0
        }
        fail secret_leaked
      fi
    fi
  fi
  if [[ ! -s $dir/egress-refused && -f $dir/invocation.err ]] && grep -E -q 'nodename nor servname|Could not resolve host|Operation not permitted|CONNECT tunnel failed|Network is unreachable' "$dir/invocation.err"; then
    local blocked_host
    blocked_host=$(command_disallowed_host "$command" "$dir/allow_hosts" || true)
    if [[ -n $blocked_host ]]; then
      printf '%s\n' "$blocked_host" >"$dir/egress-refused"
    fi
  fi
  if [[ -s $dir/egress-refused ]]; then
    commit_task_phase "$dir" failed || {
      printf 'verdict=ok\nagent_id=%s\nphase=%s\n' "$agent_id" "$(tr -d '[:space:]' <"$dir/phase")"
      return 0
    }
    fail host_not_allowed
  fi
  if ((exit_status == 137)); then
    commit_task_phase "$dir" failed || {
      printf 'verdict=ok\nagent_id=%s\nphase=%s\n' "$agent_id" "$(tr -d '[:space:]' <"$dir/phase")"
      return 0
    }
    fail memory_exceeded
  fi
  if ((exit_status == 125)); then
    commit_task_phase "$dir" failed || {
      printf 'verdict=ok\nagent_id=%s\nphase=%s\n' "$agent_id" "$(tr -d '[:space:]' <"$dir/phase")"
      return 0
    }
    fail memory_bound_unenforceable
  fi
  if ((exit_status == 126)); then
    commit_task_phase "$dir" failed || {
      printf 'verdict=ok\nagent_id=%s\nphase=%s\n' "$agent_id" "$(tr -d '[:space:]' <"$dir/phase")"
      return 0
    }
    fail cpu_bound_unenforceable
  fi
  if ((exit_status != 0)); then
    commit_task_phase "$dir" failed || {
      printf 'verdict=ok\nagent_id=%s\nphase=%s\n' "$agent_id" "$(tr -d '[:space:]' <"$dir/phase")"
      return 0
    }
    printf 'verdict=blocked\nreason=command_failed\nagent_id=%s\nphase=failed\n' "$agent_id" >&2
    return "$exit_status"
  fi
  commit_task_phase "$dir" suspended || {
    write_task_receipt "$dir" ok
    printf 'verdict=ok\nagent_id=%s\nphase=%s\n' "$agent_id" "$(tr -d '[:space:]' <"$dir/phase")"
    return 0
  }
  write_task_receipt "$dir" ok
  printf 'verdict=ok\nagent_id=%s\nphase=suspended\ncpu=%s\nmemory_mb=%s\nworkspace=%s\n' \
    "$agent_id" "$cpu" "$memory" "$ws"
}

cmd_task_batch() {
  local action=${1:-}
  [[ -n $action ]] || fail unknown_task_argument
  shift
  case "$action" in
    run) task_batch_run "$@" ;;
    show) task_batch_show "$@" ;;
    *) fail unknown_task_argument ;;
  esac
}

task_batch_dir() {
  printf '%s/agent-task-batches/%s\n' "$(git_common)" "$1"
}

task_slice_started() {
  local dir=$1 phase
  [[ -f $dir/phase ]] || return 1
  phase=$(tr -d '[:space:]' <"$dir/phase")
  [[ $phase != pending ]]
}

task_slice_body() {
  local agent=$1 dir phase
  dir=$(task_dir "$agent")
  if [[ -f $dir/receipt ]]; then
    cat "$dir/receipt"
    return 0
  fi
  phase=pending
  [[ -f $dir/phase ]] && phase=$(tr -d '[:space:]' <"$dir/phase")
  case $phase in
    running | suspending)
      if worker_alive "$dir"; then
        printf 'verdict=open\nreason=still_running\nagent_id=%s\nphase=%s\n' "$agent" "$phase"
      else
        printf 'verdict=blocked\nreason=execution_unresolved\nagent_id=%s\nphase=%s\n' "$agent" "$phase"
      fi
      ;;
    pending)
      printf 'verdict=open\nreason=not_started\nagent_id=%s\nphase=pending\n' "$agent"
      ;;
    *)
      printf 'verdict=blocked\nreason=missing_receipt\nagent_id=%s\nphase=%s\n' "$agent" "$phase"
      ;;
  esac
}

task_batch_run() {
  local file=
  while (($#)); do
    case "$1" in
      --file) file=$2; shift 2 ;;
      *) fail unknown_task_argument ;;
    esac
  done
  [[ -n $file && -f $file ]] || fail missing_task_file
  local batch_id
  batch_id=$(yaml_scalar batch_id "$file") || fail missing_batch_id
  [[ $batch_id =~ ^[A-Za-z0-9._-]+$ ]] || fail invalid_batch_id
  local -a task_files=()
  local task_file
  while IFS= read -r task_file; do
    [[ -n $task_file ]] || continue
    task_files+=("$task_file")
  done < <(yaml_list tasks "$file")
  [[ ${#task_files[@]} -gt 0 ]] || fail missing_batch_tasks
  local concurrency
  concurrency=$(yaml_scalar concurrency "$file" || true)
  [[ -n $concurrency ]] || concurrency=2
  [[ $concurrency =~ ^[1-9][0-9]*$ ]] || fail invalid_concurrency
  local batch_dir agent_id dir
  batch_dir=$(task_batch_dir "$batch_id")
  mkdir -p "$batch_dir"
  if [[ ! -s $batch_dir/agents ]]; then
    for task_file in "${task_files[@]}"; do
      [[ -f $task_file ]] || fail missing_task_file
      agent_id=$(yaml_scalar agent_id "$task_file") || fail missing_agent_id
      dir=$(task_dir "$agent_id")
      if [[ ! -f $dir/task.yml ]]; then
        "$0" task-declare --file "$task_file" >"$batch_dir/$agent_id.declare"
      fi
      printf '%s\n' "$agent_id" >>"$batch_dir/agents"
    done
  fi
  task_batch_write "$batch_id" >/dev/null
  local -a pending=()
  while IFS= read -r agent_id; do
    [[ -n $agent_id ]] || continue
    dir=$(task_dir "$agent_id")
    if task_slice_started "$dir"; then
      continue
    fi
    pending+=("$agent_id")
  done <"$batch_dir/agents"
  local -a pids=()
  local next=0
  while ((next < ${#pending[@]})) || ((${#pids[@]} > 0)); do
    while ((${#pids[@]} < concurrency && next < ${#pending[@]})); do
      agent_id=${pending[$next]}
      next=$((next + 1))
      "$0" task-invoke "$agent_id" >"$batch_dir/$agent_id.invoke" 2>&1 &
      pids+=("$!")
    done
    if ((${#pids[@]} == 0)); then
      break
    fi
    wait "${pids[0]}" || true
    if ((${#pids[@]} > 1)); then
      pids=("${pids[@]:1}")
    else
      pids=()
    fi
  done
  task_batch_write "$batch_id"
}

task_batch_show() {
  local batch_id=
  while (($#)); do
    case "$1" in
      --batch) batch_id=$2; shift 2 ;;
      *) fail unknown_task_argument ;;
    esac
  done
  [[ -n $batch_id ]] || fail missing_batch_id
  local batch_dir
  batch_dir=$(task_batch_dir "$batch_id")
  [[ -s $batch_dir/agents ]] || fail batch_not_found
  task_batch_write "$batch_id"
}

task_batch_write() {
  local batch_id=$1 agent_id slice verdict=ok saw_open=0 saw_blocked=0
  local batch_dir tmp
  batch_dir=$(task_batch_dir "$batch_id")
  tmp=$(mktemp "$batch_dir/receipt.XXXXXX")
  while IFS= read -r agent_id; do
    [[ -n $agent_id ]] || continue
    {
      printf 'slice_begin\n'
      task_slice_body "$agent_id"
      printf 'slice_end\n'
    } >>"$tmp"
  done <"$batch_dir/agents"
  while IFS= read -r slice; do
    case $slice in
      verdict=open) saw_open=1 ;;
      verdict=ok) ;;
      verdict=*) saw_blocked=1 ;;
    esac
  done <"$tmp"
  if ((saw_open)); then
    verdict=open
  elif ((saw_blocked)); then
    verdict=blocked
  fi
  {
    printf 'verdict=%s\nbatch_id=%s\n' "$verdict" "$batch_id"
    cat "$tmp"
  } >"$batch_dir/receipt"
  rm -f "$tmp"
  cat "$batch_dir/receipt"
}

usage() {
  echo "usage: $0 check-managed|report|list|get|apply|release|spawn|sweep|task-declare|task-invoke|task-suspend|task-status|task-egress|task-batch"
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
  task-batch) shift; cmd_task_batch "$@" ;;
  help|--help|-h) usage ;;
  *) usage >&2; exit 2 ;;
esac
