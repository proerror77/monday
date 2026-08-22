#!/usr/bin/env bash
# Static contract greps intentionally use literal shell expressions.
# shellcheck disable=SC2016
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
readonly SCRIPT_DIR
readonly WORKFLOW="$SCRIPT_DIR/../../.github/workflows/acr-publish.yml"
readonly ARTIFACT_HELPER="$SCRIPT_DIR/../../.github/scripts/polymarket-market-recorder-release-artifact.sh"
readonly DOCKERFILE="$SCRIPT_DIR/../../rust_hft/deployment/docker/Dockerfile.polymarket-market-recorder"
readonly RUNNER="$SCRIPT_DIR/../../rust_hft/prediction-markets/apps/new-ploy-runner/src/main.rs"
readonly DEPLOY="$SCRIPT_DIR/polymarket-market-recorder-deploy.sh"
readonly SERVICE="$SCRIPT_DIR/polymarket-market-tape.service"
readonly CONFIG="$SCRIPT_DIR/polymarket-market-tape.toml"

shellcheck "$0"
test -x "$ARTIFACT_HELPER"

grep -Fq 'ARG SOURCE_REVISION' "$DOCKERFILE"
grep -Fq "grep -Eq '^[0-9a-f]{40}$'" "$DOCKERFILE"
grep -Fq 'MONDAY_SOURCE_REVISION="$SOURCE_REVISION" cargo' "$DOCKERFILE"

grep -Fq 'option_env!("MONDAY_SOURCE_REVISION")' "$RUNNER"
grep -Fq 'std::env::args_os()' "$RUNNER"
grep -Fq 'new-ploy-runner {BUILD_SOURCE_REVISION}' "$RUNNER"

grep -Fq 'Extract bare-metal Polymarket market recorder' "$WORKFLOW"
grep -Fq 'polymarket-market-recorder-release-artifact.sh create' "$WORKFLOW"
grep -Fq 'polymarket-market-recorder-release-artifact.sh verify' "$WORKFLOW"
grep -Fq 'new-ploy-runner $source_revision' "$ARTIFACT_HELPER"
grep -Fq 'monday.polymarket_market_recorder_release.v1' "$ARTIFACT_HELPER"
grep -Fq 'polymarket-market-recorder-linux-amd64-${{ needs.selector.outputs.source_sha }}' "$WORKFLOW"
grep -Fq 'tar --format=ustar -cf polymarket-market-recorder-linux-amd64.tar' "$WORKFLOW"
grep -Fq 'path: polymarket-market-recorder-linux-amd64.tar' "$WORKFLOW"

if [[ ${MONDAY_TEST_RECORDER_IMAGE:-0} == 1 ]]; then
  : "${SOURCE_REVISION:?set SOURCE_REVISION for the image contract test}"
  [[ $SOURCE_REVISION =~ ^[0-9a-f]{40}$ ]]
  for command in docker jq sha256sum tar; do
    command -v "$command" >/dev/null
  done

  tmp_root=$(mktemp -d)
  release_dir="$tmp_root/release"
  mkdir "$release_dir"
  trap 'rm -rf "$tmp_root"' EXIT
  image="monday-polymarket-market-recorder-test:$SOURCE_REVISION"
  docker build --quiet \
    --build-arg "SOURCE_REVISION=$SOURCE_REVISION" \
    -f "$DOCKERFILE" \
    -t "$image" \
    "$SCRIPT_DIR/../../rust_hft" >/dev/null
  if docker build --quiet \
    --build-arg SOURCE_REVISION=invalid \
    -f "$DOCKERFILE" \
    -t monday-polymarket-market-recorder-invalid-test \
    "$SCRIPT_DIR/../../rust_hft" >/dev/null 2>&1; then
    printf 'market-recorder image accepted an invalid source revision\n' >&2
    exit 1
  fi

  container_id=$(docker create "$image")
  trap 'docker rm -f "$container_id" >/dev/null 2>&1 || true; rm -rf "$tmp_root"' EXIT
  docker cp "$container_id:/usr/local/bin/new-ploy-runner" \
    "$release_dir/new-ploy-runner"
  chmod 0755 "$release_dir/new-ploy-runner"
  image_id=$(docker image inspect --format '{{.Id}}' "$image")
  "$ARTIFACT_HELPER" create "$release_dir" "$SOURCE_REVISION" "$image_id"
  "$ARTIFACT_HELPER" verify "$release_dir" "$SOURCE_REVISION" "$image_id"

  archive="$tmp_root/polymarket-market-recorder-linux-amd64.tar"
  transported_release="$tmp_root/transported-release"
  tar --format=ustar -cf "$archive" -C "$release_dir" .
  chmod 0644 "$release_dir/new-ploy-runner"
  mkdir "$transported_release"
  tar -xf "$archive" -C "$transported_release"
  "$ARTIFACT_HELPER" verify \
    "$transported_release" "$SOURCE_REVISION" "$image_id"
  chmod 0755 "$release_dir/new-ploy-runner"

  cp "$release_dir/polymarket-market-recorder-release.json" \
    "$tmp_root/release.json.good"
  jq '.source_revision = "0000000000000000000000000000000000000000"' \
    "$tmp_root/release.json.good" \
    > "$release_dir/polymarket-market-recorder-release.json"
  (
    cd "$release_dir"
    sha256sum polymarket-market-recorder-release.json \
      > polymarket-market-recorder-release.json.sha256
  )
  if "$ARTIFACT_HELPER" verify "$release_dir" "$SOURCE_REVISION" "$image_id"; then
    printf 'release verifier accepted the wrong source revision\n' >&2
    exit 1
  fi
fi

release_test_root=$(mktemp -d)
release_test_root=$(cd -- "$release_test_root" && pwd -P)
trap 'chmod 0755 "$SCRIPT_DIR" "${ARTIFACT_HELPER%/*}" "$ARTIFACT_HELPER"; chmod 0644 "$SERVICE"; chmod -R u+w "$release_test_root" 2>/dev/null || true; rm -rf "$release_test_root"' EXIT
fixture="$release_test_root/preflight"
fake_bin="$fixture/fake-bin"
state="$fixture/systemctl-state.json"
mutation_log="$fixture/systemctl-mutations.log"
source_revision=$(printf 'a%.0s' {1..40})
baseline_source=$(printf 'b%.0s' {1..40})
image_digest="sha256:$(printf 'c%.0s' {1..64})"
release_dir="$fixture/artifact"
archive="$fixture/polymarket-market-recorder-linux-amd64.tar"
baseline_binary="$fixture/opt/monday/bin/new-ploy-runner"
unit="$fixture/etc/systemd/system/polymarket-market-tape.service"
config="$fixture/etc/monday/polymarket-market-tape.toml"
output="$fixture/data/monday/spool/polymarket/market-updates.ndjson"
mkdir -p "$fake_bin" "$release_dir" "${baseline_binary%/*}" "${unit%/*}" \
  "${config%/*}" \
  "$fixture/proc/4101" "$fixture/run/lock" \
  "$fixture/opt/monday/releases/polymarket-market-recorder" "${output%/*}"

write_runner() {
  local destination=$1 source=$2
  sed "s/@SOURCE@/$source/" >"$destination" <<'RUNNER_FIXTURE'
#!/bin/sh
if [ -n "${MONDAY_FAKE_EXPECT_RUNUSER:-}" ] \
  && [ "${MONDAY_FAKE_RUNNER_AS_USER:-}" != "$MONDAY_FAKE_EXPECT_RUNUSER" ]; then
  exit 97
fi
if [ "${1:-}" = --version ]; then
  printf '%s\n' 'new-ploy-runner @SOURCE@'
  exit 0
fi
exit 0
RUNNER_FIXTURE
  chmod 0755 "$destination"
}

cat >"$fake_bin/runuser" <<'RUNUSER'
#!/bin/sh
set -eu
user=$2
shift 3
printf '%s %s\n' "$user" "$*" >>"$FAKE_RUNUSER_LOG"
MONDAY_FAKE_RUNNER_AS_USER=$user "$@"
RUNUSER
chmod 0755 "$fake_bin/runuser"

write_runner "$release_dir/new-ploy-runner" "$source_revision"
"$ARTIFACT_HELPER" create "$release_dir" "$source_revision" "$image_digest"
COPYFILE_DISABLE=1 tar --format=ustar -cf "$archive" -C "$release_dir" .
candidate_sha=$(sha256sum "$release_dir/new-ploy-runner" | awk '{print $1}')
runuser_log="$fixture/runuser.log"
MONDAY_FAKE_EXPECT_RUNUSER=hftcollector FAKE_RUNUSER_LOG="$runuser_log" \
  PATH="$fake_bin:$PATH" "$ARTIFACT_HELPER" verify \
  "$release_dir" "$source_revision" "$image_digest" hftcollector
grep -Fqx "hftcollector $release_dir/new-ploy-runner --version" "$runuser_log"

write_runner "$baseline_binary" "$baseline_source"
baseline_sha=$(sha256sum "$baseline_binary" | awk '{print $1}')
sed "s|/opt/monday/releases/polymarket-market-recorder/@POLYMARKET_MARKET_RECORDER_SHA256@/new-ploy-runner|$baseline_binary|" \
  "$SERVICE" >"$unit"
chmod 0644 "$unit"
cp "$CONFIG" "$config"
chmod 0640 "$config"
ln -s "$baseline_binary" "$fixture/proc/4101/exe"
printf '{"fixture":"fresh"}\n' >"$output"

baseline_exec="$baseline_binary --config /etc/monday/polymarket-market-tape.toml --deployment-id polymarket-market-data-ecs --dry-run"
service_args='--config /etc/monday/polymarket-market-tape.toml --deployment-id polymarket-market-data-ecs --dry-run'
jq -n --arg fragment "$unit" --arg exec "$baseline_exec" '
  {active:true,substate:"running",enabled:"enabled",fragment:$fragment,
    drop_ins:"",exec:$exec,pid:"4101",invocation:("d"*32),restarts:"0"}' >"$state"
: >"$mutation_log"

cat >"$fake_bin/systemctl" <<'SYSTEMCTL'
#!/usr/bin/env bash
set -euo pipefail
command=${1:?expected systemctl command}
shift
case "$command" in
  is-active)
    [[ $(jq -r .active "$FAKE_SYSTEMCTL_STATE") == true ]]
    ;;
  is-enabled)
    enabled=$(jq -r .enabled "$FAKE_SYSTEMCTL_STATE")
    printf '%s\n' "$enabled"
    [[ $enabled == enabled ]]
    ;;
  show)
    property=${1#--property=}
    [[ ${FAKE_SYSTEMCTL_QUERY_ERROR_PROPERTY:-} != "$property" ]] || exit 1
    case "$property" in
      ActiveState)
        if jq -e .active "$FAKE_SYSTEMCTL_STATE" >/dev/null; then
          printf 'active\n'
        else
          printf 'inactive\n'
        fi
        ;;
      SubState) jq -r .substate "$FAKE_SYSTEMCTL_STATE" ;;
      FragmentPath) jq -r .fragment "$FAKE_SYSTEMCTL_STATE" ;;
      DropInPaths) jq -r .drop_ins "$FAKE_SYSTEMCTL_STATE" ;;
      ExecStart) printf '{ argv[]=%s ; }\n' "$(jq -r .exec "$FAKE_SYSTEMCTL_STATE")" ;;
      MainPID) jq -r .pid "$FAKE_SYSTEMCTL_STATE" ;;
      InvocationID) jq -r .invocation "$FAKE_SYSTEMCTL_STATE" ;;
      NRestarts) jq -r .restarts "$FAKE_SYSTEMCTL_STATE" ;;
      *) exit 2 ;;
    esac
    ;;
  daemon-reload|disable|enable|reset-failed|restart|stop)
    printf '%s\n' "$command $*" >>"$FAKE_SYSTEMCTL_MUTATION_LOG"
    case "$command" in
      daemon-reload)
        binary=$(sed -n 's|^ExecStart=\([^ ]*\).*$|\1|p' "$FAKE_SYSTEMCTL_UNIT")
        jq --arg exec "$binary $FAKE_SYSTEMCTL_EXPECTED_ARGS" \
          '.exec=$exec' "$FAKE_SYSTEMCTL_STATE" >"$FAKE_SYSTEMCTL_STATE.tmp" \
          && mv "$FAKE_SYSTEMCTL_STATE.tmp" "$FAKE_SYSTEMCTL_STATE"
        ;;
      disable|enable)
        unit_state=disabled
        [[ $command == enable ]] && unit_state=enabled
        jq --arg enabled "$unit_state" \
          '.enabled=$enabled' "$FAKE_SYSTEMCTL_STATE" >"$FAKE_SYSTEMCTL_STATE.tmp" \
          && mv "$FAKE_SYSTEMCTL_STATE.tmp" "$FAKE_SYSTEMCTL_STATE"
        ;;
      reset-failed)
        jq '.restarts = "0"' "$FAKE_SYSTEMCTL_STATE" >"$FAKE_SYSTEMCTL_STATE.tmp" \
          && mv "$FAKE_SYSTEMCTL_STATE.tmp" "$FAKE_SYSTEMCTL_STATE"
        ;;
      restart)
        binary=$(sed -n 's|^ExecStart=\([^ ]*\).*$|\1|p' "$FAKE_SYSTEMCTL_UNIT")
        [[ -x $binary ]]
        proc_binary=$binary
        if [[ -n $FAKE_SYSTEMCTL_BAD_ONCE_FILE \
          && -s $FAKE_SYSTEMCTL_BAD_ONCE_FILE \
          && $(<"$FAKE_SYSTEMCTL_BAD_ONCE_FILE") == "$binary" ]]; then
          proc_binary=$FAKE_SYSTEMCTL_BAD_PROC_EXE
          mv "$FAKE_SYSTEMCTL_BAD_ONCE_FILE" "$FAKE_SYSTEMCTL_BAD_ONCE_FILE.used"
        fi
        pid=$(( $(jq -r .pid "$FAKE_SYSTEMCTL_STATE") + 1 ))
        invocation=$(printf '%032x' "$pid")
        mkdir -p "$FAKE_SYSTEMCTL_ROOT/proc/$pid"
        ln -sfn "$proc_binary" "$FAKE_SYSTEMCTL_ROOT/proc/$pid/exe"
        jq --arg exec "$binary $FAKE_SYSTEMCTL_EXPECTED_ARGS" \
          --arg pid "$pid" --arg invocation "$invocation" \
          '.active=true | .substate="running" | .exec=$exec | .pid=$pid | .invocation=$invocation | .restarts="0"' \
          "$FAKE_SYSTEMCTL_STATE" >"$FAKE_SYSTEMCTL_STATE.tmp" \
          && mv "$FAKE_SYSTEMCTL_STATE.tmp" "$FAKE_SYSTEMCTL_STATE"
        printf '{"fixture":"fresh-after-restart"}\n' >>"$FAKE_SYSTEMCTL_OUTPUT"
        ;;
      stop)
        jq '.active=false | .substate="dead" | .pid="0" | .invocation=""' \
          "$FAKE_SYSTEMCTL_STATE" >"$FAKE_SYSTEMCTL_STATE.tmp" \
          && mv "$FAKE_SYSTEMCTL_STATE.tmp" "$FAKE_SYSTEMCTL_STATE"
        ;;
    esac
    ;;
  *) exit 2 ;;
esac
SYSTEMCTL
chmod 0755 "$fake_bin/systemctl"
cat >"$fake_bin/mv" <<'MV'
#!/usr/bin/env bash
set -euo pipefail
destination=${!#}
if [[ -n ${FAKE_MV_FAIL_DESTINATION:-} && $destination == "$FAKE_MV_FAIL_DESTINATION" ]]; then
  exit 1
fi
exec /bin/mv "$@"
MV
chmod 0755 "$fake_bin/mv"
printf '#!/bin/sh\nexit 0\n' >"$fake_bin/flock"
chmod 0755 "$fake_bin/flock"

test -x "$DEPLOY"
export MONDAY_MARKET_RECORDER_DEPLOY_TEST_ROOT="$fixture" \
  MONDAY_MARKET_RECORDER_DEPLOY_TEST_MODE=1 \
  MONDAY_MARKET_RECORDER_VERIFY_ATTEMPTS=1 \
  MONDAY_MARKET_RECORDER_VERIFY_SLEEP_SECONDS=0 \
  FAKE_SYSTEMCTL_STATE="$state" FAKE_SYSTEMCTL_MUTATION_LOG="$mutation_log" \
  FAKE_SYSTEMCTL_ROOT="$fixture" FAKE_SYSTEMCTL_UNIT="$unit" \
  FAKE_SYSTEMCTL_OUTPUT="$output" FAKE_SYSTEMCTL_EXPECTED_ARGS="$service_args"
run_deploy() {
  FAKE_SYSTEMCTL_BAD_ONCE_FILE="${bad_once:-}" \
  FAKE_SYSTEMCTL_BAD_PROC_EXE="${bad_proc_exe:-}" \
  FAKE_SYSTEMCTL_QUERY_ERROR_PROPERTY="${query_error_property:-}" \
  FAKE_MV_FAIL_DESTINATION="${mv_fail_destination:-}" \
  PATH="$fake_bin:$PATH" \
    "$DEPLOY" "$@"
}

chmod 0666 "$SERVICE"
run_deploy preflight "$archive" "$source_revision" "$image_digest" >/dev/null 2>&1 && exit 1
chmod 0644 "$SERVICE"
chmod 0777 "$ARTIFACT_HELPER"
run_deploy preflight "$archive" "$source_revision" "$image_digest" >/dev/null 2>&1 && exit 1
chmod 0755 "$ARTIFACT_HELPER"
chmod 0777 "$SCRIPT_DIR"
run_deploy preflight "$archive" "$source_revision" "$image_digest" >/dev/null 2>&1 && exit 1
chmod 0755 "$SCRIPT_DIR"
chmod 0777 "${ARTIFACT_HELPER%/*}"
run_deploy preflight "$archive" "$source_revision" "$image_digest" >/dev/null 2>&1 && exit 1
chmod 0755 "${ARTIFACT_HELPER%/*}"
run_deploy preflight "$archive" "$source_revision" "$image_digest"
[[ ! -s $mutation_log ]]
[[ ! -e $fixture/opt/monday/releases/polymarket-market-recorder/$candidate_sha ]]
bad_archive="$fixture/unexpected.tar"
cp "$archive" "$bad_archive"
printf 'unexpected\n' >"$fixture/unexpected"
tar -rf "$bad_archive" -C "$fixture" unexpected
run_deploy preflight "$bad_archive" "$source_revision" "$image_digest" >/dev/null 2>&1 && exit 1
jq '.drop_ins="/etc/systemd/system/polymarket-market-tape.service.d/override.conf"' \
  "$state" >"$state.tmp" && mv "$state.tmp" "$state"
run_deploy dry-run "$archive" "$source_revision" "$image_digest" >/dev/null 2>&1 && exit 1
jq '.drop_ins=""' "$state" >"$state.tmp" && mv "$state.tmp" "$state"
[[ ! -s $mutation_log ]]
candidate_binary="$fixture/opt/monday/releases/polymarket-market-recorder/$candidate_sha/new-ploy-runner"
candidate_release=${candidate_binary%/*}
other_image_digest="sha256:$(printf 'd%.0s' {1..64})"
mkdir "$candidate_release"
cp "$release_dir/new-ploy-runner" "$candidate_release/new-ploy-runner"
chmod 0555 "$candidate_release/new-ploy-runner"
"$ARTIFACT_HELPER" create "$candidate_release" "$source_revision" "$other_image_digest"
chmod 0444 "$candidate_release"/*
chmod 0555 "$candidate_release/new-ploy-runner" "$candidate_release"
run_deploy preflight "$archive" "$source_revision" "$image_digest" \
  >"$fixture/existing-candidate.out" 2>&1 && exit 1
grep -Fq 'existing candidate release does not match requested identity' \
  "$fixture/existing-candidate.out"
jq -e --arg image "$other_image_digest" '.image_digest == $image' \
  "$candidate_release/polymarket-market-recorder-release.json" >/dev/null
[[ $(find "$fixture/opt/monday/releases/polymarket-market-recorder" -mindepth 1 -maxdepth 2 -print | wc -l) -eq 5 ]]
[[ ! -e $fixture/opt/monday/releases/polymarket-market-recorder/$baseline_sha && ! -s $mutation_log ]]
chmod -R u+w "$candidate_release"
rm -rf "$candidate_release"

cp "$release_dir/new-ploy-runner" "$baseline_binary"
run_deploy preflight "$archive" "$source_revision" "$image_digest" \
  >"$fixture/same-sha-preflight.out" 2>&1 && exit 1
grep -Fq 'candidate matches current runtime; no distinct rollback target' \
  "$fixture/same-sha-preflight.out"
[[ -z $(find "$fixture/opt/monday/releases/polymarket-market-recorder" -mindepth 1 -maxdepth 1 -print -quit) && ! -s $mutation_log ]]
write_runner "$baseline_binary" "$baseline_source"
run_deploy install "$archive" "$source_revision" "$image_digest"
baseline_release="$fixture/opt/monday/releases/polymarket-market-recorder/$baseline_sha"
state_file="$fixture/opt/monday/state/polymarket-market-recorder-deploy.json"
[[ -x $candidate_binary && ! -L $candidate_binary ]]
[[ -x $baseline_release/new-ploy-runner && ! -L $baseline_release/new-ploy-runner ]]
[[ $(<"$baseline_release/source-revision.txt") == "$baseline_source" ]]
grep -Fqx "ExecStart=$candidate_binary \\" "$unit"
jq -e --arg current "$candidate_sha" --arg previous "$baseline_sha" '
  .schema == "monday.polymarket_market_recorder_deploy.v2"
  and .current.sha256 == $current and .current.activity == "active"
  and .previous.sha256 == $previous and .previous.activity == "active"' \
  "$state_file" >/dev/null
pid=$(jq -r .pid "$state")
[[ $(readlink -f "$fixture/proc/$pid/exe") == "$candidate_binary" ]]
[[ $(sha256sum "$fixture/proc/$pid/exe" | awk '{print $1}') == "$candidate_sha" ]]
run_deploy verify
[[ $(grep -c '^restart polymarket-market-tape.service$' "$mutation_log") == 1 ]]

run_deploy rollback
grep -Fqx "ExecStart=$baseline_release/new-ploy-runner \\" "$unit"
jq -e --arg current "$baseline_sha" --arg previous "$candidate_sha" '
  .current.sha256 == $current and .current.activity == "active"
  and .previous.sha256 == $previous and .previous.activity == "active"' \
  "$state_file" >/dev/null
pid=$(jq -r .pid "$state")
[[ $(readlink -f "$fixture/proc/$pid/exe") == "$baseline_release/new-ploy-runner" ]]
[[ $(grep -c '^restart polymarket-market-tape.service$' "$mutation_log") == 2 ]]

bad_once="$fixture/fail-candidate-once"
bad_proc_exe="$baseline_release/new-ploy-runner"
printf '%s\n' "$candidate_binary" >"$bad_once"
run_deploy install "$archive" "$source_revision" "$image_digest" \
  >"$fixture/failed-install.out" 2>&1 && exit 1
grep -Fq "restored sha256=$baseline_sha source_revision=$baseline_source" \
  "$fixture/failed-install.out"
grep -Fqx "ExecStart=$baseline_release/new-ploy-runner \\" "$unit"
pid=$(jq -r .pid "$state")
[[ $(readlink -f "$fixture/proc/$pid/exe") == "$baseline_release/new-ploy-runner" ]]
jq -e --arg current "$baseline_sha" '.current.sha256 == $current' \
  "$state_file" >/dev/null
[[ $(grep -c '^restart polymarket-market-tape.service$' "$mutation_log") == 4 ]]

printf '%s\n' "$candidate_binary" >"$bad_once"
query_error_property=ActiveState
if run_deploy rollback >"$fixture/failed-recovery.out" 2>&1; then exit 1; fi
grep -Fq 'recovery_status=stop_failed' "$fixture/failed-recovery.out"
jq -e '.active == false and .enabled == "disabled" and .pid == "0"' "$state" >/dev/null

query_error_property=
baseline_release_binary="$baseline_release/new-ploy-runner"
baseline_release_exec="$baseline_release_binary $service_args"
sed "s|/opt/monday/releases/polymarket-market-recorder/@POLYMARKET_MARKET_RECORDER_SHA256@/new-ploy-runner|$baseline_release_binary|" \
  "$SERVICE" >"$unit"
chmod 0644 "$unit"
jq --arg exec "$baseline_release_exec" '
  .active=false | .substate="dead" | .enabled="disabled" | .exec=$exec
  | .pid="0" | .invocation="" | .restarts="0"' \
  "$state" >"$state.tmp" && mv "$state.tmp" "$state"
jq -S -n --arg current_sha "$baseline_sha" \
  --arg current_source "$baseline_source" --arg previous_sha "$candidate_sha" \
  --arg previous_source "$source_revision" '
  {schema:"monday.polymarket_market_recorder_deploy.v1",
    current:{sha256:$current_sha,source_revision:$current_source},
    previous:{sha256:$previous_sha,source_revision:$previous_source}}' \
  >"$state_file.tmp"
chmod 0600 "$state_file.tmp"
mv "$state_file.tmp" "$state_file"
: >"$output"
: >"$mutation_log"

jq '.pid="77"' "$state" >"$state.tmp" && mv "$state.tmp" "$state"
run_deploy preflight "$archive" "$source_revision" "$image_digest" \
  >"$fixture/inactive-pid.out" 2>&1 && exit 1
jq '.pid="0"' "$state" >"$state.tmp" && mv "$state.tmp" "$state"
jq '.enabled="enabled"' "$state" >"$state.tmp" && mv "$state.tmp" "$state"
run_deploy preflight "$archive" "$source_revision" "$image_digest" \
  >"$fixture/inactive-enabled.out" 2>&1 && exit 1
jq '.enabled="disabled"' "$state" >"$state.tmp" && mv "$state.tmp" "$state"
printf '# drift\n' >>"$config"
run_deploy preflight "$archive" "$source_revision" "$image_digest" \
  >"$fixture/inactive-config-drift.out" 2>&1 && exit 1
cp "$CONFIG" "$config"
chmod 0640 "$config"

run_deploy preflight "$archive" "$source_revision" "$image_digest"
[[ ! -s $mutation_log && ! -s $output ]]
run_deploy install "$archive" "$source_revision" "$image_digest"
jq -e '.active == true and .substate == "running" and .enabled == "enabled" and .pid != "0"' "$state" >/dev/null
pid=$(jq -r .pid "$state")
[[ $(readlink -f "$fixture/proc/$pid/exe") == "$candidate_binary" ]]
jq -e --arg current "$candidate_sha" --arg previous "$baseline_sha" '
  .schema == "monday.polymarket_market_recorder_deploy.v2"
  and .current.sha256 == $current and .current.activity == "active"
  and .previous.sha256 == $previous and .previous.activity == "inactive"' \
  "$state_file" >/dev/null
[[ $(grep -c '^restart polymarket-market-tape.service$' "$mutation_log") == 1 ]]

# A failed soak may leave the exact candidate running without fresh output and
# with a recorded restart. Contained rollback must still restore the stopped
# baseline instead of reviving its old hot loop.
: >"$output"
jq '.restarts="1"' "$state" >"$state.tmp" && mv "$state.tmp" "$state"
run_deploy rollback
grep -Fqx "ExecStart=$baseline_release_binary \\" "$unit"
cmp -s "$CONFIG" "$config"
jq -e --arg exec "$baseline_release_exec" '
  .active == false and .substate == "dead" and .enabled == "disabled"
  and .pid == "0" and .exec == $exec' "$state" >/dev/null
jq -e --arg current "$baseline_sha" --arg previous "$candidate_sha" '
  .current.sha256 == $current and .current.activity == "inactive"
  and .previous.sha256 == $previous and .previous.activity == "active"' \
  "$state_file" >/dev/null
run_deploy verify
[[ $(grep -c '^restart polymarket-market-tape.service$' "$mutation_log") == 1 ]]
[[ $(grep -c '^stop polymarket-market-tape.service$' "$mutation_log") == 1 ]]

: >"$output"
: >"$mutation_log"
rm -f -- "$bad_once" "$bad_once.used"
printf '%s\n' "$candidate_binary" >"$bad_once"
run_deploy install "$archive" "$source_revision" "$image_digest" \
  >"$fixture/failed-inactive-install.out" 2>&1 && exit 1
grep -Fq "restored sha256=$baseline_sha source_revision=$baseline_source activity=inactive" \
  "$fixture/failed-inactive-install.out"
grep -Fqx "ExecStart=$baseline_release_binary \\" "$unit"
cmp -s "$CONFIG" "$config"
jq -e --arg exec "$baseline_release_exec" '
  .active == false and .substate == "dead" and .enabled == "disabled"
  and .pid == "0" and .exec == $exec' "$state" >/dev/null
jq -e --arg current "$baseline_sha" --arg previous "$candidate_sha" '
  .schema == "monday.polymarket_market_recorder_deploy.v2"
  and .current.sha256 == $current and .current.activity == "inactive"
  and .previous.sha256 == $previous and .previous.activity == "active"' \
  "$state_file" >/dev/null
[[ $(grep -c '^restart polymarket-market-tape.service$' "$mutation_log") == 1 ]]
[[ $(grep -c '^stop polymarket-market-tape.service$' "$mutation_log") == 1 ]]

# A candidate that has already exited is still an exact rollback source. The
# stopped baseline must be restored without starting either release.
: >"$output"
: >"$mutation_log"
rm -f -- "$bad_once" "$bad_once.used"
rm -rf -- "$fixture/proc/1"
run_deploy install "$archive" "$source_revision" "$image_digest"
jq '.active=false | .substate="dead" | .pid="0" | .invocation=""' \
  "$state" >"$state.tmp" && mv "$state.tmp" "$state"
run_deploy rollback
grep -Fqx "ExecStart=$baseline_release_binary \\" "$unit"
jq -e --arg exec "$baseline_release_exec" '
  .active == false and .substate == "dead" and .enabled == "disabled"
  and .pid == "0" and .exec == $exec' "$state" >/dev/null
jq -e --arg current "$baseline_sha" --arg previous "$candidate_sha" '
  .current.sha256 == $current and .current.activity == "inactive"
  and .previous.sha256 == $previous and .previous.activity == "active"' \
  "$state_file" >/dev/null
[[ $(grep -c '^restart polymarket-market-tape.service$' "$mutation_log") == 1 ]]

# A healthy candidate remains the authoritative active state if an ordinary
# rollback cannot publish its state transition.
: >"$output"
: >"$mutation_log"
rm -rf -- "$fixture/proc/1"
run_deploy install "$archive" "$source_revision" "$image_digest"
mv_fail_destination=$state_file
run_deploy rollback >"$fixture/failed-healthy-state-publication.out" 2>&1 && exit 1
mv_fail_destination=
grep -Fq 'rollback was reversed because deployment state could not be published' \
  "$fixture/failed-healthy-state-publication.out"
grep -Fqx "ExecStart=$candidate_binary \\" "$unit"
jq -e --arg exec "$candidate_binary $service_args" '
  .active == true and .substate == "running" and .enabled == "enabled"
  and .pid != "0" and .exec == $exec' "$state" >/dev/null
jq -e --arg current "$candidate_sha" --arg previous "$baseline_sha" '
  .current.sha256 == $current and .current.activity == "active"
  and .previous.sha256 == $previous and .previous.activity == "inactive"' \
  "$state_file" >/dev/null
[[ $(grep -c '^restart polymarket-market-tape.service$' "$mutation_log") == 2 ]]
run_deploy rollback

# If state publication fails after a degraded candidate is rolled back, keep
# the contained baseline stopped instead of restarting the failed candidate.
: >"$output"
: >"$mutation_log"
rm -f -- "$bad_once" "$bad_once.used"
rm -rf -- "$fixture/proc/1"
run_deploy install "$archive" "$source_revision" "$image_digest"
: >"$output"
jq '.restarts="1"' "$state" >"$state.tmp" && mv "$state.tmp" "$state"
mv_fail_destination=$state_file
run_deploy rollback >"$fixture/failed-contained-state-publication.out" 2>&1 && exit 1
mv_fail_destination=
grep -Fq 'contained baseline remains stopped' \
  "$fixture/failed-contained-state-publication.out"
grep -Fq "recovery_status=stopped failed_sha256=$candidate_sha recovery_sha256=$baseline_sha" \
  "$fixture/failed-contained-state-publication.out"
grep -Fqx "ExecStart=$baseline_release_binary \\" "$unit"
jq -e --arg exec "$baseline_release_exec" '
  .active == false and .substate == "dead" and .enabled == "disabled"
  and .pid == "0" and .exec == $exec' "$state" >/dev/null
jq -e --arg current "$candidate_sha" --arg previous "$baseline_sha" '
  .current.sha256 == $current and .current.activity == "active"
  and .previous.sha256 == $previous and .previous.activity == "inactive"' \
  "$state_file" >/dev/null
[[ $(grep -c '^restart polymarket-market-tape.service$' "$mutation_log") == 1 ]]

printf 'Polymarket market-recorder release contract tests passed\n'
