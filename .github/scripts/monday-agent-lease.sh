#!/usr/bin/env bash
# Lease helpers for monday-agent. Sourced by agent-worktree-preflight.sh.
# Lab-only: mutates worktree registration and ownership records. Never spawns agents.

yaml_scalar() {
  local key=$1 file=$2
  local line
  line=$(grep -E "^${key}:" "$file" | head -1 || true)
  [[ -n $line ]] || return 1
  line=${line#*:}
  line=${line#"${line%%[![:space:]]*}"}
  line=${line%"${line##*[![:space:]]}"}
  line=${line#\"}
  line=${line%\"}
  line=${line#\'}
  line=${line%\'}
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
  git rev-parse --git-common-dir
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
  # shellcheck disable=SC2254
  [[ $a == $b ]] && return 0
  # shellcheck disable=SC2254
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
    [[ $(yaml_scalar status "$f" || true) == active ]] && printf '%s\n' "$f"
  done
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
  local dest=$1
  mkdir -p "$(dirname "$dest")"
  cat >"$dest" <<EOF
schema: monday.agent_lease.v1
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
pr: $LEASE_PR
deadline: $LEASE_DEADLINE
trading_gates: $LEASE_GATES
EOF
  if [[ $LEASE_STATUS == released ]]; then
    cat >>"$dest" <<EOF
released_at: $(date -u +"%Y-%m-%dT%H:%M:%SZ")
recovery_sha: $LEASE_RECOVERY_SHA
recovery_pr: $LEASE_PR
EOF
  fi
}

cmd_apply() {
  local packet= worktree_override=
  while (($#)); do
    case "$1" in
      --packet-file) packet=$2; shift 2 ;;
      --worktree) worktree_override=$2; shift 2 ;;
      *) fail "unknown_apply_argument" ;;
    esac
  done
  [[ -n $packet && -f $packet ]] || fail missing_packet_file
  packet=$(cd "$(dirname "$packet")" && pwd -P)/$(basename "$packet")

  local from to routed goal evidence constraints done_criteria gates branch writer deadline pr
  from=$(yaml_scalar from "$packet") || fail missing_from
  to=$(yaml_scalar to "$packet") || fail missing_to
  routed=$(yaml_scalar routed_by "$packet") || fail missing_routed_by
  goal=$(yaml_scalar goal "$packet") || fail missing_goal
  evidence=$(yaml_scalar evidence_paths "$packet") || fail missing_evidence_paths
  constraints=$(yaml_scalar constraints "$packet") || fail missing_constraints
  done_criteria=$(yaml_scalar done_criteria "$packet") || fail missing_done_criteria
  gates=$(yaml_scalar trading_gates "$packet") || fail missing_trading_gates
  branch=$(yaml_scalar branch "$packet") || fail missing_branch
  writer=$(yaml_scalar writer "$packet") || fail missing_writer
  deadline=$(yaml_scalar deadline "$packet") || fail missing_deadline
  pr=$(yaml_scalar pr "$packet") || fail missing_pr
  : "$from" "$to" "$routed" "$evidence" "$constraints" "$done_criteria"

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

  local f other_owner other_branch other_pr other_files item other
  while IFS= read -r f; do
    [[ -n $f ]] || continue
    other_owner=$(yaml_scalar owner "$f")
    other_branch=$(yaml_scalar branch "$f")
    other_pr=$(yaml_scalar pr "$f")
    other_files=$(yaml_scalar allowed_files "$f")
    [[ $other_owner != "$writer" ]] || fail_apply writer_already_active
    [[ $other_branch != "$branch" ]] || fail_apply branch_already_leased
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

  LEASE_ID=$(new_lease_id)
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
  LEASE_PR=$pr
  LEASE_DEADLINE=$deadline
  LEASE_GATES=$gates

  local record store_file
  record=$(git -C "$wt" rev-parse --git-path agent-worktree.yml)
  store_file="$(lease_store)/${LEASE_ID}.yml"
  write_lease_record "$record"
  write_lease_record "$store_file"
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

find_lease_file() {
  local key=$1
  local store f id wt
  store=$(lease_store)
  [[ -d $store ]] || return 1
  for f in "$store"/*.yml; do
    [[ -f $f ]] || continue
    id=$(yaml_scalar lease_id "$f" || true)
    wt=$(yaml_scalar worktree "$f" || true)
    if [[ $id == "$key" || $wt == "$key" ]]; then
      printf '%s\n' "$f"
      return 0
    fi
  done
  return 1
}

cmd_release() {
  local key= discard=0
  while (($#)); do
    case "$1" in
      --discard-unique) discard=1; shift ;;
      --*) fail unknown_release_argument ;;
      *) key=$1; shift ;;
    esac
  done
  [[ -n $key ]] || fail missing_lease_id
  lease_lock_acquire
  local store_file
  store_file=$(find_lease_file "$key") || fail_apply lease_not_found
  local status wt
  status=$(yaml_scalar status "$store_file")
  [[ $status == active ]] || fail_apply lease_not_active
  wt=$(yaml_scalar worktree "$store_file")
  [[ -d $wt ]] || fail_apply worktree_missing

  if [[ -n $(git -C "$wt" status --porcelain) ]]; then
    fail_apply dirty_worktree
  fi
  local ahead=0
  ahead=$(unpushed_count "$wt")
  if ((ahead > 0)) && ! head_in_tip "$wt"; then
    if ((discard != 1)); then
      fail_apply unique_unpushed
    fi
  fi

  LEASE_ID=$(yaml_scalar lease_id "$store_file")
  LEASE_STATUS=released
  LEASE_CONTRACT=$(yaml_scalar contract "$store_file")
  LEASE_OWNER=$(yaml_scalar owner "$store_file")
  LEASE_SEAT=$(yaml_scalar seat "$store_file")
  LEASE_WORKTREE=$wt
  LEASE_BRANCH=$(yaml_scalar branch "$store_file")
  LEASE_BASE=$(yaml_scalar base_sha "$store_file")
  LEASE_ALLOWED=$(yaml_scalar allowed_files "$store_file")
  LEASE_DEPENDENCY=$(yaml_scalar dependency "$store_file")
  LEASE_PACKET_SHA=$(yaml_scalar packet_sha256 "$store_file")
  LEASE_PR=$(yaml_scalar pr "$store_file")
  LEASE_DEADLINE=$(yaml_scalar deadline "$store_file")
  LEASE_GATES=$(yaml_scalar trading_gates "$store_file")
  LEASE_RECOVERY_SHA=$(integration_tip)

  git worktree remove "$wt"
  write_lease_record "$store_file"
  lease_lock_release
  printf 'verdict=ok\nlease_id=%s\nstatus=released\nrecovery_sha=%s\n' \
    "$LEASE_ID" "$LEASE_RECOVERY_SHA"
}

cleanup_class() {
  local path=$1 state=$2 lease_status=$3 ahead=$4
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
  if [[ $lease_status == active ]]; then
    printf 'keep\tactive lease\n'
    return
  fi
  if ((ahead > 0)); then
    if head_in_tip "$path"; then
      printf 'cleanup-safe\thead contained in integration tip\n'
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
    local store f wt_rec
    store=$(lease_store)
    if [[ -d $store ]]; then
      for f in "$store"/*.yml; do
        [[ -f $f ]] || continue
        wt_rec=$(yaml_scalar worktree "$f" || true)
        if [[ $wt_rec == "$path" ]]; then
          lease_id=$(yaml_scalar lease_id "$f")
          lease_status=$(yaml_scalar status "$f")
          owner=$(yaml_scalar owner "$f")
          pr=$(yaml_scalar pr "$f")
          break
        fi
      done
    fi
    local ahead=0
    if [[ -d $path ]]; then
      ahead=$(unpushed_count "$path")
    fi
    local safety reason
    IFS=$'\t' read -r safety reason < <(cleanup_class "$path" "$state" "$lease_status" "$ahead")
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
