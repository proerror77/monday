#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd -P)
readonly SCRIPT_DIR
ARTIFACT_HELPER_DIR=$(cd -- "$SCRIPT_DIR/../../.github/scripts" && pwd -P)
readonly ARTIFACT_HELPER_DIR
readonly ARTIFACT_HELPER="$ARTIFACT_HELPER_DIR/polymarket-market-recorder-release-artifact.sh"
readonly UNIT_NAME=polymarket-market-tape.service
readonly UNIT_TEMPLATE="$SCRIPT_DIR/$UNIT_NAME"
readonly CONFIG_TEMPLATE="$SCRIPT_DIR/polymarket-market-tape.toml"
readonly UNIT_BINARY_MARKER=/opt/monday/releases/polymarket-market-recorder/@POLYMARKET_MARKET_RECORDER_SHA256@/new-ploy-runner
readonly RELEASE_SUBDIR=/opt/monday/releases/polymarket-market-recorder
readonly UNIT_SUBPATH="/etc/systemd/system/$UNIT_NAME"
readonly CONFIG_SUBPATH=/etc/monday/polymarket-market-tape.toml
readonly OUTPUT_SUBDIR=/data/monday/spool/polymarket
readonly LOCK_SUBPATH=/run/lock/monday-polymarket-market-recorder-deploy.lock
readonly STATE_SUBPATH=/opt/monday/state/polymarket-market-recorder-deploy.json
readonly EXPECTED_ARGS='--config /etc/monday/polymarket-market-tape.toml --deployment-id polymarket-market-data-ecs --dry-run'
readonly MAX_OUTPUT_AGE_SECONDS=120

test_root=${MONDAY_MARKET_RECORDER_DEPLOY_TEST_ROOT:-}
expected_uid=0
verify_user=hftcollector
if [[ -n $test_root ]]; then
  [[ ${MONDAY_MARKET_RECORDER_DEPLOY_TEST_MODE:-0} == 1 && $test_root == /* \
    && $test_root != / && -d $test_root ]] \
    || { printf 'invalid market-recorder deploy test root\n' >&2; exit 2; }
  test_root=$(cd -- "$test_root" && pwd -P)
  expected_uid=$(id -u)
  verify_user=
else
  PATH=/usr/sbin:/usr/bin:/sbin:/bin
fi
readonly test_root expected_uid verify_user PATH
readonly RELEASE_ROOT="$test_root$RELEASE_SUBDIR"
readonly UNIT_PATH="$test_root$UNIT_SUBPATH"
readonly CONFIG_PATH="$test_root$CONFIG_SUBPATH"
readonly PROC_ROOT="$test_root/proc"
readonly OUTPUT_DIR="$test_root$OUTPUT_SUBDIR"
readonly LOCK_PATH="$test_root$LOCK_SUBPATH"
readonly STATE_PATH="$test_root$STATE_SUBPATH"
readonly STATE_DIR=${STATE_PATH%/*}
verify_attempts=24
verify_sleep_seconds=5
if [[ -n $test_root ]]; then
  verify_attempts=${MONDAY_MARKET_RECORDER_VERIFY_ATTEMPTS:-1}
  verify_sleep_seconds=${MONDAY_MARKET_RECORDER_VERIFY_SLEEP_SECONDS:-0}
fi
[[ $verify_attempts =~ ^[1-9][0-9]*$ && $verify_sleep_seconds =~ ^[0-9]+$ ]] \
  || { printf 'invalid runtime verification bounds\n' >&2; exit 2; }
readonly verify_attempts verify_sleep_seconds

candidate_extract=
rendered_unit=
release_staging=
cleanup() {
  [[ -z $candidate_extract ]] || { chmod u+w "$candidate_extract"; rm -rf -- "$candidate_extract"; }
  [[ -z $rendered_unit ]] || rm -f -- "$rendered_unit"
  [[ -z $release_staging ]] || rm -rf -- "$release_staging"
}
trap cleanup EXIT
die() { printf 'market-recorder deploy: %s\n' "$*" >&2; exit 1; }
usage() {
  printf '%s\n' 'Usage: polymarket-market-recorder-deploy.sh COMMAND [arguments]' \
    'Commands: preflight|dry-run|install ARTIFACT SOURCE IMAGE; verify; rollback' >&2
  exit 2
}
stat_value() {
  local gnu_format=$1 bsd_format=$2 file=$3
  if stat -c "$gnu_format" "$file" >/dev/null 2>&1; then
    stat -c "$gnu_format" "$file"
  else
    stat -f "$bsd_format" "$file"
  fi
}
stat_uid() { stat_value %u %u "$1"; }
stat_mode() { stat_value %a %Lp "$1"; }
stat_mtime() { stat_value %Y %m "$1"; }
secure_regular_file() {
  local file=$1 expected_mode=${2:-}
  [[ -f $file && ! -L $file ]] || return 1
  [[ $(stat_uid "$file") == "$expected_uid" ]] || return 1
  if [[ -n $expected_mode ]]; then
    [[ $(stat_mode "$file") == "$expected_mode" ]] || return 1
  else
    (( (8#$(stat_mode "$file") & 8#022) == 0 )) || return 1
  fi
}
secure_directory_chain() {
  local directory=$1 uid mode
  while :; do
    [[ -d $directory && ! -L $directory ]] || return 1
    uid=$(stat_uid "$directory") || return 1
    [[ $uid == 0 || (-n $test_root && $uid == "$expected_uid") ]] || return 1
    mode=$(stat_mode "$directory") || return 1
    (( (8#$mode & 8#022) == 0 )) || return 1
    [[ $directory == / ]] && return 0
    directory=${directory%/*}; [[ -n $directory ]] || directory=/
  done
}
runner_version() {
  [[ -z $verify_user ]] && { "$1" --version; return; }
  runuser -u "$verify_user" -- "$1" --version
}
systemctl_value() { systemctl show --property="$2" --value "$1"; }
effective_exec_argv() {
  local raw argv
  raw=$(systemctl_value "$1" ExecStart) || return 1
  argv=$(sed -nE 's/^.*argv\[\]=([^;]+);.*$/\1/p' <<<"$raw" | sed -E 's/[[:space:]]+$//')
  [[ -n $argv ]] || return 1
  printf '%s\n' "$argv"
}
render_unit() {
  local binary=$1 destination=$2
  [[ $(grep -Fc "$UNIT_BINARY_MARKER" "$UNIT_TEMPLATE") == 1 ]] || return 1
  sed "s|$UNIT_BINARY_MARKER|$binary|" "$UNIT_TEMPLATE" >"$destination"
}
verify_unit() {
  local binary=$1 fragment drop_ins exec_argv
  secure_regular_file "$UNIT_PATH" 644 || return 1
  fragment=$(systemctl_value "$UNIT_NAME" FragmentPath) || return 1
  [[ $fragment == "$UNIT_PATH" ]] || return 1
  drop_ins=$(systemctl_value "$UNIT_NAME" DropInPaths) || return 1
  [[ -z $drop_ins ]] || return 1
  exec_argv=$(effective_exec_argv "$UNIT_NAME") || return 1
  [[ $exec_argv == "$binary $EXPECTED_ARGS" ]] || return 1
  rendered_unit=$(mktemp "${TMPDIR:-/tmp}/polymarket-market-tape.service.XXXXXX")
  render_unit "$binary" "$rendered_unit" || return 1
  cmp -s "$rendered_unit" "$UNIT_PATH" || return 1
  rm -f -- "$rendered_unit"
  rendered_unit=
}
verify_config() {
  secure_regular_file "$CONFIG_PATH" 644 || return 1
  cmp -s "$CONFIG_TEMPLATE" "$CONFIG_PATH"
}
verify_fresh_output() {
  local not_before=$1 file mtime latest=0 now
  [[ -d $OUTPUT_DIR && ! -L $OUTPUT_DIR ]] || return 1
  while IFS= read -r file; do
    [[ -s $file && ! -L $file ]] || continue
    mtime=$(stat_mtime "$file") || return 1
    ((mtime > latest)) && latest=$mtime
  done < <(find "$OUTPUT_DIR" -maxdepth 1 -type f \
    -name 'market-updates*.ndjson' -print)
  now=$(date +%s)
  ((latest > 0 && latest >= not_before && latest <= now))
}
capture_runtime() {
  local configured_exec actual_exe
  systemctl is-active --quiet "$UNIT_NAME" || return 1
  configured_exec=$(effective_exec_argv "$UNIT_NAME") || return 1
  runtime_binary=${configured_exec%% *}
  [[ $configured_exec == "$runtime_binary $EXPECTED_ARGS" ]] || return 1
  verify_unit "$runtime_binary" || return 1
  secure_regular_file "$runtime_binary" || return 1
  runtime_pid=$(systemctl_value "$UNIT_NAME" MainPID) || return 1
  [[ $runtime_pid =~ ^[1-9][0-9]*$ ]] || return 1
  runtime_invocation=$(systemctl_value "$UNIT_NAME" InvocationID) || return 1
  [[ $runtime_invocation =~ ^[0-9a-f]{32}$ ]] || return 1
  runtime_restarts=$(systemctl_value "$UNIT_NAME" NRestarts) || return 1
  [[ $runtime_restarts =~ ^[0-9]+$ ]] || return 1
  actual_exe=$(readlink -f -- "$PROC_ROOT/$runtime_pid/exe") || return 1
  [[ $actual_exe == "$(readlink -f -- "$runtime_binary")" ]] || return 1
  runtime_sha=$(sha256sum "$PROC_ROOT/$runtime_pid/exe" | awk '{print $1}') || return 1
  [[ $runtime_sha =~ ^[0-9a-f]{64}$ ]] || return 1
  runtime_source=$(runner_version "$PROC_ROOT/$runtime_pid/exe") || return 1
  runtime_source=${runtime_source#new-ploy-runner }
  [[ $runtime_source =~ ^[0-9a-f]{40}$ ]] || return 1
  [[ $(systemctl_value "$UNIT_NAME" MainPID) == "$runtime_pid" \
    && $(systemctl_value "$UNIT_NAME" InvocationID) == "$runtime_invocation" \
    && $(systemctl_value "$UNIT_NAME" NRestarts) == "$runtime_restarts" ]] || return 1
  [[ $(readlink -f -- "$PROC_ROOT/$runtime_pid/exe") == "$actual_exe" \
    && $(sha256sum "$PROC_ROOT/$runtime_pid/exe" | awk '{print $1}') == "$runtime_sha" ]]
}
verify_installed_release() {
  local sha=$1 source_revision=$2 expected_image=${3:-}
  local directory="$RELEASE_ROOT/$sha" binary="$RELEASE_ROOT/$sha/new-ploy-runner"
  local image
  [[ $sha =~ ^[0-9a-f]{64}$ && $source_revision =~ ^[0-9a-f]{40}$ ]] || return 1
  [[ -d $directory && ! -L $directory \
    && $(stat_uid "$directory") == "$expected_uid" \
    && $(stat_mode "$directory") == 555 ]] || return 1
  secure_regular_file "$binary" 555 || return 1
  [[ $(sha256sum "$binary" | awk '{print $1}') == "$sha" ]] || return 1
  if [[ -e $directory/polymarket-market-recorder-release.json \
    || -L $directory/polymarket-market-recorder-release.json ]]; then
    secure_regular_file "$directory/new-ploy-runner.sha256" 444 || return 1
    secure_regular_file "$directory/polymarket-market-recorder-release.json" 444 || return 1
    secure_regular_file "$directory/polymarket-market-recorder-release.json.sha256" 444 \
      || return 1
    image=$(jq -er .image_digest \
      "$directory/polymarket-market-recorder-release.json") || return 1
    [[ -z $expected_image || $image == "$expected_image" ]] || return 1
    "$ARTIFACT_HELPER" verify \
      "$directory" "$source_revision" "$image" "$verify_user" || return 1
  else
    [[ -z $expected_image ]] || return 1
    [[ $(runner_version "$binary") == "new-ploy-runner $source_revision" ]] || return 1
    secure_regular_file "$directory/new-ploy-runner.sha256" 444 || return 1
    secure_regular_file "$directory/source-revision.txt" 444 || return 1
    [[ $(<"$directory/source-revision.txt") == "$source_revision" ]] || return 1
    [[ $(<"$directory/new-ploy-runner.sha256") == "$sha  new-ploy-runner" ]] \
      || return 1
    [[ $(find "$directory" -mindepth 1 -maxdepth 1 -print | wc -l) -eq 3 ]] \
      || return 1
  fi
}
capture_contained_inactive_runtime() {
  local configured_exec enabled source_readback
  [[ $(systemctl_value "$UNIT_NAME" ActiveState) == inactive ]] || return 1
  [[ $(systemctl_value "$UNIT_NAME" SubState) == dead ]] || return 1
  runtime_pid=$(systemctl_value "$UNIT_NAME" MainPID) || return 1
  [[ $runtime_pid == 0 ]] || return 1
  enabled=$(systemctl is-enabled "$UNIT_NAME") || return 1
  [[ $enabled == enabled ]] || return 1
  configured_exec=$(effective_exec_argv "$UNIT_NAME") || return 1
  runtime_binary=${configured_exec%% *}
  [[ $configured_exec == "$runtime_binary $EXPECTED_ARGS" ]] || return 1
  verify_unit "$runtime_binary" || return 1
  verify_config || return 1
  secure_regular_file "$runtime_binary" 555 || return 1
  runtime_sha=$(sha256sum "$runtime_binary" | awk '{print $1}') || return 1
  [[ $runtime_sha =~ ^[0-9a-f]{64}$ ]] || return 1
  runtime_source=$(runner_version "$runtime_binary") || return 1
  runtime_source=${runtime_source#new-ploy-runner }
  [[ $runtime_source =~ ^[0-9a-f]{40}$ ]] || return 1
  [[ $runtime_binary == "$RELEASE_ROOT/$runtime_sha/new-ploy-runner" ]] || return 1
  verify_installed_release "$runtime_sha" "$runtime_source" || return 1
  source_readback=$(runner_version "$runtime_binary") || return 1
  [[ $(systemctl_value "$UNIT_NAME" ActiveState) == inactive \
    && $(systemctl_value "$UNIT_NAME" SubState) == dead \
    && $(systemctl_value "$UNIT_NAME" MainPID) == 0 \
    && $(systemctl is-enabled "$UNIT_NAME") == enabled \
    && $(effective_exec_argv "$UNIT_NAME") == "$configured_exec" \
    && $(sha256sum "$runtime_binary" | awk '{print $1}') == "$runtime_sha" \
    && $source_readback == "new-ploy-runner $runtime_source" ]]
}
publish_runtime_snapshot() {
  local sha=$1 source_revision=$2 proc_exe=$3 destination
  destination="$RELEASE_ROOT/$sha"
  if [[ -e $destination || -L $destination ]]; then
    verify_installed_release "$sha" "$source_revision"
    return
  fi
  release_staging=$(mktemp -d "$RELEASE_ROOT/.${sha}.new.XXXXXX")
  install -m 0555 "$proc_exe" "$release_staging/new-ploy-runner"
  printf '%s  new-ploy-runner\n' "$sha" >"$release_staging/new-ploy-runner.sha256"
  printf '%s\n' "$source_revision" >"$release_staging/source-revision.txt"
  chmod 0444 "$release_staging/new-ploy-runner.sha256" \
    "$release_staging/source-revision.txt"
  chmod 0555 "$release_staging"
  [[ ! -e $destination && ! -L $destination ]] || return 1
  mv "$release_staging" "$destination"
  release_staging=
  verify_installed_release "$sha" "$source_revision"
}
publish_candidate() {
  local sha=$1 source_revision=$2 image_digest=$3 destination
  local file
  destination="$RELEASE_ROOT/$sha"
  if [[ -e $destination || -L $destination ]]; then
    verify_installed_release "$sha" "$source_revision" "$image_digest"
    return
  fi
  release_staging=$(mktemp -d "$RELEASE_ROOT/.${sha}.new.XXXXXX")
  for file in new-ploy-runner new-ploy-runner.sha256 \
    polymarket-market-recorder-release.json \
    polymarket-market-recorder-release.json.sha256; do
    install -m 0444 "$candidate_extract/$file" "$release_staging/$file"
  done
  chmod 0555 "$release_staging/new-ploy-runner"
  chmod 0555 "$release_staging"
  [[ ! -e $destination && ! -L $destination ]] || return 1
  mv "$release_staging" "$destination"
  release_staging=
  verify_installed_release "$sha" "$source_revision" "$image_digest"
}
verify_running_release() {
  local sha=$1 source_revision=$2 not_before=$3 expected_binary
  expected_binary="$RELEASE_ROOT/$sha/new-ploy-runner"
  verify_installed_release "$sha" "$source_revision" || return 1
  capture_runtime || return 1
  [[ $runtime_binary == "$expected_binary" && $runtime_sha == "$sha" \
    && $runtime_source == "$source_revision" && $runtime_restarts == 0 ]] || return 1
  verify_fresh_output "$not_before"
}
install_release_unit() {
  local sha=$1 source_revision=$2
  local binary="$RELEASE_ROOT/$sha/new-ploy-runner"
  verify_installed_release "$sha" "$source_revision" || return 1
  rendered_unit=$(mktemp "${UNIT_PATH}.new.XXXXXX") || return 1
  render_unit "$binary" "$rendered_unit" || return 1
  chmod 0644 "$rendered_unit" || return 1
  mv -f "$rendered_unit" "$UNIT_PATH" || return 1
  rendered_unit=
  systemctl daemon-reload || return 1
}
activate_release() {
  local sha=$1 source_revision=$2 started attempt
  install_release_unit "$sha" "$source_revision" || return 1
  systemctl reset-failed "$UNIT_NAME" || return 1
  [[ $(systemctl_value "$UNIT_NAME" NRestarts) == 0 ]] || return 1
  started=$(date +%s)
  systemctl restart "$UNIT_NAME" || return 1
  for ((attempt = 1; attempt <= verify_attempts; attempt++)); do
    if verify_running_release "$sha" "$source_revision" "$started"; then
      return 0
    fi
    ((attempt == verify_attempts)) || sleep "$verify_sleep_seconds"
  done
  return 1
}
restore_contained_inactive_release() {
  local sha=$1 source_revision=$2
  systemctl stop "$UNIT_NAME" || return 1
  install_release_unit "$sha" "$source_revision" || return 1
  capture_contained_inactive_runtime || return 1
  [[ $runtime_sha == "$sha" && $runtime_source == "$source_revision" ]]
}
restore_baseline_after_failure() {
  local activity=$1 sha=$2 source_revision=$3
  if [[ $activity == active ]]; then
    activate_release "$sha" "$source_revision"
  else
    [[ $activity == inactive ]] || return 1
    restore_contained_inactive_release "$sha" "$source_revision"
  fi
}
load_state() {
  secure_regular_file "$STATE_PATH" 600 || return 1
  jq -e '
    (keys | sort) == ["current", "previous", "schema"]
    and .schema == "monday.polymarket_market_recorder_deploy.v1"
    and all(.current, .previous;
      (keys | sort) == ["sha256", "source_revision"]
      and (.sha256 | test("^[0-9a-f]{64}$"))
      and (.source_revision | test("^[0-9a-f]{40}$")))' "$STATE_PATH" >/dev/null \
    || return 1
  IFS=$'\t' read -r state_current_sha state_current_source \
    state_previous_sha state_previous_source < <(jq -er \
    '[.current.sha256,.current.source_revision,.previous.sha256,.previous.source_revision] | @tsv' "$STATE_PATH")
}
write_state() {
  local current_sha=$1 current_source=$2 previous_sha=$3 previous_source=$4 temporary
  mkdir -p "$STATE_DIR" || return 1
  chmod 0750 "$STATE_DIR" || return 1
  [[ $(stat_uid "$STATE_DIR") == "$expected_uid" ]] || return 1
  temporary=$(mktemp "${STATE_PATH}.new.XXXXXX") || return 1
  jq -S -n --arg current_sha "$current_sha" --arg current_source "$current_source" \
    --arg previous_sha "$previous_sha" --arg previous_source "$previous_source" '
    {schema:"monday.polymarket_market_recorder_deploy.v1",
      current:{sha256:$current_sha,source_revision:$current_source},
      previous:{sha256:$previous_sha,source_revision:$previous_source}}' >"$temporary"
  chmod 0600 "$temporary"
  mv -f "$temporary" "$STATE_PATH"
  load_state
}
stop_with_recovery_evidence() {
  local failed_sha=$1 recovery_sha=$2 recovery_status=stop_failed active_state main_pid
  if systemctl stop "$UNIT_NAME" >/dev/null 2>&1 \
    && active_state=$(systemctl_value "$UNIT_NAME" ActiveState) \
    && main_pid=$(systemctl_value "$UNIT_NAME" MainPID) \
    && [[ $active_state == inactive && $main_pid == 0 ]]; then
    recovery_status=stopped
  fi
  printf 'market-recorder deploy: recovery_status=%s failed_sha256=%s recovery_sha256=%s unit=%s state=%s\n' \
    "$recovery_status" "$failed_sha" "$recovery_sha" "$UNIT_PATH" "$STATE_PATH" >&2
  return 1
}
prepare_candidate() {
  local archive=$1 source_revision=$2 image_digest=$3 expected actual
  secure_regular_file "$archive" || die 'artifact archive is missing, indirect, or unsafe'
  [[ $source_revision =~ ^[0-9a-f]{40}$ ]] || die 'expected source revision is invalid'
  [[ $image_digest =~ ^sha256:[0-9a-f]{64}$ ]] || die 'expected image digest is invalid'
  expected=$'new-ploy-runner\nnew-ploy-runner.sha256\npolymarket-market-recorder-release.json\npolymarket-market-recorder-release.json.sha256'
  actual=$(tar -tf "$archive" | sed -e '/^[.][\/]$/d' -e 's|^[.]/||' | sort) \
    || die 'artifact archive cannot be listed'
  [[ $actual == "$expected" ]] || die 'artifact archive membership is not exact'
  tar -tvf "$archive" | awk '$NF != "./" && substr($1, 1, 1) != "-" {exit 1}' \
    || die 'artifact archive membership is not exact'
  candidate_extract=$(mktemp -d "${TMPDIR:-/tmp}/polymarket-market-recorder.XXXXXX")
  tar -xf "$archive" -C "$candidate_extract"
  chown "$expected_uid" "$candidate_extract" "$candidate_extract"/*
  chmod 0555 "$candidate_extract" "$candidate_extract/new-ploy-runner"
  chmod 0444 "$candidate_extract"/*.json "$candidate_extract"/*.sha256
  "$ARTIFACT_HELPER" verify \
    "$candidate_extract" "$source_revision" "$image_digest" "$verify_user" \
    || die 'release artifact verification failed'
  candidate_sha=$(sha256sum "$candidate_extract/new-ploy-runner" | awk '{print $1}')
}
pre_mutation_feasibility() {
  local source_revision=$1 image_digest=$2 now
  local candidate_directory="$RELEASE_ROOT/$candidate_sha"
  [[ -d $RELEASE_ROOT && ! -L $RELEASE_ROOT ]] \
    || die 'immutable release root is missing or indirect'
  [[ $(stat_uid "$RELEASE_ROOT") == "$expected_uid" ]] \
    || die 'immutable release root is not owned by root'
  (( (8#$(stat_mode "$RELEASE_ROOT") & 8#022) == 0 )) \
    || die 'immutable release root is writable outside its owner'
  if systemctl is-active --quiet "$UNIT_NAME"; then
    capture_runtime || die 'current active market-recorder runtime identity is not exact'
    runtime_activity=active
    now=$(date +%s)
    verify_fresh_output "$((now - MAX_OUTPUT_AGE_SECONDS))" \
      || die 'current market-tape output is stale or missing'
  else
    capture_contained_inactive_runtime \
      || die 'current stopped market-recorder baseline is not exact and contained'
    runtime_activity=inactive
  fi
  if [[ -e $STATE_PATH || -L $STATE_PATH ]]; then
    load_state || die 'existing deployment state is not exact'
    [[ $state_current_sha == "$runtime_sha" \
      && $state_current_source == "$runtime_source" ]] \
      || die 'current runtime does not match deployment state'
  fi
  [[ $candidate_sha != "$runtime_sha" ]] \
    || die 'candidate matches current runtime; no distinct rollback target'
  if [[ -e $candidate_directory || -L $candidate_directory ]]; then
    verify_installed_release "$candidate_sha" "$source_revision" "$image_digest" \
      || die 'existing candidate release does not match requested identity'
  fi
}

install_release() {
  local archive=$1 source_revision=$2 image_digest=$3 baseline_sha baseline_source
  local baseline_activity baseline_pid
  [[ -n $test_root || $EUID -eq 0 ]] || die 'install and rollback require root'
  prepare_candidate "$archive" "$source_revision" "$image_digest"
  pre_mutation_feasibility "$source_revision" "$image_digest"
  baseline_sha=$runtime_sha
  baseline_source=$runtime_source
  baseline_activity=$runtime_activity
  baseline_pid=$runtime_pid
  if [[ $baseline_activity == active ]]; then
    publish_runtime_snapshot "$baseline_sha" "$baseline_source" \
      "$PROC_ROOT/$baseline_pid/exe" \
      || die 'could not preserve the verified current release'
  else
    verify_installed_release "$baseline_sha" "$baseline_source" \
      || die 'stopped baseline release is not immutable and verified'
  fi
  publish_candidate "$candidate_sha" "$source_revision" "$image_digest" \
    || die 'could not publish the immutable candidate release'
  if ! activate_release "$candidate_sha" "$source_revision"; then
    printf 'market-recorder deploy: candidate verification failed; restoring sha256=%s\n' \
      "$baseline_sha" >&2
    if ! restore_baseline_after_failure \
      "$baseline_activity" "$baseline_sha" "$baseline_source"; then
      stop_with_recovery_evidence "$candidate_sha" "$baseline_sha"
      return 1
    fi
    die "candidate failed post-start verification; restored sha256=$baseline_sha source_revision=$baseline_source activity=$baseline_activity"
  fi
  if ! write_state "$candidate_sha" "$source_revision" \
    "$baseline_sha" "$baseline_source"; then
    printf 'market-recorder deploy: state publication failed; restoring sha256=%s\n' \
      "$baseline_sha" >&2
    if ! restore_baseline_after_failure \
      "$baseline_activity" "$baseline_sha" "$baseline_source"; then
      stop_with_recovery_evidence "$candidate_sha" "$baseline_sha"
      return 1
    fi
    die 'candidate was rolled back because deployment state could not be published'
  fi
  printf 'install passed candidate_sha256=%s source_revision=%s previous_sha256=%s previous_source_revision=%s\n' \
    "$candidate_sha" "$source_revision" "$baseline_sha" "$baseline_source"
}

verify_current() {
  load_state || die 'deployment state is missing or invalid'
  verify_running_release "$state_current_sha" "$state_current_source" \
    "$(($(date +%s) - MAX_OUTPUT_AGE_SECONDS))" \
    || die 'configured and running market-recorder release is not exact'
  printf 'verify passed sha256=%s source_revision=%s pid=%s invocation_id=%s n_restarts=%s\n' \
    "$state_current_sha" "$state_current_source" \
    "$runtime_pid" "$runtime_invocation" "$runtime_restarts"
}

rollback_release() {
  [[ -n $test_root || $EUID -eq 0 ]] || die 'install and rollback require root'
  load_state || die 'deployment state is missing or invalid'
  verify_running_release "$state_current_sha" "$state_current_source" \
    "$(($(date +%s) - MAX_OUTPUT_AGE_SECONDS))" \
    || die 'current release is not verified; refusing rollback'
  verify_installed_release "$state_previous_sha" "$state_previous_source" \
    || die 'previous release is not immutable and verified'
  if ! activate_release "$state_previous_sha" "$state_previous_source"; then
    stop_with_recovery_evidence "$state_previous_sha" "$state_current_sha"
    die 'rollback target failed verification; see recovery evidence'
  fi
  if ! write_state "$state_previous_sha" "$state_previous_source" \
    "$state_current_sha" "$state_current_source"; then
    printf 'market-recorder deploy: rollback state publication failed; restoring sha256=%s\n' \
      "$state_current_sha" >&2
    if ! activate_release "$state_current_sha" "$state_current_source"; then
      stop_with_recovery_evidence "$state_previous_sha" "$state_current_sha"
      return 1
    fi
    die 'rollback was reversed because deployment state could not be published'
  fi
  printf 'rollback passed current_sha256=%s current_source_revision=%s previous_sha256=%s previous_source_revision=%s\n' \
    "$state_current_sha" "$state_current_source" \
    "$state_previous_sha" "$state_previous_source"
}

for command in awk chmod chown cmp date find flock grep id install jq mkdir mktemp mv readlink \
  rm sed sha256sum sleep sort stat systemctl tar wc; do
  command -v "$command" >/dev/null || die "required command is missing: $command"
done
[[ -z $verify_user ]] || command -v runuser >/dev/null \
  || die 'required command is missing: runuser'
secure_directory_chain "$SCRIPT_DIR" || die 'deployment control path is indirect, writable, or untrusted'
secure_directory_chain "$ARTIFACT_HELPER_DIR" || die 'artifact helper path is indirect, writable, or untrusted'
[[ -x $ARTIFACT_HELPER ]] || die 'artifact verifier is missing or not executable'
secure_regular_file "$ARTIFACT_HELPER" || die 'artifact verifier is indirect or unsafe'
secure_regular_file "$UNIT_TEMPLATE" \
  || die 'service template is missing, indirect, or unsafe'
secure_regular_file "$CONFIG_TEMPLATE" \
  || die 'config template is missing, indirect, or unsafe'
[[ $# -ge 1 ]] || usage
mode=$1
shift
exec 9>"$LOCK_PATH"
flock -n 9 || die 'could not acquire the market-recorder deployment lock'

case "$mode" in
  preflight|dry-run)
    [[ $# -eq 3 ]] || usage
    prepare_candidate "$@"
    pre_mutation_feasibility "$2" "$3"
    printf 'preflight passed candidate_sha256=%s source_revision=%s current_sha256=%s current_source_revision=%s\n' \
      "$candidate_sha" "$2" "$runtime_sha" "$runtime_source"
    ;;
  install)
    [[ $# -eq 3 ]] || usage
    install_release "$@"
    ;;
  verify)
    [[ $# -eq 0 ]] || usage
    verify_current
    ;;
  rollback)
    [[ $# -eq 0 ]] || usage
    rollback_release
    ;;
  *) usage ;;
esac
