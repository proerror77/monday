#!/usr/bin/env bash
# Linux behavioral regressions: real immutable records, filesystem metadata and flock.
# shellcheck disable=SC2034,SC2317,SC2329 # Sourced native functions dynamically consume fixture globals/overrides.
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR
# shellcheck source=test-recovery-retention-fixture.sh
. "$script_dir/test-recovery-retention-fixture.sh"
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
setup_retention_fixture "$fixture"

rejected() {
  local label=$1; shift
  if ( "$@" ) >"$fixture/rejected.out" 2>"$fixture/rejected.err"; then
    printf 'unexpected acceptance: %s\n' "$label" >&2; exit 1
  else
    REJECTED_STATUS=$?
  fi
}

original_files() {
  (cd "$RETENTION_FIXTURE_JOB_DIR"; find . -type f -print0 | sort -z | xargs -0 sha256sum)
  (cd "$RETENTION_FIXTURE_EVIDENCE"; find . -path ./retention -prune -o -type f -print0 | sort -z | xargs -0 sha256sum)
}

retention_fixture_job 1
original_files >"$fixture/original.before"
retention_fixture_retain >"$fixture/first.json"
original_files >"$fixture/original.after"
cmp "$fixture/original.before" "$fixture/original.after"
[[ -d $RETENTION_FIXTURE_JOB_DIR && $(cat "$RETENTION_FIXTURE_JOB_DIR/.binance-lob-archiver.lock") == lock-evidence ]]
[[ $(jq .failure_count "$RETENTION_FIXTURE_JOB_DIR/upload-status.json") == 7 ]]
pointer="$EVIDENCE_ROOT/retained/spot/$RETAIN_JOB_ID.json"
record="$RETENTION_FIXTURE_EVIDENCE/retention/$(jq -r .request_sha256 "$pointer")"
jq -e '.data_recovered==false and .delivery_verified==false and .automatic_retry==false
  and .replay_eligibility=="not_assessed" and .recovery_result=="failed"
  and .request.identity.original_controller_sha256==null' "$record/receipt.json" >/dev/null
jq -e 'any(.entries[]; .scope=="queue" and .path=="date=2026-09-01/hour=00/part-one.jsonl.part")' "$record/inventory.json" >/dev/null
before=$(retention_fingerprint "$pointer")
retention_fixture_retain >"$fixture/repeat.json"
cmp "$fixture/first.json" "$fixture/repeat.json"
[[ $(retention_fingerprint "$pointer") == "$before" ]]
conflicting_repeat() { RETAIN_REQUEST_ID=conflict; retention_fixture_retain; }
rejected conflicting-repeat conflicting_repeat
resume_retained() {
  RESUME_JOB_ID=$RETAIN_JOB_ID; RESUME_JOB_SHA256=$RETAIN_JOB_SHA256
  RESUME_FROM_CONTROLLER=$RETAIN_CONTROLLER; RESUME_CONTROLLER=$RETAIN_CONTROLLER
  RESUME_TRANSITION_SHA256=$(printf '%064d' 1); RESUME_REQUEST_ID=forbidden
  RESUME_TRANSITION_RECEIPT="$DATA_ROOT/monday/evidence/cutovers/$RETAIN_CONTROLLER/transition.json"
  resume_market
}
rejected resume-retained resume_retained
resume_guard_before_drain() {
  drain_lock() { : >"$ROOT_PREFIX/resume-entered-drain"; return 1; }
  resume_retained
}
assert_resume_guard() {
  rejected resume-claim-without-pointer resume_guard_before_drain
  [[ ! -e $ROOT_PREFIX/resume-entered-drain ]]
  grep -Fq 'a retained or partially retained historical job cannot be resumed' "$fixture/rejected.err"
}

# Reader never hashes payload bodies, creates locks or changes metadata. Linux
# ctime remains an independent guard even if a writer restores the old mtime.
eval "$(declare -f retention_bounded | sed '1s/retention_bounded/fixture_real_bounded/')"
retention_bounded() {
  if [[ $1 == sha256sum && ${*: -1} == *.part* ]] || [[ $1 == sha256sum && ${*: -1} == *.zst ]]; then
    printf 'hot reader tried to hash payload bytes\n' >&2; return 1
  fi
  fixture_real_bounded "$@"
}
readonly_snapshot() {
  local path
  while IFS= read -r -d '' path; do
    printf '%s %s\n' "$path" "$(retention_fingerprint "$path")"
  done < <(find "$QUEUE_ROOT" "$EVIDENCE_ROOT" "$LOCK_ROOT" -print0 | sort -z)
}
readonly_snapshot >"$fixture/read.before"
retention_fixture_check >"$fixture/check.json"
readonly_snapshot >"$fixture/read.after"
cmp "$fixture/read.before" "$fixture/read.after"
jq -e '.retained_failed_count==1 and .invalid_retention_count==0 and (.failed_job_ids|length)==1' "$fixture/check.json" >/dev/null
eval "$(declare -f fixture_real_bounded | sed '1s/fixture_real_bounded/retention_bounded/')"

retention_fixture_job 2 "$(printf '%064d' 51)" v2
retention_fixture_check | jq -e '.retained_failed_count==1 and (.failed_job_ids|length)==2' >/dev/null
retention_fixture_retain >/dev/null
second_receipt="$RETENTION_FIXTURE_EVIDENCE/retention/$(jq -r .request_sha256 "$EVIDENCE_ROOT/retained/spot/$RETAIN_JOB_ID.json")/receipt.json"
jq -e --arg controller "$(printf '%064d' 44)" '.request.identity.original_controller_sha256==$controller' "$second_receipt" >/dev/null
retention_fixture_check | jq -e '.retained_failed_count==2 and .invalid_retention_count==0' >/dev/null
printf 'Retain preserves both full old payload identities, physical failures and original bytes; exact repeat is read-only\n'

retention_fixture_job 3 "$RETENTION_FIXTURE_PAYLOAD"
rejected current-payload retention_fixture_retain
retention_fixture_job 4
saved_result=$RETAIN_RESULT_SHA256
RETAIN_RESULT_SHA256=$(printf '%064d' 99)
rejected wrong-result-digest retention_fixture_retain
RETAIN_RESULT_SHA256=$saved_result
mkdir "$RETENTION_FIXTURE_EVIDENCE/attempts"
rejected adopted-attempt retention_fixture_retain
retention_fixture_job 5
mv "$RETENTION_FIXTURE_JOB_DIR" "${RETENTION_FIXTURE_JOB_DIR%.failed}.stale"
rejected stale-state retention_fixture_retain
retention_fixture_job 6
wrong_market() { MARKET=usdm; retention_fixture_retain; }
rejected other-market wrong_market
retention_fixture_job 7
printf 'changed environment\n' >>"$RETENTION_FIXTURE_JOB_DIR/recovery.env"
rejected environment-drift retention_fixture_retain
retention_fixture_job 8
jq '.schema="unrecognized"' "$RETENTION_FIXTURE_JOB_DIR/job.json" >"$fixture/changed.json"
mv "$fixture/changed.json" "$RETENTION_FIXTURE_JOB_DIR/job.json"
RETAIN_JOB_SHA256=$(sha256sum "$RETENTION_FIXTURE_JOB_DIR/job.json" | awk '{print $1}')
rejected malformed-original-schema retention_fixture_retain
retention_fixture_job 9
jq '.completed_at="invalid"' "$RETENTION_FIXTURE_EVIDENCE/result.json" >"$fixture/changed.json"
mv "$fixture/changed.json" "$RETENTION_FIXTURE_EVIDENCE/result.json"
RETAIN_RESULT_SHA256=$(sha256sum "$RETENTION_FIXTURE_EVIDENCE/result.json" | awk '{print $1}')
rejected malformed-terminal-result retention_fixture_retain
retention_fixture_job 10
rm "$RETENTION_FIXTURE_JOB_DIR/.binance-lob-archiver.lock"
rejected absent-existing-spool-lock retention_fixture_retain
retention_fixture_job 11 "$(printf '%064d' 51)" v2
jq --arg sha "$(printf '%064d' 99)" '.executing_controller_sha256=$sha' "$RETENTION_FIXTURE_EVIDENCE/result.json" >"$fixture/changed.json"
mv "$fixture/changed.json" "$RETENTION_FIXTURE_EVIDENCE/result.json"
RETAIN_RESULT_SHA256=$(sha256sum "$RETENTION_FIXTURE_EVIDENCE/result.json" | awk '{print $1}')
rejected v2-executor-mismatch retention_fixture_retain
retention_fixture_job 12 "$(printf '%064d' 51)" v2
jq --arg sha "$(printf '%064d' 99)" '.job_receipt_sha256=$sha' "$RETENTION_FIXTURE_EVIDENCE/result.json" >"$fixture/changed.json"
mv "$fixture/changed.json" "$RETENTION_FIXTURE_EVIDENCE/result.json"
RETAIN_RESULT_SHA256=$(sha256sum "$RETENTION_FIXTURE_EVIDENCE/result.json" | awk '{print $1}')
rejected v2-job-receipt-mismatch retention_fixture_retain
number=13
for field in executing_deployment_bundle_sha256 executing_deployment_source_revision; do
  for invalid in missing wrong; do
    retention_fixture_job "$number" "$(printf '%064d' 51)" v2
    jq --arg field "$field" --arg invalid "$invalid" \
      'if $invalid=="missing" then del(.[$field]) else .[$field]="wrong-identity" end' \
      "$RETENTION_FIXTURE_EVIDENCE/result.json" >"$fixture/changed.json"
    mv "$fixture/changed.json" "$RETENTION_FIXTURE_EVIDENCE/result.json"
    RETAIN_RESULT_SHA256=$(sha256sum "$RETENTION_FIXTURE_EVIDENCE/result.json" | awk '{print $1}')
    rejected "v2-$field-$invalid" retention_fixture_retain
    number=$((number + 1))
  done
done
printf 'Current payload, stale state, adoption, wrong digest and other markets are refused\n'
(
  root="$fixture/pending-pointer-only"; mkdir "$root"; setup_retention_fixture "$root"
  retention_fixture_job 17
  mkdir -p "$EVIDENCE_ROOT/retained/spot"
  printf '{}\n' >"$EVIDENCE_ROOT/retained/spot/$RETAIN_JOB_ID.json.pending"
  assert_resume_guard
)

# Kill the real writer immediately before/after each no-clobber rename. Both
# states must be recoverable by the exact request, including a pending final
# pointer and a renamed pointer whose directory fsync was interrupted.
for publication in intent inventory request receipt pointer; do
  for edge in before after; do
    (
      root="$fixture/crash-$publication-$edge"; mkdir "$root"; setup_retention_fixture "$root"
      retention_fixture_job 60 "$(printf '%064d' 51)" v2
      original_files >"$root/original.before"
      leaf="$publication.json"; [[ $publication != pointer ]] || leaf="$RETAIN_JOB_ID.json"
      publication_root="$RETENTION_FIXTURE_EVIDENCE/retention"
      [[ $publication != pointer ]] || publication_root="$EVIDENCE_ROOT/retained/spot"
      mv() {
        local destination=${*: -1}
        if [[ $1 == -nT && ${destination##*/} == "$leaf" ]]; then
          if [[ $edge == before ]]; then kill -KILL "$BASHPID"; fi
          command mv "$@"
          if [[ $edge == after ]]; then kill -KILL "$BASHPID"; fi
        else command mv "$@"; fi
      }
      rejected "interrupted-$publication-$edge" retention_fixture_retain
      [[ $REJECTED_STATUS == 137 ]]
      unset -f mv
      if [[ $edge == before ]]; then expected_leaf="$leaf.pending"; else expected_leaf=$leaf; fi
      published=$(find "$publication_root" -name "$expected_leaf" -type f -print)
      [[ -n $published && $(stat -c %h "$published") == 1 ]]
      published_sha=$(sha256sum "$published" | awk '{print $1}')
      assert_resume_guard
      # A process crash must not leave the old implementation's two-link alias.
      [[ -z $(find "$EVIDENCE_ROOT" -type f -links +1 -print) ]]
      retention_fixture_retain >"$root/resumed.json"
      retention_fixture_check | jq -e '.retained_failed_count==1 and .invalid_retention_count==0' >/dev/null
      published=$(find "$publication_root" -name "$leaf" -type f -print)
      [[ $(sha256sum "$published" | awk '{print $1}') == "$published_sha" ]]
      [[ -z $(find "$EVIDENCE_ROOT" -name '*.pending' -o -name '*.tmp.*') ]]
      original_files >"$root/original.after"
      cmp "$root/original.before" "$root/original.after"
      printf 'Recovered %s publication, killed %s rename; exact bytes preserved\n' "$publication" "$edge"
    )
  done
done
printf 'All five metadata publications survive real process kills before and after atomic rename\n'

# Unknown aliases/pending content are never reclaimed. A real mv -n collision
# returns success without moving; the writer must detect that and preserve both
# the foreign destination and its own pending bytes for inspection.
(
  root="$fixture/publication-conflicts"; mkdir "$root"; setup_retention_fixture "$root"
  RETENTION_DEADLINE=$((SECONDS + 900))
  mkdir -m 0750 "$root/records"
  printf '{}\n' >"$root/records/foreign.json"; chmod 0440 "$root/records/foreign.json"
  rejected unknown-destination retention_write_json "$root/records/foreign.json" '{"owned":true}'
  [[ $(cat "$root/records/foreign.json") == '{}' ]]
  printf '{}\n' >"$root/records/pending.json.pending"; chmod 0440 "$root/records/pending.json.pending"
  rejected unknown-pending retention_write_json "$root/records/pending.json" '{"owned":true}'
  [[ $(cat "$root/records/pending.json.pending") == '{}' && ! -e $root/records/pending.json ]]
  printf '{}\n' >"$root/records/linked.json.pending"; chmod 0440 "$root/records/linked.json.pending"
  ln "$root/records/linked.json.pending" "$root/unknown-alias"
  rejected unknown-alias retention_write_json "$root/records/linked.json" '{}'
  [[ -f $root/unknown-alias && -f $root/records/linked.json.pending && ! -e $root/records/linked.json ]]
  [[ $(stat -c %h "$root/unknown-alias") == 2 ]]
  mv() {
    local destination=${*: -1}
    printf '{"foreign":true}\n' >"$destination"; chmod 0440 "$destination"
    command mv "$@"
  }
  rejected no-clobber-no-op retention_write_json "$root/records/race.json" '{}'
  [[ $(cat "$root/records/race.json") == '{"foreign":true}' && $(cat "$root/records/race.json.pending") == '{}' ]]
)
printf 'Foreign destinations, pending files and hard-link aliases remain untouched on rejection\n'

# Each mutation starts from an independently committed record; no test repairs
# metadata to try to re-acknowledge evidence that has changed.
mutate_evidence() {
  local kind=$1 path="$RETENTION_FIXTURE_SEGMENT_DIR/part-one.jsonl.part" old_time
  case $kind in
    same-size-restored-mtime)
      old_time=$(stat -c %y "$path")
      printf 'UNFINISHED-PART\n' >"$path"
      touch -d "$old_time" "$path" ;;
    added) printf 'new\n' >"$RETENTION_FIXTURE_JOB_DIR/new.part" ;;
    missing) rm "$path" ;;
    renamed) mv "$path" "$path.renamed" ;;
    replaced) cp -p "$path" "$fixture/replacement"; mv "$fixture/replacement" "$path" ;;
    hardlink) ln "$path" "$fixture/hardlink" ;;
    symlink) rm "$path"; ln -s /etc/hostname "$path" ;;
    mode) chmod g+w "$path" ;;
    result) printf '{}\n' >"$RETENTION_FIXTURE_EVIDENCE/result.json" ;;
    status) printf '{}\n' >"$RETENTION_FIXTURE_JOB_DIR/upload-status.json" ;;
    receipt)
      local receipt
      receipt="$RETENTION_FIXTURE_EVIDENCE/retention/$(jq -r .request_sha256 "$EVIDENCE_ROOT/retained/spot/$RETAIN_JOB_ID.json")/receipt.json"
      chmod u+w "$receipt"; printf '{}\n' >"$receipt"; chmod u-w "$receipt" ;;
    record-extra) printf '{}\n' >"$RETENTION_FIXTURE_EVIDENCE/retention/$(jq -r .request_sha256 "$EVIDENCE_ROOT/retained/spot/$RETAIN_JOB_ID.json")/extra.json" ;;
    sibling-record) mkdir "$RETENTION_FIXTURE_EVIDENCE/retention/unreviewed" ;;
    pointer-missing) rm "$EVIDENCE_ROOT/retained/spot/$RETAIN_JOB_ID.json" ;;
    queue-missing) mv "$RETENTION_FIXTURE_JOB_DIR" "$fixture/removed-job" ;;
    queue-and-index-missing)
      mv "$RETENTION_FIXTURE_JOB_DIR" "$fixture/removed-job"
      rm -rf "$EVIDENCE_ROOT/retained" ;;
  esac
}
number=20
for mutation in same-size-restored-mtime added missing renamed replaced hardlink symlink mode result status receipt record-extra sibling-record pointer-missing queue-missing queue-and-index-missing; do
  # Keep each reader batch small and independent of previously invalid records.
  (
    root="$fixture/case-$mutation"; mkdir "$root"; setup_retention_fixture "$root"
    retention_fixture_job "$number"; retention_fixture_retain >/dev/null
    mutate_evidence "$mutation"
    retention_fixture_check | jq -e '.retained_failed_count==0 and .invalid_retention_count==1' >/dev/null
    if [[ $mutation == pointer-missing ]]; then assert_resume_guard; fi
  )
  number=$((number + 1))
done
printf 'Payload, metadata, membership, link, result, status and retained receipt drift all remain actionable\n'

# A FIFO handshake stops precisely inside the full payload hash. A different
# open file description must acquire the market lock while global+spool remain
# held. No sleep or mocked flock substitutes for the actual lock ownership.
lock_case() (
  local blocked=$1 root="$fixture/lock-$1" worker code
  mkdir "$root"; setup_retention_fixture "$root"; retention_fixture_job 70
  local QUEUE_LOCK="$LOCK_ROOT/monday-rust-lob-recovery-queue-spot.lock"
  local DRAIN_LOCK="$LOCK_ROOT/monday-rust-lob-recovery-drain.lock"
  mkfifo "$root/hash-entered" "$root/hash-release" "$root/done"
  exec 20<>"$root/hash-entered" 21<>"$root/hash-release" 22<>"$root/done"
  retention_bounded() {
    if [[ $1 == sha256sum && ${*: -1} == "$RETENTION_FIXTURE_SEGMENT_DIR/part-one.jsonl.part" ]]; then
      printf 'entered\n' >&20
      read -r -t 20 -u 21 _ || return 1
    fi
    fixture_real_bounded "$@"
  }
  (
    set +e
    (set -e; retention_fixture_retain) >"$root/result.json" 2>"$root/result.err"
    code=$?; printf '%s\n' "$code" >&22
  ) & worker=$!
  read -r -t 20 -u 20 _ || { kill "$worker"; exit 1; }
  flock -n "$QUEUE_LOCK" true
  if flock -n "$DRAIN_LOCK" true; then printf 'global lock was not held\n' >&2; exit 1; fi
  if flock -n "$RETENTION_FIXTURE_JOB_DIR/.binance-lob-archiver.lock" true; then printf 'spool lock was not held\n' >&2; exit 1; fi
  if [[ $blocked == yes ]]; then exec 23<>"$QUEUE_LOCK"; flock -n 23; fi
  printf 'release\n' >&21
  read -r -t 20 -u 22 code || { kill "$worker"; exit 1; }
  wait "$worker"
  if [[ $blocked == yes ]]; then
    [[ $code != 0 && ! -e $EVIDENCE_ROOT/retained/spot/$RETAIN_JOB_ID.json ]]
    grep -Fq 'market queue is busy; retention remains uncommitted' "$root/result.err"
    flock -u 23; exec 23>&-
    # The exact unfinished request can complete after contention is removed.
    eval "$(declare -f fixture_real_bounded | sed '1s/fixture_real_bounded/retention_bounded/')"
    retention_fixture_retain >/dev/null
  else [[ $code == 0 ]]; fi
  retention_fixture_check | jq -e '.retained_failed_count==1 and .invalid_retention_count==0' >/dev/null
)
lock_case no
lock_case yes
printf 'Real global/spool ownership, released market during hashing, nonblocking commit and exact retry passed\n'

# The real incident has 23 independent jobs. Validate the complete batch under
# the same reader deadline, with all physical failures still counted.
(
  root="$fixture/batch"; mkdir "$root"; setup_retention_fixture "$root"
  for number in $(seq 1 23); do
    retention_fixture_job "$number" "$(printf '%064d' 41)" v2
    # Real host inventory has 12-24 files/job and an oldest job with roughly
    # 108 unique directories. Cover both metadata fan-out and empty partitions.
    for metadata in $(seq 1 8); do printf '{}\n' >"$RETENTION_FIXTURE_EVIDENCE/recovery-input/validation-$metadata.json"; done
    if [[ $number == 1 ]]; then
      for empty in $(seq 0 99); do
        printf -v partition 'date=2026-08-%02d/hour=%02d' "$((empty / 24 + 1))" "$((empty % 24))"
        mkdir -p "$RETENTION_FIXTURE_JOB_DIR/$partition"
      done
    fi
    retention_fixture_retain >/dev/null
  done
  started=$SECONDS
  retention_fixture_check >"$root/batch.json"
  jq -e '.retained_failed_count==23 and (.failed_job_ids|length)==23 and .invalid_retention_count==0' "$root/batch.json" >/dev/null
  printf '23-job metadata-only reader completed in %ss (25s deadline)\n' "$((SECONDS - started))"
  retention_fixture_job 24
  retention_fixture_check | jq -e '.retained_failed_count==23 and (.failed_job_ids|length)==24 and .invalid_retention_count==0' >/dev/null
)
