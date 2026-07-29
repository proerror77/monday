#!/usr/bin/env bash
set -euo pipefail

umask 027
export LC_ALL=C
export TZ=UTC

readonly UNIT_TEMPLATE=polymarket-raw-ops-gate@.service
readonly UNIT_PREFIX=polymarket-raw-ops-gate
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd -P)
readonly SCRIPT_DIR
readonly CONTROL="$SCRIPT_DIR/${0##*/}"
readonly GATE="$SCRIPT_DIR/polymarket-raw-ops-shadow-gate.sh"
readonly UNIT_ASSET="$SCRIPT_DIR/$UNIT_TEMPLATE"

die() {
  printf 'Polymarket Gate control failed: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    "Usage: ${0##*/} install" \
    "Usage: ${0##*/} start <candidate-binary> <sha256> <source-revision>" \
    "       ${0##*/} status <sha256> <systemd-invocation-id>" \
    "       ${0##*/} cancel <sha256> <systemd-invocation-id>"
}

test_mode=false
root_prefix=
if [[ ${MONDAY_ALLOW_POLYMARKET_GATE_CONTROL_TEST_MODE:-0} == 1 ]]; then
  [[ -n ${MONDAY_POLYMARKET_GATE_CONTROL_TEST_ROOT:-} ]] \
    || die 'test mode requires MONDAY_POLYMARKET_GATE_CONTROL_TEST_ROOT'
  root_prefix=$(cd -- "$MONDAY_POLYMARKET_GATE_CONTROL_TEST_ROOT" && pwd -P)
  [[ $root_prefix == /* && $root_prefix != / ]] || die 'invalid test root'
  test_mode=true
else
  [[ -z ${MONDAY_POLYMARKET_GATE_CONTROL_TEST_ROOT+x} ]] \
    || die 'test root requires explicit test mode'
  [[ ${EUID} -eq 0 ]] || die 'must run as root'
fi

prefix_path() { printf '%s%s\n' "$root_prefix" "$1"; }

RUN_ROOT=$(prefix_path /run/monday/polymarket-raw-ops-gates)
readonly RUN_ROOT
RECEIPT_ROOT=$(prefix_path /data/monday/evidence/polymarket-gate-jobs)
readonly RECEIPT_ROOT
GATE_EVIDENCE_ROOT=$(prefix_path /data/monday/evidence/polymarket-shadow-gates)
readonly GATE_EVIDENCE_ROOT
INSTALLED_UNIT=$(prefix_path "/etc/systemd/system/$UNIT_TEMPLATE")
readonly INSTALLED_UNIT
SYSTEMD_UNIT_DIR=$(prefix_path /etc/systemd/system)
readonly SYSTEMD_UNIT_DIR
readonly CONTROL_LOCK="$RUN_ROOT/control.lock"

for command in awk chmod cmp date flock install jq mkdir mv rm sha256sum stat \
  sync systemctl tr; do
  command -v "$command" >/dev/null 2>&1 \
    || die "missing required command: $command"
done

direct_directory() {
  [[ -d $1 && ! -L $1 && $(cd -- "$1" && pwd -P) == "$1" ]]
}

secure_root_directory() {
  local path=$1 owner mode
  direct_directory "$path" || return 1
  [[ $test_mode == true ]] && return 0
  owner=$(stat -c %u -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == 0 ]] && (( (8#$mode & 0022) == 0 ))
}

secure_control_file() {
  local path=$1 owner mode
  [[ -f $path && ! -L $path ]] || die "missing direct control file: $path"
  [[ $test_mode == true ]] && return 0
  owner=$(stat -c %u -- "$path") || die "cannot read owner: $path"
  mode=$(stat -c %a -- "$path") || die "cannot read mode: $path"
  [[ $owner == 0 ]] || die "control file is not root-owned: $path"
  (( (8#$mode & 0022) == 0 )) \
    || die "control file is group/world writable: $path"
}

valid_candidate() { [[ $1 =~ ^[a-f0-9]{64}$ ]]; }
valid_source() { [[ $1 =~ ^[a-f0-9]{40,64}$ ]]; }
valid_invocation() { [[ $1 =~ ^[a-f0-9]{32}$ ]]; }
unit_for() { printf '%s@%s.service\n' "$UNIT_PREFIX" "$1"; }
runtime_request_for() { printf '%s/%s.request.json\n' "$RUN_ROOT" "$1"; }
invocation_dir_for() { printf '%s/%s/%s\n' "$RECEIPT_ROOT" "$1" "$2"; }
systemctl_value() { systemctl show "$1" --property="$2" --value; }

sync_file() { [[ $test_mode == true ]] || sync "$1"; }
sync_dir() { [[ $test_mode == true ]] || sync -f "$1"; }

install_gate_unit() {
  local temporary unit_sha
  for file in "$CONTROL" "$GATE" "$UNIT_ASSET"; do
    secure_control_file "$file"
  done
  secure_root_directory "$SYSTEMD_UNIT_DIR" \
    || die 'systemd unit directory is indirect or insecure'
  install -d -m 0755 "$RUN_ROOT"
  secure_root_directory "$RUN_ROOT" || die 'Gate runtime directory is insecure'
  exec 9>"$CONTROL_LOCK"
  flock -x 9
  if [[ -e $INSTALLED_UNIT || -L $INSTALLED_UNIT ]]; then
    secure_control_file "$INSTALLED_UNIT"
    cmp -s "$UNIT_ASSET" "$INSTALLED_UNIT" \
      || die 'existing Gate unit differs from the controller bundle'
  else
    temporary="$SYSTEMD_UNIT_DIR/.${UNIT_TEMPLATE}.$$"
    (
      trap 'rm -f -- "$temporary"' EXIT
      install -m 0644 "$UNIT_ASSET" "$temporary"
      secure_control_file "$temporary"
      cmp -s "$UNIT_ASSET" "$temporary" || die 'staged Gate unit differs'
      sync_file "$temporary"
      mv -n "$temporary" "$INSTALLED_UNIT"
      [[ ! -e $temporary ]] \
        || die 'Gate unit target appeared during atomic install'
      secure_control_file "$INSTALLED_UNIT"
      cmp -s "$UNIT_ASSET" "$INSTALLED_UNIT" \
        || die 'installed Gate unit differs from the controller bundle'
      sync_file "$INSTALLED_UNIT"
      sync_dir "$SYSTEMD_UNIT_DIR"
      trap - EXIT
    )
  fi
  systemctl daemon-reload
  unit_sha=$(sha256sum "$INSTALLED_UNIT" | awk '{print $1}')
  jq -cn --arg template "$UNIT_TEMPLATE" \
    --arg fragment "/etc/systemd/system/$UNIT_TEMPLATE" \
    --arg sha "$unit_sha" '
    {unit_template:$template,fragment_path:$fragment,sha256:$sha,
      validated:true}
  '
}

write_runtime_request() {
  local candidate_path=$1 candidate_sha=$2 source_revision=$3
  local destination temporary control_sha gate_sha unit_sha
  destination=$(runtime_request_for "$candidate_sha")
  [[ ! -L $destination ]] || die 'runtime request is a symlink'
  temporary="${destination}.tmp.$$"
  control_sha=$(sha256sum "$CONTROL" | awk '{print $1}')
  gate_sha=$(sha256sum "$GATE" | awk '{print $1}')
  unit_sha=$(sha256sum "$UNIT_ASSET" | awk '{print $1}')
  jq -n --arg candidate "$candidate_sha" --arg candidate_path "$candidate_path" \
    --arg source "$source_revision" \
    --arg control_sha "$control_sha" --arg gate_sha "$gate_sha" \
    --arg unit_sha "$unit_sha" '
    {schema:"monday.polymarket_gate_request.v1",candidate_sha256:$candidate,
      candidate_path:$candidate_path,
      source_revision:$source,control_sha256:$control_sha,
      gate_sha256:$gate_sha,unit_sha256:$unit_sha}
  ' >"$temporary"
  chmod 0444 "$temporary"
  mv "$temporary" "$destination"
  sync_file "$destination"
}

record_invocation() {
  local candidate_sha=$1 invocation=$2 request destination parent temporary
  request=$(runtime_request_for "$candidate_sha")
  secure_control_file "$request"
  parent="$RECEIPT_ROOT/$candidate_sha"
  install -d -m 0750 "$parent"
  secure_root_directory "$parent" || die 'candidate receipt directory is insecure'
  destination=$(invocation_dir_for "$candidate_sha" "$invocation")
  [[ ! -e $destination && ! -L $destination ]] \
    || die 'refusing to reuse a Gate invocation directory'
  mkdir -m 0750 "$destination" || die 'could not create immutable invocation directory'
  temporary="$destination/.request.json.tmp"
  jq --arg invocation "$invocation" \
    '. + {systemd_invocation_id:$invocation}' "$request" >"$temporary"
  chmod 0444 "$temporary"
  mv "$temporary" "$destination/request.json"
  : >"$destination/commit.lock"
  chmod 0600 "$destination/commit.lock"
  rm -f -- "$request"
  sync_file "$destination/request.json"
  sync_dir "$destination"
}

prepare_gate() {
  local candidate_sha=$1 invocation=${INVOCATION_ID:-}
  if ! valid_candidate "$candidate_sha" || ! valid_invocation "$invocation"; then
    die 'systemd preparer has no exact candidate/invocation identity'
  fi
  record_invocation "$candidate_sha" "$invocation"
}

load_invocation_request() {
  local candidate_sha=$1 invocation=$2 directory request
  directory=$(invocation_dir_for "$candidate_sha" "$invocation")
  secure_root_directory "$directory" || die 'unknown or insecure Gate invocation'
  request="$directory/request.json"
  secure_control_file "$request"
  jq -e --arg candidate "$candidate_sha" --arg invocation "$invocation" \
    '.schema == "monday.polymarket_gate_request.v1"
      and .candidate_sha256 == $candidate
      and .systemd_invocation_id == $invocation' "$request" >/dev/null \
    || die 'Gate invocation request identity is invalid'
  printf '%s\n' "$request"
}

print_status() {
  local candidate_sha=$1 expected_invocation=$2 request unit receipt
  local source_revision evidence_dir directory prepared_receipt staged_receipt
  local active_state invocation main_pid fragment drop_ins phase
  request=$(load_invocation_request "$candidate_sha" "$expected_invocation")
  unit=$(unit_for "$candidate_sha")
  directory=${request%/request.json}
  receipt="$directory/receipt.json"
  prepared_receipt="$directory/.receipt.json.tmp"
  staged_receipt="$directory/.receipt.json.ready"
  if [[ -e $receipt || -L $receipt \
    || -e $prepared_receipt || -L $prepared_receipt \
    || -e $staged_receipt || -L $staged_receipt ]]; then
    exec 8>>"$directory/commit.lock"
    flock -x 8
    commit_terminal_receipt "$candidate_sha" "$expected_invocation" "$request"
  fi
  if [[ -e $receipt || -L $receipt ]]; then
    secure_control_file "$receipt"
    jq -e --arg candidate "$candidate_sha" --arg invocation "$expected_invocation" \
      '.candidate_sha256 == $candidate
        and .systemd_invocation_id == $invocation
        and (.terminal_state == "passed" or .terminal_state == "failed"
          or .terminal_state == "cancelled")' "$receipt" >/dev/null \
      || die 'terminal receipt identity is invalid'
    if [[ $(jq -er .terminal_state "$receipt") == passed ]]; then
      source_revision=$(jq -er .source_revision "$request")
      evidence_dir="$GATE_EVIDENCE_ROOT/$candidate_sha/$expected_invocation"
      valid_pass_marker "$evidence_dir" "$candidate_sha" "$source_revision" \
        "$expected_invocation" || die 'passed receipt has no valid pass evidence'
    fi
    jq -c . "$receipt"
    return
  fi
  fragment=$(systemctl_value "$unit" FragmentPath) \
    || die 'cannot read Gate unit fragment'
  drop_ins=$(systemctl_value "$unit" DropInPaths) \
    || die 'cannot read Gate unit drop-ins'
  [[ $fragment == "/etc/systemd/system/$UNIT_TEMPLATE" && -z $drop_ins ]] \
    || die 'Gate unit fragment or drop-ins are not exact'
  invocation=$(systemctl_value "$unit" InvocationID) \
    || die 'cannot read Gate invocation ID'
  [[ $invocation == "$expected_invocation" ]] \
    || die 'current Gate invocation differs from the requested identity'
  active_state=$(systemctl_value "$unit" ActiveState) \
    || die 'cannot read Gate active state'
  main_pid=$(systemctl_value "$unit" MainPID) \
    || die 'cannot read Gate MainPID'
  [[ $main_pid =~ ^[0-9]+$ ]] || die 'Gate MainPID is invalid'
  case "$active_state" in
    activating) phase=admission ;;
    active) phase=running ;;
    deactivating) phase=cleanup ;;
    *) die 'inactive Gate has no terminal receipt' ;;
  esac
  [[ $active_state == deactivating || $main_pid =~ ^[1-9][0-9]*$ ]] \
    || die 'Gate has no managed MainPID'
  jq -cn --arg unit "$unit" --arg candidate "$candidate_sha" \
    --arg source "$(jq -er .source_revision "$request")" \
    --arg invocation "$invocation" --arg phase "$phase" \
    --arg active_state "$active_state" --argjson main_pid "$main_pid" '
    {unit:$unit,candidate_sha256:$candidate,source_revision:$source,
      systemd_invocation_id:$invocation,phase:$phase,terminal_state:null,
      systemd:{active_state:$active_state,main_pid:$main_pid}}
  '
}

run_gate() {
  local candidate_sha=$1 invocation request candidate_path source_revision
  local path field expected_sha invocation_dir
  invocation=${INVOCATION_ID:-}
  if ! valid_candidate "$candidate_sha" || ! valid_invocation "$invocation"; then
    die 'systemd runner has no exact candidate/invocation identity'
  fi
  request=$(load_invocation_request "$candidate_sha" "$invocation")
  candidate_path=$(jq -er .candidate_path "$request")
  source_revision=$(jq -er .source_revision "$request")
  for path in "$CONTROL" "$GATE" "$UNIT_ASSET"; do
    secure_control_file "$path"
    case $path in
      "$CONTROL") field=control_sha256 ;;
      "$GATE") field=gate_sha256 ;;
      *) field=unit_sha256 ;;
    esac
    expected_sha=$(jq -er ".$field" "$request")
    [[ $(sha256sum "$path" | awk '{print $1}') == "$expected_sha" ]] \
      || die "requested control identity changed: ${path##*/}"
  done
  secure_control_file "$INSTALLED_UNIT"
  cmp -s "$UNIT_ASSET" "$INSTALLED_UNIT" \
    || die 'installed Gate unit changed before systemd execution'
  invocation_dir=$(invocation_dir_for "$candidate_sha" "$invocation")
  export MONDAY_POLYMARKET_GATE_INVOCATION_ID="$invocation"
  export MONDAY_POLYMARKET_GATE_INVOCATION_DIR="$invocation_dir"
  exec "$GATE" "$candidate_path" "$candidate_sha" "$source_revision"
}

valid_pass_marker() {
  local directory=$1 candidate_sha=$2 source_revision=$3 invocation=$4
  local marker_name=${5:-PASSED.sha256}
  local marker="$directory/$marker_name" gate_json="$directory/gate.json"
  [[ -f $gate_json && ! -L $gate_json && -f $marker && ! -L $marker ]] \
    || return 1
  [[ $(awk 'NF == 2 && $2 == "gate.json" {count++} END {print count+0}' \
    "$marker") == 1 ]] || return 1
  (
    cd "$directory"
    sha256sum --check --strict "$marker_name" >/dev/null
  ) || return 1
  jq -e --arg candidate "$candidate_sha" --arg source "$source_revision" \
    --arg invocation "$invocation" '
    .candidate_sha256 == $candidate
    and .deployment_source_revision == $source
    and .shadow_run_id == $invocation
    and .production_eligible == true and .passed == true
  ' "$gate_json" >/dev/null
}

validate_terminal_receipt() {
  local receipt=$1 candidate_sha=$2 source_revision=$3 invocation=$4
  secure_control_file "$receipt"
  jq -e --arg candidate "$candidate_sha" --arg source "$source_revision" \
    --arg invocation "$invocation" '
    .schema == "monday.polymarket_gate_receipt.v1"
    and .candidate_sha256 == $candidate
    and .source_revision == $source
    and .systemd_invocation_id == $invocation
    and .phase == "terminal"
    and (.terminal_state == "passed" or .terminal_state == "failed"
      or .terminal_state == "cancelled")
    and (if .terminal_state == "passed" then
      .systemd.result == "success" and .systemd.exit_code == "exited"
      and .systemd.exit_status == "0" and .shadow.containment == "contained"
    else true end)' "$receipt" >/dev/null
}

commit_terminal_receipt() {
  local candidate_sha=$1 invocation=$2 request=$3 directory receipt
  local prepared_receipt staged_receipt
  local commit_tmp source_revision terminal_state evidence_dir marker ready_marker
  local ready_tmp
  directory=${request%/request.json}
  receipt="$directory/receipt.json"
  prepared_receipt="$directory/.receipt.json.tmp"
  staged_receipt="$directory/.receipt.json.ready"
  source_revision=$(jq -er .source_revision "$request")
  evidence_dir="$GATE_EVIDENCE_ROOT/$candidate_sha/$invocation"
  marker="$evidence_dir/PASSED.sha256"
  ready_marker="$evidence_dir/.PASSED.sha256.ready"
  ready_tmp="$evidence_dir/..PASSED.sha256.ready.tmp"

  if [[ -e $prepared_receipt || -L $prepared_receipt ]]; then
    validate_terminal_receipt "$prepared_receipt" "$candidate_sha" \
      "$source_revision" "$invocation"
    if [[ -e $staged_receipt || -L $staged_receipt ]]; then
      cmp -s "$prepared_receipt" "$staged_receipt" \
        || die 'prepared and staged Gate receipts differ'
    elif [[ -e $receipt || -L $receipt ]]; then
      cmp -s "$prepared_receipt" "$receipt" \
        || die 'prepared and published Gate receipts differ'
    else
      mv "$prepared_receipt" "$staged_receipt"
      sync_file "$staged_receipt"
      sync_dir "$directory"
    fi
  fi
  if [[ -e $staged_receipt || -L $staged_receipt ]]; then
    validate_terminal_receipt "$staged_receipt" "$candidate_sha" \
      "$source_revision" "$invocation"
  fi
  if [[ -e $receipt || -L $receipt ]]; then
    validate_terminal_receipt "$receipt" "$candidate_sha" \
      "$source_revision" "$invocation"
    [[ ! -e $staged_receipt && ! -L $staged_receipt ]] \
      || cmp -s "$staged_receipt" "$receipt" \
      || die 'staged and published Gate receipts differ'
  else
    [[ -f $staged_receipt && ! -L $staged_receipt ]] \
      || die 'terminal Gate receipt is missing'
    terminal_state=$(jq -er .terminal_state "$staged_receipt")
    if [[ $terminal_state != passed ]]; then
      rm -f -- "$marker" "$ready_marker" "$ready_tmp"
      [[ ! -d $evidence_dir ]] || sync_dir "$evidence_dir"
    fi
    commit_tmp="$directory/.receipt.json.commit"
    rm -f -- "$commit_tmp"
    install -m 0444 "$staged_receipt" "$commit_tmp"
    sync_file "$commit_tmp"
    mv "$commit_tmp" "$receipt"
    sync_file "$receipt"
    sync_dir "$directory"
  fi

  terminal_state=$(jq -er .terminal_state "$receipt")
  if [[ $terminal_state == passed ]]; then
    if [[ -e $marker || -L $marker ]]; then
      valid_pass_marker "$evidence_dir" "$candidate_sha" "$source_revision" \
        "$invocation" || die 'published Gate pass marker is invalid'
    else
      valid_pass_marker "$evidence_dir" "$candidate_sha" "$source_revision" \
        "$invocation" ".${marker##*/}.ready" \
        || die 'staged Gate pass marker is invalid'
      mv "$ready_marker" "$marker"
      sync_file "$marker"
      sync_dir "$evidence_dir"
    fi
    rm -f -- "$ready_tmp"
  else
    rm -f -- "$marker" "$ready_marker" "$ready_tmp"
    [[ ! -d $evidence_dir ]] || sync_dir "$evidence_dir"
  fi
  if [[ -e $prepared_receipt || -L $prepared_receipt \
    || -e $staged_receipt || -L $staged_receipt ]]; then
    rm -f -- "$prepared_receipt" "$staged_receipt"
    sync_dir "$directory"
  fi
}

finalize_gate() {
  local candidate_sha=$1 invocation=${INVOCATION_ID:-}
  local service_result=${SERVICE_RESULT:-} exit_code=${EXIT_CODE:-}
  local exit_status=${EXIT_STATUS:-} request directory receipt temporary
  local unit source_revision shadow_unit shadow_state shadow_pid evidence_dir
  local ready_marker terminal_state shadow_stop_result shadow_containment
  local staged_receipt shadow_state_ok=false shadow_pid_ok=false
  if ! valid_candidate "$candidate_sha" || ! valid_invocation "$invocation"; then
    die 'systemd finalizer has no exact candidate/invocation identity'
  fi
  [[ -n $service_result && -n $exit_code && -n $exit_status ]] \
    || die 'systemd finalizer has no terminal process result'
  request=$(load_invocation_request "$candidate_sha" "$invocation")
  directory=${request%/request.json}
  receipt="$directory/receipt.json"
  temporary="$directory/.receipt.json.tmp"
  staged_receipt="$directory/.receipt.json.ready"
  exec 8>>"$directory/commit.lock"
  flock -x 8
  if [[ -e $receipt || -L $receipt \
    || -e $temporary || -L $temporary \
    || -e $staged_receipt || -L $staged_receipt ]]; then
    commit_terminal_receipt "$candidate_sha" "$invocation" "$request"
    jq -c . "$receipt"
    return
  fi
  unit=$(unit_for "$candidate_sha")
  source_revision=$(jq -er .source_revision "$request")
  shadow_unit="polymarket-reference-collector-shadow@${candidate_sha}.service"
  shadow_stop_result=success
  systemctl stop "$shadow_unit" >/dev/null 2>&1 \
    || shadow_stop_result=failed
  shadow_state=query-error
  shadow_pid=query-error
  if shadow_state=$(systemctl_value "$shadow_unit" ActiveState); then
    shadow_state_ok=true
  else
    shadow_state=query-error
  fi
  if shadow_pid=$(systemctl_value "$shadow_unit" MainPID); then
    shadow_pid_ok=true
  else
    shadow_pid=query-error
  fi
  if [[ $shadow_state_ok == true && $shadow_pid_ok == true \
    && $shadow_pid =~ ^[0-9]+$ ]]; then
    if [[ ($shadow_state == inactive || $shadow_state == failed) \
      && $shadow_pid == 0 ]]; then
      shadow_containment=contained
    else
      shadow_containment=active
    fi
  else
    shadow_containment=unverified
  fi
  evidence_dir="$GATE_EVIDENCE_ROOT/$candidate_sha/$invocation"
  ready_marker="$evidence_dir/.PASSED.sha256.ready"
  terminal_state=failed
  if [[ $shadow_containment != contained ]]; then
    terminal_state=failed
  elif [[ -e $directory/cancel.requested || -L $directory/cancel.requested ]]; then
    [[ -f $directory/cancel.requested && ! -L $directory/cancel.requested ]] \
      || die 'Gate cancellation intent is indirect or invalid'
    terminal_state=cancelled
  elif [[ $service_result == success && $exit_code == exited \
    && $exit_status == 0 ]] \
    && valid_pass_marker "$evidence_dir" "$candidate_sha" "$source_revision" \
      "$invocation" ".PASSED.sha256.ready"; then
    terminal_state=passed
  fi
  jq -n --arg unit "$unit" --arg candidate "$candidate_sha" \
    --arg source "$source_revision" --arg invocation "$invocation" \
    --arg terminal_state "$terminal_state" --arg result "$service_result" \
    --arg exit_code "$exit_code" --arg exit_status "$exit_status" \
    --arg shadow_unit "$shadow_unit" --arg shadow_stop "$shadow_stop_result" \
    --arg shadow_containment "$shadow_containment" \
    --arg shadow_state "$shadow_state" --arg shadow_pid "$shadow_pid" '
    {schema:"monday.polymarket_gate_receipt.v1",unit:$unit,
      candidate_sha256:$candidate,source_revision:$source,
      systemd_invocation_id:$invocation,phase:"terminal",
      terminal_state:$terminal_state,systemd:{result:$result,
        exit_code:$exit_code,exit_status:$exit_status},
      shadow:{unit:$shadow_unit,stop_result:$shadow_stop,
        containment:$shadow_containment,active_state:$shadow_state,
        main_pid:$shadow_pid}}
  ' >"$temporary"
  chmod 0444 "$temporary"
  sync_file "$temporary"
  mv "$temporary" "$staged_receipt"
  sync_file "$staged_receipt"
  sync_dir "$directory"
  commit_terminal_receipt "$candidate_sha" "$invocation" "$request"
  jq -c . "$receipt"
  [[ $shadow_containment != active ]] \
    || die 'candidate shadow remains active after Gate finalization'
}

cancel_gate() {
  local candidate_sha=$1 invocation=$2 request directory receipt unit
  local current_invocation active_state cancel_tmp source_revision evidence_dir
  if ! valid_candidate "$candidate_sha" || ! valid_invocation "$invocation"; then
    die 'cancel requires exact candidate and invocation IDs'
  fi
  request=$(load_invocation_request "$candidate_sha" "$invocation")
  directory=${request%/request.json}
  receipt="$directory/receipt.json"
  exec 8>>"$directory/commit.lock"
  flock -x 8
  if [[ -e $receipt || -L $receipt ]]; then
    secure_control_file "$receipt"
    [[ $(jq -er .terminal_state "$receipt") == cancelled ]] \
      || die 'cannot cancel a terminal Gate invocation'
    jq -c . "$receipt"
    return
  fi
  unit=$(unit_for "$candidate_sha")
  current_invocation=$(systemctl_value "$unit" InvocationID) \
    || die 'cannot read the current Gate invocation ID'
  [[ $current_invocation == "$invocation" ]] \
    || die 'refusing to cancel a different Gate invocation'
  active_state=$(systemctl_value "$unit" ActiveState) \
    || die 'cannot read the current Gate state'
  [[ $active_state == activating || $active_state == active \
    || $active_state == deactivating ]] \
    || die 'inactive Gate has no terminal receipt'
  source_revision=$(jq -er .source_revision "$request")
  evidence_dir="$GATE_EVIDENCE_ROOT/$candidate_sha/$invocation"
  if valid_pass_marker "$evidence_dir" "$candidate_sha" "$source_revision" \
    "$invocation"; then
    die 'Gate success is already committed; cancellation is no longer valid'
  fi
  if [[ ! -e $directory/cancel.requested && ! -L $directory/cancel.requested ]]; then
    cancel_tmp="$directory/.cancel.requested.tmp"
    jq -n --arg candidate "$candidate_sha" --arg invocation "$invocation" '
      {schema:"monday.polymarket_gate_cancel.v1",
        candidate_sha256:$candidate,systemd_invocation_id:$invocation}
    ' >"$cancel_tmp"
    chmod 0444 "$cancel_tmp"
    mv "$cancel_tmp" "$directory/cancel.requested"
    sync_file "$directory/cancel.requested"
    sync_dir "$directory"
  fi
  [[ -f $directory/cancel.requested && ! -L $directory/cancel.requested ]] \
    || die 'cancellation intent is not a direct file'
  exec 8>&-
  systemctl stop "$unit" || die 'systemd could not stop the Gate invocation'
  [[ -f $receipt && ! -L $receipt ]] \
    || die 'Gate cancellation produced no terminal receipt'
  [[ $(jq -er .terminal_state "$receipt") == cancelled ]] \
    || die 'Gate cancellation did not produce a cancelled receipt'
  jq -c . "$receipt"
}

start_gate() {
  local candidate_path=$1 candidate_sha source_revision=$3 unit invocation env_file
  local active_state main_pid
  candidate_sha=$(printf '%s' "$2" | tr '[:upper:]' '[:lower:]')
  source_revision=$(printf '%s' "$source_revision" | tr '[:upper:]' '[:lower:]')
  valid_candidate "$candidate_sha" || die 'candidate SHA-256 is invalid'
  valid_source "$source_revision" || die 'source revision is invalid'
  [[ $candidate_path == /* && -f $candidate_path && ! -L $candidate_path \
    && -x $candidate_path ]] || die 'candidate must be a direct absolute executable'
  [[ $(sha256sum "$candidate_path" | awk '{print $1}') == "$candidate_sha" ]] \
    || die 'candidate checksum mismatch'
  for file in "$CONTROL" "$GATE" "$UNIT_ASSET" "$INSTALLED_UNIT"; do
    secure_control_file "$file"
  done
  cmp -s "$UNIT_ASSET" "$INSTALLED_UNIT" \
    || die 'installed Gate unit differs from the controller bundle'
  [[ $CONTROL =~ ^/[A-Za-z0-9_./-]+$ ]] \
    || die 'Gate controller path is not safe for systemd EnvironmentFile'
  if [[ $test_mode == false ]]; then
    command -v mountpoint >/dev/null 2>&1 || die 'missing required command: mountpoint'
    mountpoint -q /data || die '/data must be a mount point'
  fi
  install -d -m 0755 "$RUN_ROOT"
  install -d -m 0750 "$RECEIPT_ROOT"
  secure_root_directory "$RUN_ROOT" || die 'Gate runtime directory is insecure'
  secure_root_directory "$RECEIPT_ROOT" || die 'Gate receipt root is insecure'
  exec 9>"$CONTROL_LOCK"
  flock -x 9
  unit=$(unit_for "$candidate_sha")
  active_state=$(systemctl_value "$unit" ActiveState) \
    || die 'cannot read Gate state before start'
  [[ $active_state == inactive || $active_state == failed ]] \
    || die 'a Gate job already owns this candidate'
  write_runtime_request "$candidate_path" "$candidate_sha" "$source_revision"
  env_file="$RUN_ROOT/$candidate_sha.env"
  [[ ! -L $env_file ]] || die 'Gate EnvironmentFile is a symlink'
  printf 'MONDAY_POLYMARKET_GATE_CONTROL=%s\n' "$CONTROL" >"${env_file}.tmp.$$"
  chmod 0600 "${env_file}.tmp.$$"
  mv "${env_file}.tmp.$$" "$env_file"
  sync_file "$env_file"
  systemctl daemon-reload
  systemctl start "$unit" || die 'systemd rejected the Gate job'
  if ! invocation=$(systemctl_value "$unit" InvocationID) \
    || ! valid_invocation "$invocation"; then
    systemctl stop "$unit" >/dev/null \
      || die 'cannot read the started Gate invocation ID; containment failed'
    active_state=$(systemctl_value "$unit" ActiveState) \
      || die 'cannot read the started Gate invocation ID; containment is unverified'
    main_pid=$(systemctl_value "$unit" MainPID) \
      || die 'cannot read the started Gate invocation ID; containment PID is unverified'
    [[ ($active_state == inactive || $active_state == failed) \
      && $main_pid == 0 ]] \
      || die 'cannot read the started Gate invocation ID; Gate remains active'
    die 'cannot read a valid started Gate invocation ID; Gate was contained'
  fi
  print_status "$candidate_sha" "$invocation"
}

action=${1:-}
case "$action" in
  install)
    [[ $# -eq 1 ]] || { usage >&2; exit 2; }
    install_gate_unit
    ;;
  start)
    [[ $# -eq 4 ]] || { usage >&2; exit 2; }
    start_gate "$2" "$3" "$4"
    ;;
  status)
    [[ $# -eq 3 ]] || { usage >&2; exit 2; }
    if ! valid_candidate "$2" || ! valid_invocation "$3"; then
      die 'status requires exact candidate and invocation IDs'
    fi
    print_status "$2" "$3"
    ;;
  cancel)
    [[ $# -eq 3 ]] || { usage >&2; exit 2; }
    cancel_gate "$2" "$3"
    ;;
  run)
    [[ $# -eq 2 ]] || { usage >&2; exit 2; }
    run_gate "$2"
    ;;
  prepare)
    [[ $# -eq 2 ]] || { usage >&2; exit 2; }
    prepare_gate "$2"
    ;;
  finalize)
    [[ $# -eq 2 ]] || { usage >&2; exit 2; }
    finalize_gate "$2"
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
