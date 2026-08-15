#!/usr/bin/env bash
# Static contract greps intentionally use literal shell expressions.
# Extracted production snippets invoke test doubles and variables indirectly.
# shellcheck disable=SC1090,SC2016,SC2030,SC2031,SC2034,SC2154,SC2317,SC2329
set -euo pipefail

export LC_ALL=C
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
readonly SCRIPT_DIR
readonly RUST_MANIFEST="$SCRIPT_DIR/../../rust_hft/Cargo.toml"
readonly VERIFY="$SCRIPT_DIR/../../rust_hft/target/debug/polymarket-raw-ops"
readonly POLICY="$SCRIPT_DIR/polymarket-shadow-gate-policy.jq"
readonly LEGACY_HEALTH_POLICY="$SCRIPT_DIR/polymarket-legacy-health-policy.jq"
readonly RUST_HEALTH_POLICY="$SCRIPT_DIR/polymarket-rust-health-policy.jq"
readonly GATE="$SCRIPT_DIR/polymarket-raw-ops-shadow-gate.sh"
readonly GATE_CONTROL="$SCRIPT_DIR/polymarket-raw-ops-gate-control.sh"
readonly GATE_UNIT="$SCRIPT_DIR/polymarket-raw-ops-gate@.service"
readonly CUTOVER="$SCRIPT_DIR/polymarket-raw-ops-cutover.sh"
readonly WORKFLOW="$SCRIPT_DIR/../../.github/workflows/acr-publish.yml"
readonly CI_WORKFLOW="$SCRIPT_DIR/../../.github/workflows/ci.yml"
readonly README="$SCRIPT_DIR/README.md"
readonly POLYMARKET_COMPILER_DOCKERFILE="$SCRIPT_DIR/../../rust_hft/deployment/docker/Dockerfile.polymarket-evidence-compiler"

if command -v gsha256sum >/dev/null 2>&1; then
  sha256sum() {
    command gsha256sum "$@"
  }
fi

for command in cargo chmod cp grep jq ln mkdir mktemp mv rm sed sha256sum \
  shellcheck sort sync wc zstd; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'missing control-plane test dependency: %s\n' "$command" >&2
    exit 2
  }
done

[[ -x $GATE_CONTROL && -f $GATE_UNIT ]] || {
  printf 'missing supervised Gate control-plane assets\n' >&2
  exit 1
}

gate_privilege_transition_contract() {
  grep -Fq 'runuser -u hftcollector -- env HOME=/var/lib/hft-collector' "$1" &&
    grep -Fxq 'Environment=HOME=/var/lib/hft-collector' "$2" &&
    grep -Fxq 'AmbientCapabilities=CAP_SETUID CAP_SETGID' "$2" &&
    grep -Fxq 'NoNewPrivileges=true' "$2" &&
    grep -Fxq 'RestrictSUIDSGID=true' "$2" &&
    ! grep -Fxq 'RestrictSUIDSGID=false' "$2"
}

join_shell_continuations() {
  awk '{
    line=$0
    while (sub(/[[:space:]]*\\$/, "", line)) {
      if ((getline next_line) <= 0) break
      sub(/^[[:space:]]*/, "", next_line)
      line=line " " next_line
    }
    print line
  }' "$1"
}

shellcheck "$GATE" "$GATE_CONTROL" "$CUTOVER" "$0"
for unit_line in \
  'Type=exec' 'Restart=no' 'KillMode=control-group' 'RuntimeMaxSec=18000' \
  'TimeoutStopSec=120' \
  'ExecStartPre=/usr/bin/env -- ${MONDAY_POLYMARKET_GATE_CONTROL} prepare %i' \
  'ExecStart=/usr/bin/env -- ${MONDAY_POLYMARKET_GATE_CONTROL} run %i' \
  'ExecStopPost=/usr/bin/env -- ${MONDAY_POLYMARKET_GATE_CONTROL} finalize %i'; do
  grep -Fxq "$unit_line" "$GATE_UNIT"
done
if grep -Eq '^\[(Install)\]$|^(Wants|Requires|Conflicts)=.*polymarket-reference' \
  "$GATE_UNIT"; then
  printf 'Gate supervisor must not install, start, or conflict with a collector\n' >&2
  exit 1
fi
grep -Fq "trap 'exit 143' HUP INT TERM" "$GATE"
grep -Fq 'run_id=$supervised_invocation_id' "$GATE"
if ! awk '
  /^verify_gate_supervisor "\$candidate_sha" "\$supervised_invocation_id"/ {guard=NR}
  /^real_market_preflight_json=$/ {preflight=NR}
  /^systemctl start "\$shadow_unit"$/ && !shadow {shadow=NR}
  END {exit !(guard && guard < preflight && preflight < shadow)}
' "$GATE"; then
  printf 'unmanaged Gate can reach preflight or shadow startup\n' >&2
  exit 1
fi
grep -Fq 'pass_ready_marker="$evidence_dir/.PASSED.sha256.ready"' "$GATE"
if grep -Fq 'marker="$evidence_dir/PASSED.sha256"' "$GATE"; then
  printf 'running Gate publishes the official pass marker before finalization\n' >&2
  exit 1
fi

supervisor_tmp=$(mktemp -d)
trap 'rm -rf "$supervisor_tmp"' EXIT
blocked_gate_unit="$supervisor_tmp/blocked-gate.service"
blocked_gate_home="$supervisor_tmp/blocked-gate-home.service"
sed '/^AmbientCapabilities=CAP_SETUID CAP_SETGID$/d' \
  "$GATE_UNIT" >"$blocked_gate_unit"
sed '/^Environment=HOME=\/var\/lib\/hft-collector$/d' \
  "$GATE_UNIT" >"$blocked_gate_home"
gate_privilege_transition_contract "$GATE" "$GATE_UNIT"
if gate_privilege_transition_contract "$GATE" "$blocked_gate_unit"; then
  printf 'Gate privilege contract accepted a unit without UID/GID capabilities\n' >&2
  exit 1
fi
if gate_privilege_transition_contract "$GATE" "$blocked_gate_home"; then
  printf 'Gate privilege contract accepted a unit without an explicit HOME\n' >&2
  exit 1
fi
supervisor_root="$supervisor_tmp/root"
supervisor_fake_bin="$supervisor_tmp/bin"
supervisor_control_dir="$supervisor_tmp/control"
supervisor_control="$supervisor_control_dir/${GATE_CONTROL##*/}"
supervisor_state="$supervisor_tmp/systemctl"
supervisor_calls="$supervisor_tmp/systemctl.calls"
supervisor_gate_calls="$supervisor_tmp/gate.calls"
supervisor_candidate="$supervisor_tmp/polymarket-raw-ops"
supervisor_source=$(printf 'b%.0s' {1..40})
supervisor_invocation=$(printf '1%.0s' {1..32})
mkdir -p "$supervisor_fake_bin" "$supervisor_control_dir" \
  "$supervisor_root/etc/systemd/system" \
  "$supervisor_root/run/monday" "$supervisor_root/opt/monday/bin" \
  "$supervisor_root/data/monday/evidence"
cp "$GATE_CONTROL" "$supervisor_control"
cp "$GATE_UNIT" "$supervisor_control_dir/${GATE_UNIT##*/}"
cat >"$supervisor_control_dir/${GATE##*/}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s|%s|%s\n' "$*" "${INVOCATION_ID:-}" \
  "${MONDAY_POLYMARKET_GATE_INVOCATION_ID:-}" >>"$FAKE_GATE_CALLS"
exit "${FAKE_GATE_EXIT:-0}"
EOF
chmod 0755 "$supervisor_control" \
  "$supervisor_control_dir/${GATE##*/}"
printf '#!/usr/bin/env bash\nexit 0\n' >"$supervisor_candidate"
chmod 0755 "$supervisor_candidate"
supervisor_candidate_sha=$(sha256sum "$supervisor_candidate" | awk '{print $1}')
supervisor_unit="polymarket-raw-ops-gate@${supervisor_candidate_sha}.service"
supervisor_baseline="$supervisor_root/opt/monday/bin/polymarket-raw-ops"
printf '#!/usr/bin/env bash\nexit 1\n' >"$supervisor_baseline"
chmod 0755 "$supervisor_baseline"
supervisor_baseline_sha=$(sha256sum "$supervisor_baseline" | awk '{print $1}')
supervisor_probe_root="$supervisor_root/data/monday/evidence/polymarket-candidate-probes/$supervisor_candidate_sha"
mkdir -p "$supervisor_probe_root"
supervisor_probe="$supervisor_probe_root/gamma-closed-200.json"
jq -n --arg candidate "$supervisor_candidate_sha" --arg source "$supervisor_source" \
  --arg observed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
  {schema:"monday.polymarket_gamma_closed_200_recovery_probe.v1",
   candidate_sha256:$candidate,source_revision:$source,observed_at:$observed_at,
   gamma:{tagged_closed:{query:"closed=true&tag_id=21",attempts:3,http_status:200},
          untagged_closed:{query:"closed=true",attempts:3,http_status:200}},
   candidate_once:{exit_status:0,duration_seconds:23,health_updated_at:$observed_at}}
' >"$supervisor_probe"
mkdir "$supervisor_state"
printf 'inactive\n' >"$supervisor_state/active"
printf '%s\n' "$supervisor_invocation" >"$supervisor_state/invocation"
printf 'inactive\n' >"$supervisor_state/shadow"
printf 'inactive\n' >"$supervisor_state/baseline-active"
printf '0\n' >"$supervisor_state/baseline-main-pid"
printf '2\n' >"$supervisor_state/baseline-restarts"
printf '%s\n' "$(printf 'a%.0s' {1..32})" >"$supervisor_state/baseline-invocation"
printf '%s\n' '/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200' \
  >"$supervisor_state/baseline-exec"
for uploader_unit in \
  polymarket-reference-upload.service polymarket-reference-upload.timer \
  polymarket-market-tape-upload.service polymarket-market-tape-upload.timer; do
  printf 'inactive\n' >"$supervisor_state/uploader-active-$uploader_unit"
  printf '0\n' >"$supervisor_state/uploader-main-pid-$uploader_unit"
done
: >"$supervisor_calls"
: >"$supervisor_gate_calls"

cat >"$supervisor_fake_bin/systemctl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

read_state() { tr -d '\n' <"$FAKE_SYSTEMCTL_STATE/$1"; }
write_state() { printf '%s\n' "$2" >"$FAKE_SYSTEMCTL_STATE/$1"; }
uploader_state_file() { printf 'uploader-active-%s\n' "$1"; }

printf '%s\n' "$*" >>"$FAKE_SYSTEMCTL_CALLS"
case "${1:-}" in
  is-active)
    [[ ${2:-} == --quiet && $# -eq 3 ]] || exit 2
    unit=${3:-}
    case "$unit" in
      polymarket-reference-collector.service)
        [[ $(read_state baseline-active) == active ]]
        ;;
      polymarket-reference-upload.service|polymarket-reference-upload.timer|\
      polymarket-market-tape-upload.service|polymarket-market-tape-upload.timer)
        [[ $(read_state "$(uploader_state_file "$unit")") == active ]]
        ;;
      *) exit 2 ;;
    esac
    ;;
  daemon-reload)
    [[ $# -eq 1 ]] || exit 2
    ;;
  start)
    [[ $# -eq 2 ]] || exit 2
    unit=${2:-}
    candidate=${unit#polymarket-raw-ops-gate@}
    candidate=${candidate%.service}
    if [[ ${FAKE_START_REJECT:-0} == 1 ]]; then
      [[ ${FAKE_START_REJECT_ACTIVE:-0} != 1 ]] || write_state active active
      exit 17
    fi
    INVOCATION_ID=$(read_state invocation) \
      "$FAKE_GATE_CONTROL" prepare "$candidate"
    write_state active active
    ;;
  stop)
    [[ $# -ge 2 ]] || exit 2
    unit=${!#}
    if [[ $unit == polymarket-reference-collector-shadow@* ]]; then
      [[ ${FAKE_SHADOW_STOP_FAIL:-0} != 1 ]] || exit 5
      write_state shadow inactive
    elif [[ $unit == polymarket-raw-ops-gate@* ]]; then
      candidate=${unit#polymarket-raw-ops-gate@}
      candidate=${candidate%.service}
      invocation=$(read_state invocation)
      write_state active inactive
      INVOCATION_ID=$invocation \
        SERVICE_RESULT=signal \
        EXIT_CODE=killed \
        EXIT_STATUS=15 \
        "$FAKE_GATE_CONTROL" finalize "$candidate" >/dev/null
    else
      exit 2
    fi
    ;;
  show)
    [[ $# -ge 2 ]] || exit 2
    unit=${2:-}
    property=
    for argument in "${@:3}"; do
      [[ $argument == --property=* ]] && property=${argument#--property=}
    done
    [[ ${FAKE_SHOW_FAIL:-} != "$property" ]] || exit 9
    case "$property" in
      ActiveState)
        if [[ $unit == polymarket-reference-collector-shadow@* ]]; then
          read_state shadow
        elif [[ $unit == polymarket-reference-collector.service ]]; then
          read_state baseline-active
        elif [[ $unit == polymarket-reference-upload.service \
          || $unit == polymarket-reference-upload.timer \
          || $unit == polymarket-market-tape-upload.service \
          || $unit == polymarket-market-tape-upload.timer ]]; then
          read_state "$(uploader_state_file "$unit")"
        else
          read_state active
        fi
        ;;
      InvocationID)
        if [[ $unit == polymarket-reference-collector.service ]]; then
          read_state baseline-invocation
        else
          read_state invocation
        fi
        ;;
      MainPID)
        if [[ $unit == polymarket-reference-collector-shadow@* ]]; then
          [[ $(read_state shadow) == active ]] && printf '5252\n' || printf '0\n'
        elif [[ $unit == polymarket-reference-collector.service ]]; then
          read_state baseline-main-pid
        elif [[ $unit == polymarket-reference-upload.service \
          || $unit == polymarket-reference-upload.timer \
          || $unit == polymarket-market-tape-upload.service \
          || $unit == polymarket-market-tape-upload.timer ]]; then
          read_state "uploader-main-pid-$unit"
        else
          [[ $(read_state active) == active ]] && printf '4242\n' || printf '0\n'
        fi
        ;;
      NRestarts)
        [[ $unit == polymarket-reference-collector.service ]] || exit 2
        read_state baseline-restarts
        ;;
      ExecStart)
        [[ $unit == polymarket-reference-collector.service ]] || exit 2
        printf 'argv[]=%s;\n' "$(read_state baseline-exec)"
        ;;
      FragmentPath)
        if [[ $unit == polymarket-reference-collector.service ]]; then
          printf '/etc/systemd/system/polymarket-reference-collector.service\n'
        else
          printf '/etc/systemd/system/polymarket-raw-ops-gate@.service\n'
        fi
        ;;
      DropInPaths) printf '\n' ;;
      *) exit 2 ;;
    esac
    ;;
  reset-failed)
    [[ $# -eq 2 ]] || exit 2
    unit=${2:-}
    # reset-failed is a governed mutation: it must happen only while the
    # caller holds the Gate control lock.
    exec 8>>"$FAKE_CONTROL_LOCK"
    if flock -n 8; then
      printf 'reset-failed outside the Gate control lock: %s\n' "$unit" >&2
      exit 9
    fi
    exec 8>&-
    case "$unit" in
      polymarket-reference-collector.service)
        [[ $(read_state baseline-active) == failed ]] || exit 5
        if [[ ${FAKE_RESET_FAILED_STUCK:-0} != 1 ]]; then
          write_state baseline-active inactive
          write_state baseline-main-pid 0
          write_state baseline-restarts 0
        fi
        ;;
      polymarket-reference-upload.service|polymarket-reference-upload.timer|\
      polymarket-market-tape-upload.service|polymarket-market-tape-upload.timer)
        state_file=$(uploader_state_file "$unit")
        [[ $(read_state "$state_file") == failed ]] || exit 5
        write_state "$state_file" inactive
        ;;
      *) exit 2 ;;
    esac
    ;;
  list-units)
    if [[ ${FAKE_GATE_UNITS_RUNNING:-0} == 1 ]]; then
      printf 'polymarket-raw-ops-gate@active.service loaded active running\n'
    fi
    ;;
  *) exit 2 ;;
esac
EOF
chmod 0755 "$supervisor_fake_bin/systemctl"
cat >"$supervisor_fake_bin/flock" <<'EOF'
#!/usr/bin/perl
use Fcntl qw(LOCK_EX LOCK_NB);
open my $lock, '<&=', $ARGV[-1] or die "dup lock fd: $!\n";
my $operation = LOCK_EX;
$operation |= LOCK_NB if grep { $_ eq '-n' } @ARGV;
exit(flock($lock, $operation) ? 0 : 1);
EOF
chmod 0755 "$supervisor_fake_bin/flock"
cat >"$supervisor_fake_bin/mv" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
destination=${!#}
source=${@: -2:1}
[[ ${FAKE_MV_FAIL_RECEIPT_STAGE:-0} != 1 \
  || $destination != */.receipt.json.ready ]] \
  || exit 74
[[ ${FAKE_MV_FAIL_RECEIPT:-0} != 1 || $destination != */receipt.json ]] \
  || exit 75
[[ ${FAKE_MV_FAIL_GATE_INSTALL:-0} != 1 \
  || $destination != */polymarket-raw-ops-gate@.service \
  || $source == *.rollback.* ]] \
  || exit 76
exec /bin/mv "$@"
EOF
chmod 0755 "$supervisor_fake_bin/mv"
cat >"$supervisor_fake_bin/rm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
destination=${!#}
[[ ${FAKE_RM_FAIL_RECEIPT_READY:-0} != 1 \
  || $destination != */.receipt.json.ready ]] \
  || exit 75
exec /bin/rm "$@"
EOF
chmod 0755 "$supervisor_fake_bin/rm"

flock_probe="$supervisor_tmp/flock-probe.lock"
exec 7>"$flock_probe"
"$supervisor_fake_bin/flock" 7
exec 8>"$flock_probe"
if "$supervisor_fake_bin/flock" -n 8; then
  printf 'fake flock did not preserve the parent-held lock\n' >&2
  exit 1
fi
exec 7>&-
"$supervisor_fake_bin/flock" -n 8 || {
  printf 'fake flock did not release the closed parent lock\n' >&2
  exit 1
}
exec 8>&-

gate_control_env=(
  MONDAY_ALLOW_POLYMARKET_GATE_CONTROL_TEST_MODE=1
  MONDAY_POLYMARKET_GATE_CONTROL_TEST_ROOT="$supervisor_root"
  FAKE_SYSTEMCTL_STATE="$supervisor_state"
  FAKE_SYSTEMCTL_CALLS="$supervisor_calls"
  FAKE_GATE_CALLS="$supervisor_gate_calls"
  FAKE_GATE_CONTROL="$supervisor_control"
  FAKE_CONTROL_LOCK="$supervisor_root/run/monday/polymarket-raw-ops-gates/control.lock"
  PATH="$supervisor_fake_bin:$PATH"
)
gate_control() { env "${gate_control_env[@]}" "$supervisor_control" "$@"; }
set_supervisor_state() { printf '%s\n' "$2" >"$supervisor_state/$1"; }
reject() { if "$@" >/dev/null 2>&1; then return 1; fi; }
start_supervisor() {
  set_supervisor_state invocation "$1"
  set_supervisor_state active inactive
  gate_control start "$supervisor_candidate" "$supervisor_candidate_sha" \
    "$supervisor_source"
}
make_supervisor_ready() {
  local invocation=$1 evidence
  evidence="$supervisor_root/data/monday/evidence/polymarket-shadow-gates/$supervisor_candidate_sha/$invocation"
  mkdir -p "$evidence"
  jq -n --arg candidate "$supervisor_candidate_sha" \
    --arg source "$supervisor_source" --arg invocation "$invocation" \
    '{candidate_sha256:$candidate,deployment_source_revision:$source,
      shadow_run_id:$invocation,production_eligible:true,passed:true}' \
    >"$evidence/gate.json"
  (cd "$evidence" && sha256sum gate.json >.PASSED.sha256.ready)
  printf '%s\n' "$evidence"
}
assert_terminal() { jq -e --arg state "$2" '.phase == "terminal" and .terminal_state == $state' "$1" >/dev/null; }
assert_running_status() {
  gate_control status "$supervisor_candidate_sha" "$1" \
    | jq -e '.phase == "running" and .terminal_state == null' >/dev/null
}
assert_terminal_status() {
  gate_control status "$supervisor_candidate_sha" "$1" \
    | jq -e --arg state "$2" '.phase == "terminal" and .terminal_state == $state' >/dev/null
}
finalize_supervisor() {
  env "${gate_control_env[@]}" INVOCATION_ID="$1" SERVICE_RESULT="$2" \
    EXIT_CODE="$3" EXIT_STATUS="$4" "$supervisor_control" finalize \
    "$supervisor_candidate_sha"
}
installed_supervisor_unit="$supervisor_root/etc/systemd/system/${GATE_UNIT##*/}"
gate_control install >"$supervisor_tmp/install.json"
jq -e '.validated == true' "$supervisor_tmp/install.json" >/dev/null
cmp -s "$supervisor_control_dir/${GATE_UNIT##*/}" "$installed_supervisor_unit"
if compgen -G \
  "$supervisor_root/etc/systemd/system/.polymarket-raw-ops-gate@.service.*" \
  >/dev/null; then
  printf 'Gate unit install left a partial temporary file\n' >&2
  exit 1
fi
printf 'mismatch\n' >"$installed_supervisor_unit"
if env "${gate_control_env[@]}" FAKE_MV_FAIL_GATE_INSTALL=1 \
  "$supervisor_control" install >/dev/null 2>&1; then
  printf 'Gate unit installer ignored an atomic replacement failure\n' >&2
  exit 1
fi
[[ $(<"$installed_supervisor_unit") == mismatch ]]
if compgen -G \
  "$supervisor_root/etc/systemd/system/.polymarket-raw-ops-gate@.service.*" \
  >/dev/null; then
  printf 'failed Gate unit upgrade left a temporary or rollback file\n' >&2
  exit 1
fi
if env "${gate_control_env[@]}" FAKE_GATE_UNITS_RUNNING=1 \
  "$supervisor_control" install >/dev/null 2>&1; then
  printf 'Gate unit installer replaced a unit while a Gate was running\n' >&2
  exit 1
fi
[[ $(<"$installed_supervisor_unit") == mismatch ]]
gate_control install >"$supervisor_tmp/upgrade.json"
jq -e '.validated == true and .replaced == true
  and (.previous_sha256 | test("^[a-f0-9]{64}$"))' \
  "$supervisor_tmp/upgrade.json" >/dev/null
cmp -s "$supervisor_control_dir/${GATE_UNIT##*/}" "$installed_supervisor_unit"
if compgen -G \
  "$supervisor_root/etc/systemd/system/.polymarket-raw-ops-gate@.service.*" \
  >/dev/null; then
  printf 'Gate unit upgrade left a temporary or rollback file\n' >&2
  exit 1
fi
rm "$installed_supervisor_unit"
ln -s "$supervisor_control_dir/${GATE_UNIT##*/}" "$installed_supervisor_unit"
reject gate_control install
rm "$installed_supervisor_unit"
gate_control install >/dev/null

# A recovery admission is not a generic gate bypass: it starts only with the
# stopped direct bootstrap identity, a fresh exact candidate probe, and every
# uploader/timer contained. Every invocation publishes a distinct
# content-addressed admission record that is never replaced.
admission_record_dir="$supervisor_root/data/monday/evidence/polymarket-shadow-gates/$supervisor_candidate_sha"
admission_records_matching() {
  # Prints each per-invocation admission record matching the given jq filter;
  # all arguments pass through to jq -e.
  local file
  for file in "$admission_record_dir"/recovery-admission-*.json; do
    [[ -e $file && ! -L $file ]] || continue
    if jq -e "$@" "$file" >/dev/null 2>&1; then
      printf '%s\n' "$file"
    fi
  done
}
supervisor_recovery_gate_invocation=$(printf '9%.0s' {1..32})
set_supervisor_state invocation "$supervisor_recovery_gate_invocation"
set_supervisor_state active inactive
set_supervisor_state baseline-active inactive
gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe" >"$supervisor_tmp/recover-start.json"
jq -e --arg invocation "$supervisor_recovery_gate_invocation" '
  .phase == "running" and .systemd_invocation_id == $invocation
' "$supervisor_tmp/recover-start.json" >/dev/null
supervisor_recovery_request="$supervisor_root/data/monday/evidence/polymarket-gate-jobs/$supervisor_candidate_sha/$supervisor_recovery_gate_invocation/request.json"
jq -e --arg candidate "$supervisor_candidate_sha" --arg source "$supervisor_source" \
  --arg baseline "$supervisor_baseline_sha" '
  .recovery.mode == "gamma_closed_200"
  and .recovery.candidate_probe.candidate_sha256 == $candidate
  and .recovery.candidate_probe.source_revision == $source
  and .recovery.baseline.binary_sha256 == $baseline
  and .recovery.baseline.active_state == "inactive"
  and .recovery.baseline.main_pid == 0
' "$supervisor_recovery_request" >/dev/null
[[ $(admission_records_matching \
  --arg candidate "$supervisor_candidate_sha" --arg source "$supervisor_source" \
  --arg baseline "$supervisor_baseline_sha" '
  .schema == "monday.polymarket_gate_recovery_admission.v1"
  and .result == "admitted"
  and .refusal_reason == null
  and .candidate_sha256 == $candidate
  and .source_revision == $source
  and .baseline.binary_sha256 == $baseline
  and .baseline.active_state == "inactive"
  and .baseline.main_pid == 0
  and (.candidate_probe.sha256 | test("^[a-f0-9]{64}$"))
  and .reset_failed_units == []
' | grep -c .) -eq 1 ]]
if grep -Fq 'reset-failed' "$supervisor_calls"; then
  printf 'inactive recovery admission triggered reset-failed\n' >&2
  exit 1
fi
gate_control cancel "$supervisor_candidate_sha" "$supervisor_recovery_gate_invocation" >/dev/null

set_supervisor_state active inactive
set_supervisor_state baseline-active active
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe"
set_supervisor_state baseline-active inactive
set_supervisor_state uploader-active-polymarket-reference-upload.service active
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe"
set_supervisor_state uploader-active-polymarket-reference-upload.service inactive
set_supervisor_state baseline-exec '/opt/monday/bin/not-polymarket-raw-ops collect-reference'
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe"
set_supervisor_state baseline-exec '/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200'
jq '.observed_at = "1970-01-01T00:00:00Z"' "$supervisor_probe" >"$supervisor_probe.stale"
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe.stale"
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe_root/missing.json"
jq --arg wrong_candidate "$(printf 'f%.0s' {1..64})" \
  '.candidate_sha256 = $wrong_candidate' "$supervisor_probe" >"$supervisor_probe.wrong-candidate"
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe.wrong-candidate"
recovery_mutations=$(grep -E \
  '^(start|stop|restart|enable|disable) (polymarket-reference-collector[.]service|polymarket-reference-upload[.](service|timer)|polymarket-market-tape-upload[.](service|timer))$' \
  "$supervisor_calls" || true)
if [[ -n $recovery_mutations ]]; then
  printf 'recovery admission mutated the contained baseline: %s\n' \
    "$recovery_mutations" >&2
  exit 1
fi

# A refused recovery admission leaves durable evidence with exact identities
# and the refusal reason. Per-invocation records are content-addressed and
# never replaced: a second refusal for the same candidate leaves the first
# record available and unchanged.
set_supervisor_state baseline-active active
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe"
first_refusal_record=$(admission_records_matching \
  --arg candidate "$supervisor_candidate_sha" --arg source "$supervisor_source" '
  .result == "refused"
  and (.refusal_reason | test("baseline to be stopped"))
  and .candidate_sha256 == $candidate
  and .source_revision == $source
  and .baseline == null
' | sort | sed -n '$p')
[[ -n $first_refusal_record ]]
first_refusal_sha=$(sha256sum "$first_refusal_record" | awk '{print $1}')
set_supervisor_state baseline-active inactive
set_supervisor_state uploader-active-polymarket-reference-upload.service active
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe"
set_supervisor_state uploader-active-polymarket-reference-upload.service inactive
[[ $(admission_records_matching \
  '.result == "refused" and (.refusal_reason | test("inactive uploader/timer"))' \
  | grep -c .) -ge 1 ]]
[[ -f $first_refusal_record && ! -L $first_refusal_record ]]
[[ $(sha256sum "$first_refusal_record" | awk '{print $1}') == "$first_refusal_sha" ]]

# Activating and deactivating baselines remain refused; only inactive or a
# contained failed state can be admitted.
for blocked_state in activating deactivating; do
  set_supervisor_state baseline-active "$blocked_state"
  reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
    "$supervisor_source" "$supervisor_probe"
done
set_supervisor_state baseline-active inactive

# A failed baseline whose governed reset-failed cannot reach inactive remains
# refused and leaves durable evidence.
set_supervisor_state baseline-active failed
reject env "${gate_control_env[@]}" FAKE_RESET_FAILED_STUCK=1 \
  "$supervisor_control" recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe"
[[ $(admission_records_matching \
  '.result == "refused" and (.refusal_reason | test("after reset-failed"))' \
  | grep -c .) -ge 1 ]]

# Containment is proven before any governed reset: a failed baseline or
# uploader with a managed process is refused and no reset-failed is issued.
: >"$supervisor_calls"
set_supervisor_state baseline-active failed
set_supervisor_state baseline-main-pid 4242
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe"
if grep -Fq 'reset-failed' "$supervisor_calls"; then
  printf 'recovery reset a baseline that still has a managed process\n' >&2
  exit 1
fi
[[ $(admission_records_matching \
  '.result == "refused" and (.refusal_reason | test("managed process"))' \
  | grep -c .) -ge 1 ]]
set_supervisor_state baseline-active inactive
set_supervisor_state baseline-main-pid 0
printf 'failed\n' >"$supervisor_state/uploader-active-polymarket-market-tape-upload.timer"
printf '4242\n' >"$supervisor_state/uploader-main-pid-polymarket-market-tape-upload.timer"
: >"$supervisor_calls"
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe"
if grep -Fq 'reset-failed' "$supervisor_calls"; then
  printf 'recovery reset an uploader that still has a managed process\n' >&2
  exit 1
fi
set_supervisor_state uploader-active-polymarket-market-tape-upload.timer inactive
set_supervisor_state uploader-main-pid-polymarket-market-tape-upload.timer 0

# A failed baseline is contained: admission performs a governed reset-failed
# inside the control lock and records the post-reset snapshot.
: >"$supervisor_calls"
set_supervisor_state baseline-active failed
set_supervisor_state baseline-restarts 5
supervisor_failed_baseline_invocation=$(printf 'd%.0s' {1..32})
set_supervisor_state invocation "$supervisor_failed_baseline_invocation"
set_supervisor_state active inactive
gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe" >/dev/null
grep -Fqx 'reset-failed polymarket-reference-collector.service' "$supervisor_calls"
failed_baseline_request="$supervisor_root/data/monday/evidence/polymarket-gate-jobs/$supervisor_candidate_sha/$supervisor_failed_baseline_invocation/request.json"
jq -e '
  .recovery.baseline.active_state == "inactive"
  and .recovery.baseline.main_pid == 0
  and .recovery.baseline.restarts == 0
' "$failed_baseline_request" >/dev/null
[[ $(admission_records_matching --arg baseline "$supervisor_baseline_sha" '
  .result == "admitted"
  and .baseline.binary_sha256 == $baseline
  and .baseline.active_state == "inactive"
  and .baseline.main_pid == 0
  and (.reset_failed_units | index("polymarket-reference-collector.service") != null)
' | grep -c .) -eq 1 ]]
gate_control cancel "$supervisor_candidate_sha" \
  "$supervisor_failed_baseline_invocation" >/dev/null
set_supervisor_state baseline-active inactive
set_supervisor_state baseline-restarts 2

# systemd clears the invocation ID of an inactive Type=simple baseline: a
# clean stop clears it, and the governed reset-failed of a failed unit clears
# it as well (observed on systemd 259). Admission binds the recorded empty
# identity verbatim; a malformed ID remains refused.
: >"$supervisor_state/baseline-invocation"
supervisor_empty_id_invocation=$(printf '0%.0s' {1..32})
set_supervisor_state invocation "$supervisor_empty_id_invocation"
set_supervisor_state active inactive
gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe" >/dev/null
empty_id_request="$supervisor_root/data/monday/evidence/polymarket-gate-jobs/$supervisor_candidate_sha/$supervisor_empty_id_invocation/request.json"
jq -e '.recovery.baseline.invocation_id == ""
  and .recovery.baseline.active_state == "inactive"
  and .recovery.baseline.main_pid == 0
' "$empty_id_request" >/dev/null
[[ $(admission_records_matching '
  .result == "admitted" and .baseline.invocation_id == ""
' | grep -c .) -eq 1 ]]
gate_control cancel "$supervisor_candidate_sha" \
  "$supervisor_empty_id_invocation" >/dev/null
printf 'not-a-valid-invocation-id\n' >"$supervisor_state/baseline-invocation"
reject gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" "$supervisor_probe"
[[ $(admission_records_matching \
  '.result == "refused" and (.refusal_reason | test("invocation ID"))' \
  | grep -c .) -ge 1 ]]
printf '%s\n' "$(printf 'a%.0s' {1..32})" >"$supervisor_state/baseline-invocation"
set_supervisor_state baseline-restarts 2

# Each failed uploader unit is admitted through a governed reset-failed inside
# the control lock, exactly like the failed baseline.
failed_uploader_letters=(c e f a)
failed_uploader_index=0
for failed_uploader in \
  polymarket-reference-upload.service polymarket-reference-upload.timer \
  polymarket-market-tape-upload.service polymarket-market-tape-upload.timer; do
  printf 'failed\n' >"$supervisor_state/uploader-active-$failed_uploader"
  supervisor_failed_uploader_invocation=$(printf \
    "${failed_uploader_letters[$failed_uploader_index]}%.0s" {1..32})
  failed_uploader_index=$((failed_uploader_index + 1))
  set_supervisor_state invocation "$supervisor_failed_uploader_invocation"
  set_supervisor_state active inactive
  gate_control recover "$supervisor_candidate" "$supervisor_candidate_sha" \
    "$supervisor_source" "$supervisor_probe" >/dev/null
  grep -Fqx "reset-failed $failed_uploader" "$supervisor_calls"
  [[ $(admission_records_matching --arg unit "$failed_uploader" '
    .result == "admitted"
    and (.reset_failed_units | index($unit) != null)
  ' | grep -c .) -eq 1 ]]
  gate_control cancel "$supervisor_candidate_sha" \
    "$supervisor_failed_uploader_invocation" >/dev/null
  set_supervisor_state "uploader-active-$failed_uploader" inactive
done
set_supervisor_state baseline-active inactive

# A fresh probe admits once; its binding remains valid through the 3600-second Gate.
recovery_binding_contract="$supervisor_tmp/recovery-binding-contract.sh"
sed -n '/^verify_recovery_binding() {$/,/^}$/p' "$GATE" >"$recovery_binding_contract"
sed -n '/^verify_recovery_admission() {$/,/^}$/p' "$GATE" >>"$recovery_binding_contract"
sed -n '/^verify_contained_recovery_baseline() {$/,/^}$/p' "$GATE" >>"$recovery_binding_contract"
(
  set -euo pipefail
  RUST_ACTIVE_BINARY="$supervisor_baseline" LEGACY_UNIT=polymarket-reference-collector.service
  supervisor_baseline_invocation=$(<"$supervisor_state/baseline-invocation")
  recovery_active_state=inactive recovery_fragment=/etc/systemd/system/polymarket-reference-collector.service
  recovery_drop_ins='' recovery_exec='/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200'
  recovery_restarts=2 recovery_invocation=$supervisor_baseline_invocation recovery_binary_sha=$supervisor_baseline_sha
  recovery_binary_secure=true
  recovery=$(jq -c .recovery "$supervisor_recovery_request")
  recovery_observed_epoch=1000 recovery_now=$recovery_observed_epoch
  date() {
    [[ $1 == -u ]] || { command date "$@"; return; }
    case "$2" in -d) printf '%s\n' "$recovery_observed_epoch" ;; +%s) printf '%s\n' "$recovery_now" ;; *) command date "$@" ;; esac
  }
  systemctl() {
    case "$*" in
      *ActiveState*) printf '%s\n' "$recovery_active_state" ;; *MainPID*) printf '0\n' ;;
      *FragmentPath*) printf '%s\n' "$recovery_fragment" ;;
      *DropInPaths*) printf '%s\n' "$recovery_drop_ins" ;; *NRestarts*) printf '%s\n' "$recovery_restarts" ;;
      *InvocationID*) printf '%s\n' "$recovery_invocation" ;; *) return 1 ;;
    esac
  }
  effective_exec_argv() { printf '%s\n' "$recovery_exec"; }
  secure_control_file() { [[ $recovery_binary_secure == true ]]; }
  sha256sum() { printf '%s  %s\n' "$recovery_binary_sha" "$1"; }
  # shellcheck source=/dev/null
  source "$recovery_binding_contract"
  verify_recovery_admission "$recovery" "$supervisor_candidate_sha" "$supervisor_source"
  recovery_now=$((recovery_observed_epoch + 901))
  if verify_recovery_admission "$recovery" "$supervisor_candidate_sha" "$supervisor_source"; then
    printf 'recovery admission accepted a stale probe\n' >&2; exit 1
  fi
  verify_contained_recovery_baseline "$recovery" "$supervisor_candidate_sha" "$supervisor_source" || {
    printf 'recovery identity check aged a valid Gate probe\n' >&2; exit 1
  }
  recovery_binary_secure=false
  if verify_contained_recovery_baseline "$recovery" "$supervisor_candidate_sha" "$supervisor_source"; then
    printf 'recovery identity check accepted an insecure baseline binary\n' >&2; exit 1
  fi
  recovery_binary_secure=true
  # A contained simple baseline has a systemd-cleared (empty) invocation ID:
  # the recorded empty identity verifies against an empty live value, and an
  # ID assigned by a later start is drift.
  recovery_empty_invocation=$(jq -c '.baseline.invocation_id = ""' <<<"$recovery")
  recovery_invocation=''
  verify_recovery_binding "$recovery_empty_invocation" \
    "$supervisor_candidate_sha" "$supervisor_source"
  verify_contained_recovery_baseline "$recovery_empty_invocation" \
    "$supervisor_candidate_sha" "$supervisor_source"
  recovery_invocation=$supervisor_baseline_invocation
  if verify_contained_recovery_baseline "$recovery_empty_invocation" \
    "$supervisor_candidate_sha" "$supervisor_source"; then
    printf 'recovery identity check accepted an assigned ID over recorded empty\n' >&2
    exit 1
  fi
  for recovery_drift in active fragment drop_ins exec restarts invocation binary; do
    recovery_active_state=inactive recovery_fragment=/etc/systemd/system/polymarket-reference-collector.service
    recovery_drop_ins='' recovery_exec='/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200'
    recovery_restarts=2 recovery_invocation=$supervisor_baseline_invocation recovery_binary_sha=$supervisor_baseline_sha
    case "$recovery_drift" in
      active) recovery_active_state=active ;; fragment) recovery_fragment=/tmp/wrong.service ;;
      drop_ins) recovery_drop_ins=/etc/systemd/system/recovery.conf ;;
      exec) recovery_exec='/opt/monday/bin/not-polymarket-raw-ops collect-reference' ;;
      restarts) recovery_restarts=3 ;; invocation) recovery_invocation=$(printf 'b%.0s' {1..32}) ;;
      binary) recovery_binary_sha=$(printf 'c%.0s' {1..64}) ;;
    esac
    if verify_contained_recovery_baseline "$recovery" "$supervisor_candidate_sha" "$supervisor_source"; then
      printf 'recovery identity check accepted %s drift\n' "$recovery_drift" >&2; exit 1
    fi
  done
)
: >"$supervisor_calls"

# Normal Gate requests intentionally omit recovery metadata. Both consumers
# must preserve literal null rather than turning that optional value into a
# jq failure.
normal_optional_gate_contract="$supervisor_tmp/normal-optional-gate.sh"
sed -n '/^recovery_json=$(jq -c.*\.recovery \/\/ null/,/^legacy_pid=0$/p' \
  "$GATE" >"$normal_optional_gate_contract"
normal_optional_cutover_contract="$supervisor_tmp/normal-optional-cutover.sh"
sed -n '/^recovery_json=$(jq -c.*\.recovery \/\/ null/,/^contained_recovery=false$/p' \
  "$CUTOVER" >"$normal_optional_cutover_contract"
[[ -s $normal_optional_gate_contract && -s $normal_optional_cutover_contract ]] || {
  printf 'optional recovery consumers are missing\n' >&2
  exit 1
}
normal_optional_dir="$supervisor_tmp/normal-optional"
mkdir "$normal_optional_dir"
printf '%s\n' '{"schema":"monday.polymarket_gate_request.v1"}' \
  >"$normal_optional_dir/request.json"
if ! (
  set -euo pipefail
  MONDAY_POLYMARKET_GATE_INVOCATION_DIR="$normal_optional_dir"
  die() { exit 77; }
  # shellcheck source=/dev/null
  source "$normal_optional_gate_contract"
  [[ $recovery_json == null && $legacy_pid == 0 ]]
); then
  printf 'normal Gate request rejected an absent recovery binding\n' >&2
  exit 1
fi
if ! (
  set -euo pipefail
  gate_json="$normal_optional_dir/request.json"
  die() { exit 77; }
  # shellcheck source=/dev/null
  source "$normal_optional_cutover_contract"
  [[ $recovery_json == null && $contained_recovery == false ]]
); then
  printf 'normal cutover rejected an absent recovery binding\n' >&2
  exit 1
fi

supervisor_env_file="$supervisor_root/run/monday/polymarket-raw-ops-gates/$supervisor_candidate_sha.env"
set_supervisor_state active inactive
if env "${gate_control_env[@]}" FAKE_START_REJECT=1 \
  "$supervisor_control" start "$supervisor_candidate" \
  "$supervisor_candidate_sha" "$supervisor_source" >/dev/null 2>&1; then
  printf 'rejected Gate start unexpectedly passed\n' >&2
  exit 1
fi
[[ ! -e $supervisor_env_file ]]
set_supervisor_state active inactive
if env "${gate_control_env[@]}" FAKE_START_REJECT=1 \
  FAKE_START_REJECT_ACTIVE=1 "$supervisor_control" start \
  "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source" >/dev/null 2>&1; then
  printf 'active rejected Gate start unexpectedly passed\n' >&2
  exit 1
fi
[[ -f $supervisor_env_file ]]
rm -f -- "$supervisor_env_file"
set_supervisor_state active inactive

supervisor_starts_before_normal=$(grep -Fxc "start $supervisor_unit" "$supervisor_calls" || true)
start_supervisor "$supervisor_invocation" >"$supervisor_tmp/start.json"
jq -e --arg unit "$supervisor_unit" --arg invocation "$supervisor_invocation" '
  .unit == $unit and .systemd_invocation_id == $invocation
  and .phase == "running" and .terminal_state == null' \
  "$supervisor_tmp/start.json" >/dev/null
supervisor_starts_after_normal=$(grep -Fxc "start $supervisor_unit" "$supervisor_calls")
[[ $supervisor_starts_after_normal -eq $((supervisor_starts_before_normal + 1)) ]]
[[ -f $supervisor_env_file ]]
assert_running_status "$supervisor_invocation"
reject gate_control start "$supervisor_candidate" "$supervisor_candidate_sha" \
  "$supervisor_source"
[[ $(grep -Fxc "start $supervisor_unit" "$supervisor_calls") == "$supervisor_starts_after_normal" ]]
if env "${gate_control_env[@]}" INVOCATION_ID="$supervisor_invocation" \
  FAKE_GATE_EXIT=17 "$supervisor_control" run "$supervisor_candidate_sha" \
  >/dev/null 2>&1; then
  printf 'fake Gate unexpectedly passed\n' >&2
  exit 1
else
  supervisor_run_status=$?
fi
[[ $supervisor_run_status == 17 ]]
grep -Fqx \
  "$supervisor_candidate $supervisor_candidate_sha $supervisor_source|$supervisor_invocation|$supervisor_invocation" \
  "$supervisor_gate_calls"
set_supervisor_state active inactive
failed_evidence=$(make_supervisor_ready "$supervisor_invocation")
printf 'partial\n' >"$failed_evidence/..PASSED.sha256.ready.tmp"
FAKE_SHADOW_STOP_FAIL=1 finalize_supervisor "$supervisor_invocation" \
  exit-code exited 17 \
  >"$supervisor_tmp/failed.json"
assert_terminal "$supervisor_tmp/failed.json" failed
[[ ! -e $supervisor_env_file ]]
[[ $(<"$supervisor_state/shadow") == inactive ]]
[[ ! -e $failed_evidence/PASSED.sha256 \
  && ! -e $failed_evidence/.PASSED.sha256.ready \
  && ! -e $failed_evidence/..PASSED.sha256.ready.tmp ]]
assert_terminal_status "$supervisor_invocation" failed
supervisor_start_query_invocation=$(printf '4%.0s' {1..32})
set_supervisor_state invocation "$supervisor_start_query_invocation"
set_supervisor_state active inactive
reject env "${gate_control_env[@]}" FAKE_SHOW_FAIL=InvocationID \
  "$supervisor_control" start "$supervisor_candidate" \
  "$supervisor_candidate_sha" "$supervisor_source"
[[ $(<"$supervisor_state/active") == inactive ]]
start_query_dir="$supervisor_root/data/monday/evidence/polymarket-gate-jobs/$supervisor_candidate_sha/$supervisor_start_query_invocation"
assert_terminal "$start_query_dir/receipt.json" failed
reject env "${gate_control_env[@]}" FAKE_SHOW_FAIL=ActiveState \
  "$supervisor_control" start "$supervisor_candidate" \
  "$supervisor_candidate_sha" "$supervisor_source"
supervisor_finalize_query_invocation=$(printf '5%.0s' {1..32})
start_supervisor "$supervisor_finalize_query_invocation" >/dev/null
set_supervisor_state shadow active
finalize_query_evidence=$(make_supervisor_ready \
  "$supervisor_finalize_query_invocation")
set_supervisor_state active inactive
env "${gate_control_env[@]}" INVOCATION_ID="$supervisor_finalize_query_invocation" \
  SERVICE_RESULT=success EXIT_CODE=exited EXIT_STATUS=0 \
  FAKE_SHOW_FAIL=ActiveState "$supervisor_control" finalize \
  "$supervisor_candidate_sha" >"$supervisor_tmp/finalize-query-failed.json"
assert_terminal "$supervisor_tmp/finalize-query-failed.json" failed
jq -e '
  .shadow.stop_result == "success"
  and .shadow.containment == "unverified"
  and .shadow.active_state == "query-error"
  and .shadow.main_pid == "0"
' "$supervisor_tmp/finalize-query-failed.json" >/dev/null
[[ ! -e $finalize_query_evidence/PASSED.sha256 \
  && ! -e $finalize_query_evidence/.PASSED.sha256.ready ]]
assert_terminal_status "$supervisor_finalize_query_invocation" failed
supervisor_cancel_invocation=$(printf '2%.0s' {1..32})
start_supervisor "$supervisor_cancel_invocation" >/dev/null
set_supervisor_state shadow active
cancel_evidence=$(make_supervisor_ready "$supervisor_cancel_invocation")
gate_control cancel "$supervisor_candidate_sha" \
  "$supervisor_cancel_invocation" >"$supervisor_tmp/cancelled.json"
assert_terminal "$supervisor_tmp/cancelled.json" cancelled
cancel_dir="$supervisor_root/data/monday/evidence/polymarket-gate-jobs/$supervisor_candidate_sha/$supervisor_cancel_invocation"
[[ -f $cancel_dir/cancel.requested ]]
[[ ! -e $cancel_evidence/PASSED.sha256 \
  && ! -e $cancel_evidence/.PASSED.sha256.ready ]]
[[ $(<"$supervisor_state/shadow") == inactive ]]

supervisor_prepare_invocation=$(printf '8%.0s' {1..32})
start_supervisor "$supervisor_prepare_invocation" >/dev/null
set_supervisor_state shadow active
prepare_evidence=$(make_supervisor_ready "$supervisor_prepare_invocation")
set_supervisor_state active inactive
prepare_dir="$supervisor_root/data/monday/evidence/polymarket-gate-jobs/$supervisor_candidate_sha/$supervisor_prepare_invocation"
if env "${gate_control_env[@]}" \
  INVOCATION_ID="$supervisor_prepare_invocation" \
  SERVICE_RESULT=success EXIT_CODE=exited EXIT_STATUS=0 \
  FAKE_MV_FAIL_RECEIPT_STAGE=1 \
  "$supervisor_control" finalize "$supervisor_candidate_sha" >/dev/null 2>&1; then
  printf 'receipt staging interruption unexpectedly passed\n' >&2
  exit 1
fi
[[ ! -e $prepare_evidence/PASSED.sha256 \
  && -f $prepare_evidence/.PASSED.sha256.ready \
  && ! -e $prepare_dir/receipt.json \
  && ! -e $prepare_dir/.receipt.json.ready \
  && -f $prepare_dir/.receipt.json.tmp ]]
gate_control status "$supervisor_candidate_sha" \
  "$supervisor_prepare_invocation" >"$supervisor_tmp/prepare-recovered.json"
assert_terminal "$supervisor_tmp/prepare-recovered.json" passed
[[ ! -e $supervisor_env_file ]]
[[ -f $prepare_evidence/PASSED.sha256 \
  && -f $prepare_dir/receipt.json \
  && ! -e $prepare_dir/.receipt.json.ready \
  && ! -e $prepare_dir/.receipt.json.tmp ]]

supervisor_precommit_invocation=$(printf '7%.0s' {1..32})
start_supervisor "$supervisor_precommit_invocation" >/dev/null
set_supervisor_state shadow active
precommit_evidence=$(make_supervisor_ready "$supervisor_precommit_invocation")
set_supervisor_state active inactive
precommit_dir="$supervisor_root/data/monday/evidence/polymarket-gate-jobs/$supervisor_candidate_sha/$supervisor_precommit_invocation"
if env "${gate_control_env[@]}" \
  INVOCATION_ID="$supervisor_precommit_invocation" \
  SERVICE_RESULT=success EXIT_CODE=exited EXIT_STATUS=0 \
  FAKE_MV_FAIL_RECEIPT=1 \
  "$supervisor_control" finalize "$supervisor_candidate_sha" >/dev/null 2>&1; then
  printf 'pre-receipt Gate finalization interruption unexpectedly passed\n' >&2
  exit 1
fi
[[ ! -e $precommit_evidence/PASSED.sha256 \
  && -f $precommit_evidence/.PASSED.sha256.ready \
  && ! -e $precommit_dir/receipt.json \
  && -f $precommit_dir/.receipt.json.ready \
  && -f $precommit_dir/.receipt.json.commit ]]
gate_control status "$supervisor_candidate_sha" \
  "$supervisor_precommit_invocation" >"$supervisor_tmp/precommit-recovered.json"
assert_terminal "$supervisor_tmp/precommit-recovered.json" passed
[[ -f $precommit_evidence/PASSED.sha256 \
  && -f $precommit_dir/receipt.json \
  && ! -e $precommit_dir/.receipt.json.ready \
  && ! -e $precommit_dir/.receipt.json.commit ]]

supervisor_recovery_invocation=$(printf '6%.0s' {1..32})
start_supervisor "$supervisor_recovery_invocation" >/dev/null
set_supervisor_state shadow active
recovery_evidence=$(make_supervisor_ready "$supervisor_recovery_invocation")
set_supervisor_state active inactive
recovery_dir="$supervisor_root/data/monday/evidence/polymarket-gate-jobs/$supervisor_candidate_sha/$supervisor_recovery_invocation"
if env "${gate_control_env[@]}" \
  INVOCATION_ID="$supervisor_recovery_invocation" \
  SERVICE_RESULT=success EXIT_CODE=exited EXIT_STATUS=0 \
  FAKE_RM_FAIL_RECEIPT_READY=1 \
  "$supervisor_control" finalize "$supervisor_candidate_sha" >/dev/null 2>&1; then
  printf 'interrupted Gate finalization unexpectedly passed\n' >&2
  exit 1
fi
[[ -f $recovery_evidence/PASSED.sha256 \
  && -f $recovery_dir/receipt.json \
  && -f $recovery_dir/.receipt.json.ready ]]
gate_control status "$supervisor_candidate_sha" \
  "$supervisor_recovery_invocation" >"$supervisor_tmp/recovered.json"
assert_terminal "$supervisor_tmp/recovered.json" passed
[[ -f $recovery_dir/receipt.json \
  && ! -e $recovery_dir/.receipt.json.ready ]]

supervisor_pass_invocation=$(printf '3%.0s' {1..32})
start_supervisor "$supervisor_pass_invocation" >/dev/null
set_supervisor_state shadow active
pass_evidence=$(make_supervisor_ready "$supervisor_pass_invocation")
set_supervisor_state active inactive
finalize_supervisor "$supervisor_pass_invocation" success exited 0 \
  >"$supervisor_tmp/passed.json"
assert_terminal "$supervisor_tmp/passed.json" passed
(cd "$pass_evidence" && sha256sum --check --strict PASSED.sha256 >/dev/null)
[[ $(<"$supervisor_state/shadow") == inactive ]]
rm "$pass_evidence/PASSED.sha256"
reject gate_control status "$supervisor_candidate_sha" \
  "$supervisor_pass_invocation"
if grep -Fq 'polymarket-reference-collector.service' "$supervisor_calls"; then
  printf 'Gate supervisor mutated the legacy baseline\n' >&2
  exit 1
fi

rm -rf "$supervisor_tmp"
trap - EXIT
grep -Fxq 'export TZ=UTC' "$GATE" || {
  printf 'Gate does not force UTC for jq date builtins\n' >&2
  exit 1
}
cargo build --quiet --manifest-path "$RUST_MANIFEST" -p hft-collector \
  --bin polymarket-raw-ops --no-default-features --locked
"$VERIFY" verify-shadow-parity --help >/dev/null

[[ -f $POLYMARKET_COMPILER_DOCKERFILE ]] || {
  printf 'missing Polymarket evidence compiler Dockerfile\n' >&2
  exit 1
}
grep -Fq 'polymarket-evidence-compiler' "$WORKFLOW"
grep -Fq 'Dockerfile.polymarket-evidence-compiler' "$WORKFLOW"
grep -Fq 'org.opencontainers.image.revision=${{ needs.selector.outputs.source_sha }}' "$WORKFLOW"
grep -Fq 'Verify Polymarket compiler source binding' "$WORKFLOW"
grep -Fq '/usr/local/bin/polymarket-raw-ops' "$POLYMARKET_COMPILER_DOCKERFILE"
if grep -Fq '/usr/local/bin/binance-lob-archiver' "$POLYMARKET_COMPILER_DOCKERFILE"; then
  printf 'Polymarket evidence compiler image includes a Binance LOB executable\n' >&2
  exit 1
fi

tmp_dir=$(mktemp -d)
tmp_dir=$(cd -- "$tmp_dir" && pwd -P)
trap 'rm -rf "$tmp_dir"' EXIT

# A production Gate must exercise the candidate against a real closed segment,
# not a compatible fixture manufactured by the Gate itself.
preflight_verifier="$tmp_dir/real-market-preflight.sh"
sed -n \
  -e '/^readonly REAL_MARKET_PREFLIGHT_BUDGET_SECONDS=/p' \
  -e '/^readonly PREFLIGHT_SCAN_WINDOW_RECORDS=/p' \
  -e '/^remaining_seconds_before_deadline() {$/,/^}$/p' \
  -e '/^run_before_deadline() {$/,/^}$/p' \
  -e '/^oss_download_with_retry() {$/,/^}$/p' "$GATE" \
  >"$preflight_verifier"
sed -n '/^download_and_verify_oss_triplet() {$/,/^}$/p' "$GATE" \
  >>"$preflight_verifier"
sed -n '/^real_market_segment_preflight() {$/,/^}$/p' "$GATE" \
  >>"$preflight_verifier"
# shellcheck source=/dev/null
source "$preflight_verifier"
if ! declare -F download_and_verify_oss_triplet >/dev/null \
  || ! declare -F real_market_segment_preflight >/dev/null; then
  printf 'Gate does not expose the real market-segment preflight helpers\n' >&2
  exit 1
fi
secure_collector_directory() {
  [[ ${INSECURE_SOURCE_SPOOL:-} != "$1" ]]
}

preflight_root="$tmp_dir/real-market-preflight"
remote_root="$preflight_root/remote"
fake_bin="$preflight_root/bin"
mkdir -p "$remote_root" "$fake_bin"

make_remote_triplet() {
  local source=$1 uri=$2 dataset=$3 remote_data remote_manifest
  local data_sha data_bytes source_bytes
  remote_data="$remote_root/${uri#oss://bucket/}"
  remote_manifest="${remote_data}.manifest.json"
  mkdir -p "${remote_data%/*}"
  zstd -q -T1 -3 -c "$source" >"$remote_data"
  data_sha=$(sha256sum "$remote_data" | awk '{print $1}')
  data_bytes=$(wc -c <"$remote_data" | tr -d ' ')
  source_bytes=$(wc -c <"$source" | tr -d ' ')
  jq -n --arg dataset "$dataset" --arg file "${remote_data##*/}" \
    --arg sha "$data_sha" --argjson bytes "$data_bytes" \
    --argjson source_bytes "$source_bytes" \
    '{venue:"polymarket",dataset:$dataset,file:$file,bytes:$bytes,
      sha256:$sha,source_bytes:$source_bytes,canonical:true,
      segment_complete:true,source_session_closed:true,sequence_gaps:0}' \
    >"$remote_manifest"
  printf '%s\n' "$data_sha" >"${remote_data}._SUCCESS"
}

cat >"$fake_bin/aliyun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ $1 == ossutil ]]
case "$2" in
  cp)
    if [[ $3 == oss://bucket/* ]]; then
      cp "$FAKE_OSS_ROOT/${3#oss://bucket/}" "$4"
    elif [[ $4 == oss://bucket/* ]]; then
      remote="$FAKE_OSS_ROOT/${4#oss://bucket/}"
      mkdir -p "${remote%/*}"
      [[ -e $remote ]] || cp "$3" "$remote"
    else
      exit 2
    fi
    ;;
  ls)
    remote="$FAKE_OSS_ROOT/${3#oss://bucket/}"
    [[ ! -e $remote ]] || printf '%s\n' "$3"
    ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$fake_bin/aliyun"

cat >"$fake_bin/runuser" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
while [[ $1 != -- ]]; do shift; done
shift
exec "$@"
EOF
chmod +x "$fake_bin/runuser"

cat >"$fake_bin/chown" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$fake_bin/chown"

preflight_stat=$(command -v gstat || command -v stat)
cat >"$fake_bin/stat" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
last=${!#}
if [[ -n ${UNSTABLE_STAT_PATH:-} && $last == "$UNSTABLE_STAT_PATH" \
  && ${1:-} == -c && ${2:-} == '%d:%i:%s:%Y:%Z' ]]; then
  count=0
  [[ ! -f $UNSTABLE_STAT_COUNTER ]] || count=$(<"$UNSTABLE_STAT_COUNTER")
  count=$((count + 1))
  printf '%s\n' "$count" >"$UNSTABLE_STAT_COUNTER"
  printf '%s:%s\n' "$("$PREFLIGHT_STAT" "$@")" "$count"
  exit 0
fi
exec "$PREFLIGHT_STAT" "$@"
EOF
chmod +x "$fake_bin/stat"

cat >"$preflight_root/candidate" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
spool= dataset= bucket=
while (($#)); do
  case "$1" in
    --spool-dir) spool=$2; shift 2 ;;
    --dataset) dataset=$2; shift 2 ;;
    --bucket) bucket=$2; shift 2 ;;
    *) shift ;;
  esac
done
source_file=$(find "$spool" -maxdepth 1 -type f -name 'market-updates.*.ndjson' \
  | head -n 1)
canonical_count=${FAKE_CANONICAL_COUNT:-1}
if jq -e 'select(.update.kind == "quote"
    and (.update.request_status != "success"
      or (.update.collection_result | type != "string")))' \
    "$source_file" >/dev/null; then
  printf 'quote requires request_status=success\n' >&2
  exit 1
fi
name=market-updates.20260101T010000.20260101T00.ndjson.zst
sha=$(jq -er .sha256 "$FAKE_CANDIDATE_TEMPLATE/$name.manifest.json")
relative="lake/raw/venue=polymarket/dataset=$dataset/date=2026-01-01/hour=00/sha256=$sha/$name"
destination="$FAKE_OSS_ROOT/$relative"
mkdir -p "${destination%/*}"
cp "$FAKE_CANDIDATE_TEMPLATE/$name" "$destination"
jq --arg dataset "$dataset" '.dataset = $dataset' \
  "$FAKE_CANDIDATE_TEMPLATE/$name.manifest.json" \
  >"$destination.manifest.json"
cp "$FAKE_CANDIDATE_TEMPLATE/$name._SUCCESS" "$destination._SUCCESS"
uri="oss://$bucket/$relative"
jq -n --arg uri "$uri" --argjson canonical_count "$canonical_count" \
  '{updated_at:"2026-01-01T01:00:00Z",last_success_at:"2026-01-01T01:00:00Z",
    last_uploaded_object:$uri,uploaded_segments:1,
    canonical_uploaded_segments:$canonical_count,
    pending_segments:0,failed_segments:[],last_error:null,last_error_at:null}' \
  >"$spool/upload-status.json"
jq -cn --argjson canonical_count "$canonical_count" \
  '{uploaded_segments:1,canonical_uploaded_segments:$canonical_count}'
EOF
chmod +x "$preflight_root/candidate"

incompatible="$preflight_root/incompatible.ndjson"
compatible="$preflight_root/compatible.ndjson"
noncanonical="$preflight_root/noncanonical.ndjson"
no_quote="$preflight_root/no-quote.ndjson"
cross_hour="$preflight_root/cross-hour.ndjson"
unrelated="$preflight_root/unrelated.ndjson"
printf '%s\n' \
  '{"sequence":0,"recorded_at":"2026-01-01T00:00:00Z","update":{"kind":"event_discovered","event_id":"event-1","symbol":"BTCUSDT","up_token":"up","down_token":"down","end_time":"2026-01-01T00:05:00Z","window_secs":300,"price_to_beat":"100","resolved_up_won":null}}' \
  '{"sequence":1,"recorded_at":"2026-01-01T00:00:01Z","update":{"kind":"quote","token_id":"up","bid":"0.49","ask":"0.51","bid_size":"10","ask_size":"10","bid_levels":[],"ask_levels":[],"ts":"2026-01-01T00:00:01Z"}}' \
  >"$incompatible"
printf '%s\n' \
  '{"sequence":0,"recorded_at":"2026-01-01T00:00:00Z","update":{"kind":"event_discovered","event_id":"event-1","symbol":"BTCUSDT","up_token":"up","down_token":"down","end_time":"2026-01-01T00:05:00Z","window_secs":300,"price_to_beat":"100","resolved_up_won":null}}' \
  '{"sequence":1,"recorded_at":"2026-01-01T00:00:01Z","update":{"kind":"quote","token_id":"up","bid":"0.49","ask":"0.51","bid_size":"10","ask_size":"10","bid_levels":[],"ask_levels":[],"request_status":"success","collection_result":"executable","ts":"2026-01-01T00:00:01Z"}}' \
  '{"sequence":2,"recorded_at":"2026-01-01T00:00:01Z","update":{"kind":"quote","token_id":"down","bid":"0.49","ask":"0.51","bid_size":"10","ask_size":"10","bid_levels":[],"ask_levels":[],"request_status":"success","collection_result":"executable","ts":"2026-01-01T00:00:01Z"}}' \
  >"$compatible"
printf '%s\n' \
  "$(head -n 1 "$compatible")" \
  "$(sed -n '2p' "$compatible")" \
  '{"sequence":2,"recorded_at":"2026-01-01T00:00:02Z","update":{"kind":"quote_collection_failure","token_id":"down","request_status":"failure","collection_result":"api_failure","request_started_at":"2026-01-01T00:00:01.900Z","http_status":null,"error_kind":"websocket_receive","ts":"2026-01-01T00:00:02Z"}}' \
  >"$noncanonical"
head -n 1 "$compatible" >"$no_quote"
jq -c 'if .sequence == 2 then
  .recorded_at = "2026-01-01T01:00:01Z"
  | .update.ts = "2026-01-01T01:00:01Z"
else . end' "$compatible" >"$cross_hour"
sed 's/"token_id":"up"/"token_id":"unrelated"/' "$compatible" >"$unrelated"

input_bad_uri='oss://bucket/lake/raw/venue=polymarket/dataset=crypto_expiry/date=2026-01-01/hour=00/market-updates.20260101T010000.20260101T00.ndjson.zst'
input_good_uri='oss://bucket/lake/raw/venue=polymarket/dataset=crypto_expiry/date=2026-01-01/hour=00/market-updates.20260101T020000.20260101T00.ndjson.zst'
input_no_quote_uri='oss://bucket/lake/raw/venue=polymarket/dataset=crypto_expiry/date=2026-01-01/hour=00/market-updates.20260101T030000.20260101T00.ndjson.zst'
make_remote_triplet "$incompatible" "$input_bad_uri" crypto_expiry
make_remote_triplet "$compatible" "$input_good_uri" crypto_expiry
make_remote_triplet "$no_quote" "$input_no_quote_uri" crypto_expiry
matching_template_dir="$preflight_root/matching-candidate-template"
unrelated_template_dir="$preflight_root/unrelated-candidate-template"
mkdir "$matching_template_dir" "$unrelated_template_dir"
template_uri='oss://bucket/template/market-updates.20260101T010000.20260101T00.ndjson.zst'
make_remote_triplet "$compatible" "$template_uri" placeholder
cp "$remote_root/${template_uri#oss://bucket/}"* "$matching_template_dir/"
make_remote_triplet "$unrelated" "$template_uri" placeholder
cp "$remote_root/${template_uri#oss://bucket/}"* "$unrelated_template_dir/"

original_path=$PATH
export PATH="$fake_bin:$PATH"
export FAKE_OSS_ROOT="$remote_root"
export FAKE_CANDIDATE_TEMPLATE="$matching_template_dir"
export PREFLIGHT_STAT="$preflight_stat"
oss_bucket=bucket
oss_endpoint=endpoint
oss_region=region
aliyun_profile=profile
zstd_timeout_seconds=30
oss_copy_timeout_seconds=30
oss_config_sha=$(printf 'd%.0s' {1..64})
candidate_sha=$(printf 'a%.0s' {1..64})
source_revision=$(printf 'b%.0s' {1..40})
deployment_bundle_sha=$(printf 'c%.0s' {1..64})
release_manifest_sha=$(printf '1%.0s' {1..64})
control_archive_sha=$(printf '2%.0s' {1..64})
fake_release_binary="$preflight_root/candidate"
release_binary="$VERIFY"
run_id=20260101T000000Z-1
verify_current_oss_config() { :; }
timeout() {
  shift 2
  "$@"
}

wrong_path_sha=$(printf '0%.0s' {1..64})
wrong_path_uri="oss://bucket/lake/raw/venue=polymarket/dataset=crypto_expiry/date=2026-01-01/hour=00/sha256=$wrong_path_sha/market-updates.20260101T040000.20260101T00.ndjson.zst"
make_remote_triplet "$compatible" "$wrong_path_uri" crypto_expiry
if download_and_verify_oss_triplet "$wrong_path_uri" crypto_expiry \
  "$preflight_root/wrong-path-download" \
  "$((SECONDS + REAL_MARKET_PREFLIGHT_BUDGET_SECONDS))" >/dev/null; then
  printf 'triplet verifier accepted a content-addressed path with the wrong digest\n' >&2
  exit 1
fi
superseded_uri="$input_good_uri.SUPERSEDED.json"
printf '{}\n' >"$remote_root/${superseded_uri#oss://bucket/}"
if download_and_verify_oss_triplet "$input_good_uri" crypto_expiry \
  "$preflight_root/superseded-download" \
  "$((SECONDS + REAL_MARKET_PREFLIGHT_BUDGET_SECONDS))" >/dev/null; then
  printf 'triplet verifier accepted a superseded production segment\n' >&2
  exit 1
fi
rm "$remote_root/${superseded_uri#oss://bucket/}"

bad_case="$preflight_root/bad-case"
mkdir -p "$bad_case/source" "$bad_case/spool" "$bad_case/download" \
  "$bad_case/evidence"
cp "$incompatible" \
  "$bad_case/source/market-updates.20260101T010000000000.ndjson"
if real_market_segment_preflight "$bad_case/source" "$bad_case/spool" \
  "$bad_case/download" "$bad_case/evidence"; then
  printf 'real-segment preflight accepted an incompatible production format\n' >&2
  exit 1
fi
jq -e '.status == "failed" and .candidate_exit_code == 1
  and .source_segment.file == "market-updates.20260101T010000000000.ndjson"' \
  "$bad_case/evidence/real-market-preflight.json" >/dev/null
grep -Fq 'quote requires request_status=success' \
  "$bad_case/evidence/real-market-uploader.stderr"

no_quote_case="$preflight_root/no-quote-case"
mkdir -p "$no_quote_case/source" "$no_quote_case/spool" "$no_quote_case/download" \
  "$no_quote_case/evidence"
cp "$no_quote" \
  "$no_quote_case/source/market-updates.20260101T030000000000.ndjson"
if real_market_segment_preflight "$no_quote_case/source" \
  "$no_quote_case/spool" "$no_quote_case/download" "$no_quote_case/evidence"; then
  printf 'real-segment preflight accepted a segment with no quote records\n' >&2
  exit 1
fi

cross_hour_case="$preflight_root/cross-hour-case"
mkdir -p "$cross_hour_case/source" "$cross_hour_case/spool" \
  "$cross_hour_case/download" "$cross_hour_case/evidence"
cp "$cross_hour" \
  "$cross_hour_case/source/market-updates.20260101T035000000000.ndjson"
if real_market_segment_preflight "$cross_hour_case/source" \
  "$cross_hour_case/spool" "$cross_hour_case/download" \
  "$cross_hour_case/evidence"; then
  printf 'real-segment preflight accepted a cross-hour source as one triplet\n' >&2
  exit 1
fi

unrelated_case="$preflight_root/unrelated-case"
mkdir -p "$unrelated_case/source" "$unrelated_case/spool" "$unrelated_case/download" \
  "$unrelated_case/evidence"
cp "$compatible" \
  "$unrelated_case/source/market-updates.20260101T040000000000.ndjson"
export FAKE_CANDIDATE_TEMPLATE="$unrelated_template_dir"
release_binary="$fake_release_binary"
if real_market_segment_preflight "$unrelated_case/source" \
  "$unrelated_case/spool" "$unrelated_case/download" \
  "$unrelated_case/evidence"; then
  printf 'real-segment preflight accepted output unrelated to its input\n' >&2
  exit 1
fi
export FAKE_CANDIDATE_TEMPLATE="$matching_template_dir"
release_binary="$VERIFY"

inconsistent_canonical_case="$preflight_root/inconsistent-canonical-case"
mkdir -p "$inconsistent_canonical_case/source" \
  "$inconsistent_canonical_case/spool" "$inconsistent_canonical_case/download" \
  "$inconsistent_canonical_case/evidence"
cp "$compatible" \
  "$inconsistent_canonical_case/source/market-updates.20260101T041000000000.ndjson"
export FAKE_CANONICAL_COUNT=0
release_binary="$fake_release_binary"
if real_market_segment_preflight "$inconsistent_canonical_case/source" \
  "$inconsistent_canonical_case/spool" "$inconsistent_canonical_case/download" \
  "$inconsistent_canonical_case/evidence"; then
  printf 'real-segment preflight accepted a canonical count inconsistent with its manifest\n' >&2
  exit 1
fi
unset FAKE_CANONICAL_COUNT
release_binary="$VERIFY"

empty_case="$preflight_root/empty-case"
mkdir -p "$empty_case/source" "$empty_case/spool" "$empty_case/download" \
  "$empty_case/evidence"
empty_stderr="$empty_case/evidence/preflight.stderr"
if real_market_segment_preflight "$empty_case/source" "$empty_case/spool" \
  "$empty_case/download" "$empty_case/evidence" 2>"$empty_stderr"; then
  printf 'real-segment preflight accepted an empty production spool\n' >&2
  exit 1
fi
grep -Fxq 'real market preflight found no eligible closed market segment' \
  "$empty_stderr" || {
  printf 'empty production spool has no exact fail-closed reason\n' >&2
  exit 1
}
if grep -Fq \
  'candidate rejected a real production closed market segment before shadow startup' \
  "$GATE"; then
  printf 'Gate still misattributes preflight setup failures to the candidate\n' >&2
  exit 1
fi
grep -Fq 'real production closed-market preflight failed before shadow startup' \
  "$GATE" || {
  printf 'Gate has no neutral real-market preflight failure summary\n' >&2
  exit 1
}

insecure_case="$preflight_root/insecure-case"
mkdir -p "$insecure_case/source" "$insecure_case/spool" \
  "$insecure_case/download" "$insecure_case/evidence"
cp "$compatible" \
  "$insecure_case/source/market-updates.20260101T045000000000.ndjson"
export INSECURE_SOURCE_SPOOL="$insecure_case/source"
if real_market_segment_preflight "$insecure_case/source" "$insecure_case/spool" \
  "$insecure_case/download" "$insecure_case/evidence"; then
  printf 'real-segment preflight accepted an insecure production spool\n' >&2
  exit 1
fi
unset INSECURE_SOURCE_SPOOL

symlink_case="$preflight_root/symlink-case"
mkdir -p "$symlink_case/source" "$symlink_case/spool" \
  "$symlink_case/download" "$symlink_case/evidence"
ln -s "$compatible" \
  "$symlink_case/source/market-updates.20260101T050000000000.ndjson"
if real_market_segment_preflight "$symlink_case/source" "$symlink_case/spool" \
  "$symlink_case/download" "$symlink_case/evidence"; then
  printf 'real-segment preflight accepted a symlinked production segment\n' >&2
  exit 1
fi

unstable_case="$preflight_root/unstable-case"
mkdir -p "$unstable_case/source" "$unstable_case/spool" \
  "$unstable_case/download" "$unstable_case/evidence"
unstable_source="$unstable_case/source/market-updates.20260101T060000000000.ndjson"
cp "$compatible" "$unstable_source"
export UNSTABLE_STAT_PATH="$unstable_source"
export UNSTABLE_STAT_COUNTER="$unstable_case/stat-counter"
if real_market_segment_preflight "$unstable_case/source" "$unstable_case/spool" \
  "$unstable_case/download" "$unstable_case/evidence"; then
  printf 'real-segment preflight accepted an unstable production segment\n' >&2
  exit 1
fi
unset UNSTABLE_STAT_PATH UNSTABLE_STAT_COUNTER

good_case="$preflight_root/good-case"
mkdir -p "$good_case/source" "$good_case/spool" "$good_case/download" \
  "$good_case/evidence"
good_base="$good_case/source/market-updates.20260101T020000000000.ndjson"
good_uuid="$good_case/source/market-updates.20260101T020000000000.123e4567-e89b-12d3-a456-426614174000.ndjson"
cp "$no_quote" "$good_base"
cp "$noncanonical" "$good_uuid"
jq -n '{last_uploaded_object:null,failed_segments:["legacy uploader rejected it"],
  last_error:"legacy uploader rejected it"}' >"$good_case/source/upload-status.json"
real_market_segment_preflight "$good_case/source" "$good_case/spool" \
  "$good_case/download" "$good_case/evidence" || {
  sed -n '1,20p' "$good_case/evidence/real-market-uploader.stderr" >&2
  printf 'real-segment preflight rejected a verified non-canonical upload\n' >&2
  exit 1
}
jq -e '.status == "passed"
  and .schema == "monday.polymarket_real_market_preflight.v2"
  and .source_segment.file
    == "market-updates.20260101T020000000000.123e4567-e89b-12d3-a456-426614174000.ndjson"
  and .source_segment.path
    == $source
  and .source_segment.sha256 == .source_content_sha256
  and .source_segment.bytes == .uploaded_triplet.source_bytes
  and (.uploaded_triplet.dataset | startswith("crypto_expiry_preflight_"))
  and .uploaded_triplet.canonical == false
  and .uploaded_triplet.segment_complete == false
  and .source_quote_records == 1
  and .source_recorded_hours == 1
  and .source_content_sha256 == .uploaded_content_sha256
  and .upload_summary.uploaded_segments == 1
  and .upload_summary.canonical_uploaded_segments == 0' \
  --arg source "$good_uuid" \
  "$good_case/evidence/real-market-preflight.json" >/dev/null

# Counterexample (issue #586): a tick-level segment much larger than the scan
# window must not make the bounded SCAN exceed its budget. The upload path
# legitimately scales with segment size, so this isolates the scan: a segment
# at 2x the window must be scanned in bounded time because the quote counter
# caps and the hour check samples head+tail.
large_scan_tmp="$tmp_dir/large-scan"
mkdir -p "$large_scan_tmp"
large_segment="$large_scan_tmp/large.ndjson"
{
  for i in $(seq 1 "$((PREFLIGHT_SCAN_WINDOW_RECORDS * 2))"); do
    if (( i % 2 == 0 )); then kind="quote"; else kind="event_discovered"; fi
    printf '%s\n' \
      "{\"sequence\":$i,\"recorded_at\":\"2026-01-01T00:00:00Z\",\"update\":{\"kind\":\"$kind\",\"token_id\":\"up\"}}"
  done
} >"$large_segment"
large_bytes=$(wc -c <"$large_segment" | tr -d ' ')
scan_start=$(date +%s)
large_quotes=$(head -n "$PREFLIGHT_SCAN_WINDOW_RECORDS" "$large_segment" \
  | jq -c 'select(.update.kind == "quote")' \
  | wc -l | tr -d ' ')
large_hours=$(bash -c 'head -n "$1" "$2"; tail -n "$1" "$2"' _ \
  "$PREFLIGHT_SCAN_WINDOW_RECORDS" "$large_segment" \
  | jq -r '.recorded_at | select(type=="string") | .[0:13]' \
  | sort -u | wc -l | tr -d ' ')
scan_end=$(date +%s)
scan_elapsed=$((scan_end - scan_start))
# The scan must be bounded: it reads at most PREFLIGHT_SCAN_WINDOW_RECORDS from
# the head (and a head+tail window for hours), so a 2x-window segment scans in
# ~the same time as a small one. No full-file linear scan of the segment.
if (( scan_elapsed > 30 )); then
  printf 'bounded scan took %ss for a %s-byte segment; must not scale with size\n' \
    "$scan_elapsed" "$large_bytes" >&2
  exit 1
fi
# The fixture alternates quote/event records, so the head window
# (PREFLIGHT_SCAN_WINDOW_RECORDS) contains exactly half quotes. Assert the exact
# count: a full-file scan of the 2x fixture would return WINDOW quotes, which
# fails this check — proving the scan is bounded.
expected_large_quotes=$((PREFLIGHT_SCAN_WINDOW_RECORDS / 2))
(( large_quotes == expected_large_quotes )) || {
  printf 'large-segment quote count %s, expected %s from the head window\n' \
    "$large_quotes" "$expected_large_quotes" >&2
  exit 1
}
[[ $large_hours -eq 1 ]] || {
  printf 'large-segment hours %s was not 1\n' "$large_hours" >&2
  exit 1
}
printf 'bounded scan OK: %s quotes in window, %s hour, %s bytes in %ss\n' \
  "$large_quotes" "$large_hours" "$large_bytes" "$scan_elapsed"
export PATH=$original_path

# The live parity interval begins with the Rust shadow; settlement maturity is
# proven inside the bounded observation rather than appended after it.
parity_window_verifier="$tmp_dir/valid-parity-window.sh"
sed -n '/^valid_parity_window() {$/,/^}$/p' "$GATE" >"$parity_window_verifier"
sed -n '/^bounded_parity_window_start() {$/,/^}$/p' "$GATE" \
  >>"$parity_window_verifier"
# shellcheck source=/dev/null
source "$parity_window_verifier"
valid_parity_window 100 1000 || {
  printf 'parity-window verifier rejected an ordered window\n' >&2
  exit 1
}
if valid_parity_window 1900 1800 || valid_parity_window 1800 1800; then
  printf 'parity-window verifier accepted a clamped start at/after cutoff\n' >&2
  exit 1
fi
[[ $(bounded_parity_window_start 1000 1000 true) == 999 ]] || {
  printf 'short test gate did not cap parity start below the cutoff\n' >&2
  exit 1
}
[[ $(bounded_parity_window_start 100 1000 false) == 100 ]] || {
  printf 'production gate appended settlement maturity after shadow start\n' >&2
  exit 1
}
[[ $(bounded_parity_window_start 3000 3840 false) == 3000 ]] || {
  printf 'production gate moved parity start across an hour boundary\n' >&2
  exit 1
}

# Exercise the production marker verifier itself. A marker is valid only when
# it contains the one content-addressed gate.json entry; sha256sum otherwise
# accepts an unrelated entry or a valid gate entry followed by extra entries.
marker_verifier="$tmp_dir/verify-gate-marker.sh"
sed -n '/^verify_gate_marker() {$/,/^}$/p' "$CUTOVER" >"$marker_verifier"
# shellcheck source=/dev/null
source "$marker_verifier"
declare -F verify_gate_marker >/dev/null || {
  printf 'cutover does not expose its gate-marker verifier to contract tests\n' >&2
  exit 1
}
marker_dir="$tmp_dir/marker"
mkdir "$marker_dir"
printf 'gate evidence\n' >"$marker_dir/gate.json"
printf 'unrelated evidence\n' >"$marker_dir/other.json"
(
  cd "$marker_dir"
  sha256sum gate.json >PASSED.sha256
)
verify_gate_marker "$marker_dir" || {
  printf 'gate-marker verifier rejected the exact gate.json marker\n' >&2
  exit 1
}
(
  cd "$marker_dir"
  sha256sum other.json >PASSED.sha256
)
if verify_gate_marker "$marker_dir"; then
  printf 'gate-marker verifier accepted a marker for another file\n' >&2
  exit 1
fi
(
  cd "$marker_dir"
  sha256sum gate.json other.json >PASSED.sha256
)
if verify_gate_marker "$marker_dir"; then
  printf 'gate-marker verifier accepted a multi-entry marker\n' >&2
  exit 1
fi

# Cutover must consume the supervisor's immutable terminal receipt, not a
# caller-selected gate.json. The receipt fixes the candidate, source, systemd
# invocation, terminal result, shadow containment, and exact Gate evidence path.
terminal_receipt_verifier="$tmp_dir/verify-gate-terminal-receipt.sh"
sed -n \
  -e '/^verify_gate_marker() {$/,/^}$/p' \
  -e '/^verify_gate_terminal_receipt() {$/,/^}$/p' \
  "$CUTOVER" >"$terminal_receipt_verifier"
grep -Fq 'verify_gate_terminal_receipt() {' "$terminal_receipt_verifier" || {
  printf 'cutover does not expose its terminal-receipt admission contract\n' >&2
  exit 1
}
(
  candidate=$(printf 'a%.0s' {1..64})
  source_revision=$(printf 'b%.0s' {1..40})
  invocation=$(printf 'c%.0s' {1..32})
  other_candidate=$(printf 'd%.0s' {1..64})
  GATE_RECEIPT_ROOT="$tmp_dir/terminal-receipts"
  GATE_EVIDENCE_ROOT="$tmp_dir/terminal-gates"
  receipt_dir="$GATE_RECEIPT_ROOT/$candidate/$invocation"
  gate_dir="$GATE_EVIDENCE_ROOT/$candidate/$invocation"
  receipt="$receipt_dir/receipt.json"
  gate_json="$gate_dir/gate.json"
  mkdir -p "$receipt_dir" "$gate_dir"
  secure_receipt=true
  secure_regular_file() {
    [[ $secure_receipt == true && -f $1 && ! -L $1 ]]
  }
  secure_root_chain() { [[ -d $1 && ! -L $1 ]]; }
  write_receipt() {
    jq -n --arg candidate "$candidate" --arg source "$source_revision" \
      --arg invocation "$invocation" '
      {schema:"monday.polymarket_gate_receipt.v1",
        unit:("polymarket-raw-ops-gate@" + $candidate + ".service"),
        candidate_sha256:$candidate,source_revision:$source,
        systemd_invocation_id:$invocation,phase:"terminal",
        terminal_state:"passed",systemd:{result:"success",exit_code:"exited",
          exit_status:"0"},shadow:{
          unit:("polymarket-reference-collector-shadow@" + $candidate + ".service"),
          stop_result:"success",containment:"contained",active_state:"inactive",
          main_pid:"0"}}' >"$receipt"
  }
  write_gate() {
    jq -n --arg candidate "$candidate" --arg source "$source_revision" \
      --arg invocation "$invocation" '
      {candidate_sha256:$candidate,deployment_source_revision:$source,
        shadow_run_id:$invocation,production_eligible:true,passed:true}' \
      >"$gate_json"
    (cd "$gate_dir" && sha256sum gate.json >PASSED.sha256)
  }
  # shellcheck source=/dev/null
  source "$terminal_receipt_verifier"
  write_receipt
  write_gate
  binding=$(verify_gate_terminal_receipt "$receipt" "$candidate") || {
    printf 'cutover terminal-receipt verifier rejected valid evidence\n' >&2
    exit 1
  }
  IFS='|' read -r bound_invocation bound_source bound_receipt_sha \
    bound_gate_sha bound_gate_json bound_receipt extra <<<"$binding"
  [[ $bound_invocation == "$invocation" \
    && $bound_source == "$source_revision" \
    && $bound_receipt_sha == "$(sha256sum "$receipt" | awk '{print $1}')" \
    && $bound_gate_sha == "$(sha256sum "$gate_json" | awk '{print $1}')" \
    && $bound_gate_json == "$gate_json" && $bound_receipt == "$receipt" \
    && -z $extra ]] || {
    printf 'cutover terminal-receipt verifier returned the wrong binding\n' >&2
    exit 1
  }
  jq '.passed = false' "$gate_json" >"$gate_json.tmp"
  mv "$gate_json.tmp" "$gate_json"
  if [[ $(sha256sum "$gate_json" | awk '{print $1}') == "$bound_gate_sha" ]]; then
    printf 'cutover Gate digest did not detect post-admission evidence drift\n' >&2
    exit 1
  fi
  write_gate

  rm "$receipt"
  if verify_gate_terminal_receipt "$receipt" "$candidate" >/dev/null 2>&1; then
    printf 'cutover admitted standalone Gate evidence without a receipt\n' >&2
    exit 1
  fi
  write_receipt
  for mutation in \
    '.terminal_state = "failed"' \
    '.terminal_state = "cancelled"' \
    '.systemd.result = "signal"' \
    '.systemd.exit_code = "killed"' \
    '.systemd.exit_status = "1"' \
    '.shadow.containment = "active"' \
    '.systemd.extra = true' \
    '.shadow.extra = true' \
    '.extra = true'; do
    jq "$mutation" "$receipt" >"$receipt.tmp"
    mv "$receipt.tmp" "$receipt"
    if verify_gate_terminal_receipt "$receipt" "$candidate" >/dev/null 2>&1; then
      printf 'cutover admitted invalid terminal receipt mutation: %s\n' \
        "$mutation" >&2
      exit 1
    fi
    write_receipt
  done
  jq --arg candidate "$other_candidate" '.candidate_sha256 = $candidate' \
    "$receipt" >"$receipt.tmp"
  mv "$receipt.tmp" "$receipt"
  if verify_gate_terminal_receipt "$receipt" "$candidate" >/dev/null 2>&1; then
    printf 'cutover admitted a receipt for another candidate\n' >&2
    exit 1
  fi
  write_receipt
  jq '.systemd_invocation_id = ("f" * 32)' "$receipt" >"$receipt.tmp"
  mv "$receipt.tmp" "$receipt"
  if verify_gate_terminal_receipt "$receipt" "$candidate" >/dev/null 2>&1; then
    printf 'cutover admitted a receipt for another invocation\n' >&2
    exit 1
  fi
  write_receipt
  jq '.source_revision = ("e" * 40)' "$receipt" >"$receipt.tmp"
  mv "$receipt.tmp" "$receipt"
  if verify_gate_terminal_receipt "$receipt" "$candidate" >/dev/null 2>&1; then
    printf 'cutover admitted a receipt whose source differs from gate evidence\n' >&2
    exit 1
  fi
  write_receipt
  cp "$receipt" "$tmp_dir/wrong-receipt.json"
  if verify_gate_terminal_receipt "$tmp_dir/wrong-receipt.json" "$candidate" \
    >/dev/null 2>&1; then
    printf 'cutover admitted a receipt outside the fixed receipt root\n' >&2
    exit 1
  fi
  printf 'tampered marker\n' >"$gate_dir/PASSED.sha256"
  if verify_gate_terminal_receipt "$receipt" "$candidate" >/dev/null 2>&1; then
    printf 'cutover admitted a receipt bound to a tampered Gate marker\n' >&2
    exit 1
  fi
  (cd "$gate_dir" && sha256sum gate.json >PASSED.sha256)
  secure_receipt=false
  if verify_gate_terminal_receipt "$receipt" "$candidate" >/dev/null 2>&1; then
    printf 'cutover admitted an insecure terminal receipt\n' >&2
    exit 1
  fi
  secure_receipt=true
  mv "$receipt" "$receipt_dir/real.json"
  ln -s real.json "$receipt"
  if verify_gate_terminal_receipt "$receipt" "$candidate" >/dev/null 2>&1; then
    printf 'cutover admitted an indirect terminal receipt\n' >&2
    exit 1
  fi
)

# Exercise the installed immutable release manifest parser. The manifest is the
# sole binding for source, candidate, control-manifest, and control-archive IDs.
(
release_manifest_verifier="$tmp_dir/verify-release-manifest.sh"
sed -n \
  -e '/^release_control_assets() {$/,/^}$/p' \
  -e '/^verify_release_manifest() {$/,/^}$/p' \
  -e '/^verify_release_binding() {$/,/^}$/p' \
  -e '/^verify_control_release() {$/,/^}$/p' "$GATE" \
  >"$release_manifest_verifier"
readonly RELEASE_MANIFEST_SCHEMA=monday.polymarket_raw_ops_release.v1
readonly RELEASE_MANIFEST="$tmp_dir/polymarket-raw-ops-release.json"
readonly -a BUNDLE_ASSETS=(
  polymarket-raw-ops-shadow-gate.sh
  candidate-only-control
)
secure_control_file() {
  [[ -f $1 && ! -L $1 ]]
}
# shellcheck source=/dev/null
source "$release_manifest_verifier"
release_manifest_dir="$tmp_dir/release-manifest"
mkdir "$release_manifest_dir"
release_manifest="$release_manifest_dir/polymarket-raw-ops-release.json"
candidate_file="$release_manifest_dir/polymarket-raw-ops"
printf 'verified candidate\n' >"$candidate_file"
cat >"$release_manifest_dir/polymarket-raw-ops-shadow-gate.sh" <<'EOF'
readonly -a BUNDLE_ASSETS=(
  polymarket-raw-ops-shadow-gate.sh
  baseline-only-control
)
EOF
: >"$release_manifest_dir/baseline-only-control"
candidate_sha=$(sha256sum "$candidate_file" | awk '{print $1}')
source_revision=$(printf 'b%.0s' {1..40})
bundle_fixture_sha=$(
  cd "$release_manifest_dir"
  sha256sum polymarket-raw-ops-shadow-gate.sh baseline-only-control \
    | sha256sum | awk '{print $1}'
)
installed_bundle_sha=$bundle_fixture_sha
control_archive_sha=$(printf 'd%.0s' {1..64})
jq -S -n \
  --arg candidate "$candidate_sha" \
  --arg source "$source_revision" \
  --arg control_manifest "$bundle_fixture_sha" \
  --arg control_archive "$control_archive_sha" \
  '{schema:"monday.polymarket_raw_ops_release.v1",source_revision:$source,
    candidate:{file:"polymarket-raw-ops",sha256:$candidate},
    control_manifest:{file:"polymarket-raw-ops-control-assets.sha256",
      sha256:$control_manifest},
    control_archive:{file:"polymarket-raw-ops-control.tar.gz",
      sha256:$control_archive}}' >"$release_manifest"
release_manifest_sha=$(sha256sum "$release_manifest" | awk '{print $1}')
bundle_sha256() {
  printf '%s\n' "$bundle_fixture_sha"
}
verify_release_manifest "$release_manifest" || {
  printf 'release manifest verifier rejected a valid identity binding\n' >&2
  exit 1
}
verify_release_binding "$release_manifest" "$release_manifest_sha" \
  "$candidate_sha" "$source_revision" "$bundle_fixture_sha" \
  "$control_archive_sha" "$candidate_file" || {
  printf 'release binding rejected a valid immutable release\n' >&2
  exit 1
}
verify_control_release "$release_manifest_dir" "$candidate_sha" "$candidate_file" || {
  printf 'global controls rejected the active Rust baseline release\n' >&2
  exit 1
}
if verify_control_release "$release_manifest_dir" "$(printf '0%.0s' {1..64})" \
  "$candidate_file"; then
  printf 'global controls accepted a different Rust baseline release\n' >&2
  exit 1
fi
rm "$release_manifest_dir/baseline-only-control"
if verify_control_release "$release_manifest_dir" "$candidate_sha" "$candidate_file"; then
  printf 'global controls accepted a missing bundled control asset\n' >&2
  exit 1
fi
: >"$release_manifest_dir/baseline-only-control"
cp "$release_manifest_dir/polymarket-raw-ops-shadow-gate.sh" \
  "$release_manifest_dir/valid-shadow-gate.sh"
printf '%s\n' 'readonly -a BUNDLE_ASSETS=(' \
  '  polymarket-raw-ops-shadow-gate.sh' '  -option-like-control' ')' \
  >"$release_manifest_dir/polymarket-raw-ops-shadow-gate.sh"
if release_control_assets "$release_manifest_dir" >/dev/null; then
  printf 'release control parser accepted an option-like asset name\n' >&2
  exit 1
fi
mv "$release_manifest_dir/valid-shadow-gate.sh" \
  "$release_manifest_dir/polymarket-raw-ops-shadow-gate.sh"
if verify_release_binding "$release_manifest" "$(printf '0%.0s' {1..64})" \
  "$candidate_sha" "$source_revision" "$bundle_fixture_sha" \
  "$control_archive_sha" "$candidate_file"; then
  printf 'release binding accepted a different manifest identity\n' >&2
  exit 1
fi
if verify_release_binding "$release_manifest" "$release_manifest_sha" \
  "$(printf '0%.0s' {1..64})" "$source_revision" "$bundle_fixture_sha" \
  "$control_archive_sha" "$candidate_file"; then
  printf 'release binding accepted a different candidate identity\n' >&2
  exit 1
fi
printf 'tampered candidate\n' >>"$candidate_file"
if verify_release_binding "$release_manifest" "$release_manifest_sha" \
  "$candidate_sha" "$source_revision" "$bundle_fixture_sha" \
  "$control_archive_sha" "$candidate_file" >/dev/null 2>&1; then
  printf 'release binding accepted modified candidate bytes\n' >&2
  exit 1
fi
printf 'verified candidate\n' >"$candidate_file"
bundle_fixture_sha=$(printf 'e%.0s' {1..64})
if verify_release_binding "$release_manifest" "$release_manifest_sha" \
  "$candidate_sha" "$source_revision" "$(printf 'c%.0s' {1..64})" \
  "$control_archive_sha" "$candidate_file"; then
  printf 'release binding accepted a modified installed control bundle\n' >&2
  exit 1
fi
bundle_fixture_sha=$installed_bundle_sha
jq '.extra = true' "$release_manifest" >"$release_manifest_dir/extra.json"
if verify_release_manifest "$release_manifest_dir/extra.json"; then
  printf 'release manifest verifier accepted an extra identity field\n' >&2
  exit 1
fi
jq '.candidate.file = "other-binary"' "$release_manifest" \
  >"$release_manifest_dir/wrong-file.json"
if verify_release_manifest "$release_manifest_dir/wrong-file.json"; then
  printf 'release manifest verifier accepted a different candidate filename\n' >&2
  exit 1
fi
printf '{}\n%s\n' "$(<"$release_manifest")" \
  >"$release_manifest_dir/multiple.json"
if verify_release_manifest "$release_manifest_dir/multiple.json" 2>/dev/null; then
  printf 'release manifest verifier accepted multiple JSON values\n' >&2
  exit 1
fi
)

# Exercise the Rust-only wrapper around the established systemd identity check.
baseline_identity_contract="$tmp_dir/verify-baseline-identity.sh"
sed -n '/^verify_baseline_identity() {$/,/^}$/p' "$GATE" >"$baseline_identity_contract"
exercise_rust_baseline_identity() (
  set -euo pipefail
  RUST_ACTIVE_BINARY="$tmp_dir/active" baseline_mode=rust_release legacy_pid=42
  RUST_PRODUCTION_EXEC='/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200'
  legacy_restarts=1 legacy_invocation_id=$(printf '1%.0s' {1..32})
  baseline_release_sha=$(printf '9%.0s' {1..64})
  baseline_release_path="$tmp_dir/$baseline_release_sha/polymarket-raw-ops"
  mkdir -p "${baseline_release_path%/*}"
  printf 'test\n' >"$baseline_release_path"; chmod +x "$baseline_release_path"
  mock_active=$baseline_release_path mock_proc=$baseline_release_path
  mock_digest=true mock_runtime=true
  verify_runtime_identity() { [[ $mock_runtime == true ]]; }
  verify_legacy_identity() { return 1; }
  secure_release_directory() { return 0; }
  secure_control_file() { return 0; }
  readlink() {
    [[ $3 == "$RUST_ACTIVE_BINARY" ]] && printf '%s\n' "$mock_active" \
      || printf '%s\n' "$mock_proc"
  }
  sha256sum() { cat >/dev/null; [[ $mock_digest == true ]]; }
  # shellcheck source=/dev/null
  source "$baseline_identity_contract"
  verify_baseline_identity
  for drift in active proc digest runtime; do
    mock_active=$baseline_release_path mock_proc=$baseline_release_path
    mock_digest=true mock_runtime=true
    case "$drift" in
      active) mock_active=/tmp/wrong ;;
      proc) mock_proc=/tmp/wrong ;;
      digest) mock_digest=false ;;
      runtime) mock_runtime=false ;;
    esac
    verify_baseline_identity && { printf 'accepted %s baseline drift\n' "$drift" >&2; exit 1; }
  done
  return 0
)
exercise_rust_baseline_identity

exercise_rust_bootstrap_identity() (
  set -euo pipefail
  RUST_ACTIVE_BINARY="$tmp_dir/bootstrap-active" baseline_mode=rust_bootstrap legacy_pid=42
  RUST_PRODUCTION_EXEC='/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200'
  legacy_restarts=1 legacy_invocation_id=$(printf '1%.0s' {1..32})
  baseline_release_sha=$(printf '8%.0s' {1..64})
  baseline_release_path="$RUST_ACTIVE_BINARY"
  printf 'test\n' >"$RUST_ACTIVE_BINARY"; chmod +x "$RUST_ACTIVE_BINARY"
  mock_active=$RUST_ACTIVE_BINARY mock_proc=$RUST_ACTIVE_BINARY
  mock_digest=true mock_runtime=true
  verify_runtime_identity() { [[ $mock_runtime == true ]]; }
  verify_legacy_identity() { return 1; }
  secure_release_directory() { return 1; }
  secure_control_file() { [[ -f $1 && ! -L $1 ]]; }
  readlink() {
    [[ $3 == "$RUST_ACTIVE_BINARY" ]] && printf '%s\n' "$mock_active" \
      || printf '%s\n' "$mock_proc"
  }
  sha256sum() { cat >/dev/null; [[ $mock_digest == true ]]; }
  # shellcheck source=/dev/null
  source "$baseline_identity_contract"
  verify_baseline_identity || {
    printf 'bootstrap Rust baseline identity was rejected\n' >&2
    exit 1
  }
  for drift in active proc digest runtime; do
    mock_active=$RUST_ACTIVE_BINARY mock_proc=$RUST_ACTIVE_BINARY
    mock_digest=true mock_runtime=true
    case "$drift" in
      active) mock_active=/tmp/wrong ;;
      proc) mock_proc=/tmp/wrong ;;
      digest) mock_digest=false ;;
      runtime) mock_runtime=false ;;
    esac
    verify_baseline_identity && {
      printf 'accepted bootstrap %s drift\n' "$drift" >&2
      exit 1
    }
  done
  return 0
)
exercise_rust_bootstrap_identity

bootstrap_baseline_selection="$tmp_dir/bootstrap-baseline-selection.sh"
sed -n '/^  baseline_exec=$(effective_exec_argv/,/^  esac$/p' "$GATE" \
  | sed 's/^  //' >"$bootstrap_baseline_selection"
(
  set -euo pipefail
  LEGACY_UNIT=polymarket-reference-collector.service
  LEGACY_EXEC='/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py'
  RUST_PRODUCTION_EXEC='/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200'
  RUST_ACTIVE_BINARY="$tmp_dir/bootstrap-selected-active"
  RELEASE_ROOT="$tmp_dir/releases"
  candidate_sha=$(printf 'a%.0s' {1..64})
  printf 'bootstrap\n' >"$RUST_ACTIVE_BINARY"
  effective_exec_argv() { printf '%s\n' "$RUST_PRODUCTION_EXEC"; }
  die() { printf 'bootstrap baseline selection was rejected\n' >&2; return 1; }
  # shellcheck source=/dev/null
  source "$bootstrap_baseline_selection"
  [[ $baseline_mode == rust_bootstrap \
    && $baseline_release_path == "$RUST_ACTIVE_BINARY" \
    && $baseline_release_sha =~ ^[a-f0-9]{64}$ ]]
)

bootstrap_health_admission="$tmp_dir/bootstrap-health-admission.sh"
sed -n '/^  elif \[\[ \$baseline_mode == rust_bootstrap \]\]; then$/,/^  fi$/p' \
  "$GATE" | sed '1d; $d; s/^    //' >"$bootstrap_health_admission"
(
  set -euo pipefail
  baseline_mode=rust_bootstrap
  LEGACY_SPOOL="$tmp_dir/bootstrap-health-spool"
  bootstrap_health_policy=
  verify_fresh_baseline_health() {
    bootstrap_health_policy=${2:-}
    return 1
  }
  die() { printf 'bootstrap health admission was rejected\n' >&2; return 1; }
  # shellcheck source=/dev/null
  source "$bootstrap_health_admission"
  [[ $baseline_degraded == true && $bootstrap_health_policy == "$RUST_HEALTH_POLICY" ]]
)

# The long shadow observation must not start when the production cutover target
# already contains state that the promotion path will reject. Keep the same
# read-only contract in both scripts so the Gate and final cutover cannot drift.
gate_cutover_target_contract=$(sed -n \
  '/^verify_cutover_target_preflight() {$/,/^}$/p' "$GATE")
cutover_target_contract=$(sed -n \
  '/^verify_cutover_target_preflight() {$/,/^}$/p' "$CUTOVER")
[[ -n $gate_cutover_target_contract \
  && $gate_cutover_target_contract == "$cutover_target_contract" ]] || {
  printf 'shadow and cutover target preflight contracts are missing or differ\n' >&2
  exit 1
}
cutover_target_contract_file="$tmp_dir/cutover-target-preflight.sh"
sed -n '/^release_control_assets() {$/,/^}$/p' "$GATE" \
  >"$cutover_target_contract_file"
printf '%s\n' "$gate_cutover_target_contract" >>"$cutover_target_contract_file"
gate_cutover_target_line=$(grep -n '^verify_cutover_target_preflight ' "$GATE" \
  | cut -d: -f1)
gate_shadow_start_line=$(grep -n '^systemctl start "$shadow_unit"$' "$GATE" \
  | cut -d: -f1)
gate_started_at_line=$(grep -n '^started_at_unix=' "$GATE" | cut -d: -f1)
gate_start_uptime_line=$(grep -n '^start_uptime=' "$GATE" | cut -d: -f1)
gate_rust_control_line=$(grep -n \
  '^  || verify_control_release "$CONTROL_DIR" "$baseline_release_sha"' "$GATE" \
  | cut -d: -f1)
cutover_target_line=$(grep -n '^verify_cutover_target_preflight ' "$CUTOVER" \
  | cut -d: -f1)
cutover_transition_line=$(grep -n '^transition_started=true$' "$CUTOVER" \
  | cut -d: -f1)
[[ $gate_cutover_target_line =~ ^[1-9][0-9]*$ \
  && $gate_shadow_start_line =~ ^[1-9][0-9]*$ \
  && $gate_started_at_line =~ ^[1-9][0-9]*$ \
  && $gate_start_uptime_line =~ ^[1-9][0-9]*$ \
  && $gate_rust_control_line =~ ^[1-9][0-9]*$ \
  && $cutover_target_line =~ ^[1-9][0-9]*$ \
  && $cutover_transition_line =~ ^[1-9][0-9]*$ \
  && $gate_cutover_target_line -lt $gate_started_at_line \
  && $gate_cutover_target_line -lt $gate_start_uptime_line \
  && $gate_cutover_target_line -lt $gate_shadow_start_line \
  && $gate_rust_control_line -lt $gate_started_at_line \
  && $gate_rust_control_line -lt $gate_start_uptime_line \
  && $gate_rust_control_line -lt $gate_shadow_start_line \
  && $cutover_target_line -lt $cutover_transition_line ]] || {
  printf 'cutover target preflight runs after observation or transition begins\n' >&2
  exit 1
}
(
  baseline_mode=legacy_python
  active_binary="$tmp_dir/active-polymarket-raw-ops"
  control_dir="$tmp_dir/global-control"
  release_manifest_name=polymarket-raw-ops-release.json
  readonly -a BUNDLE_ASSETS=(
    polymarket-raw-ops-shadow-gate.sh
    polymarket-raw-ops-cutover.sh
    polymarket-shadow-gate-policy.jq
  )
  drop_in_unit=
  fragment_drift_unit=
  insecure_unit_file=
  unsafe_control_parent=false
  systemctl() {
    [[ $1 == show && $3 == --value && $# -eq 4 ]] || return 64
    case "$2" in
      --property=DropInPaths)
        [[ $4 != "$drop_in_unit" ]] \
          || printf '%s\n' /run/systemd/system.control/stale.conf
        ;;
      --property=FragmentPath)
        if [[ $4 == "$fragment_drift_unit" ]]; then
          printf '/run/systemd/transient/%s\n' "$4"
        else
          printf '/etc/systemd/system/%s\n' "$4"
        fi
        ;;
      *) return 64 ;;
    esac
  }
  direct_directory() { [[ -d $1 && ! -L $1 ]]; }
  secure_root_chain() { [[ -d $1 && ! -L $1 ]]; }
  secure_root_chain_or_absent() { [[ $unsafe_control_parent == false ]]; }
  secure_test_file() {
    if [[ $1 == /etc/systemd/system/* ]]; then
      [[ ${1##*/} != "$insecure_unit_file" ]]
    else
      [[ -f $1 && ! -L $1 ]]
    fi
  }
  # shellcheck source=/dev/null
  source "$cutover_target_contract_file"

  verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file || {
    printf 'cutover target preflight rejected clean legacy state\n' >&2
    exit 1
  }

  unsafe_control_parent=true
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted an unsafe absent control parent\n' >&2
    exit 1
  fi
  unsafe_control_parent=false

  mkdir "$control_dir"
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted an empty global control directory\n' >&2
    exit 1
  fi
  : >"$control_dir/polymarket-raw-ops-control-assets.sha256"
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted an incomplete global control directory\n' >&2
    exit 1
  fi

  printf '%s\n' 'readonly -a BUNDLE_ASSETS=(' \
    '  polymarket-raw-ops-shadow-gate.sh' '  baseline-only-control' ')' \
    >"$control_dir/polymarket-raw-ops-shadow-gate.sh"
  : >"$control_dir/baseline-only-control"
  : >"$control_dir/$release_manifest_name"
  verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file || {
    printf 'cutover target preflight rejected a complete global control directory\n' >&2
    exit 1
  }

  fragment_drift_unit=polymarket-reference-upload.service
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted a noncanonical unit fragment\n' >&2
    exit 1
  fi
  fragment_drift_unit=

  insecure_unit_file=polymarket-reference-upload.timer
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted an insecure unit file\n' >&2
    exit 1
  fi
  insecure_unit_file=

  for drop_in_unit in polymarket-reference-collector.service \
    polymarket-reference-upload.service polymarket-reference-upload.timer \
    polymarket-market-tape-upload.service polymarket-market-tape-upload.timer; do
    if verify_cutover_target_preflight \
      "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
      secure_test_file; then
      printf 'cutover target preflight accepted a drop-in for %s\n' \
        "$drop_in_unit" >&2
      exit 1
    fi
  done
  drop_in_unit=

  : >"$active_binary"
  if verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file; then
    printf 'cutover target preflight accepted a stale Rust target in legacy mode\n' >&2
    exit 1
  fi
  baseline_mode=rust_release
  verify_cutover_target_preflight \
    "$baseline_mode" "$active_binary" "$control_dir" "$release_manifest_name" \
    secure_test_file || {
    printf 'cutover target preflight rejected a governed Rust baseline target\n' >&2
    exit 1
  }
)

# A Python-to-Rust cutover must not import Python's long-cycle transient
# failure clocks into Rust's 180-second stale window. The read-only admission
# contract is shared by Gate and cutover; only the stopped legacy transition
# may apply the atomic transform.
gate_state_handoff_contract=$(sed -n \
  '/^verify_legacy_state_handoff_preflight() {$/,/^}$/p' "$GATE")
cutover_state_handoff_contract=$(sed -n \
  '/^verify_legacy_state_handoff_preflight() {$/,/^}$/p' "$CUTOVER")
[[ -n $gate_state_handoff_contract \
  && $gate_state_handoff_contract == "$cutover_state_handoff_contract" ]] || {
  printf 'Gate and cutover legacy-state handoff preflight contracts differ\n' >&2
  exit 1
}
state_handoff_contract_file="$tmp_dir/legacy-state-handoff.sh"
printf '%s\n' "$cutover_state_handoff_contract" >"$state_handoff_contract_file"
sed -n '/^apply_legacy_state_handoff() {$/,/^}$/p' "$CUTOVER" \
  >>"$state_handoff_contract_file"
grep -Fq 'apply_legacy_state_handoff() {' "$state_handoff_contract_file" || {
  printf 'cutover has no atomic legacy-state handoff implementation\n' >&2
  exit 1
}
(
  state_dir="$tmp_dir/legacy-state-spool"
  state="$state_dir/collector-state.json"
  evidence="$tmp_dir/legacy-state-evidence"
  mkdir "$state_dir" "$evidence"
  cat >"$state" <<'EOF'
{
  "trade_id_version": "v2",
  "trade_completion_version": "v1",
  "top_extra": {"preserve": true},
  "markets": {
    "market-a": {
      "condition_id": "condition-a",
      "trade_failure_since": "2026-07-30T23:55:44Z",
      "trade_last_error": "HTTP Error 429: Too Many Requests",
      "settlement_failure_since": "2026-07-30T23:55:44Z",
      "settlement_last_error": "temporary gamma error",
      "trade_complete": false,
      "market_extra": [1, 2, 3]
    },
    "market-b": {
      "condition_id": "condition-b",
      "trade_failure_since": "2026-07-30T23:55:44Z",
      "trade_complete": true
    }
  },
  "trade_seen": {"condition-a": {"id-a": 1, "id-b": 2}}
}
EOF
  state_owner=hftcollector
  state_group=hftcollector
  state_mode=640
  secure_collector_directory() { [[ $1 == "$state_dir" ]]; }
  chown() { :; }
  chmod() { :; }
  mv() {
    if [[ ${1:-} == -Tf ]]; then
      /bin/mv -f "$2" "$3"
    else
      /bin/mv "$@"
    fi
  }
  sync() { :; }
  stat() {
    [[ $1 == -c && $3 == -- && $4 == "$state" ]] || return 64
    case "$2" in
      %U) printf '%s\n' "$state_owner" ;;
      %G) printf '%s\n' "$state_group" ;;
      %a) printf '%s\n' "$state_mode" ;;
      *) return 64 ;;
    esac
  }
  # shellcheck source=/dev/null
  source "$state_handoff_contract_file"

  verify_legacy_state_handoff_preflight legacy_python "$state" || {
    printf 'legacy-state handoff rejected a valid direct state file\n' >&2
    exit 1
  }
  expected="$tmp_dir/legacy-state-expected.json"
  jq '(.markets[] | objects) |= del(
      .trade_failure_since,.trade_last_error,
      .settlement_failure_since,.settlement_last_error)' \
    "$state" >"$expected"
  apply_legacy_state_handoff "$state" "$evidence" || {
    printf 'legacy-state handoff failed to transform a valid state\n' >&2
    exit 1
  }
  jq -S . "$state" >"$state.normalized"
  jq -S . "$expected" >"$expected.normalized"
  cmp -s "$state.normalized" "$expected.normalized" || {
    printf 'legacy-state handoff changed durable collector state\n' >&2
    exit 1
  }
  jq -e '
    .schema == "monday.polymarket_legacy_state_handoff.v1"
    and .status == "applied"
    and .cleared_fields == {
      trade_failure_since:2,trade_last_error:1,
      settlement_failure_since:1,settlement_last_error:1}
    and .durable_state == {
      markets:2,trade_seen_conditions:1,retained_trade_ids:2}
    and (.before_sha256 | test("^[a-f0-9]{64}$"))
    and (.after_sha256 | test("^[a-f0-9]{64}$"))
    and .before_sha256 != .after_sha256
    and .snapshot.sha256 == .before_sha256
  ' "$evidence/legacy-state-handoff.json" >/dev/null || {
    printf 'legacy-state handoff evidence is incomplete\n' >&2
    exit 1
  }
  [[ $(sha256sum "$state" | awk '{print $1}') \
      == "$(jq -r .after_sha256 "$evidence/legacy-state-handoff.json")" ]] || {
    printf 'legacy-state handoff evidence does not bind the transformed state\n' >&2
    exit 1
  }
  jq -e '.markets["market-a"].trade_failure_since
      == "2026-07-30T23:55:44Z"' \
    "$evidence/pre-handoff-collector-state.json" >/dev/null || {
    printf 'legacy-state handoff did not preserve the original snapshot\n' >&2
    exit 1
  }

  printf 'not-json\n' >"$state"
  if verify_legacy_state_handoff_preflight legacy_python "$state"; then
    printf 'legacy-state handoff admitted malformed JSON\n' >&2
    exit 1
  fi
  printf '{"markets":{"bad":[]},"trade_seen":{}}\n' >"$state"
  if verify_legacy_state_handoff_preflight legacy_python "$state"; then
    printf 'legacy-state handoff admitted a non-object market state\n' >&2
    exit 1
  fi
  printf '{"markets":{},"trade_seen":{}}\n' >"$state"
  state_mode=660
  if verify_legacy_state_handoff_preflight legacy_python "$state"; then
    printf 'legacy-state handoff admitted a writable state file\n' >&2
    exit 1
  fi
  state_mode=640
  state_owner=root
  if verify_legacy_state_handoff_preflight legacy_python "$state"; then
    printf 'legacy-state handoff admitted a root-owned state file\n' >&2
    exit 1
  fi
  state_owner=hftcollector
  rm "$state"
  ln -s state-target.json "$state"
  printf '{"markets":{},"trade_seen":{}}\n' >"$state_dir/state-target.json"
  if verify_legacy_state_handoff_preflight legacy_python "$state"; then
    printf 'legacy-state handoff admitted an indirect state file\n' >&2
    exit 1
  fi
  verify_legacy_state_handoff_preflight rust_release "$state_dir/absent.json" || {
    printf 'Rust-to-Rust promotion incorrectly required legacy-state handoff\n' >&2
    exit 1
  }
)

gate_state_handoff_line=$(grep -n \
  '^verify_legacy_state_handoff_preflight "$baseline_mode" "$LEGACY_STATE"' \
  "$GATE" | cut -d: -f1)
cutover_state_handoff_line=$(grep -n \
  '^verify_legacy_state_handoff_preflight "$baseline_mode" "$LEGACY_STATE"' \
  "$CUTOVER" | cut -d: -f1)
cutover_apply_handoff_line=$(grep -n \
  '^apply_legacy_state_handoff "$LEGACY_STATE" "$evidence_dir"' \
  "$CUTOVER" | cut -d: -f1)
cutover_stop_collector_line=$(grep -n '^[[:space:]]*systemctl stop "$COLLECTOR_UNIT"$' \
  "$CUTOVER" | tail -1 | cut -d: -f1)
cutover_start_rust_line=$(grep -n '^systemctl restart "$COLLECTOR_UNIT"$' \
  "$CUTOVER" | tail -1 | cut -d: -f1)
[[ $gate_state_handoff_line =~ ^[1-9][0-9]*$ \
  && $cutover_state_handoff_line =~ ^[1-9][0-9]*$ \
  && $cutover_apply_handoff_line =~ ^[1-9][0-9]*$ \
  && $cutover_stop_collector_line =~ ^[1-9][0-9]*$ \
  && $cutover_start_rust_line =~ ^[1-9][0-9]*$ \
  && $gate_state_handoff_line -lt $gate_shadow_start_line \
  && $cutover_state_handoff_line -lt $cutover_transition_line \
  && $cutover_stop_collector_line -lt $cutover_apply_handoff_line \
  && $cutover_apply_handoff_line -lt $cutover_start_rust_line ]] || {
  printf 'legacy-state handoff runs outside the fail-closed transition order\n' >&2
  exit 1
}
if ! grep -Fq -- '--argjson legacy_state_handoff "$legacy_state_handoff_json"' \
    "$CUTOVER" \
  || ! grep -Fq 'legacy_state_handoff:$legacy_state_handoff' "$CUTOVER"; then
  printf 'cutover evidence does not bind the legacy-state handoff\n' >&2
  exit 1
fi

# Rollback must preserve the baseline release's own control membership rather
# than substituting the candidate script's BUNDLE_ASSETS.
rollback_control_contract="$tmp_dir/rollback-control-files.sh"
sed -n '/^rollback_control_files() {$/,/^}$/p' "$CUTOVER" \
  >"$rollback_control_contract"
sed -n '/^remove_snapshotted_control_files() {$/,/^}$/p' "$CUTOVER" \
  >>"$rollback_control_contract"
(
  RELEASE_MANIFEST=/tmp/polymarket-raw-ops-release.json
  # shellcheck source=/dev/null
  source "$rollback_control_contract"
  rollback_state="$tmp_dir/rollback-control-state.json"
  jq -n '{control_files:["polymarket-raw-ops-shadow-gate.sh",
      "baseline-only-control","polymarket-raw-ops-release.json"]}' \
    >"$rollback_state"
  [[ $(rollback_control_files "$rollback_state") == \
      $'polymarket-raw-ops-shadow-gate.sh\nbaseline-only-control\npolymarket-raw-ops-release.json' ]] \
    || {
      printf 'rollback control list rejected release-specific baseline assets\n' >&2
      exit 1
    }
  jq '.control_files[1] = "-option-like-control"' "$rollback_state" \
    >"$rollback_state.invalid"
  if rollback_control_files "$rollback_state.invalid" >/dev/null; then
    printf 'rollback control list accepted an option-like asset\n' >&2
    exit 1
  fi
  CONTROL_DIR=$tmp_dir/remove-control-files
  mkdir "$CONTROL_DIR"
  rm_calls=0
  rm() {
    rm_calls=$((rm_calls + 1))
    [[ $rm_calls -ne 1 ]]
  }
  jq '.control_dir_present = true' "$rollback_state" >"$rollback_state.present"
  if remove_snapshotted_control_files "$rollback_state.present"; then
    printf 'control cleanup hid an earlier removal failure\n' >&2
    exit 1
  fi
  [[ $rm_calls -eq 1 ]] || {
    printf 'control cleanup continued after a removal failure\n' >&2
    exit 1
  }
  rm_calls=0
  if remove_snapshotted_control_files "$rollback_state"; then
    printf 'control cleanup accepted a missing directory-state field\n' >&2
    exit 1
  fi
  [[ $rm_calls -eq 0 ]] || {
    printf 'control cleanup removed files with invalid directory state\n' >&2
    exit 1
  }
  jq '.control_dir_present = false' "$rollback_state" >"$rollback_state.absent"
  remove_snapshotted_control_files "$rollback_state.absent" || {
    printf 'control cleanup rejected an explicitly absent control directory\n' >&2
    exit 1
  }
  [[ $rm_calls -eq 0 ]] || {
    printf 'control cleanup removed files for an absent control directory\n' >&2
    exit 1
  }
  jq '.control_dir_present = "false"' "$rollback_state" >"$rollback_state.invalid-state"
  rm_calls=0
  if remove_snapshotted_control_files "$rollback_state.invalid-state"; then
    printf 'control cleanup accepted a non-boolean directory-state field\n' >&2
    exit 1
  fi
  [[ $rm_calls -eq 0 ]] || {
    printf 'control cleanup removed files with a non-boolean directory state\n' >&2
    exit 1
  }
)
grep -Fq 'control_assets=$(release_control_assets "$CONTROL_DIR")' "$CUTOVER"
grep -Fq 'remove_snapshotted_control_files "$rollback_dir/state.json"' "$CUTOVER"
[[ $(grep -Fc 'control_files=$(rollback_control_files "$rollback_dir/state.json")' \
  "$CUTOVER") -eq 1 ]]

# Both controls must carry the same root-chain contract. Run it with deterministic
# stat/direct-directory doubles so these macOS-hosted tests cover Linux ownership
# semantics without requiring local root-owned fixtures.
root_chain_contract="$tmp_dir/root-chain-contract.sh"
gate_root_chain_contract=$(sed -n \
  -e '/^valid_absolute_path() {$/,/^}$/p' \
  -e '/^secure_root_directory() {$/,/^}$/p' \
  -e '/^secure_root_chain() {$/,/^}$/p' \
  -e '/^secure_root_chain_or_absent() {$/,/^}$/p' \
  -e '/^secure_collector_directory() {$/,/^}$/p' "$GATE")
cutover_root_chain_contract=$(sed -n \
  -e '/^valid_absolute_path() {$/,/^}$/p' \
  -e '/^secure_root_directory() {$/,/^}$/p' \
  -e '/^secure_root_chain() {$/,/^}$/p' \
  -e '/^secure_root_chain_or_absent() {$/,/^}$/p' \
  -e '/^secure_collector_directory() {$/,/^}$/p' "$CUTOVER")
[[ -n $gate_root_chain_contract \
  && $gate_root_chain_contract == "$cutover_root_chain_contract" ]] || {
  printf 'shadow and cutover trusted-directory contracts differ\n' >&2
  exit 1
}
printf '%s\n' "$gate_root_chain_contract" >"$root_chain_contract"
(
  mock_root_uid=0
  mock_root_mode=755
  mock_leaf_mode=750
  mock_leaf_owner=hftcollector
  mock_leaf_group=hftcollector
  direct_directory() {
    return 0
  }
  stat() {
    [[ $1 == -c && $3 == -- && $# -eq 4 ]] || return 64
    case "$2" in
      %u) printf '%s\n' "$mock_root_uid" ;;
      %a)
        if [[ $4 == /trusted/spool ]]; then
          printf '%s\n' "$mock_leaf_mode"
        else
          printf '%s\n' "$mock_root_mode"
        fi
        ;;
      %U) printf '%s\n' "$mock_leaf_owner" ;;
      %G) printf '%s\n' "$mock_leaf_group" ;;
      *) return 65 ;;
    esac
  }
  # shellcheck source=/dev/null
  source "$root_chain_contract"
  secure_root_chain /trusted/leaf || exit 1
  secure_collector_directory /trusted/spool || exit 1
  mock_root_mode=775
  if secure_root_chain /trusted/leaf; then
    printf 'root-chain contract accepted a writable ancestor\n' >&2
    exit 1
  fi
  mock_root_mode=755
  mock_root_uid=1000
  if secure_root_chain /trusted/leaf; then
    printf 'root-chain contract accepted a non-root ancestor\n' >&2
    exit 1
  fi
  mock_root_uid=0
  mock_leaf_mode=770
  if secure_collector_directory /trusted/spool; then
    printf 'collector-directory contract accepted writable leaf permissions\n' >&2
    exit 1
  fi
  if secure_root_chain /trusted/../leaf; then
    printf 'root-chain contract accepted a parent traversal\n' >&2
    exit 1
  fi
  if secure_root_chain_or_absent /trusted/missing/../leaf; then
    printf 'absent root-chain contract accepted a parent traversal\n' >&2
    exit 1
  fi
  dangling_parent="$tmp_dir/root-chain-dangling"
  mkdir "$dangling_parent"
  ln -s missing "$dangling_parent/link"
  if secure_root_chain_or_absent "$dangling_parent/link/child"; then
    printf 'root-chain contract accepted a dangling intermediate symlink\n' >&2
    exit 1
  fi
)

# Exercise the journal cursor/restart detector with deterministic journal JSON.
# Both release scripts must carry the exact same implementation so one behavior
# test covers the code that guards both the shadow stop and production cutover.
cutover_journal_contract=$(sed -n \
  -e '/^journal_cursor() {$/,/^}$/p' \
  -e '/^verify_no_restart_after_cursor() {$/,/^}$/p' "$CUTOVER")
gate_journal_contract=$(sed -n \
  -e '/^journal_cursor() {$/,/^}$/p' \
  -e '/^verify_no_restart_after_cursor() {$/,/^}$/p' "$GATE")
[[ -n $cutover_journal_contract \
  && $cutover_journal_contract == "$gate_journal_contract" ]] || {
  printf 'shadow and cutover journal restart guards are missing or differ\n' >&2
  exit 1
}
journal_contract="$tmp_dir/journal-contract.sh"
printf '%s\n' "$cutover_journal_contract" >"$journal_contract"
# shellcheck source=/dev/null
source "$journal_contract"

journal_test_unit=polymarket-contract-test.service
journal_cursor_fixture='s=contract-cursor'
journal_fixture=
journal_failure_mode=none
journalctl() {
  local arguments="$*"
  if [[ $arguments == '--sync' ]]; then
    [[ $journal_failure_mode != sync ]] || return 70
    return 0
  fi
  if [[ $arguments == \
    "--unit $journal_test_unit --lines=0 --show-cursor --no-pager" ]]; then
    [[ $journal_failure_mode != cursor ]] || return 71
    printf '%s\n' "-- cursor: $journal_cursor_fixture"
    return 0
  fi
  if [[ $arguments == \
    "--unit $journal_test_unit --after-cursor $journal_cursor_fixture --output=json --no-pager" ]]; then
    [[ $journal_failure_mode != after-cursor ]] || return 72
    [[ -z $journal_fixture ]] || printf '%s\n' "$journal_fixture"
    return 0
  fi
  return 64
}

captured_cursor=$(journal_cursor "$journal_test_unit")
[[ $captured_cursor == "$journal_cursor_fixture" ]] || {
  printf 'journal cursor helper did not return the exact synchronized cursor\n' >&2
  exit 1
}
journal_cursor_fixture=
if journal_cursor "$journal_test_unit" >/dev/null; then
  printf 'journal cursor helper accepted an empty cursor\n' >&2
  exit 1
fi
journal_cursor_fixture='s=contract-cursor'
journal_failure_mode=sync
if journal_cursor "$journal_test_unit" >/dev/null; then
  printf 'journal cursor helper ignored journal synchronization failure\n' >&2
  exit 1
fi
journal_failure_mode=cursor
if journal_cursor "$journal_test_unit" >/dev/null; then
  printf 'journal cursor helper ignored cursor query failure\n' >&2
  exit 1
fi
journal_failure_mode=none

expected_invocation_id=$(printf '1%.0s' {1..32})
other_invocation_id=$(printf '2%.0s' {1..32})
journal_fixture=$(jq -cn '{MESSAGE_ID:"unrelated"}')
verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id" || {
    printf 'journal restart guard rejected an unrelated event\n' >&2
    exit 1
  }
journal_fixture=$(jq -cn --arg invocation "$expected_invocation_id" \
  '{MESSAGE_ID:"39f53479d3a045ac8e11786248231fbf",INVOCATION_ID:$invocation,
    _SYSTEMD_INVOCATION_ID:$invocation}')
verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id" || {
    printf 'journal restart guard rejected the expected invocation\n' >&2
    exit 1
  }
journal_fixture=$(jq -cn --arg invocation "$expected_invocation_id" \
  '{MESSAGE_ID:"5eb03494b6584870a536b337290809b3",INVOCATION_ID:$invocation}')
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id"; then
  printf 'journal restart guard accepted an automatic restart event\n' >&2
  exit 1
fi
journal_fixture=$(jq -cn --arg invocation "$other_invocation_id" \
  '{MESSAGE_ID:"be02cf6855d2428ba40df7e9d022f03d",INVOCATION_ID:$invocation}')
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id"; then
  printf 'journal restart guard accepted UNIT_FAILED from a different invocation ID\n' >&2
  exit 1
fi
journal_fixture=$(jq -cn --arg invocation "$other_invocation_id" \
  '{MESSAGE_ID:"unrelated",_SYSTEMD_INVOCATION_ID:$invocation}')
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id"; then
  printf 'journal restart guard accepted a different systemd invocation ID\n' >&2
  exit 1
fi
journal_fixture=
journal_failure_mode=sync
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id"; then
  printf 'journal restart guard ignored journal synchronization failure\n' >&2
  exit 1
fi
journal_failure_mode=after-cursor
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id"; then
  printf 'journal restart guard ignored journal query failure\n' >&2
  exit 1
fi
journal_failure_mode=none
journal_fixture='{not-json}'
if verify_no_restart_after_cursor \
  "$journal_test_unit" "$journal_cursor_fixture" "$expected_invocation_id" \
  2>/dev/null; then
  printf 'journal restart guard accepted malformed journal JSON\n' >&2
  exit 1
fi

# Execute the production success-marker statements against temporary evidence.
# The marker must be a unique, exact checksum of cutover.json and a rerun must
# fail closed rather than replacing existing evidence.
cutover_marker_publisher="$tmp_dir/publish-cutover-marker.sh"
sed -n '/^success_marker="\$evidence_dir\/PASSED.sha256"$/,/^sync -f "\$evidence_dir"$/p' \
  "$CUTOVER" >"$cutover_marker_publisher"
[[ -s $cutover_marker_publisher ]] || {
  printf 'cutover success-marker publisher is missing\n' >&2
  exit 1
}
publish_cutover_marker() (
  evidence_dir=$1
  secure_root_chain() {
    [[ -d $1 && ! -L $1 ]]
  }
  die() {
    printf 'marker publication failed: %s\n' "$*" >&2
    exit 1
  }
  mv() {
    if [[ ${1:-} == -Tf && $# -eq 3 ]]; then
      command mv -f "$2" "$3"
    else
      command mv "$@"
    fi
  }
  # shellcheck source=/dev/null
  source "$cutover_marker_publisher"
)

cutover_success_dir="$tmp_dir/cutover-success"
mkdir "$cutover_success_dir"
printf '%s\n' '{"schema":"monday.polymarket_cutover.v1"}' \
  >"$cutover_success_dir/cutover.json"
publish_cutover_marker "$cutover_success_dir"
[[ $(wc -l <"$cutover_success_dir/PASSED.sha256") -eq 1 ]] || {
  printf 'cutover success marker is not exactly one line\n' >&2
  exit 1
}
expected_cutover_marker=$(cd "$cutover_success_dir" && sha256sum cutover.json)
[[ $(<"$cutover_success_dir/PASSED.sha256") == "$expected_cutover_marker" ]] || {
  printf 'cutover success marker does not bind the exact cutover.json\n' >&2
  exit 1
}
(
  cd "$cutover_success_dir"
  sha256sum --check --strict PASSED.sha256 >/dev/null
)
if publish_cutover_marker "$cutover_success_dir" 2>/dev/null; then
  printf 'cutover marker publisher overwrote existing success evidence\n' >&2
  exit 1
fi

# Exercise the production rollback-evidence helpers and EXIT trap. Rollback
# must durably revoke PASSED before restore can mutate runtime state, then give
# the preserved evidence its final invalid/rolled-back name.
rollback_evidence_helpers="$tmp_dir/rollback-evidence-helpers.sh"
sed -n \
  -e '/^verify_named_marker() {$/,/^}$/p' \
  -e '/^prepare_rollback_evidence() {$/,/^}$/p' \
  -e '/^finalize_rollback_evidence() {$/,/^}$/p' \
  "$CUTOVER" >"$rollback_evidence_helpers"
[[ -s $rollback_evidence_helpers ]] || {
  printf 'rollback evidence helpers are missing\n' >&2
  exit 1
}
# shellcheck source=/dev/null
source "$rollback_evidence_helpers"

# Exercise the public rollback branch through its filesystem/system boundaries.
# Admission may recover markerless or pending evidence at the saved baseline,
# but must reject any unrelated active release before mutation begins.
manual_lineage_contract="$tmp_dir/manual-lineage-contract.sh"
sed -n '/^if \[\[ \$mode == rollback \]\]; then$/,/^# Cutover depends/p' "$CUTOVER" \
  | sed '$d' >"$manual_lineage_contract"
[[ -s $manual_lineage_contract ]] || {
  printf 'manual rollback lineage contract is missing\n' >&2
  exit 1
}
lineage_root="$tmp_dir/manual-lineage"
lineage_release_root="$lineage_root/releases"
lineage_evidence_root="$lineage_root/evidence"
lineage_active="$lineage_root/bin/polymarket-raw-ops"
mkdir -p "$lineage_release_root" "$lineage_evidence_root" "${lineage_active%/*}"
make_lineage_release() {
  local label=$1 staging sha path
  staging=$(mktemp "$lineage_root/release.XXXXXX")
  printf '%s\n' "$label" >"$staging"
  sha=$(sha256sum "$staging" | awk '{print $1}')
  path="$lineage_release_root/$sha/polymarket-raw-ops"
  mkdir -p "${path%/*}"
  mv "$staging" "$path"
  chmod +x "$path"
  printf '%s\n' "$path"
}
lineage_candidate=$(make_lineage_release candidate)
lineage_saved=$(make_lineage_release saved-baseline)
lineage_third=$(make_lineage_release unrelated-third-release)
lineage_candidate_sha=${lineage_candidate%/*}; lineage_candidate_sha=${lineage_candidate_sha##*/}
lineage_saved_sha=${lineage_saved%/*}; lineage_saved_sha=${lineage_saved_sha##*/}
make_lineage_evidence() {
  local name=$1 marker_state=$2 evidence manifest_sha
  evidence="$lineage_evidence_root/$name"
  mkdir -p "$evidence/rollback"
  jq -n --arg candidate "$lineage_candidate_sha" --arg saved "$lineage_saved" \
    --arg saved_sha "$lineage_saved_sha" \
    '{baseline_mode:"rust_release",candidate_sha256:$candidate,
      active_symlink:{target:$saved,sha256:$saved_sha}}' >"$evidence/rollback/state.json"
  (cd "$evidence/rollback" && sha256sum state.json >manifest.sha256)
  if [[ $marker_state == pending ]]; then
    manifest_sha=$(sha256sum "$evidence/rollback/manifest.sha256" | awk '{print $1}')
    jq -n --arg candidate "$lineage_candidate_sha" --arg manifest "$manifest_sha" \
      '{candidate_sha256:$candidate,rollback_manifest_sha256:$manifest}' \
      >"$evidence/cutover.json"
    (cd "$evidence" && sha256sum cutover.json >PASSED.rollback-pending.sha256)
  fi
  printf '%s\n' "$evidence"
}
exercise_manual_lineage() (
  set -euo pipefail
  local evidence=$1 active=$2 mutation_log=$1/mutations
  EVIDENCE_ROOT=$lineage_evidence_root RELEASE_ROOT=$lineage_release_root
  ACTIVE_BINARY=$lineage_active mode=rollback
  set -- rollback "$evidence"
  : >"$mutation_log"
  rm -f "$ACTIVE_BINARY"
  ln -s "$active" "$ACTIVE_BINARY"
  secure_root_chain() { [[ -d $1 && ! -L $1 ]]; }
  secure_release_directory() { [[ -d $1 && ! -L $1 ]]; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  prepare_rollback_evidence() { printf 'prepare\n' >>"$mutation_log"; }
  restore_legacy() { printf 'restore\n' >>"$mutation_log"; }
  finalize_rollback_evidence() { printf 'finalize\n' >>"$mutation_log"; }
  die() { printf 'manual lineage rejected: %s\n' "$*" >&2; exit 1; }
  # shellcheck source=/dev/null
  source "$manual_lineage_contract"
)
markerless_lineage=$(make_lineage_evidence markerless none)
exercise_manual_lineage "$markerless_lineage" "$lineage_saved" >/dev/null
[[ $(<"$markerless_lineage/mutations") == $'prepare\nrestore\nfinalize' ]] || {
  printf 'markerless rollback snapshot was not admitted\n' >&2
  exit 1
}
pending_lineage=$(make_lineage_evidence pending-saved pending)
exercise_manual_lineage "$pending_lineage" "$lineage_saved" >/dev/null
[[ $(<"$pending_lineage/mutations") == $'prepare\nrestore\nfinalize' ]] || {
  printf 'pending rollback rejected the exact saved baseline\n' >&2
  exit 1
}
third_lineage=$(make_lineage_evidence pending-third pending)
set +e
exercise_manual_lineage "$third_lineage" "$lineage_third" >/dev/null 2>&1
third_lineage_status=$?
set -e
[[ $third_lineage_status -ne 0 && ! -s $third_lineage/mutations ]] || {
  printf 'manual rollback admitted an unrelated active release\n' >&2
  exit 1
}

# A bootstrap recovery snapshots a direct binary, not an immutable-release
# symlink. Manual rollback must admit only that exact direct binary and must
# still reject a different direct binary before it mutates the saved state.
make_bootstrap_lineage_evidence() {
  local name=$1 marker_state=$2 direct_mode=${3:-0755} evidence manifest_sha
  evidence="$lineage_evidence_root/$name"
  mkdir -p "$evidence/rollback/bin"
  printf 'saved-baseline\n' >"$evidence/rollback/bin/polymarket-raw-ops"
  chmod +x "$evidence/rollback/bin/polymarket-raw-ops"
  jq -n --arg candidate "$lineage_candidate_sha" --arg path "$lineage_active" \
    --arg sha "$lineage_saved_sha" \
    --arg mode "$direct_mode" \
    '{baseline_mode:"rust_bootstrap",candidate_sha256:$candidate,
      active_direct:{path:$path,sha256:$sha,mode:$mode}}' \
    >"$evidence/rollback/state.json"
  (cd "$evidence/rollback" && sha256sum state.json bin/polymarket-raw-ops >manifest.sha256)
  if [[ $marker_state == pending ]]; then
    manifest_sha=$(sha256sum "$evidence/rollback/manifest.sha256" | awk '{print $1}')
    jq -n --arg candidate "$lineage_candidate_sha" --arg manifest "$manifest_sha" \
      '{candidate_sha256:$candidate,rollback_manifest_sha256:$manifest}' \
      >"$evidence/cutover.json"
    (cd "$evidence" && sha256sum cutover.json >PASSED.rollback-pending.sha256)
  fi
  printf '%s\n' "$evidence"
}
exercise_manual_bootstrap_lineage() (
  set -euo pipefail
  local evidence=$1 active_contents=$2 active_mode=${3:-0755} mutation_log=$1/mutations
  EVIDENCE_ROOT=$lineage_evidence_root RELEASE_ROOT=$lineage_release_root
  ACTIVE_BINARY=$lineage_active mode=rollback
  set -- rollback "$evidence"
  : >"$mutation_log"
  rm -f "$ACTIVE_BINARY"
  printf '%s\n' "$active_contents" >"$ACTIVE_BINARY"
  chmod "$active_mode" "$ACTIVE_BINARY"
  secure_root_chain() { [[ -d $1 && ! -L $1 ]]; }
  secure_release_directory() { [[ -d $1 && ! -L $1 ]]; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  stat() {
    [[ ${1:-} == -c && ${2:-} == %a && ${3:-} == -- && ${4:-} == "$ACTIVE_BINARY" ]] \
      || return 1
    [[ $active_mode == 0644 ]] && printf '644\n' || printf '755\n'
  }
  prepare_rollback_evidence() { printf 'prepare\n' >>"$mutation_log"; }
  restore_legacy() { printf 'restore\n' >>"$mutation_log"; }
  finalize_rollback_evidence() { printf 'finalize\n' >>"$mutation_log"; }
  die() { printf 'manual bootstrap lineage rejected: %s\n' "$*" >&2; exit 1; }
  # shellcheck source=/dev/null
  source "$manual_lineage_contract"
)
bootstrap_lineage=$(make_bootstrap_lineage_evidence bootstrap-saved pending)
exercise_manual_bootstrap_lineage "$bootstrap_lineage" saved-baseline >/dev/null
[[ $(<"$bootstrap_lineage/mutations") == $'prepare\nrestore\nfinalize' ]] || {
  printf 'manual rollback rejected the exact direct bootstrap baseline\n' >&2
  exit 1
}
bootstrap_drift_lineage=$(make_bootstrap_lineage_evidence bootstrap-drift pending)
set +e
exercise_manual_bootstrap_lineage "$bootstrap_drift_lineage" unrelated-bootstrap >/dev/null 2>&1
bootstrap_drift_status=$?
set -e
[[ $bootstrap_drift_status -ne 0 && ! -s $bootstrap_drift_lineage/mutations ]] || {
  printf 'manual rollback admitted a different direct bootstrap binary\n' >&2
  exit 1
}
bootstrap_bad_mode_lineage=$(make_bootstrap_lineage_evidence bootstrap-bad-mode pending invalid)
set +e
exercise_manual_bootstrap_lineage "$bootstrap_bad_mode_lineage" saved-baseline >/dev/null 2>&1
bootstrap_bad_mode_status=$?
set -e
[[ $bootstrap_bad_mode_status -ne 0 && ! -s $bootstrap_bad_mode_lineage/mutations ]] || {
  printf 'manual rollback admitted malformed bootstrap rollback metadata\n' >&2
  exit 1
}
bootstrap_nonexec_lineage=$(make_bootstrap_lineage_evidence bootstrap-nonexec pending 0644)
set +e
exercise_manual_bootstrap_lineage "$bootstrap_nonexec_lineage" saved-baseline >/dev/null 2>&1
bootstrap_nonexec_status=$?
set -e
[[ $bootstrap_nonexec_status -ne 0 && ! -s $bootstrap_nonexec_lineage/mutations ]] || {
  printf 'manual rollback admitted a non-executable bootstrap rollback image\n' >&2
  exit 1
}
bootstrap_active_mode_drift_lineage=$(make_bootstrap_lineage_evidence bootstrap-active-mode-drift pending)
set +e
exercise_manual_bootstrap_lineage "$bootstrap_active_mode_drift_lineage" saved-baseline 0644 \
  >/dev/null 2>&1
bootstrap_active_mode_drift_status=$?
set -e
[[ $bootstrap_active_mode_drift_status -ne 0 \
  && ! -s $bootstrap_active_mode_drift_lineage/mutations ]] || {
  printf 'manual rollback admitted bootstrap mode drift before evidence invalidation\n' >&2
  exit 1
}

# The bootstrap admission allows an absent global control directory. Its
# snapshot must still have a verifiable manifest, and a direct binary changed
# during the copy must not enter the rollback payload.
snapshot_legacy_contract="$tmp_dir/snapshot-legacy-contract.sh"
sed -n '/^snapshot_legacy() {$/,/^}$/p' "$CUTOVER" >"$snapshot_legacy_contract"
[[ -s $snapshot_legacy_contract ]] || {
  printf 'bootstrap rollback snapshot contract is missing\n' >&2
  exit 1
}
exercise_bootstrap_snapshot() (
  set -euo pipefail
  local case_name=$1 copy_drift=${2:-false} root rollback systemd_fixture candidate_sha baseline_sha
  root="$tmp_dir/bootstrap-snapshot-$case_name"
  rollback="$root/rollback"
  systemd_fixture="$root/systemd"
  mkdir -p "$systemd_fixture" "$root/bin"
  UNIT_ASSETS=(
    polymarket-reference-collector.service
    polymarket-reference-upload.service
    polymarket-reference-upload.timer
    polymarket-market-tape-upload.service
    polymarket-market-tape-upload.timer
  )
  COLLECTOR_UNIT=polymarket-reference-collector.service
  REFERENCE_UPLOAD_TIMER=polymarket-reference-upload.timer
  MARKET_UPLOAD_TIMER=polymarket-market-tape-upload.timer
  for asset in "${UNIT_ASSETS[@]}"; do
    printf 'unit=%s\n' "$asset" >"$systemd_fixture/$asset"
  done
  UPLOAD_ENV="$root/upload.env"
  printf 'upload-env\n' >"$UPLOAD_ENV"
  ACTIVE_BINARY="$root/bin/polymarket-raw-ops"
  printf 'bootstrap-original\n' >"$ACTIVE_BINARY"
  chmod 0755 "$ACTIVE_BINARY"
  CONTROL_DIR="$root/absent-control"
  candidate_sha=$(printf 'c%.0s' {1..64})
  baseline_sha=$(sha256sum "$ACTIVE_BINARY" | awk '{print $1}')
  printf 'bootstrap-replaced\n' >"$root/replaced"
  secure_root_chain() { return 0; }
  secure_regular_file() { return 0; }
  unit_enabled() { return 1; }
  unit_active() { return 1; }
  stat() { printf '0755\n'; }
  sync() { :; }
  install() {
    local install_mode input destination
    if [[ ${1:-} == -d ]]; then
      command install "$@"
      return
    fi
    [[ ${1:-} == -m && $# -eq 4 ]] || {
      command install "$@"
      return
    }
    install_mode=$2
    input=$3
    destination=$4
    [[ $input != /etc/systemd/system/* ]] \
      || input="$systemd_fixture/${input##*/}"
    [[ $input != "$ACTIVE_BINARY" || $copy_drift != true ]] \
      || input="$root/replaced"
    command install -m "$install_mode" "$input" "$destination"
  }
  die() { printf 'snapshot rejected: %s\n' "$*" >&2; exit 1; }
  # shellcheck source=/dev/null
  source "$snapshot_legacy_contract"
  (
    snapshot_legacy "$rollback" rust_bootstrap "$ACTIVE_BINARY" "$baseline_sha" "$candidate_sha"
    # The snapshot_legacy copies ACTIVE_BINARY into the rollback dir; force a real
    # sync (the test's sync() override is a no-op) so the digest check does not
    # read a partially-flushed file under CI cache/IO pressure (#731 flake). Fail
    # closed if sync itself fails, so the checksum check never runs on a
    # potentially unflushed file.
    command sync >/dev/null 2>&1 || die 'filesystem sync failed before snapshot digest check'
    (cd "$rollback" && sha256sum --check --strict manifest.sha256 >/dev/null)
    jq -e '.control_dir_present == false' "$rollback/state.json" >/dev/null
  )
)
exercise_bootstrap_snapshot absent-control
if exercise_bootstrap_snapshot copied-binary-drift true; then
  printf 'bootstrap snapshot accepted a binary changed during copy\n' >&2
  exit 1
fi

cutover_failure_cleanup="$tmp_dir/cutover-failure-cleanup.sh"
sed -n '/^on_exit() {$/,/^}$/p' "$CUTOVER" >"$cutover_failure_cleanup"
[[ -s $cutover_failure_cleanup ]] || {
  printf 'cutover automatic failure cleanup is missing\n' >&2
  exit 1
}
exercise_failed_cutover_cleanup() (
  set +e
  evidence_dir=$1
  restore_result=$2
  cutover_succeeded=false
  transition_started=true
  restore_legacy() {
    [[ ! -e $evidence_dir/PASSED.sha256 \
      && -f $evidence_dir/PASSED.rollback-pending.sha256 ]] || return 90
    printf '%s\n' pending >"$evidence_dir/restore-observed-pending"
    return "$restore_result"
  }
  secure_regular_file() {
    [[ -f $1 && ! -L $1 ]]
  }
  secure_root_chain() {
    [[ -d $1 && ! -L $1 ]]
  }
  die() {
    printf 'rollback evidence transition failed: %s\n' "$*" >&2
    exit 1
  }
  mv() {
    if [[ ${1:-} == -Tf && $# -eq 3 ]]; then
      command mv -f "$2" "$3"
    else
      command mv "$@"
    fi
  }
  # shellcheck source=/dev/null
  source "$cutover_failure_cleanup"
  false
  on_exit
)

automatic_failure_dir="$tmp_dir/cutover-automatic-failure"
mkdir "$automatic_failure_dir"
printf '%s\n' '{"schema":"monday.polymarket_cutover.v1"}' \
  >"$automatic_failure_dir/cutover.json"
publish_cutover_marker "$automatic_failure_dir"
set +e
exercise_failed_cutover_cleanup "$automatic_failure_dir" 0 >/dev/null 2>&1
automatic_failure_status=$?
set -e
[[ $automatic_failure_status -ne 0 \
  && ! -e $automatic_failure_dir/PASSED.sha256 \
  && ! -e $automatic_failure_dir/PASSED.rollback-pending.sha256 \
  && -f $automatic_failure_dir/cutover.json \
  && -f $automatic_failure_dir/PASSED.invalid.sha256 \
  && -f $automatic_failure_dir/restore-observed-pending ]] || {
  printf 'automatic rollback left success-looking cutover evidence\n' >&2
  exit 1
}
(
  cd "$automatic_failure_dir"
  sha256sum --check --strict PASSED.invalid.sha256 >/dev/null
)

automatic_restore_failure_dir="$tmp_dir/cutover-automatic-restore-failure"
mkdir "$automatic_restore_failure_dir"
printf '%s\n' '{"schema":"monday.polymarket_cutover.v1"}' \
  >"$automatic_restore_failure_dir/cutover.json"
publish_cutover_marker "$automatic_restore_failure_dir"
set +e
exercise_failed_cutover_cleanup "$automatic_restore_failure_dir" 1 >/dev/null 2>&1
automatic_restore_failure_status=$?
set -e
[[ $automatic_restore_failure_status -ne 0 \
  && ! -e $automatic_restore_failure_dir/PASSED.sha256 \
  && -f $automatic_restore_failure_dir/PASSED.rollback-pending.sha256 \
  && -f $automatic_restore_failure_dir/cutover.json \
  && ! -e $automatic_restore_failure_dir/PASSED.invalid.sha256 \
  && -f $automatic_restore_failure_dir/restore-observed-pending ]] || {
  printf 'failed automatic restore left ambiguous cutover evidence\n' >&2
  exit 1
}
(
  cd "$automatic_restore_failure_dir"
  sha256sum --check --strict PASSED.rollback-pending.sha256 >/dev/null
)

automatic_finalize_conflict_dir="$tmp_dir/cutover-automatic-finalize-conflict"
mkdir "$automatic_finalize_conflict_dir"
printf '%s\n' '{"schema":"monday.polymarket_cutover.v1"}' \
  >"$automatic_finalize_conflict_dir/cutover.json"
publish_cutover_marker "$automatic_finalize_conflict_dir"
printf 'preserve existing forensic marker\n' \
  >"$automatic_finalize_conflict_dir/PASSED.invalid.sha256"
set +e
exercise_failed_cutover_cleanup "$automatic_finalize_conflict_dir" 0 >/dev/null 2>&1
automatic_finalize_conflict_status=$?
set -e
[[ $automatic_finalize_conflict_status -ne 0 \
  && ! -e $automatic_finalize_conflict_dir/PASSED.sha256 \
  && -f $automatic_finalize_conflict_dir/PASSED.rollback-pending.sha256 \
  && $(<"$automatic_finalize_conflict_dir/PASSED.invalid.sha256") \
    == 'preserve existing forensic marker' ]] || {
  printf 'rollback finalization overwrote conflicting forensic evidence\n' >&2
  exit 1
}

# Execute the manual rollback transition itself. A failed restoration must
# leave rollback-pending evidence, and a retry must finalize it before exit 0.
manual_rollback_contract="$tmp_dir/manual-rollback-contract.sh"
sed -n '/^  prepare_rollback_evidence "\$rollback_evidence"/,/^  exit 0$/p' \
  "$CUTOVER" >"$manual_rollback_contract"
[[ -s $manual_rollback_contract ]] || {
  printf 'manual rollback evidence transition is missing\n' >&2
  exit 1
}
exercise_manual_rollback() (
  set -euo pipefail
  rollback_evidence=$1
  restore_result=$2
  restore_legacy() {
    [[ ! -e $rollback_evidence/PASSED.sha256 \
      && -f $rollback_evidence/PASSED.rollback-pending.sha256 ]] || return 90
    printf '%s\n' pending >"$rollback_evidence/restore-observed-pending"
    return "$restore_result"
  }
  secure_regular_file() {
    [[ -f $1 && ! -L $1 ]]
  }
  secure_root_chain() {
    [[ -d $1 && ! -L $1 ]]
  }
  die() {
    printf 'manual rollback evidence transition failed: %s\n' "$*" >&2
    exit 1
  }
  mv() {
    if [[ ${1:-} == -Tf && $# -eq 3 ]]; then
      command mv -f "$2" "$3"
    else
      command mv "$@"
    fi
  }
  # shellcheck source=/dev/null
  source "$manual_rollback_contract"
)

manual_failure_dir="$tmp_dir/manual-rollback-failure"
mkdir "$manual_failure_dir"
printf '%s\n' '{"schema":"monday.polymarket_cutover.v1"}' \
  >"$manual_failure_dir/cutover.json"
publish_cutover_marker "$manual_failure_dir"
set +e
exercise_manual_rollback "$manual_failure_dir" 1 >/dev/null 2>&1
manual_failure_status=$?
set -e
[[ $manual_failure_status -ne 0 \
  && ! -e $manual_failure_dir/PASSED.sha256 \
  && -f $manual_failure_dir/PASSED.rollback-pending.sha256 \
  && -f $manual_failure_dir/cutover.json \
  && -f $manual_failure_dir/restore-observed-pending \
  && ! -e $manual_failure_dir/PASSED.rolled-back.sha256 \
  && ! -e $manual_failure_dir/cutover.rolled-back.json ]] || {
  printf 'failed manual restoration did not leave rollback-pending evidence\n' >&2
  exit 1
}
(
  cd "$manual_failure_dir"
  sha256sum --check --strict PASSED.rollback-pending.sha256 >/dev/null
)

set +e
exercise_manual_rollback "$manual_failure_dir" 0 >/dev/null 2>&1
manual_success_status=$?
set -e
[[ $manual_success_status -eq 0 \
  && ! -e $manual_failure_dir/PASSED.sha256 \
  && ! -e $manual_failure_dir/PASSED.rollback-pending.sha256 \
  && -f $manual_failure_dir/cutover.json \
  && -f $manual_failure_dir/PASSED.rolled-back.sha256 \
  && ! -e $manual_failure_dir/cutover.rolled-back.json ]] || {
  printf 'successful manual rollback retained success-looking evidence\n' >&2
  exit 1
}
(
  cd "$manual_failure_dir"
  sha256sum --check --strict PASSED.rolled-back.sha256 >/dev/null
)

restore_legacy_contract="$tmp_dir/restore-legacy-contract.sh"
sed -n '/^restore_legacy() (/,/^)/p' "$CUTOVER" >"$restore_legacy_contract"
[[ -s $restore_legacy_contract ]] || {
  printf 'restore_legacy contract is missing\n' >&2
  exit 1
}
exercise_absent_control_dir_restore() (
  set +e
  evidence_dir=$1
  extra_control=${2:-false}
  rollback_dir=$evidence_dir/rollback
  CONTROL_DIR=$evidence_dir/global-control
  HEALTH=$evidence_dir/health.json
  UPLOAD_ENV=$evidence_dir/upload.env
  ACTIVE_BINARY=$evidence_dir/active-binary
  LEGACY_COLLECTOR=$evidence_dir/legacy-collector.py
  LEGACY_UPLOADER=$evidence_dir/legacy-uploader.py
  RELEASE_ROOT=$evidence_dir/releases
  RELEASE_MANIFEST=$evidence_dir/polymarket-raw-ops-release.json
  COLLECTOR_UNIT=collector.service
  REFERENCE_UPLOAD_UNIT=reference-upload.service
  REFERENCE_UPLOAD_TIMER=reference-upload.timer
  MARKET_UPLOAD_UNIT=market-upload.service
  MARKET_UPLOAD_TIMER=market-upload.timer
  UNIT_ASSETS=("$COLLECTOR_UNIT" "$REFERENCE_UPLOAD_UNIT" "$REFERENCE_UPLOAD_TIMER"
    "$MARKET_UPLOAD_UNIT" "$MARKET_UPLOAD_TIMER")
  BUNDLE_ASSETS=(candidate-control)
  PYTHON_ASSETS=(legacy-collector.py legacy-uploader.py)
  mkdir -p "$rollback_dir/control" "$CONTROL_DIR"
  printf 'legacy health policy\n' \
    >"$rollback_dir/control/polymarket-legacy-health-policy.jq"
  printf 'candidate control\n' >"$CONTROL_DIR/candidate-control"
  printf 'candidate manifest\n' \
    >"$CONTROL_DIR/${RELEASE_MANIFEST##*/}"
  [[ $extra_control == false ]] || printf 'unexpected\n' >"$CONTROL_DIR/unexpected"
  jq -n --argjson units "$(printf '%s\n' "${UNIT_ASSETS[@]}" \
      | jq -Rn '[inputs] | map({key:.,value:"0644"}) | from_entries')" '
      {baseline_mode:"legacy_python",control_dir_present:false,
       unit_modes:$units,upload_env_mode:"0640"}' \
    >"$rollback_dir/state.json"
  (
    cd "$rollback_dir"
    sha256sum state.json control/polymarket-legacy-health-policy.jq >manifest.sha256
  )
  secure_root_chain() { [[ -e $1 && ! -L $1 ]]; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  clear_health_before_restart() { :; }
  systemctl() { return 0; }
  rm() { command rm "$@"; }
  atomic_install() {
    [[ $3 != "$LEGACY_COLLECTOR" ]] || exit 77
  }
  die() { exit 66; }
  # shellcheck source=/dev/null
  source "$restore_legacy_contract"
  restore_legacy "$evidence_dir" >/dev/null 2>&1
)
absent_control_restore_dir="$tmp_dir/restore-absent-control-dir"
mkdir "$absent_control_restore_dir"
set +e
exercise_absent_control_dir_restore "$absent_control_restore_dir"
absent_control_restore_status=$?
set -e
[[ $absent_control_restore_status -ne 0 \
  && ! -e $absent_control_restore_dir/global-control ]] || {
  printf 'legacy rollback retained an absent control-directory snapshot\n' >&2
  exit 1
}
unexpected_control_restore_dir="$tmp_dir/restore-unexpected-control-dir"
mkdir "$unexpected_control_restore_dir"
set +e
exercise_absent_control_dir_restore "$unexpected_control_restore_dir" true
unexpected_control_restore_status=$?
set -e
[[ $unexpected_control_restore_status -eq 66 \
  && -f $unexpected_control_restore_dir/global-control/unexpected ]] || {
  printf 'legacy rollback removed or accepted an unexpected control entry\n' >&2
  exit 1
}
exercise_restore_manifest_guard() (
  set +e
  evidence_dir=$1
  rollback_dir=$evidence_dir/rollback
  mutation_log=$evidence_dir/mutation.log
  mkdir -p "$rollback_dir/control"
  printf 'legacy health policy\n' >"$rollback_dir/control/polymarket-legacy-health-policy.jq"
  printf '{}\n' >"$rollback_dir/state.json"
  printf 'keep spool state\n' >"$rollback_dir/spool.keep"
  (
    cd "$rollback_dir"
    sha256sum control/polymarket-legacy-health-policy.jq >manifest.sha256
  )
  jq -n --arg wrong "$(printf '0%.0s' {1..64})" \
    '{rollback_manifest_sha256:$wrong}' >"$evidence_dir/cutover.json"
  : >"$mutation_log"
  secure_root_chain() { [[ -e $1 && ! -L $1 ]]; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  clear_health_before_restart() { printf 'clear_health\n' >>"$mutation_log"; }
  atomic_install() { printf 'atomic_install\n' >>"$mutation_log"; }
  systemctl() { printf 'systemctl %s\n' "$*" >>"$mutation_log"; return 0; }
  rm() { printf 'rm %s\n' "$*" >>"$mutation_log"; return 0; }
  verify_fresh_legacy_runtime() { printf 'verify_fresh_legacy_runtime\n' >>"$mutation_log"; return 0; }
  verify_saved_unit_state() { printf 'verify_saved_unit_state\n' >>"$mutation_log"; return 0; }
  die() {
    printf 'restore manifest guard failed: %s\n' "$*" >&2
    exit 1
  }
  # shellcheck source=/dev/null
  source "$restore_legacy_contract"
  restore_legacy "$evidence_dir" >/dev/null 2>&1
)
restore_guard_dir="$tmp_dir/restore-manifest-guard"
mkdir "$restore_guard_dir"
set +e
exercise_restore_manifest_guard "$restore_guard_dir"
restore_manifest_guard_status=$?
set -e
[[ $restore_manifest_guard_status -ne 0 \
  && ! -s $restore_guard_dir/mutation.log \
  && -f $restore_guard_dir/rollback/state.json \
  && -f $restore_guard_dir/rollback/spool.keep ]] || {
  printf 'restore_legacy mutated runtime or deleted saved state before manifest lineage validation\n' >&2
  exit 1
}

exercise_restore_control_list_guard() (
  set +e
  evidence_dir=$1
  rollback_dir=$evidence_dir/rollback
  mutation_log=$evidence_dir/mutation.log
  mkdir -p "$rollback_dir/control"
  printf 'legacy health policy\n' >"$rollback_dir/control/polymarket-legacy-health-policy.jq"
  jq -n '{baseline_mode:"legacy_python",control_dir_present:true}' \
    >"$rollback_dir/state.json"
  (
    cd "$rollback_dir"
    sha256sum state.json control/polymarket-legacy-health-policy.jq >manifest.sha256
  )
  : >"$mutation_log"
  secure_root_chain() { [[ -e $1 && ! -L $1 ]]; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  systemctl() { printf 'systemctl %s\n' "$*" >>"$mutation_log"; return 0; }
  rm() { printf 'rm %s\n' "$*" >>"$mutation_log"; return 0; }
  die() {
    printf 'restore control-list guard failed: %s\n' "$*" >&2
    exit 1
  }
  # shellcheck source=/dev/null
  source "$rollback_control_contract"
  # shellcheck source=/dev/null
  source "$restore_legacy_contract"
  restore_legacy "$evidence_dir" >/dev/null 2>&1
)
restore_control_guard_dir="$tmp_dir/restore-control-list-guard"
mkdir "$restore_control_guard_dir"
set +e
exercise_restore_control_list_guard "$restore_control_guard_dir"
restore_control_guard_status=$?
set -e
[[ $restore_control_guard_status -ne 0 \
  && ! -s $restore_control_guard_dir/mutation.log ]] || {
  printf 'restore_legacy mutated runtime before validating rollback control membership\n' >&2
  exit 1
}

legacy="$tmp_dir/legacy"
rust="$tmp_dir/rust"
mkdir -p "$legacy" "$rust"
legacy_tape="$legacy/market-updates.ndjson"
rust_closed="$rust/market-updates.19700101T000400000000.ndjson"
: >"$legacy_tape"
: >"$rust_closed"
: >"$rust/market-updates.ndjson"

sequence=0
append_row() {
  local update=$1 recorded_at=${2:-1970-01-01T00:03:20Z} row
  row=$(jq -cn --argjson sequence "$sequence" --argjson update "$update" \
    --arg recorded_at "$recorded_at" \
    '{sequence:$sequence,recorded_at:$recorded_at,update:$update}')
  printf '%s\n' "$row" >>"$legacy_tape"
  printf '%s\n' "$row" >>"$rust_closed"
  sequence=$((sequence + 1))
}

for symbol in BTCUSDT ETHUSDT SOLUSDT XRPUSDT DOGEUSDT HYPEUSDT BNBUSDT; do
  case $symbol in
    BTCUSDT) market_name=Bitcoin ;;
    ETHUSDT) market_name=Ethereum ;;
    SOLUSDT) market_name=Solana ;;
    XRPUSDT) market_name=XRP ;;
    DOGEUSDT) market_name=Dogecoin ;;
    HYPEUSDT) market_name=Hyperliquid ;;
    BNBUSDT) market_name='Binance Coin' ;;
    *) printf 'unsupported fixture symbol: %s\n' "$symbol" >&2; exit 1 ;;
  esac
  append_row "$(jq -cn --arg symbol "$symbol" --arg market_name "$market_name" \
    '{kind:"market_metadata",market_id:("market-" + $symbol),
      condition_id:("condition-" + $symbol),symbol:$symbol,market_window_secs:300,
      source:"gamma_api",retrieved_at:"1970-01-01T00:03:20Z",
      market:{id:("market-" + $symbol),conditionId:("condition-" + $symbol),
        question:($market_name + " Up or Down"),slug:("market-" + $symbol),
        startDate:"1970-01-01T00:00:00Z",endDate:"1970-01-01T00:05:00Z",
        outcomes:["Up","Down"],clobTokenIds:[("up-" + $symbol),("down-" + $symbol)],
        orderPriceMinTickSize:0.01,orderMinSize:5,makerBaseFee:1000,takerBaseFee:1000,
        feesEnabled:true,negRisk:false}}')"
done

trade_id=$(printf '%s' '0x1|condition-BTCUSDT|up-BTCUSDT|BUY|200|0x2|1|0.5|0' \
  | sha256sum | awk '{print $1}')
trade=$(jq -cn --arg record_id "$trade_id" \
  '{kind:"polymarket_trade",record_id:$record_id,record_id_version:"v2",
    market_id:"market-BTCUSDT",condition_id:"condition-BTCUSDT",token_id:"up-BTCUSDT",
    symbol:"BTCUSDT",market_window_secs:300,side:"BUY",size:"1",price:"0.5",
    trade_ts:"1970-01-01T00:03:20Z",trade_ts_unix:200,
    transaction_hash:"0x1",proxy_wallet:"0x2",outcome:"Up",outcome_index:0,
    source:"polymarket_data_api",received_at:"1970-01-01T00:03:20Z",
    trade:{transactionHash:"0x1",conditionId:"condition-BTCUSDT",asset:"up-BTCUSDT",
      side:"BUY",timestamp:200,proxyWallet:"0x2",size:"1",price:"0.5",
      outcomeIndex:0,outcome:"Up"}}')
append_row "$trade"

late_settlement_trade_id=$(printf '%s' \
  '0xlate|condition-BTCUSDT|up-BTCUSDT|BUY|319|0x2|1|0.5|0' \
  | sha256sum | awk '{print $1}')
late_settlement_trade=$(jq -cn --arg record_id "$late_settlement_trade_id" \
  '{kind:"polymarket_trade",record_id:$record_id,record_id_version:"v2",
    market_id:"market-BTCUSDT",condition_id:"condition-BTCUSDT",token_id:"up-BTCUSDT",
    symbol:"BTCUSDT",market_window_secs:300,side:"BUY",size:"1",price:"0.5",
    trade_ts:"1970-01-01T00:05:19Z",trade_ts_unix:319,
    transaction_hash:"0xlate",proxy_wallet:"0x2",outcome:"Up",outcome_index:0,
    source:"polymarket_data_api",received_at:"1970-01-01T00:05:21Z",
    trade:{transactionHash:"0xlate",conditionId:"condition-BTCUSDT",asset:"up-BTCUSDT",
      side:"BUY",timestamp:319,proxyWallet:"0x2",size:"1",price:"0.5",
      outcomeIndex:0,outcome:"Up"}}')
append_row "$late_settlement_trade" "1970-01-01T00:05:21Z"

append_row "$(jq -cn \
  '{kind:"market_settlement",market_id:"market-BTCUSDT",
    condition_id:"condition-BTCUSDT",symbol:"BTCUSDT",market_window_secs:300,
    winning_token_id:"up-BTCUSDT",winning_outcome:"Up",resolved_up_won:true,
    resolution_source:"gamma_api_closed_market",retrieved_at:"1970-01-01T00:03:20Z",
    market:{id:"market-BTCUSDT",conditionId:"condition-BTCUSDT",
      question:"BTCUSDT Up or Down",startDate:"1970-01-01T00:00:00Z",
      endDate:"1970-01-01T00:05:00Z",closed:true,
      outcomes:["Up","Down"],clobTokenIds:["up-BTCUSDT","down-BTCUSDT"],
      outcomePrices:["1","0"]}}')"

# A delayed historical trade recorded only after the common cutoff must not make
# the bounded snapshot flaky, even when its source timestamp is in-window.
late_trade=$(jq -c \
  '.record_id = "trade-after-cutoff"
    | .trade_ts_unix = 200
    | .trade_ts = "1970-01-01T00:03:20Z"
    | .trade.timestamp = 200
    | .post_cutoff_only = true
    | .trade.postCutoffOnly = true' <<<"$trade")
jq -cn --argjson sequence "$sequence" --argjson update "$late_trade" \
  '{sequence:$sequence,recorded_at:"1970-01-01T00:51:41Z",update:$update}' \
  >>"$legacy_tape"

parity="$tmp_dir/parity.json"
"$VERIFY" verify-shadow-parity \
  --legacy-spool "$legacy" --rust-spool "$rust" --started-at-unix 100 \
  --ended-at-unix 3000 \
  --output "$parity"
jq -e '.passed == true and .checks.metadata_parity == true
  and .comparison_mode == "legacy_overlap"
  and ([.checks[]] | all)
  and .metrics.legacy_trade_count == 2
  and .metrics.rust_trade_count == 2
  and .metrics.trade_shared_value_mismatch_ids == []
  and (.metrics.normalized_trade_sha256 | test("^[a-f0-9]{64}$"))
  and (.metrics.normalized_metadata_sha256 | test("^[a-f0-9]{64}$"))
  and (.metrics.normalized_settlement_sha256 | test("^[a-f0-9]{64}$"))' \
  "$parity" >/dev/null

legacy_empty="$tmp_dir/legacy-empty"
mkdir "$legacy_empty"
: >"$legacy_empty/market-updates.ndjson"
rust_self_parity="$tmp_dir/rust-self-parity.json"
"$VERIFY" verify-shadow-parity \
  --trade-maturity-lag-seconds 600 \
  --legacy-spool "$legacy_empty" --rust-spool "$rust" \
  --started-at-unix 100 --ended-at-unix 1000 \
  --output "$rust_self_parity" --allow-empty-legacy
jq -e '.passed == true and .comparison_mode == "rust_self"
  and ([.checks[]] | all)
  and .metrics.legacy_trade_count == 0
  and .metrics.legacy_metadata_count == 0
  and .metrics.legacy_settlement_count == 0
  and .metrics.rust_trade_count > 0
  and .metrics.rust_metadata_count > 0
  and .metrics.rust_settlement_count > 0' "$rust_self_parity" >/dev/null

rust_self_missing_settlement="$tmp_dir/rust-self-missing-settlement"
cp -R "$rust" "$rust_self_missing_settlement"
jq -c 'select(.update.kind != "market_settlement")' \
  "$rust_self_missing_settlement/market-updates.19700101T000400000000.ndjson" \
  >"$rust_self_missing_settlement/market-updates.rewritten"
mv "$rust_self_missing_settlement/market-updates.rewritten" \
  "$rust_self_missing_settlement/market-updates.19700101T000400000000.ndjson"
if "$VERIFY" verify-shadow-parity --legacy-spool "$legacy_empty" \
  --trade-maturity-lag-seconds 600 \
  --rust-spool "$rust_self_missing_settlement" --started-at-unix 100 \
  --ended-at-unix 1000 --output "$tmp_dir/bad-rust-self-parity.json" \
  --allow-empty-legacy 2>/dev/null; then
  printf 'Rust-self parity accepted a window without settlements\n' >&2
  exit 1
fi
jq -e '.passed == false and .comparison_mode == "rust_self"
  and .checks.settlement_parity == false' \
  "$tmp_dir/bad-rust-self-parity.json" >/dev/null
jq -e 'select(.update.transaction_hash == "0xlate")
  | .update.trade_ts_unix == 319
    and .update.trade.timestamp == 319
    and .update.received_at == "1970-01-01T00:05:21Z"' \
  "$legacy_tape" "$rust_closed" >/dev/null

rust_bad="$tmp_dir/rust-bad"
cp -R "$rust" "$rust_bad"
jq -cn --argjson update "$trade" \
  '{sequence:0,recorded_at:"1970-01-01T00:03:21Z",update:$update}' \
  >"$rust_bad/market-updates.ndjson"
if "$VERIFY" verify-shadow-parity \
  --trade-maturity-lag-seconds 600 \
  --legacy-spool "$legacy" --rust-spool "$rust_bad" --started-at-unix 100 \
  --ended-at-unix 1000 \
  --output "$tmp_dir/bad-parity.json" 2>/dev/null; then
  printf 'parity verifier accepted a duplicate Rust trade ID\n' >&2
  exit 1
fi
jq -e '.passed == false and .checks.dedupe_parity == false' \
  "$tmp_dir/bad-parity.json" >/dev/null

rust_bad_metadata="$tmp_dir/rust-bad-metadata"
cp -R "$rust" "$rust_bad_metadata"
jq -c '
  if .update.kind == "market_metadata" and .update.symbol == "BTCUSDT"
  then .update.market.clobTokenIds = ["tampered-up", "tampered-down"]
  else . end
' "$rust_bad_metadata/market-updates.19700101T000400000000.ndjson" \
  >"$rust_bad_metadata/market-updates.rewritten"
mv "$rust_bad_metadata/market-updates.rewritten" \
  "$rust_bad_metadata/market-updates.19700101T000400000000.ndjson"
if "$VERIFY" verify-shadow-parity \
  --trade-maturity-lag-seconds 600 \
  --legacy-spool "$legacy" --rust-spool "$rust_bad_metadata" \
  --started-at-unix 100 --ended-at-unix 1000 \
  --output "$tmp_dir/bad-metadata-parity.json" 2>/dev/null; then
  printf 'parity verifier accepted contradictory metadata values\n' >&2
  exit 1
fi
jq -e '.passed == false and .checks.metadata_parity == false' \
  "$tmp_dir/bad-metadata-parity.json" >/dev/null

candidate=$(printf 'a%.0s' {1..64})
source_revision=$(printf 'b%.0s' {1..40})
bundle=$(printf 'c%.0s' {1..64})
oss_config=$(printf 'd%.0s' {1..64})
release_manifest_sha=$(printf '1%.0s' {1..64})
control_archive_sha=$(printf '2%.0s' {1..64})
legacy_invocation_id=$(printf 'e%.0s' {1..32})
shadow_invocation_id=$(printf 'f%.0s' {1..32})
legacy_cmdline=dffeb118d105e9312898460249f514eb982c20433cd20840ffb2107c64bbca4a
jq \
  --arg candidate "$candidate" \
  --arg source "$source_revision" \
  --arg bundle "$bundle" \
  --arg oss_config "$oss_config" \
  --arg release_manifest_sha "$release_manifest_sha" \
  --arg control_archive_sha "$control_archive_sha" \
  --arg legacy_invocation_id "$legacy_invocation_id" \
  --arg shadow_invocation_id "$shadow_invocation_id" \
  --arg legacy_cmdline "$legacy_cmdline" \
  '. + {
    schema:"monday.polymarket_shadow_gate.v1",
    candidate_sha256:$candidate,
    baseline_mode:"legacy_python",
    deployment_bundle_sha256:$bundle,
    deployment_source_revision:$source,
    release_manifest_sha256:$release_manifest_sha,
    control_archive_sha256:$control_archive_sha,
    oss_config_sha256:$oss_config,
    real_market_preflight:{
      schema:"monday.polymarket_real_market_preflight.v2",status:"passed",
      started_at:"1970-01-01T00:00:00Z",completed_at:"1970-01-01T00:01:00Z",
      candidate_sha256:$candidate,deployment_source_revision:$source,
      deployment_bundle_sha256:$bundle,
      release_manifest_sha256:$release_manifest_sha,
      control_archive_sha256:$control_archive_sha,oss_config_sha256:$oss_config,
      dataset:"crypto_expiry_preflight_aaaaaaaaaaaa_run-1",
      source_quote_records:1,source_recorded_hours:1,
      source_scan_bounded:true,
      source_content_sha256:$candidate,
      uploaded_content_sha256:$candidate,
      source_segment:{
        path:"/data/monday/spool/polymarket/market-updates.19700101T010000000000.123e4567-e89b-12d3-a456-426614174000.ndjson",
        file:"market-updates.19700101T010000000000.123e4567-e89b-12d3-a456-426614174000.ndjson",
        bytes:20,sha256:$candidate,file_identity:"1:12",modified_at_unix:1
      },
      uploaded_triplet:{
        uri:("oss://monday-lob-apne1-1045353359/lake/raw/venue=polymarket/dataset=crypto_expiry_preflight_aaaaaaaaaaaa_run-1/date=1970-01-01/hour=00/sha256=" + $candidate + "/market-updates.19700101T010000.19700101T00.ndjson.zst"),
        dataset:"crypto_expiry_preflight_aaaaaaaaaaaa_run-1",
        file:"market-updates.19700101T010000.19700101T00.ndjson.zst",
        bytes:10,source_bytes:20,sha256:$candidate,
        manifest_sha256:$bundle,success_sha256:$candidate,
        canonical:true,segment_complete:true
      },
      upload_summary:{uploaded_segments:1,canonical_uploaded_segments:1,
        pending_segments:0,failed_segments:[],last_error:null}
    },
    duration_seconds:3600,
    started_at:"1970-01-01T00:01:40Z",
    parity_window_started_at_unix:100,
    parity_window_ended_at_unix:3000,
    completed_at:"1970-01-01T01:12:01Z",
    shadow_run_id:"run-1",
    production_eligible:true,
    trade_parity_mode:"continuous_overlap",
    trade_parity_reason:"shadow and baseline trade emission semantics match; full trade coverage, field, and byte parity applies",
    shadow_emission:"continuous",
    baseline_emission:"continuous",
    parity_verifier:null,
    finalization_progress:null,
    baseline_health_start_required:true,
    baseline_runtime_stability_required:true,
    baseline_health_completion_required:true,
    baseline_health_snapshot:{
      updated_at:"1970-01-01T00:01:40.123456Z",
      last_success_at:"1970-01-01T00:01:40.123456Z",
      target_markets:14,api_errors:[],malformed_trade_rows:0,
      truncated_trade_markets:[],stale_trade_markets:[],
      stale_settlement_markets:[],overdue_unresolved_markets:[]
    },
    baseline_health_completion_snapshot:{
      updated_at:"1970-01-01T00:50:00.654321Z",
      last_success_at:"1970-01-01T00:50:00.654321Z",
      target_markets:14,api_errors:[],malformed_trade_rows:0,
      truncated_trade_markets:[],stale_trade_markets:[],
      stale_settlement_markets:[],overdue_unresolved_markets:[]
    },
    baseline_health_start_success_unix:100,
    baseline_health_cutoff_unix:3000,
    baseline_health_start_written_at_unix:100,
    baseline_health_completion_written_at_unix:3100,
    baseline_health_start_file_identity:"1:10",
    baseline_health_completion_file_identity:"1:11",
    legacy_runtime:{
      exec_start:"/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py",
      cmdline:"/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py",
      cmdline_sha256:$legacy_cmdline,
      fragment_path:"/etc/systemd/system/polymarket-reference-collector.service",
      drop_in_paths:[],main_pid:10,restarts:1,
      invocation_id:$legacy_invocation_id
    },
    shadow_runtime:{
      exec_start:("/opt/monday/releases/polymarket-raw-ops/" + $candidate
        + "/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200"
        + " --spool-dir ${MONDAY_POLYMARKET_SHADOW_SPOOL}"),
      cmdline:("/opt/monday/releases/polymarket-raw-ops/" + $candidate
        + "/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200"
        + " --spool-dir "
        + "/data/monday/spool/polymarket-reference-rust-shadow/" + $candidate + "/run-1"),
      fragment_path:"/etc/systemd/system/polymarket-reference-collector-shadow@.service",
      drop_in_paths:[],main_pid:11,restarts:0,
      invocation_id:$shadow_invocation_id,
      memory_events:{
        start:{high:0,max:0,oom:0,oom_kill:0,oom_group_kill:0},
        end:{high:0,max:0,oom:0,oom_kill:0,oom_group_kill:0}
      }
    },
    checks:(.checks + {health_freshness:true,candidate_identity:true,
      memory_events_stable:true,oss_readback_parity:true,
      market_oss_readback_parity:true,real_market_segment_preflight:true}),
    metrics:(.metrics + {
      oss_uploaded_segments:1,oss_canonical_uploaded_segments:1,
      market_oss_uploaded_segments:1,market_oss_canonical_uploaded_segments:1
    })
  } | .passed = true' "$parity" >"$tmp_dir/gate.json"
jq -e -f "$POLICY" "$tmp_dir/gate.json" >/dev/null
jq '.baseline_health_completion_required = false
  | .baseline_health_completion_snapshot = null
  | .baseline_health_cutoff_unix = null
  | .baseline_health_completion_written_at_unix = null
  | .baseline_health_completion_file_identity = null' \
  "$tmp_dir/gate.json" >"$tmp_dir/expedited-legacy-gate.json"
jq -e -f "$POLICY" "$tmp_dir/expedited-legacy-gate.json" >/dev/null || {
  printf 'gate policy rejected approved expedited legacy baseline evidence\n' >&2
  exit 1
}
legacy_rate_limit_error='trades 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa: HTTP Error 429: Too Many Requests'
jq --arg error "$legacy_rate_limit_error" \
  '.baseline_health_snapshot.api_errors = [$error]' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/rate-limited-legacy-gate.json"
jq -e -f "$POLICY" "$tmp_dir/rate-limited-legacy-gate.json" >/dev/null || {
  printf 'gate policy rejected a bounded legacy trades rate limit\n' >&2
  exit 1
}
jq -e --arg error "$legacy_rate_limit_error" \
  '.baseline_health_snapshot.api_errors == [$error]' \
  "$tmp_dir/rate-limited-legacy-gate.json" >/dev/null || {
  printf 'gate evidence did not preserve the accepted legacy rate limit\n' >&2
  exit 1
}
jq --arg error "$legacy_rate_limit_error" \
  '.baseline_health_snapshot.target_markets = 2792
    | .baseline_health_snapshot.api_errors = [range(0; 18) | $error]' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/ratio-bounded-rate-limited-legacy-gate.json"
jq -e -f "$POLICY" \
  "$tmp_dir/ratio-bounded-rate-limited-legacy-gate.json" >/dev/null || {
  printf 'gate policy rejected 18 trades rate limits for 2792 markets\n' >&2
  exit 1
}
jq -e '.baseline_health_snapshot.api_errors | length == 18' \
  "$tmp_dir/ratio-bounded-rate-limited-legacy-gate.json" >/dev/null || {
  printf 'gate evidence did not preserve all admitted trades rate limits\n' >&2
  exit 1
}
jq --arg error "$legacy_rate_limit_error" \
  '.baseline_health_snapshot.target_markets = 2792
    | .baseline_health_snapshot.api_errors = [range(0; 29) | $error]' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/ratio-excessive-rate-limited-legacy-gate.json"
if jq -e -f "$POLICY" \
  "$tmp_dir/ratio-excessive-rate-limited-legacy-gate.json" >/dev/null; then
  printf 'gate policy accepted 29 trades rate limits for 2792 markets\n' >&2
  exit 1
fi
jq --arg error "$legacy_rate_limit_error" \
  '.baseline_health_snapshot.target_markets = 10000
    | .baseline_health_snapshot.api_errors = [range(0; 33) | $error]' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/absolute-cap-rate-limited-legacy-gate.json"
if jq -e -f "$POLICY" \
  "$tmp_dir/absolute-cap-rate-limited-legacy-gate.json" >/dev/null; then
  printf 'gate policy accepted more than 32 trades rate limits\n' >&2
  exit 1
fi
jq --arg error "$legacy_rate_limit_error" \
  '.baseline_health_completion_snapshot.api_errors = [$error]' \
  "$tmp_dir/gate.json" >"$tmp_dir/rate-limited-legacy-completion-gate.json"
if jq -e -f "$POLICY" \
  "$tmp_dir/rate-limited-legacy-completion-gate.json" >/dev/null; then
  printf 'gate policy accepted a rate limit in the post-start completion snapshot\n' >&2
  exit 1
fi
jq --arg error "$legacy_rate_limit_error" \
  '.baseline_health_snapshot.api_errors = [$error, $error, $error, $error]' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/excessive-rate-limited-legacy-gate.json"
if jq -e -f "$POLICY" \
  "$tmp_dir/excessive-rate-limited-legacy-gate.json" >/dev/null; then
  printf 'gate policy accepted more than three legacy trades rate limits\n' >&2
  exit 1
fi
jq --arg rate_limit "$legacy_rate_limit_error" \
  '.baseline_health_snapshot.api_errors = [$rate_limit,
    "trades 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb: The read operation timed out"]' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/mixed-rate-limited-legacy-gate.json"
if jq -e -f "$POLICY" \
  "$tmp_dir/mixed-rate-limited-legacy-gate.json" >/dev/null; then
  printf 'gate policy accepted a mixed legacy API error list\n' >&2
  exit 1
fi
jq '.started_at = "1970-01-01T00:46:40Z"
  | .completed_at = "1970-01-01T01:46:40Z"
  | .parity_window_started_at_unix = 2800
  | .parity_window_ended_at_unix = 6400
  | .metrics.trade_event_window_started_at_unix = 2800
  | .metrics.trade_event_window_ended_at_unix = 4000
  | .metrics.settlement_event_window_started_at_unix = 1900
  | .metrics.settlement_event_window_ended_at_unix = 5800' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/start-health-age-boundary-gate.json"
jq -e -f "$POLICY" "$tmp_dir/start-health-age-boundary-gate.json" >/dev/null || {
  printf 'gate policy rejected a startup health publication exactly 2700 seconds old\n' >&2
  exit 1
}
jq '.started_at = "1970-01-01T00:46:41Z"
  | .completed_at = "1970-01-01T01:46:41Z"
  | .parity_window_started_at_unix = 2801
  | .parity_window_ended_at_unix = 6401
  | .metrics.trade_event_window_started_at_unix = 2801
  | .metrics.trade_event_window_ended_at_unix = 4001
  | .metrics.settlement_event_window_started_at_unix = 1901
  | .metrics.settlement_event_window_ended_at_unix = 5801' \
  "$tmp_dir/start-health-age-boundary-gate.json" \
  >"$tmp_dir/stale-start-health-gate.json"
if jq -e -f "$POLICY" "$tmp_dir/stale-start-health-gate.json" >/dev/null; then
  printf 'gate policy accepted a startup health publication 2701 seconds old\n' >&2
  exit 1
fi
jq '.baseline_health_start_required = false
  | .baseline_runtime_stability_required = false
  | .baseline_health_snapshot = null
  | .baseline_health_start_success_unix = null
  | .baseline_health_start_written_at_unix = null
  | .baseline_health_start_file_identity = null' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/forged-nonblocking-legacy-gate.json"
if jq -e -f "$POLICY" \
  "$tmp_dir/forged-nonblocking-legacy-gate.json" >/dev/null; then
  printf 'gate policy accepted a legacy baseline without health or identity checks\n' >&2
  exit 1
fi
jq '.comparison_mode = "rust_self"
  | .trade_parity_mode = "rust_self"
  | .metrics.legacy_trade_count = 0
  | .metrics.legacy_metadata_count = 0
  | .metrics.legacy_settlement_count = 0' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/rust-self-gate.json"
jq -e -f "$POLICY" "$tmp_dir/rust-self-gate.json" >/dev/null || {
  printf 'gate policy rejected valid Rust-self parity evidence\n' >&2
  exit 1
}
for mutation in \
  '.baseline_runtime_stability_required = false' \
  '.metrics.legacy_trade_count = 1' \
  '.metrics.rust_settlement_count = 0' \
  '.checks.settlement_parity = false'; do
  jq "$mutation" "$tmp_dir/rust-self-gate.json" \
    >"$tmp_dir/forged-rust-self-gate.json"
  if jq -e -f "$POLICY" "$tmp_dir/forged-rust-self-gate.json" >/dev/null; then
    printf 'gate policy accepted forged Rust-self parity evidence\n' >&2
    exit 1
  fi
done
jq '.baseline_health_start_required = false' "$tmp_dir/rust-self-gate.json" \
  >"$tmp_dir/health-optional-rust-self-gate.json"
jq -e -f "$POLICY" "$tmp_dir/health-optional-rust-self-gate.json" >/dev/null || {
  printf 'gate policy rejected a Rust-self gate with legacy health admission disabled\n' >&2
  exit 1
}
# Issue #868: a finalization-deferred shadow against a continuous-emission
# baseline cannot emit a mature trade inside the gate window, so the trade
# coverage/field/byte trio is adjudicated rather than required from the
# verifier; every other parity family and the finalization progression
# evidence remain fail-closed.
jq '.trade_parity_mode = "finalization_deferred_overlap"
  | .trade_parity_reason = "shadow defers trade emission until settlement plus the 1800-second finalization lag plus stable polls (post-#680) while the baseline emits trades continuously (pre-#680); trade coverage within a 3600-second gate is unsatisfiable by construction, so settlement, metadata, rotation, asset, and dedupe parity plus finalization progression and the canonical upload replace it"
  | .shadow_emission = "finalization_deferred"
  | .baseline_emission = "continuous"
  | .checks += {finalization_progress:true}
  | .parity_verifier = {passed:false, checks:{
      byte_parity:false,metadata_parity:true,field_parity:false,
      dedupe_parity:true,trade_coverage_parity:false,
      trade_contract_parity:false,settlement_parity:true,
      rotation_parity:true,asset_parity:true}}
  | .finalization_progress = {
      tracked_markets_start:160,settled_markets_start:0,stable_polls_start:0,
      tracked_markets_end:161,settled_markets_end:112,stable_polls_end:5,
      settled_markets_max:112,stable_polls_max:5}
  | .metrics.rust_trade_count = 0
  | .metrics.legacy_only_trade_ids = ["legacy-trade-1"]
  | .metrics.rust_only_trade_ids = []' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/finalization-overlap-gate.json"
jq -e -f "$POLICY" "$tmp_dir/finalization-overlap-gate.json" >/dev/null || {
  printf 'gate policy rejected finalization-deferred overlap evidence\n' >&2
  exit 1
}
# Advancement through either channel alone is sufficient: a growing settled
# maximum with flat stable polls, or a growing stable-poll maximum (zero until
# the 1800-second lag elapses) with a flat settled maximum.
jq '.finalization_progress.stable_polls_end = 0
  | .finalization_progress.stable_polls_max = 0' \
  "$tmp_dir/finalization-overlap-gate.json" \
  >"$tmp_dir/settled-advanced-overlap-gate.json"
jq -e -f "$POLICY" "$tmp_dir/settled-advanced-overlap-gate.json" >/dev/null || {
  printf 'gate policy rejected settled-only finalization advancement\n' >&2
  exit 1
}
jq '.finalization_progress.settled_markets_start = 112
  | .finalization_progress.settled_markets_end = 112' \
  "$tmp_dir/finalization-overlap-gate.json" \
  >"$tmp_dir/stable-polls-advanced-overlap-gate.json"
jq -e -f "$POLICY" \
  "$tmp_dir/stable-polls-advanced-overlap-gate.json" >/dev/null || {
  printf 'gate policy rejected stable-poll-only finalization advancement\n' >&2
  exit 1
}
for mutation in \
  '.parity_verifier.checks.settlement_parity = false' \
  '.parity_verifier.checks.metadata_parity = false' \
  '.parity_verifier.passed = true' \
  '.finalization_progress.stable_polls_start = 5
    | .finalization_progress.settled_markets_start = 112' \
  '.finalization_progress.stable_polls_start = 5
    | .finalization_progress.stable_polls_end = 5
    | .finalization_progress.stable_polls_max = 5
    | .finalization_progress.settled_markets_start = 112
    | .finalization_progress.settled_markets_end = 112' \
  '.finalization_progress.settled_markets_max = 0' \
  '.finalization_progress.settled_markets_max
    = (.finalization_progress.settled_markets_end - 1)' \
  '.finalization_progress.tracked_markets_end = 0' \
  '.metrics.rust_only_trade_ids = ["rust-only-trade"]' \
  '.metrics.rust_trade_count = -1' \
  'del(.checks.finalization_progress)' \
  '.trade_parity_mode = "continuous_overlap"' \
  '.shadow_emission = "continuous"' \
  '.baseline_emission = "finalization_deferred"' \
  '.trade_parity_reason = ""'; do
  jq "$mutation" "$tmp_dir/finalization-overlap-gate.json" \
    >"$tmp_dir/forged-finalization-overlap-gate.json"
  if jq -e -f "$POLICY" \
    "$tmp_dir/forged-finalization-overlap-gate.json" >/dev/null; then
    printf 'gate policy accepted forged finalization-deferred overlap evidence\n' >&2
    exit 1
  fi
done
# A finalization-deferred shadow against a finalization-deferred baseline keeps
# the full trade parity requirement; no adjudication applies.
jq '.shadow_emission = "finalization_deferred"
  | .baseline_emission = "finalization_deferred"' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/same-semantics-gate.json"
jq -e -f "$POLICY" "$tmp_dir/same-semantics-gate.json" >/dev/null || {
  printf 'gate policy rejected same-semantics overlap evidence\n' >&2
  exit 1
}
for mutation in \
  '.metrics.rust_trade_count = 0' \
  '.metrics.legacy_only_trade_ids = ["legacy-trade-1"]' \
  '.parity_verifier = {passed:false, checks:{trade_coverage_parity:false}}' \
  '.checks += {finalization_progress:true}' \
  '.checks.trade_coverage_parity = false'; do
  jq "$mutation" "$tmp_dir/same-semantics-gate.json" \
    >"$tmp_dir/forged-same-semantics-gate.json"
  if jq -e -f "$POLICY" \
    "$tmp_dir/forged-same-semantics-gate.json" >/dev/null; then
    printf 'gate policy relaxed trade parity for same-semantics overlap\n' >&2
    exit 1
  fi
done
jq '.metrics.rust_trade_count = 0' "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/empty-shadow-trades-gate.json"
if jq -e -f "$POLICY" "$tmp_dir/empty-shadow-trades-gate.json" >/dev/null; then
  printf 'gate policy accepted an empty shadow trade set in continuous overlap\n' >&2
  exit 1
fi
for mutation in \
  'del(.baseline_health_start_required)' \
  '.baseline_runtime_stability_required = false' \
  '.checks.metadata_parity = false'; do
  jq "$mutation" "$tmp_dir/expedited-legacy-gate.json" \
    >"$tmp_dir/forged-expedited-legacy-gate.json"
  if jq -e -f "$POLICY" \
    "$tmp_dir/forged-expedited-legacy-gate.json" >/dev/null; then
    printf 'gate policy accepted forged legacy baseline evidence\n' >&2
    exit 1
  fi
done
jq '.baseline_health_start_required = false
  | .baseline_health_snapshot = false
  | del(.legacy_runtime.release_sha256,
      .legacy_runtime.release_path,
      .legacy_runtime.proc_exe)' \
  "$tmp_dir/expedited-legacy-gate.json" \
  >"$tmp_dir/health-optional-legacy-gate.json"
jq -e -f "$POLICY" "$tmp_dir/health-optional-legacy-gate.json" >/dev/null || {
  printf 'gate policy rejected a legacy gate with health admission disabled\n' >&2
  exit 1
}
if grep -Fq 'wait_for_fresh_legacy_health_observation' "$GATE"; then
  printf 'production Gate still waits for a legacy health publication\n' >&2
  exit 1
fi
for mutation in \
  'del(.baseline_health_snapshot)' \
  '.baseline_health_completion_snapshot = .baseline_health_snapshot' \
  '.baseline_health_cutoff_unix = 1000' \
  '.baseline_health_completion_written_at_unix = 1301' \
  '.baseline_health_completion_file_identity = "1:11"'; do
  jq "$mutation" "$tmp_dir/expedited-legacy-gate.json" \
    >"$tmp_dir/forged-expedited-legacy-gate.json"
  if jq -e -f "$POLICY" "$tmp_dir/forged-expedited-legacy-gate.json" >/dev/null; then
    printf 'gate policy accepted forged post-start legacy completion in expedited evidence\n' >&2
    exit 1
  fi
done
jq 'del(.real_market_preflight)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-real-market-preflight.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-real-market-preflight.json" >/dev/null; then
  printf 'gate policy accepted evidence without a real market-segment preflight\n' >&2
  exit 1
fi
jq '.real_market_preflight.completed_at = "1970-01-01T00:03:00Z"' \
  "$tmp_dir/gate.json" >"$tmp_dir/late-real-market-preflight.json"
if jq -e -f "$POLICY" "$tmp_dir/late-real-market-preflight.json" >/dev/null; then
  printf 'gate policy accepted a real segment preflight after shadow startup\n' >&2
  exit 1
fi
jq '.real_market_preflight.source_segment.path =
    "/tmp/synthetic/market-updates.19700101T010000000000.123e4567-e89b-12d3-a456-426614174000.ndjson"' \
  "$tmp_dir/gate.json" >"$tmp_dir/synthetic-market-preflight.json"
if jq -e -f "$POLICY" "$tmp_dir/synthetic-market-preflight.json" >/dev/null; then
  printf 'gate policy accepted a synthetic market preflight source\n' >&2
  exit 1
fi
jq '.real_market_preflight.source_recorded_hours = 2' \
  "$tmp_dir/gate.json" >"$tmp_dir/cross-hour-market-preflight.json"
if jq -e -f "$POLICY" "$tmp_dir/cross-hour-market-preflight.json" >/dev/null; then
  printf 'gate policy accepted a cross-hour source as one triplet\n' >&2
  exit 1
fi
jq --arg wrong "$bundle" \
  '.real_market_preflight.uploaded_triplet.uri |=
    sub("/sha256=[a-f0-9]{64}/"; "/sha256=" + $wrong + "/")' \
  "$tmp_dir/gate.json" >"$tmp_dir/mislabeled-market-preflight.json"
if jq -e -f "$POLICY" "$tmp_dir/mislabeled-market-preflight.json" >/dev/null; then
  printf 'gate policy accepted a content-addressed URI with the wrong digest\n' >&2
  exit 1
fi
jq '.real_market_preflight.uploaded_triplet.success_sha256 =
    .real_market_preflight.uploaded_triplet.manifest_sha256' \
  "$tmp_dir/gate.json" >"$tmp_dir/mismatched-success-marker.json"
if jq -e -f "$POLICY" "$tmp_dir/mismatched-success-marker.json" >/dev/null; then
  printf 'gate policy accepted a success marker for different content\n' >&2
  exit 1
fi
jq '.real_market_preflight.upload_summary.pending_segments = 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/pending-market-preflight.json"
if jq -e -f "$POLICY" "$tmp_dir/pending-market-preflight.json" >/dev/null; then
  printf 'gate policy accepted a preflight with pending segments\n' >&2
  exit 1
fi
jq '.real_market_preflight.upload_summary.failed_segments = ["segment"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/failed-market-preflight.json"
if jq -e -f "$POLICY" "$tmp_dir/failed-market-preflight.json" >/dev/null; then
  printf 'gate policy accepted a preflight with failed segments\n' >&2
  exit 1
fi
jq '.real_market_preflight.upload_summary.last_error = "failed"' \
  "$tmp_dir/gate.json" >"$tmp_dir/error-market-preflight.json"
if jq -e -f "$POLICY" "$tmp_dir/error-market-preflight.json" >/dev/null; then
  printf 'gate policy accepted a preflight with a terminal error\n' >&2
  exit 1
fi
jq '.baseline_health_completion_file_identity =
      .baseline_health_start_file_identity' "$tmp_dir/gate.json" \
  >"$tmp_dir/reused-baseline-health-file.json"
if jq -e -f "$POLICY" "$tmp_dir/reused-baseline-health-file.json" >/dev/null; then
  printf 'gate policy accepted completion evidence from the startup health file\n' >&2
  exit 1
fi
jq 'del(.baseline_health_snapshot)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-baseline-health-snapshot.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-baseline-health-snapshot.json" >/dev/null; then
  printf 'gate policy accepted legacy evidence without its frozen health snapshot\n' >&2
  exit 1
fi
jq 'del(.baseline_health_completion_snapshot)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-baseline-health-completion.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-baseline-health-completion.json" >/dev/null; then
  printf 'gate policy accepted legacy evidence without a post-start clean cycle\n' >&2
  exit 1
fi
jq 'del(.baseline_health_start_written_at_unix)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-baseline-health-start-write.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-baseline-health-start-write.json" >/dev/null; then
  printf 'gate policy accepted legacy evidence without its startup file-write time\n' >&2
  exit 1
fi
jq 'del(.baseline_health_completion_written_at_unix)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-baseline-health-completion-write.json"
if jq -e -f "$POLICY" \
  "$tmp_dir/missing-baseline-health-completion-write.json" >/dev/null; then
  printf 'gate policy accepted legacy evidence without its completion file-write time\n' >&2
  exit 1
fi
jq '.baseline_health_start_written_at_unix = 121' "$tmp_dir/gate.json" \
  >"$tmp_dir/post-start-baseline-health-write.json"
if jq -e -f "$POLICY" "$tmp_dir/post-start-baseline-health-write.json" >/dev/null; then
  printf 'gate policy accepted a startup health write after the Gate boundary\n' >&2
  exit 1
fi
jq '.baseline_health_completion_written_at_unix = 999' "$tmp_dir/gate.json" \
  >"$tmp_dir/pre-success-baseline-health-write.json"
if jq -e -f "$POLICY" \
  "$tmp_dir/pre-success-baseline-health-write.json" >/dev/null; then
  printf 'gate policy accepted a completed health write before its payload cutoff\n' >&2
  exit 1
fi
jq '.baseline_health_completion_snapshot.last_success_at =
      .baseline_health_snapshot.last_success_at' "$tmp_dir/gate.json" \
  >"$tmp_dir/stale-baseline-health-completion.json"
if jq -e -f "$POLICY" "$tmp_dir/stale-baseline-health-completion.json" >/dev/null; then
  printf 'gate policy accepted updated_at progress without a newer completed legacy cycle\n' >&2
  exit 1
fi
jq '.baseline_health_start_success_unix = 99' "$tmp_dir/gate.json" \
  >"$tmp_dir/unbound-baseline-health-start.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-baseline-health-start.json" >/dev/null; then
  printf 'gate policy accepted an unbound legacy startup success epoch\n' >&2
  exit 1
fi
jq '.baseline_health_cutoff_unix = 1001' "$tmp_dir/gate.json" \
  >"$tmp_dir/unbound-baseline-health-cutoff.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-baseline-health-cutoff.json" >/dev/null; then
  printf 'gate policy accepted a cutoff after legacy completed-cycle evidence\n' >&2
  exit 1
fi
jq '.baseline_health_completion_snapshot.last_success_at =
      "1970-01-01T00:16:40.badZ"' "$tmp_dir/gate.json" \
  >"$tmp_dir/malformed-baseline-health-cutoff.json"
if jq -e -f "$POLICY" "$tmp_dir/malformed-baseline-health-cutoff.json" \
  >/dev/null; then
  printf 'gate policy accepted a malformed fractional legacy success timestamp\n' >&2
  exit 1
fi
jq '.baseline_health_completion_snapshot.updated_at =
      "1970-02-30T00:00:00Z"' "$tmp_dir/gate.json" \
  >"$tmp_dir/impossible-baseline-health-updated-at.json"
if jq -e -f "$POLICY" \
  "$tmp_dir/impossible-baseline-health-updated-at.json" >/dev/null; then
  printf 'gate policy accepted an impossible legacy updated_at timestamp\n' >&2
  exit 1
fi
jq '.baseline_health_completion_snapshot.last_success_at =
      "1970-02-30T00:00:00.1Z"
    | .baseline_health_cutoff_unix = 5184000
    | .baseline_health_completion_written_at_unix = 5184001
    | .completed_at = "1970-03-02T00:00:02Z"
    | .parity_window_ended_at_unix = 1000' "$tmp_dir/gate.json" \
  >"$tmp_dir/impossible-baseline-health-cutoff.json"
if jq -e -f "$POLICY" "$tmp_dir/impossible-baseline-health-cutoff.json" \
  >/dev/null; then
  printf 'gate policy accepted an impossible legacy success calendar date\n' >&2
  exit 1
fi
jq '.started_at = "1970-02-30T00:00:00Z"
    | .baseline_health_snapshot.updated_at = "1970-03-01T23:58:20Z"
    | .baseline_health_snapshot.last_success_at = "1970-03-01T23:58:20Z"
    | .baseline_health_start_success_unix = 5183900
    | .baseline_health_start_written_at_unix = 5183901
    | .baseline_health_completion_snapshot.updated_at = "1970-03-02T00:16:40Z"
    | .baseline_health_completion_snapshot.last_success_at = "1970-03-02T00:16:40Z"
    | .baseline_health_cutoff_unix = 5185000
    | .baseline_health_completion_written_at_unix = 5185001
    | .completed_at = "1970-03-02T01:12:01Z"
    | .parity_window_started_at_unix = 5184200
    | .parity_window_ended_at_unix = 5184900' "$tmp_dir/gate.json" \
  >"$tmp_dir/impossible-gate-start.json"
if jq -e -f "$POLICY" "$tmp_dir/impossible-gate-start.json" >/dev/null; then
  printf 'gate policy accepted an impossible Gate start calendar date\n' >&2
  exit 1
fi
jq '.completed_at = "1970-02-30T00:00:00Z"' "$tmp_dir/gate.json" \
  >"$tmp_dir/impossible-gate-completion.json"
if jq -e -f "$POLICY" "$tmp_dir/impossible-gate-completion.json" >/dev/null; then
  printf 'gate policy accepted an impossible Gate completion calendar date\n' >&2
  exit 1
fi
jq '.shadow_runtime.memory_events.start.high = 1
    | .shadow_runtime.memory_events.end.high = 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/nonzero-memory-high-baseline.json"
if jq -e -f "$POLICY" "$tmp_dir/nonzero-memory-high-baseline.json" >/dev/null; then
  printf 'gate policy accepted a nonzero memory.events high baseline\n' >&2
  exit 1
fi
jq '.shadow_runtime.memory_events.end.high += 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/growing-memory-high.json"
if jq -e -f "$POLICY" "$tmp_dir/growing-memory-high.json" >/dev/null; then
  printf 'gate policy accepted a growing memory.events high counter\n' >&2
  exit 1
fi
for memory_counter_path in \
  start.max start.oom start.oom_kill start.oom_group_kill \
  end.max end.oom end.oom_kill end.oom_group_kill; do
  memory_counter_file=${memory_counter_path//./-}
  jq --arg path "$memory_counter_path" \
    'setpath((["shadow_runtime","memory_events"] + ($path | split("."))); 1)' \
    "$tmp_dir/gate.json" >"$tmp_dir/nonzero-memory-$memory_counter_file.json"
  if jq -e -f "$POLICY" \
    "$tmp_dir/nonzero-memory-$memory_counter_file.json" >/dev/null; then
    printf 'gate policy accepted nonzero memory.events %s\n' \
      "$memory_counter_path" >&2
    exit 1
  fi
done
jq '.metrics.trade_maturity_lag_seconds = 2399' \
  "$tmp_dir/gate.json" >"$tmp_dir/short-trade-maturity.json"
if jq -e -f "$POLICY" "$tmp_dir/short-trade-maturity.json" >/dev/null; then
  printf 'gate policy accepted a shortened trade maturity lag\n' >&2
  exit 1
fi
jq '.metrics.trade_event_window_ended_at_unix += 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/unbound-trade-end.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-trade-end.json" >/dev/null; then
  printf 'gate policy accepted an unbound mature trade window\n' >&2
  exit 1
fi
jq '.parity_window_ended_at_unix = 700
    | .metrics.trade_event_window_ended_at_unix = 100
    | .metrics.settlement_event_window_ended_at_unix = 100' \
  "$tmp_dir/gate.json" >"$tmp_dir/empty-mature-trade-window.json"
if jq -e -f "$POLICY" "$tmp_dir/empty-mature-trade-window.json" >/dev/null; then
  printf 'gate policy accepted an empty mature trade window\n' >&2
  exit 1
fi
jq '.metrics.rust_trade_metadata_context_mismatch_market_ids = ["missing-context"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/trade-context-mismatch.json"
if jq -e -f "$POLICY" "$tmp_dir/trade-context-mismatch.json" >/dev/null; then
  printf 'gate policy accepted a trade metadata context mismatch\n' >&2
  exit 1
fi
jq '.metrics.legacy_only_trade_ids = ["missing-from-rust"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/legacy-trade-omission.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-trade-omission.json" >/dev/null; then
  printf 'gate policy accepted a mature trade missing from Rust\n' >&2
  exit 1
fi
jq '.metrics.rust_only_trade_ids = ["missing-from-legacy"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/rust-trade-addition.json"
jq -e -f "$POLICY" "$tmp_dir/rust-trade-addition.json" >/dev/null
jq '.metrics.trade_metadata_shared_value_mismatch_market_ids = ["market-extra"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/trade-metadata-value-mismatch.json"
if jq -e -f "$POLICY" "$tmp_dir/trade-metadata-value-mismatch.json" >/dev/null; then
  printf 'gate policy accepted contradictory trade metadata context\n' >&2
  exit 1
fi
jq '.metrics.rust_settlement_metadata_context_mismatch_market_ids = ["market-extra"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/settlement-context-mismatch.json"
if jq -e -f "$POLICY" "$tmp_dir/settlement-context-mismatch.json" >/dev/null; then
  printf 'gate policy accepted a settlement metadata context mismatch\n' >&2
  exit 1
fi
jq 'del(.metrics.normalized_settlement_sha256)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-settlement-digest.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-settlement-digest.json" >/dev/null; then
  printf 'gate policy accepted evidence without a normalized settlement digest\n' >&2
  exit 1
fi
jq '.metrics.normalized_settlement_sha256 = "not-a-digest"' \
  "$tmp_dir/gate.json" >"$tmp_dir/invalid-settlement-digest.json"
if jq -e -f "$POLICY" "$tmp_dir/invalid-settlement-digest.json" >/dev/null; then
  printf 'gate policy accepted an invalid normalized settlement digest\n' >&2
  exit 1
fi
jq '.metrics.rust_only_metadata_ids = ["future-market"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/rust-metadata-superset.json"
jq -e -f "$POLICY" "$tmp_dir/rust-metadata-superset.json" >/dev/null
jq '.metrics.legacy_only_metadata_ids = ["missing-from-rust"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/legacy-metadata-omission.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-metadata-omission.json" >/dev/null; then
  printf 'gate policy accepted metadata missing from Rust\n' >&2
  exit 1
fi
jq '.metrics.legacy_only_settlement_ids = ["missing-from-rust"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/legacy-settlement-omission.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-settlement-omission.json" >/dev/null; then
  printf 'gate policy accepted a mature settlement missing from Rust\n' >&2
  exit 1
fi
jq '.metrics.settlement_shared_values_match = false' \
  "$tmp_dir/gate.json" >"$tmp_dir/settlement-value-mismatch.json"
if jq -e -f "$POLICY" "$tmp_dir/settlement-value-mismatch.json" >/dev/null; then
  printf 'gate policy accepted contradictory settlement values\n' >&2
  exit 1
fi
jq '.metrics.settlement_event_window_started_at_unix += 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/unbound-settlement-start.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-settlement-start.json" >/dev/null; then
  printf 'gate policy accepted an unbound settlement start window\n' >&2
  exit 1
fi
jq '.metrics.settlement_event_window_started_at_unix = -800' \
  "$tmp_dir/gate.json" >"$tmp_dir/non-saturating-settlement-start.json"
if jq -e -f "$POLICY" "$tmp_dir/non-saturating-settlement-start.json" >/dev/null; then
  printf 'gate policy accepted a negative settlement start below the Unix epoch\n' >&2
  exit 1
fi
jq '.metrics.settlement_event_window_ended_at_unix += 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/unbound-settlement-end.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-settlement-end.json" >/dev/null; then
  printf 'gate policy accepted an unbound settlement end window\n' >&2
  exit 1
fi
jq '.duration_seconds = 3599' "$tmp_dir/gate.json" >"$tmp_dir/short.json"
if jq -e -f "$POLICY" "$tmp_dir/short.json" >/dev/null; then
  printf 'gate policy accepted a shadow shorter than one hour total\n' >&2
  exit 1
fi
jq '.duration_seconds = 3601' "$tmp_dir/gate.json" >"$tmp_dir/long.json"
if ! jq -e -f "$POLICY" "$tmp_dir/long.json" >/dev/null; then
  printf 'gate policy rejected one second of elapsed-time rounding\n' >&2
  exit 1
fi
jq '.duration_seconds = 3602' "$tmp_dir/gate.json" >"$tmp_dir/too-long.json"
if jq -e -f "$POLICY" "$tmp_dir/too-long.json" >/dev/null; then
  printf 'gate policy accepted more than one second of elapsed-time rounding\n' >&2
  exit 1
fi
jq '.started_at = "1970-01-01T00:01:41Z"' \
  "$tmp_dir/expedited-legacy-gate.json" >"$tmp_dir/pre-shadow-parity.json"
if jq -e -f "$POLICY" "$tmp_dir/pre-shadow-parity.json" >/dev/null; then
  printf 'gate policy accepted parity beginning before the formal Gate\n' >&2
  exit 1
fi
jq '.completed_at = "1970-01-01T00:16:39Z"' \
  "$tmp_dir/expedited-legacy-gate.json" >"$tmp_dir/unelapsed-gate.json"
if jq -e -f "$POLICY" "$tmp_dir/unelapsed-gate.json" >/dev/null; then
  printf 'gate policy accepted 3600 seconds without 3600 elapsed wall-clock seconds\n' >&2
  exit 1
fi
jq '.production_eligible = false' "$tmp_dir/gate.json" >"$tmp_dir/test-only.json"
if jq -e -f "$POLICY" "$tmp_dir/test-only.json" >/dev/null; then
  printf 'gate policy accepted test-only evidence\n' >&2
  exit 1
fi
jq '.parity_window_ended_at_unix = 700' "$tmp_dir/gate.json" \
  >"$tmp_dir/short-parity-tail.json"
if jq -e -f "$POLICY" "$tmp_dir/short-parity-tail.json" >/dev/null; then
  printf 'gate policy accepted a parity tail too short for mature trades\n' >&2
  exit 1
fi
jq '.checks.metadata_parity = false | .passed = true' "$tmp_dir/gate.json" \
  >"$tmp_dir/bad-metadata-gate.json"
if jq -e -f "$POLICY" "$tmp_dir/bad-metadata-gate.json" >/dev/null; then
  printf 'gate policy ignored failed metadata parity\n' >&2
  exit 1
fi
jq '.checks.market_oss_readback_parity = false | .passed = true' \
  "$tmp_dir/gate.json" >"$tmp_dir/bad-market-upload-gate.json"
if jq -e -f "$POLICY" "$tmp_dir/bad-market-upload-gate.json" >/dev/null; then
  printf 'gate policy ignored failed market-tape uploader readback\n' >&2
  exit 1
fi
jq '.metrics.oss_canonical_uploaded_segments = 0' "$tmp_dir/gate.json" \
  >"$tmp_dir/noncanonical-reference-upload.json"
if jq -e -f "$POLICY" "$tmp_dir/noncanonical-reference-upload.json" >/dev/null; then
  printf 'gate policy accepted a noncanonical reference upload\n' >&2
  exit 1
fi
jq '.real_market_preflight.upload_summary.canonical_uploaded_segments = 0
  | .real_market_preflight.uploaded_triplet.canonical = false
  | .real_market_preflight.uploaded_triplet.segment_complete = false
  | .metrics.market_oss_canonical_uploaded_segments = 0' \
  "$tmp_dir/gate.json" >"$tmp_dir/noncanonical-market-upload.json"
if ! jq -e -f "$POLICY" "$tmp_dir/noncanonical-market-upload.json" >/dev/null; then
  printf 'gate policy rejected a verified noncanonical market-tape upload\n' >&2
  exit 1
fi
jq '.real_market_preflight.upload_summary.uploaded_segments = 2
  | .metrics.market_oss_uploaded_segments = 2' \
  "$tmp_dir/gate.json" >"$tmp_dir/multiple-market-preflight-uploads.json"
if jq -e -f "$POLICY" "$tmp_dir/multiple-market-preflight-uploads.json" \
  >/dev/null; then
  printf 'gate policy accepted multiple uploads for a one-hour preflight\n' >&2
  exit 1
fi
jq '.real_market_preflight.upload_summary.canonical_uploaded_segments = 0
  | .metrics.market_oss_canonical_uploaded_segments = 0' \
  "$tmp_dir/gate.json" >"$tmp_dir/inconsistent-market-canonical-count.json"
if jq -e -f "$POLICY" "$tmp_dir/inconsistent-market-canonical-count.json" \
  >/dev/null; then
  printf 'gate policy accepted a canonical count inconsistent with its manifest\n' >&2
  exit 1
fi
jq 'del(.real_market_preflight.uploaded_triplet.canonical)' \
  "$tmp_dir/gate.json" >"$tmp_dir/unbound-market-canonical.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-market-canonical.json" >/dev/null; then
  printf 'gate policy accepted evidence without the uploaded canonical flag\n' >&2
  exit 1
fi
jq '.metrics.market_oss_canonical_uploaded_segments = -1' "$tmp_dir/gate.json" \
  >"$tmp_dir/invalid-market-upload-count.json"
if jq -e -f "$POLICY" "$tmp_dir/invalid-market-upload-count.json" >/dev/null; then
  printf 'gate policy accepted a negative market-tape canonical count\n' >&2
  exit 1
fi
jq 'del(.oss_config_sha256)' "$tmp_dir/gate.json" >"$tmp_dir/unbound-oss-config.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-oss-config.json" >/dev/null; then
  printf 'gate policy accepted evidence without an OSS configuration identity\n' >&2
  exit 1
fi
jq 'del(.release_manifest_sha256)' "$tmp_dir/gate.json" \
  >"$tmp_dir/unbound-release-manifest.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-release-manifest.json" >/dev/null; then
  printf 'gate policy accepted evidence without a release manifest identity\n' >&2
  exit 1
fi
jq 'del(.control_archive_sha256)' "$tmp_dir/gate.json" \
  >"$tmp_dir/unbound-control-archive.json"
if jq -e -f "$POLICY" "$tmp_dir/unbound-control-archive.json" >/dev/null; then
  printf 'gate policy accepted evidence without a control archive identity\n' >&2
  exit 1
fi
jq '.legacy_runtime.restarts = -1' "$tmp_dir/gate.json" >"$tmp_dir/negative-restarts.json"
if jq -e -f "$POLICY" "$tmp_dir/negative-restarts.json" >/dev/null; then
  printf 'gate policy accepted a negative legacy restart counter\n' >&2
  exit 1
fi
jq '.legacy_runtime.restarts = 1.5' "$tmp_dir/gate.json" >"$tmp_dir/fractional-restarts.json"
if jq -e -f "$POLICY" "$tmp_dir/fractional-restarts.json" >/dev/null; then
  printf 'gate policy accepted a fractional legacy restart counter\n' >&2
  exit 1
fi
jq 'del(.legacy_runtime.invocation_id)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-legacy-invocation.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-legacy-invocation.json" >/dev/null; then
  printf 'gate policy accepted evidence without a legacy invocation ID\n' >&2
  exit 1
fi
jq '.legacy_runtime.invocation_id = ("E" * 32)' "$tmp_dir/gate.json" \
  >"$tmp_dir/invalid-legacy-invocation.json"
if jq -e -f "$POLICY" "$tmp_dir/invalid-legacy-invocation.json" >/dev/null; then
  printf 'gate policy accepted a malformed legacy invocation ID\n' >&2
  exit 1
fi
jq 'del(.shadow_runtime.invocation_id)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-shadow-invocation.json"
if jq -e -f "$POLICY" "$tmp_dir/missing-shadow-invocation.json" >/dev/null; then
  printf 'gate policy accepted evidence without a shadow invocation ID\n' >&2
  exit 1
fi
jq '.shadow_runtime.invocation_id = ("f" * 31)' "$tmp_dir/gate.json" \
  >"$tmp_dir/invalid-shadow-invocation.json"
if jq -e -f "$POLICY" "$tmp_dir/invalid-shadow-invocation.json" >/dev/null; then
  printf 'gate policy accepted a malformed shadow invocation ID\n' >&2
  exit 1
fi
jq '.legacy_runtime.drop_in_paths = ["/etc/systemd/system/polymarket-reference-collector.service.d/override.conf"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/legacy-drop-in.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-drop-in.json" >/dev/null; then
  printf 'gate policy accepted a legacy collector with a systemd drop-in\n' >&2
  exit 1
fi
jq '.legacy_runtime.cmdline_sha256 = "not-a-digest"' "$tmp_dir/gate.json" \
  >"$tmp_dir/unverified-legacy-cmdline.json"
if jq -e -f "$POLICY" "$tmp_dir/unverified-legacy-cmdline.json" >/dev/null; then
  printf 'gate policy accepted an unverified legacy command line\n' >&2
  exit 1
fi
jq '.legacy_runtime.cmdline_sha256 = ("f" * 64)' "$tmp_dir/gate.json" \
  >"$tmp_dir/mismatched-legacy-cmdline-digest.json"
if jq -e -f "$POLICY" "$tmp_dir/mismatched-legacy-cmdline-digest.json" >/dev/null; then
  printf 'gate policy accepted a mismatched legacy command-line digest\n' >&2
  exit 1
fi
jq '.legacy_runtime.cmdline += " --once"' "$tmp_dir/gate.json" \
  >"$tmp_dir/legacy-once-cmdline.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-once-cmdline.json" >/dev/null; then
  printf 'gate policy accepted a legacy --once command line\n' >&2
  exit 1
fi
jq '.shadow_runtime.drop_in_paths = ["/etc/systemd/system/polymarket-reference-collector-shadow@.service.d/override.conf"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/shadow-drop-in.json"
if jq -e -f "$POLICY" "$tmp_dir/shadow-drop-in.json" >/dev/null; then
  printf 'gate policy accepted a Rust shadow with a systemd drop-in\n' >&2
  exit 1
fi
jq '.shadow_runtime.cmdline += " --once"' "$tmp_dir/gate.json" \
  >"$tmp_dir/shadow-once-cmdline.json"
if jq -e -f "$POLICY" "$tmp_dir/shadow-once-cmdline.json" >/dev/null; then
  printf 'gate policy accepted a Rust shadow --once command line\n' >&2
  exit 1
fi
baseline_sha=$(printf '9%.0s' {1..64})
jq --arg baseline "$baseline_sha" '.baseline_mode = "rust_release"
  | .baseline_health_start_required = false
  | .baseline_runtime_stability_required = true
  | .baseline_health_completion_required = false
  | .baseline_health_snapshot = null
  | .baseline_health_completion_snapshot = null
  | .baseline_health_start_success_unix = null
  | .baseline_health_cutoff_unix = null
  | .baseline_health_start_written_at_unix = null
  | .baseline_health_completion_written_at_unix = null
  | .baseline_health_start_file_identity = null
  | .baseline_health_completion_file_identity = null
  | .legacy_runtime += {
      exec_start:"/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200",
      cmdline:"/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200",
      cmdline_sha256:"6d942117a378a42f06376acb78dbc312c85e1146fc05ff17d1699b8a6007edec",
      fragment_path:"/etc/systemd/system/polymarket-reference-collector.service",
      drop_in_paths:[],
      main_pid:12,restarts:0,invocation_id:("1" * 32),
      release_sha256:$baseline,
      release_path:("/opt/monday/releases/polymarket-raw-ops/" + $baseline + "/polymarket-raw-ops"),
      proc_exe:("/opt/monday/releases/polymarket-raw-ops/" + $baseline + "/polymarket-raw-ops")
    }
' "$tmp_dir/gate.json" >"$tmp_dir/rust-release-gate.json"
jq -e -f "$POLICY" "$tmp_dir/rust-release-gate.json" >/dev/null
while IFS='|' read -r name filter; do
  jq "$filter" "$tmp_dir/rust-release-gate.json" >"$tmp_dir/rust-release-$name.json"
  if jq -e -f "$POLICY" "$tmp_dir/rust-release-$name.json" >/dev/null; then
    printf 'rust release gate accepted %s drift\n' "$name" >&2
    exit 1
  fi
done <<'EOF'
release_path|.legacy_runtime.release_path = "/tmp/polymarket-raw-ops"
release_sha|.legacy_runtime.release_sha256 = "not-a-digest"
proc_exe|.legacy_runtime.proc_exe = "/tmp/polymarket-raw-ops"
candidate_equals_baseline|.candidate_sha256 = .legacy_runtime.release_sha256
EOF
jq '.baseline_mode = "rust_bootstrap"
  | .comparison_mode = "rust_self"
  | .trade_parity_mode = "rust_self"
  | .baseline_degraded = true
  | .metrics.legacy_trade_count = 0
  | .metrics.legacy_metadata_count = 0
  | .metrics.legacy_settlement_count = 0
  | .legacy_runtime.release_path = "/opt/monday/bin/polymarket-raw-ops"
  | .legacy_runtime.proc_exe = "/opt/monday/bin/polymarket-raw-ops"' \
  "$tmp_dir/rust-release-gate.json" >"$tmp_dir/rust-bootstrap-gate.json"
jq -e -f "$POLICY" "$tmp_dir/rust-bootstrap-gate.json" >/dev/null || {
  printf 'gate policy rejected a bounded Rust bootstrap recovery\n' >&2
  exit 1
}
jq '
  .baseline_mode = "rust_bootstrap"
  | .baseline_degraded = true
  | .baseline_health_start_required = false
  | .baseline_runtime_stability_required = false
  | .baseline_health_completion_required = false
  | .baseline_health_snapshot = null
  | .baseline_health_completion_snapshot = null
  | .baseline_health_start_success_unix = null
  | .baseline_health_cutoff_unix = null
  | .baseline_health_start_written_at_unix = null
  | .baseline_health_completion_written_at_unix = null
  | .baseline_health_start_file_identity = null
  | .baseline_health_completion_file_identity = null
  | .legacy_runtime = null
  | .recovery = {
      mode:"gamma_closed_200",
      baseline:{
        active_state:"inactive",main_pid:0,
        exec_start:"/opt/monday/bin/polymarket-raw-ops collect-reference --max-trade-polls-per-cycle 200",
        fragment_path:"/etc/systemd/system/polymarket-reference-collector.service",
        drop_in_paths:[],restarts:2,invocation_id:("2" * 32),
        binary_path:"/opt/monday/bin/polymarket-raw-ops",
        binary_sha256:(if .candidate_sha256 == ("0" * 64) then ("1" * 64) else ("0" * 64) end)
      },
      candidate_probe:{
        schema:"monday.polymarket_gamma_closed_200_recovery_probe.v1",
        candidate_sha256:.candidate_sha256,
        source_revision:.deployment_source_revision,sha256:("3" * 64),
        observed_at:"2026-07-15T00:00:01Z",
        gamma:{
          tagged_closed:{query:"closed=true&tag_id=21",attempts:3,http_status:200},
          untagged_closed:{query:"closed=true",attempts:3,http_status:200}
        },
        candidate_once:{exit_status:0,duration_seconds:23,
          health_updated_at:"2026-07-15T00:00:01Z"}
      }
    }
' "$tmp_dir/rust-bootstrap-gate.json" >"$tmp_dir/contained-recovery-gate.json"
jq -e -f "$POLICY" "$tmp_dir/contained-recovery-gate.json" >/dev/null || {
  printf 'gate policy rejected a verified contained bootstrap recovery\n' >&2
  exit 1
}
jq '.recovery.baseline.invocation_id = ""' "$tmp_dir/contained-recovery-gate.json" \
  >"$tmp_dir/contained-recovery-empty-invocation.json"
jq -e -f "$POLICY" "$tmp_dir/contained-recovery-empty-invocation.json" >/dev/null || {
  printf 'gate policy rejected a contained baseline with a systemd-cleared invocation ID\n' >&2
  exit 1
}
while IFS='|' read -r name filter; do
  jq "$filter" "$tmp_dir/contained-recovery-gate.json" \
    >"$tmp_dir/contained-recovery-$name.json"
  if jq -e -f "$POLICY" "$tmp_dir/contained-recovery-$name.json" >/dev/null; then
    printf 'contained recovery policy accepted %s drift\n' "$name" >&2
    exit 1
  fi
done <<'EOF'
candidate_binding|.recovery.candidate_probe.candidate_sha256 = (if .candidate_sha256 == ("0" * 64) then ("1" * 64) else ("0" * 64) end)
source_binding|.recovery.candidate_probe.source_revision = (if .deployment_source_revision == ("0" * 40) then ("1" * 40) else ("0" * 40) end)
tagged_status|.recovery.candidate_probe.gamma.tagged_closed.http_status = 500
baseline_active|.recovery.baseline.active_state = "active"
baseline_invocation|.recovery.baseline.invocation_id = "not-an-invocation-id"
runtime_stability|.baseline_runtime_stability_required = true
missing_recovery|.recovery = null
baseline_is_candidate|.recovery.baseline.binary_sha256 = .candidate_sha256
legacy_runtime_present|.legacy_runtime = {exec_start:"/opt/monday/bin/polymarket-raw-ops collect-reference"}
EOF
jq '.metrics.legacy_metadata_count = 1' "$tmp_dir/rust-bootstrap-gate.json" \
  >"$tmp_dir/rust-bootstrap-metadata-only-gate.json"
jq -e -f "$POLICY" "$tmp_dir/rust-bootstrap-metadata-only-gate.json" >/dev/null || {
  printf 'gate policy rejected metadata-only bootstrap Rust-self evidence\n' >&2
  exit 1
}
while IFS='|' read -r name filter; do
  jq "$filter" "$tmp_dir/rust-bootstrap-gate.json" >"$tmp_dir/rust-bootstrap-$name.json"
  if jq -e -f "$POLICY" "$tmp_dir/rust-bootstrap-$name.json" >/dev/null; then
    printf 'Rust bootstrap gate accepted %s drift\n' "$name" >&2
    exit 1
  fi
done <<'EOF'
release_path|.legacy_runtime.release_path = "/tmp/polymarket-raw-ops"
proc_exe|.legacy_runtime.proc_exe = "/tmp/polymarket-raw-ops"
comparison_mode|.comparison_mode = "legacy_overlap"
degraded|.baseline_degraded = false
candidate_equals_baseline|.candidate_sha256 = .legacy_runtime.release_sha256
EOF
jq '.legacy_runtime += {
    release_sha256:("'"$baseline_sha"'"),
    release_path:("/opt/monday/releases/polymarket-raw-ops/" + "'"$baseline_sha"'" + "/polymarket-raw-ops"),
    proc_exe:("/opt/monday/releases/polymarket-raw-ops/" + "'"$baseline_sha"'" + "/polymarket-raw-ops")
  }' "$tmp_dir/gate.json" >"$tmp_dir/legacy-mixed-rust-fields.json"
if jq -e -f "$POLICY" "$tmp_dir/legacy-mixed-rust-fields.json" >/dev/null; then
  printf 'gate policy accepted legacy baseline evidence with Rust-only release fields\n' >&2
  exit 1
fi
jq -n '{
  updated_at:"2026-07-15T00:00:01Z",last_success_at:"2026-07-15T00:00:01Z",
  target_markets:14,api_errors:[],malformed_trade_rows:0,
  truncated_trade_markets:[],stale_trade_markets:[],stale_settlement_markets:[],
  overdue_unresolved_markets:[]
}' >"$tmp_dir/legacy-health.json"
jq -e -f "$LEGACY_HEALTH_POLICY" "$tmp_dir/legacy-health.json" >/dev/null
jq --arg error "$legacy_rate_limit_error" '.api_errors = [$error]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/rate-limited-legacy-health.json"
if jq -e -f "$LEGACY_HEALTH_POLICY" \
  "$tmp_dir/rate-limited-legacy-health.json" >/dev/null; then
  printf 'shared legacy health policy accepted a trades rate limit\n' >&2
  exit 1
fi
legacy_start_health_contract="$tmp_dir/legacy-start-health-contract.sh"
sed -n '/^legacy_start_health_policy_clean()/,/^}/p' "$GATE" \
  >"$legacy_start_health_contract"
# shellcheck source=/dev/null
source "$legacy_start_health_contract"
legacy_start_health_policy_clean \
  "$(jq -cS . "$tmp_dir/rate-limited-legacy-health.json")" \
  "$LEGACY_HEALTH_POLICY" || {
  printf 'Gate startup policy rejected a bounded trades rate limit\n' >&2
  exit 1
}
jq --arg error "$legacy_rate_limit_error" \
  '.api_errors = [$error, $error, $error]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/three-rate-limits-legacy-health.json"
legacy_start_health_policy_clean \
  "$(jq -cS . "$tmp_dir/three-rate-limits-legacy-health.json")" \
  "$LEGACY_HEALTH_POLICY" || {
  printf 'Gate startup policy rejected three trades rate limits\n' >&2
  exit 1
}
jq --arg error "$legacy_rate_limit_error" \
  '.target_markets = 2792 | .api_errors = [range(0; 18) | $error]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/ratio-bounded-legacy-health.json"
legacy_start_health_policy_clean \
  "$(jq -cS . "$tmp_dir/ratio-bounded-legacy-health.json")" \
  "$LEGACY_HEALTH_POLICY" || {
  printf 'Gate startup policy rejected 18 trades rate limits for 2792 markets\n' >&2
  exit 1
}
jq --arg error "$legacy_rate_limit_error" \
  '.target_markets = 2792 | .api_errors = [range(0; 29) | $error]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/ratio-excessive-legacy-health.json"
if legacy_start_health_policy_clean \
  "$(jq -cS . "$tmp_dir/ratio-excessive-legacy-health.json")" \
  "$LEGACY_HEALTH_POLICY"; then
  printf 'Gate startup policy accepted 29 trades rate limits for 2792 markets\n' >&2
  exit 1
fi
jq --arg error "$legacy_rate_limit_error" \
  '.target_markets = 10000 | .api_errors = [range(0; 33) | $error]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/absolute-cap-legacy-health.json"
if legacy_start_health_policy_clean \
  "$(jq -cS . "$tmp_dir/absolute-cap-legacy-health.json")" \
  "$LEGACY_HEALTH_POLICY"; then
  printf 'Gate startup policy accepted more than 32 trades rate limits\n' >&2
  exit 1
fi
legacy_rate_limit_with_newline="${legacy_rate_limit_error}"$'\n'
jq --arg error "$legacy_rate_limit_with_newline" '.api_errors = [$error]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/newline-rate-limit-legacy-health.json"
if legacy_start_health_policy_clean \
  "$(jq -cS . "$tmp_dir/newline-rate-limit-legacy-health.json")" \
  "$LEGACY_HEALTH_POLICY"; then
  printf 'Gate startup policy accepted a rate limit with a trailing newline\n' >&2
  exit 1
fi
jq --arg error "$legacy_rate_limit_error" \
  '.api_errors = [$error, $error, $error, $error]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/four-rate-limits-legacy-health.json"
if legacy_start_health_policy_clean \
  "$(jq -cS . "$tmp_dir/four-rate-limits-legacy-health.json")" \
  "$LEGACY_HEALTH_POLICY"; then
  printf 'Gate startup policy accepted more than three trades rate limits\n' >&2
  exit 1
fi
for error in \
  'Gamma 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa: HTTP Error 429: Too Many Requests' \
  'trades 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa: HTTP Error 500: Internal Server Error' \
  'trades 0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa: The read operation timed out'; do
  jq --arg error "$error" '.api_errors = [$error]' \
    "$tmp_dir/legacy-health.json" >"$tmp_dir/non-rate-limit-legacy-health.json"
  if legacy_start_health_policy_clean \
    "$(jq -cS . "$tmp_dir/non-rate-limit-legacy-health.json")" \
    "$LEGACY_HEALTH_POLICY"; then
    printf 'Gate startup policy accepted non-rate-limit API error: %s\n' "$error" >&2
    exit 1
  fi
done
for mutation in \
  '.api_errors = ["Gamma unavailable"]' \
  '.malformed_trade_rows = 1' \
  '.truncated_trade_markets = ["condition-1"]' \
  '.stale_trade_markets = ["condition-1"]' \
  '.stale_settlement_markets = ["market-1"]'; do
  jq "$mutation" "$tmp_dir/legacy-health.json" >"$tmp_dir/bad-legacy-health.json"
  if jq -e -f "$LEGACY_HEALTH_POLICY" "$tmp_dir/bad-legacy-health.json" >/dev/null; then
    printf 'legacy health policy accepted failure mutation: %s\n' "$mutation" >&2
    exit 1
  fi
done
jq --arg rate_limit "$legacy_rate_limit_error" \
  '.api_errors = [$rate_limit,
    "trades 0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb: The read operation timed out"]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/mixed-legacy-health.json"
if legacy_start_health_policy_clean \
  "$(jq -cS . "$tmp_dir/mixed-legacy-health.json")" \
  "$LEGACY_HEALTH_POLICY"; then
  printf 'Gate startup policy accepted a mixed API error list\n' >&2
  exit 1
fi
jq '.overdue_unresolved_markets = ["historical-market"]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/historical-overdue-legacy-health.json"
jq -e -f "$LEGACY_HEALTH_POLICY" \
  "$tmp_dir/historical-overdue-legacy-health.json" >/dev/null
for mutation in \
  'del(.overdue_unresolved_markets)' \
  '.overdue_unresolved_markets = "market-1"' \
  '.overdue_unresolved_markets = [""]' \
  '.overdue_unresolved_markets = [1]'; do
  jq "$mutation" "$tmp_dir/legacy-health.json" >"$tmp_dir/bad-legacy-overdue.json"
  if jq -e -f "$LEGACY_HEALTH_POLICY" "$tmp_dir/bad-legacy-overdue.json" >/dev/null; then
    printf 'legacy health policy accepted invalid overdue evidence: %s\n' "$mutation" >&2
    exit 1
  fi
done
legacy_health_classifier="$tmp_dir/legacy-health-classifier.sh"
sed -n \
  -e '/^baseline_health_requires_continuous_freshness()/,/^}/p' \
  -e '/^legacy_health_sample_state()/,/^}/p' \
  -e '/^legacy_health_transition()/,/^}/p' "$GATE" \
  >"$legacy_health_classifier"
# shellcheck source=/dev/null
source "$legacy_health_classifier"
if baseline_health_requires_continuous_freshness legacy_python; then
  printf 'legacy collector health incorrectly requires continuous 240-second freshness\n' >&2
  exit 1
fi
baseline_health_requires_continuous_freshness rust_release
grep -Fq '"$release_control_dir/${LEGACY_HEALTH_POLICY##*/}" true \
    "$LEGACY_START_HEALTH_MAX_AGE_SECONDS")' "$GATE"
legacy_runtime_budget_contract="$tmp_dir/legacy-runtime-budget.sh"
sed -n \
  -e '/^readonly REQUIRED_DURATION_SECONDS=/p' \
  -e '/^readonly PARITY_TAIL_SECONDS=/p' \
  -e '/^readonly MINIMUM_GATE_SECONDS=/p' \
  -e '/^readonly LEGACY_RUNTIME_STABILITY_REQUIRED=/p' \
  -e '/^readonly LEGACY_RUNTIME_MAX_SECONDS=/p' \
  -e '/^readonly LEGACY_RUNTIME_RESERVE_SECONDS=/p' \
  -e '/^readonly PARITY_CUTOFF_LAG_SECONDS=/p' \
  -e '/^readonly LEGACY_UNIT=/p' \
  -e '/^monotonic_uptime_seconds() {$/,/^}$/p' \
  -e '/^legacy_runtime_budget_observation() {$/,/^}$/p' \
  -e '/^run_budgeted_real_market_preflight() {$/,/^}$/p' "$GATE" \
  >"$legacy_runtime_budget_contract"
[[ -s $legacy_runtime_budget_contract ]] || {
  printf 'Gate has no bounded real preflight contract\n' >&2
  exit 1
}
budgeted_preflight_line=$(grep -n '^real_market_preflight_json=$(run_budgeted_real_market_preflight' \
  "$GATE" | cut -d: -f1 || true)
shadow_start_line=$(grep -n '^systemctl start "$shadow_unit"' "$GATE" \
  | cut -d: -f1 || true)
[[ $budgeted_preflight_line =~ ^[1-9][0-9]*$ \
  && $shadow_start_line =~ ^[1-9][0-9]*$ \
  && $budgeted_preflight_line -lt $shadow_start_line ]] || {
  printf 'Gate does not run bounded real preflight before shadow startup\n' >&2
  exit 1
}
if grep -Fq '[[ $zstd_timeout_seconds == 300 && $oss_copy_timeout_seconds == 300 ]]' "$GATE"; then
  printf 'Gate still hard-blocks operator-tuned upload timeouts\n' >&2
  exit 1
fi
grep -Fq '[[ $zstd_timeout_seconds =~ ^[1-9][0-9]*$' "$GATE"
grep -Fq '&& $oss_copy_timeout_seconds =~ ^[1-9][0-9]*$ ]]' "$GATE"
grep -Fq 'upload timeout values remain bound into OSS configuration evidence' "$GATE"
(
  # shellcheck source=/dev/null
  source "$legacy_runtime_budget_contract"
  [[ $REAL_MARKET_PREFLIGHT_BUDGET_SECONDS -eq 1200 \
    && $LEGACY_RUNTIME_STABILITY_REQUIRED == true \
    && $LEGACY_RUNTIME_MAX_SECONDS -eq 21600 \
    && $LEGACY_RUNTIME_RESERVE_SECONDS -eq 60 \
    && $PARITY_CUTOFF_LAG_SECONDS -eq 60 ]] || {
    printf 'Gate runtime budget does not bind the reviewed preflight and unit limits\n' >&2
    exit 1
  }
  systemctl() {
    case "$*" in
      *RuntimeMaxUSec*) printf '6h\n' ;;
      *ActiveEnterTimestampMonotonic*) printf '1000000\n' ;;
      *) return 1 ;;
    esac
  }
  monotonic_uptime_seconds() { printf '20906\n'; }
  zstd_timeout_seconds=3600
  oss_copy_timeout_seconds=1800
  required=$((REAL_MARKET_PREFLIGHT_BUDGET_SECONDS \
    + MINIMUM_GATE_SECONDS \
    + PARITY_CUTOFF_LAG_SECONDS \
    + zstd_timeout_seconds + oss_copy_timeout_seconds \
    + LEGACY_RUNTIME_RESERVE_SECONDS))
  [[ $required -eq 10320 ]] || {
    printf 'Gate runtime budget does not cover bounded post-gate uploads\n' >&2
    exit 1
  }
  if observation=$(legacy_runtime_budget_observation "$required"); then
    printf 'Gate accepted 695 seconds of remaining runtime for a %s-second gate\n' \
      "$required" >&2
    exit 1
  fi
  [[ $observation == "remaining=695 required=$required" ]] || {
    printf 'Gate runtime rejection does not report remaining and required seconds\n' >&2
    exit 1
  }
  unreserved_remaining=$((required - LEGACY_RUNTIME_RESERVE_SECONDS))
  unreserved_uptime=$((LEGACY_RUNTIME_MAX_SECONDS - unreserved_remaining + 1))
  monotonic_uptime_seconds() { printf '%s\n' "$unreserved_uptime"; }
  if observation=$(legacy_runtime_budget_observation "$required"); then
    printf 'Gate admitted the unreserved exact runtime boundary\n' >&2
    exit 1
  fi
  [[ $observation == "remaining=$unreserved_remaining required=$required" ]] || {
    printf 'Gate reserve-boundary evidence is not exact\n' >&2
    exit 1
  }
  monotonic_uptime_seconds() { printf '600\n'; }
  observation=$(legacy_runtime_budget_observation "$required") || {
    printf 'Gate rejected a fresh legacy baseline with sufficient runtime\n' >&2
    exit 1
  }
  [[ $observation == "remaining=21001 required=$required" ]] || {
    printf 'Gate sufficient-runtime evidence is not exact\n' >&2
    exit 1
  }

  baseline_mode=legacy_python
  gate_seconds=$MINIMUM_GATE_SECONDS
  candidate_sha=unused run_id=unused release_binary=unused
  oss_bucket=unused oss_endpoint=unused oss_region=unused aliyun_profile=unused
  zstd_timeout_seconds=3600 oss_copy_timeout_seconds=1800 oss_config_sha=unused
  source_revision=unused deployment_bundle_sha=unused release_manifest_sha=unused
  control_archive_sha=unused
  identity_checks=0
  timeout_log="$tmp_dir/runtime-budget-timeout.args"
  verify_baseline_identity() { identity_checks=$((identity_checks + 1)); }
  timeout() {
    printf '%s\n' "$*" >"$timeout_log"
    printf '{}\n'
  }
  legacy_runtime_budget_observation() {
    printf 'remaining=695 required=%s\n' "$1"
    return 1
  }
  admission_error="$tmp_dir/runtime-budget-admission.err"
  if run_budgeted_real_market_preflight source spool download evidence \
    2>"$admission_error" >/dev/null; then
    printf 'Gate admitted a legacy identity that cannot survive the Gate\n' >&2
    exit 1
  fi
  grep -Fq "remaining=695 required=$required" "$admission_error" || {
    printf 'Gate admission omitted bounded post-gate work from its runtime budget\n' >&2
    exit 1
  }
  [[ ! -e $timeout_log && $identity_checks -eq 1 ]] || {
    printf 'Gate did not fail closed on an unstable legacy identity before preflight\n' >&2
    exit 1
  }
  legacy_runtime_budget_observation() {
    printf 'remaining=21001 required=%s\n' "$1"
  }
  run_budgeted_real_market_preflight source spool download evidence >/dev/null || {
    printf 'Gate rejected a sufficient runtime budget at the admission seam\n' >&2
    exit 1
  }
  [[ -s $timeout_log && $identity_checks -eq 4 ]] || {
    printf 'Gate did not bind legacy identity around real preflight\n' >&2
    exit 1
  }
  [[ $(<"$timeout_log") == '--signal=KILL 1200 env '* ]] || {
    printf 'Gate real preflight does not have an exact hard deadline\n' >&2
    exit 1
  }
)
preflight_deadline_contract="$tmp_dir/preflight-deadline.sh"
sed -n \
  -e '/^remaining_seconds_before_deadline() {$/,/^}$/p' \
  -e '/^run_before_deadline() {$/,/^}$/p' "$GATE" \
  >"$preflight_deadline_contract"
grep -Fq 'run_before_deadline() {' "$preflight_deadline_contract" || {
  printf 'Gate has no bounded command runner for real preflight\n' >&2
  exit 1
}
grep -Fq 'preflight_deadline=$((SECONDS + REAL_MARKET_PREFLIGHT_BUDGET_SECONDS))' \
  "$GATE" || {
  printf 'Gate real preflight has no overall deadline\n' >&2
  exit 1
}
grep -Fq 'run_before_deadline "$preflight_deadline" runuser' "$GATE" || {
  printf 'Gate candidate preflight uploader is not deadline bounded\n' >&2
  exit 1
}
grep -Fq 'oss_download_with_retry() {' "$GATE" || {
  printf 'Gate OSS triplet readback has no retry wrapper\n' >&2
  exit 1
}
[[ $(grep -Fc 'oss_download_with_retry "$deadline"' "$GATE") -eq 3 ]] || {
  printf 'Gate OSS triplet readback is not fully retry wrapped\n' >&2
  exit 1
}
(
  # shellcheck source=/dev/null
  source "$preflight_deadline_contract"
  timeout_log="$tmp_dir/preflight-timeout.log"
  timeout() {
    printf '%s\n' "$*" >"$timeout_log"
    shift 2
    "$@"
  }
  SECONDS=20
  [[ $(run_before_deadline 30 printf 'bounded') == bounded ]] || {
    printf 'Gate bounded command runner rejected remaining time\n' >&2
    exit 1
  }
  grep -Fq -- '--signal=KILL 10 printf bounded' "$timeout_log" || {
    printf 'Gate bounded command runner did not use the exact remaining deadline\n' >&2
    exit 1
  }
  SECONDS=30
  if run_before_deadline 30 true; then
    printf 'Gate bounded command runner accepted an expired deadline\n' >&2
    exit 1
  fi
)
legacy_health_observer="$tmp_dir/legacy-health-observer.sh"
sed -n \
  -e '/^readonly MAX_HEALTH_SILENCE_SECONDS=/p' \
  -e '/^readonly LEGACY_START_HEALTH_MAX_AGE_SECONDS=/p' \
  -e '/^legacy_health_publication_after_gate() {$/,/^}$/p' \
  -e '/^fresh_legacy_health_observation() {$/,/^}$/p' "$GATE" \
  >"$legacy_health_observer"
[[ -s $legacy_health_observer ]] || {
  printf 'Gate has no completed-write freshness verifier for legacy health\n' >&2
  exit 1
}
(
  if command -v gdate >/dev/null 2>&1; then
    date() { command gdate "$@"; }
  fi
  if command -v gstat >/dev/null 2>&1; then
    stat() { command gstat "$@"; }
  fi
  # shellcheck source=/dev/null
  source "$legacy_health_observer"
  legacy_health_publication_after_gate 120 start-file 120 completion-file || {
    printf 'Gate rejected a distinct atomic health write in the Gate start second\n' >&2
    exit 1
  }
  if legacy_health_publication_after_gate 120 start-file 119 completion-file \
    || legacy_health_publication_after_gate 120 same-file 120 same-file; then
    printf 'Gate accepted a predating or reused legacy health publication\n' >&2
    exit 1
  fi
  cp "$tmp_dir/legacy-health.json" "$tmp_dir/atomic-legacy-health.json"
  mv "$tmp_dir/atomic-legacy-health.json" \
    "$tmp_dir/freshly-completed-legacy-health.json"
  observation=$(fresh_legacy_health_observation \
    "$tmp_dir/freshly-completed-legacy-health.json" "$LEGACY_HEALTH_POLICY")
  jq -e \
    --argjson written_at "$(stat -c %Y \
      "$tmp_dir/freshly-completed-legacy-health.json")" \
    --arg file_identity "$(stat -c '%d:%i' \
      "$tmp_dir/freshly-completed-legacy-health.json")" \
    '.written_at_unix == $written_at
      and .file_identity == $file_identity
      and .health.last_success_at == "2026-07-15T00:00:01Z"' \
    <<<"$observation" >/dev/null || {
      printf 'Gate rejected a fresh completed legacy cycle with an old cycle-start timestamp\n' >&2
      exit 1
    }
  (
    fixed_now=$(date -u +%s)
    date_bin=$(type -P gdate || type -P date)
    date() {
      if [[ $# -eq 2 && $1 == -u && $2 == +%s ]]; then
        printf '%s\n' "$fixed_now"
      else
        command "$date_bin" "$@"
      fi
    }
    cp "$tmp_dir/legacy-health.json" "$tmp_dir/start-age-legacy-health.json"
    TZ=UTC touch -t "$("$date_bin" -u -d \
      "@$((fixed_now - LEGACY_START_HEALTH_MAX_AGE_SECONDS))" \
      +%Y%m%d%H%M.%S)" "$tmp_dir/start-age-legacy-health.json"
    fresh_legacy_health_observation \
      "$tmp_dir/start-age-legacy-health.json" "$LEGACY_HEALTH_POLICY" \
      true "$LEGACY_START_HEALTH_MAX_AGE_SECONDS" >/dev/null || {
      printf 'Gate rejected a startup health publication exactly 2700 seconds old\n' >&2
      exit 1
    }
    if fresh_legacy_health_observation \
      "$tmp_dir/start-age-legacy-health.json" "$LEGACY_HEALTH_POLICY" \
      false >/dev/null 2>&1; then
      printf 'Gate applied startup health age to a strict freshness check\n' >&2
      exit 1
    fi
    TZ=UTC touch -t "$("$date_bin" -u -d \
      "@$((fixed_now - LEGACY_START_HEALTH_MAX_AGE_SECONDS - 1))" \
      +%Y%m%d%H%M.%S)" "$tmp_dir/start-age-legacy-health.json"
    if fresh_legacy_health_observation \
      "$tmp_dir/start-age-legacy-health.json" "$LEGACY_HEALTH_POLICY" \
      true "$LEGACY_START_HEALTH_MAX_AGE_SECONDS" >/dev/null 2>&1; then
      printf 'Gate accepted a startup health publication 2701 seconds old\n' >&2
      exit 1
    fi
  )
  touch -d '1970-01-01T00:00:00Z' \
    "$tmp_dir/freshly-completed-legacy-health.json"
  if fresh_legacy_health_observation \
    "$tmp_dir/freshly-completed-legacy-health.json" \
    "$LEGACY_HEALTH_POLICY" >/dev/null 2>&1; then
    printf 'Gate accepted an old legacy health file write\n' >&2
    exit 1
  fi
  jq '.updated_at = "2999-01-01T00:00:00Z"
    | .last_success_at = "2999-01-01T00:00:00Z"' \
    "$tmp_dir/legacy-health.json" >"$tmp_dir/future-legacy-health.json"
  touch "$tmp_dir/future-legacy-health.json"
  if fresh_legacy_health_observation "$tmp_dir/future-legacy-health.json" \
    "$LEGACY_HEALTH_POLICY" >/dev/null 2>&1; then
    printf 'Gate accepted future legacy health payload timestamps\n' >&2
    exit 1
  fi
  (
    stat_bin=$(command -v gstat || command -v stat)
    cp "$tmp_dir/legacy-health.json" "$tmp_dir/racy-legacy-health.json"
    printf '0\n' >"$tmp_dir/racy-stat-calls"
    stat() {
      local calls
      calls=$(($(<"$tmp_dir/racy-stat-calls") + 1))
      printf '%s\n' "$calls" >"$tmp_dir/racy-stat-calls"
      if ((calls == 1)); then
        printf '0:0:0:0:0\n'
      else
        command "$stat_bin" "$@"
      fi
    }
    fresh_legacy_health_observation "$tmp_dir/racy-legacy-health.json" \
      "$LEGACY_HEALTH_POLICY" >/dev/null || {
      printf 'Gate did not retry a legacy health snapshot across atomic publication\n' >&2
      exit 1
    }
  )
)
daemon_reload_line=$(grep -nF 'systemctl daemon-reload' "$GATE" | tail -1 | cut -d: -f1)
preflight_line=$(grep -nF \
  'real_market_preflight_json=$(run_budgeted_real_market_preflight' \
  "$GATE" | cut -d: -f1)
start_snapshot_line=$(grep -nF \
  'baseline_health_observation=$(fresh_legacy_health_observation' \
  "$GATE" | cut -d: -f1)
start_identity_before_line=$(grep -nF \
  "baseline identity changed before legacy health admission" \
  "$GATE" | cut -d: -f1)
start_identity_after_line=$(grep -nF \
  "baseline identity changed during legacy health admission" \
  "$GATE" | cut -d: -f1)
completion_snapshot_line=$(grep -nF \
  'baseline_health_completion_observation=$(fresh_legacy_health_observation' \
  "$GATE" | cut -d: -f1)
gate_start_line=$(grep -nF 'started_at_unix=$(date -u +%s)' "$GATE" | cut -d: -f1)
gate_start_health_recheck_line=$(grep -nF \
  'active legacy collector health aged past startup admission before observation' \
  "$GATE" | cut -d: -f1)
shadow_start_line=$(grep -nF 'systemctl start "$shadow_unit"' "$GATE" | cut -d: -f1)
if ! ((daemon_reload_line < preflight_line \
  && preflight_line < start_identity_before_line \
  && start_identity_before_line < start_snapshot_line \
  && start_snapshot_line < start_identity_after_line \
  && start_identity_after_line < shadow_start_line \
  && shadow_start_line < gate_start_line \
  && gate_start_line < gate_start_health_recheck_line \
  && gate_start_health_recheck_line < completion_snapshot_line \
  && shadow_start_line < completion_snapshot_line)); then
  printf 'legacy health admission or completion seam is ordered incorrectly\n' >&2
  exit 1
fi
grep -Fq \
  'started_at_unix - baseline_health_start_written_at_unix > LEGACY_START_HEALTH_MAX_AGE_SECONDS' \
  "$GATE" || {
  printf 'Gate does not recheck startup health age at the exact observation start\n' >&2
  exit 1
}
[[ $(legacy_health_sample_state \
  "$tmp_dir/legacy-health.json" "$LEGACY_HEALTH_POLICY" legacy_python) == clean ]]
jq '.api_errors = ["trades condition-1: The read operation timed out"]' \
  "$tmp_dir/legacy-health.json" >"$tmp_dir/transient-legacy-health.json"
[[ $(legacy_health_sample_state \
  "$tmp_dir/transient-legacy-health.json" "$LEGACY_HEALTH_POLICY" legacy_python) \
  == transient_api_error ]]
[[ $(legacy_health_sample_state \
  "$tmp_dir/transient-legacy-health.json" "$LEGACY_HEALTH_POLICY" rust_release) \
  == fatal ]]
jq '.malformed_trade_rows = 1' "$tmp_dir/transient-legacy-health.json" \
  >"$tmp_dir/fatal-legacy-health.json"
[[ $(legacy_health_sample_state \
  "$tmp_dir/fatal-legacy-health.json" "$LEGACY_HEALTH_POLICY" legacy_python) == fatal ]]
[[ $(legacy_health_transition clean '' 0 240) == advance: ]]
startup_transient=$(legacy_health_transition transient_api_error '' 0 240)
[[ $startup_transient == wait:0 ]]
[[ $(legacy_health_transition transient_api_error \
  "${startup_transient#*:}" 240 240) == wait:0 ]]
[[ $(legacy_health_transition clean "${startup_transient#*:}" 240 240) == advance: ]]
[[ $(legacy_health_transition transient_api_error \
  "${startup_transient#*:}" 241 240) == expired:0 ]]
[[ $(legacy_health_transition clean "${startup_transient#*:}" 241 240) == expired:0 ]]
[[ $(legacy_health_transition fatal '' 0 240) == fatal: ]]

jq -n '{
  updated_at:"2026-07-15T00:00:01Z",last_success_at:"2026-07-15T00:00:01Z",
  cycle_started_at:"2026-07-15T00:00:00Z",cycle_duration_ms:1000,
  target_markets:120,
  missing_target_symbols:[],api_errors:[],malformed_trade_rows:0,
  trade_poll_budget:200,trade_poll_concurrency:4,trade_request_spacing_ms:125,
  priority_trade_markets_before_market_details:108,
  market_detail_budget:4,market_detail_eligible:3,market_detail_priority:2,
  market_detail_selected:3,market_detail_deferred:0,market_detail_priority_deferred:0,
  trade_poll_budget_after_market_details:197,
  eligible_trade_markets:200,priority_trade_markets:110,
  selected_trade_markets:197,deferred_trade_markets:3,priority_trade_backlog:0,
  trade_polls:197,successful_trade_polls:197,
  truncated_trade_markets:[],non_object_trade_markets:[],invalid_settlement_markets:[],
  invalid_end_time_markets:[],stale_trade_markets:[],stale_settlement_markets:[],
  overdue_unresolved_markets:[]
}' >"$tmp_dir/rust-health.json"
jq -e -f "$RUST_HEALTH_POLICY" "$tmp_dir/rust-health.json" >/dev/null
jq --arg error "$legacy_rate_limit_error" '.api_errors = [$error]' \
  "$tmp_dir/rust-health.json" >"$tmp_dir/rate-limited-rust-health.json"
if jq -e -f "$RUST_HEALTH_POLICY" \
  "$tmp_dir/rate-limited-rust-health.json" >/dev/null; then
  printf 'Rust health policy accepted a legacy trades rate limit\n' >&2
  exit 1
fi
jq '.overdue_unresolved_markets = ["historical-market"]' \
  "$tmp_dir/rust-health.json" >"$tmp_dir/historical-overdue-rust-health.json"
jq -e -f "$RUST_HEALTH_POLICY" \
  "$tmp_dir/historical-overdue-rust-health.json" >/dev/null
for mutation in \
  'del(.overdue_unresolved_markets)' \
  '.overdue_unresolved_markets = "market-1"' \
  '.overdue_unresolved_markets = [""]' \
  '.overdue_unresolved_markets = [1]'; do
  jq "$mutation" "$tmp_dir/rust-health.json" >"$tmp_dir/bad-rust-overdue.json"
  if jq -e -f "$RUST_HEALTH_POLICY" "$tmp_dir/bad-rust-overdue.json" >/dev/null; then
    printf 'Rust health policy accepted invalid overdue evidence: %s\n' "$mutation" >&2
    exit 1
  fi
done
jq '
  .priority_trade_markets_before_market_details = 120
  | .market_detail_eligible = 0
  | .market_detail_priority = 0
  | .market_detail_selected = 0
  | .market_detail_deferred = 0
  | .market_detail_priority_deferred = 0
  | .trade_poll_budget_after_market_details = 200
  | .priority_trade_markets = 120
  | .selected_trade_markets = 200
  | .deferred_trade_markets = 0
  | .trade_polls = 200
  | .successful_trade_polls = 200
' "$tmp_dir/rust-health.json" >"$tmp_dir/saturated-rust-health.json"
jq -e -f "$RUST_HEALTH_POLICY" "$tmp_dir/saturated-rust-health.json" >/dev/null
for mutation in \
  'del(.last_success_at)' \
  'del(.cycle_started_at)' \
  '.cycle_duration_ms = -1' \
  '.cycle_duration_ms = 180001' \
  '.trade_poll_budget = 199' \
  '.trade_poll_budget = 201' \
  '.trade_poll_concurrency = 0' \
  '.trade_poll_concurrency = 5' \
  '.trade_poll_concurrency = 193' \
  'del(.priority_trade_markets_before_market_details)' \
  '.priority_trade_markets_before_market_details = 113' \
  '.market_detail_budget = 3' \
  '.market_detail_priority = 4' \
  '.market_detail_selected = 1' \
  '.market_detail_deferred = 2' \
  '.market_detail_priority_deferred = 1' \
  '.trade_poll_budget_after_market_details = 196' \
  'del(.trade_request_spacing_ms)' \
  '.trade_request_spacing_ms = 124' \
  '.priority_trade_markets = 107' \
  '.priority_trade_backlog = 1' \
  '.selected_trade_markets = 196' \
  '.deferred_trade_markets = 1' \
  '.eligible_trade_markets = 111' \
  '.successful_trade_polls = 0' \
  '.successful_trade_polls = 111' \
  '.missing_target_symbols = ["BTCUSDT"]' \
  '.api_errors = ["Gamma unavailable"]' \
  '.non_object_trade_markets = ["condition-1"]' \
  '.invalid_settlement_markets = ["market-1"]' \
  '.invalid_end_time_markets = ["market-1"]'; do
  jq "$mutation" "$tmp_dir/rust-health.json" >"$tmp_dir/bad-rust-health.json"
  if jq -e -f "$RUST_HEALTH_POLICY" "$tmp_dir/bad-rust-health.json" >/dev/null; then
    printf 'Rust health policy accepted failure mutation: %s\n' "$mutation" >&2
    exit 1
  fi
done

grep -Fq 'readonly REQUIRED_DURATION_SECONDS=3600' "$GATE"
grep -Fq 'readonly PARITY_TAIL_SECONDS=601' "$GATE"
grep -Fq 'readonly MINIMUM_GATE_SECONDS=$REQUIRED_DURATION_SECONDS' "$GATE"
grep -Fq 'production gate duration must be exactly 3600 seconds' "$GATE"
grep -Fq 'readonly LEGACY_HEALTH_COMPLETION_REQUIRED=false' "$GATE"
grep -Fxq 'readonly LEGACY_HEALTH_START_REQUIRED=false' "$GATE"
grep -Fq 'readonly LEGACY_RUNTIME_STABILITY_REQUIRED=true' "$GATE"
grep -Fq 'bounded_parity_window_start' "$GATE"
grep -Fq 'readonly MAX_ACCEPTED_CYCLE_SECONDS=180' "$GATE"
grep -Fq 'readonly INITIAL_HEALTH_GRACE_SECONDS=60' "$GATE"
grep -Fq 'readonly HEALTH_SETTLE_SECONDS=$((MAX_ACCEPTED_CYCLE_SECONDS + INITIAL_HEALTH_GRACE_SECONDS))' "$GATE"
gate_health_budget_contract="$tmp_dir/gate-health-budget-contract.sh"
sed -n \
  -e '/^readonly MAX_ACCEPTED_CYCLE_SECONDS=/p' \
  -e '/^readonly INITIAL_HEALTH_GRACE_SECONDS=/p' \
  -e '/^readonly MAX_HEALTH_SILENCE_SECONDS=/p' "$GATE" \
  >"$gate_health_budget_contract"
# shellcheck source=/dev/null
source "$gate_health_budget_contract"
expected_health_silence_seconds=$((MAX_ACCEPTED_CYCLE_SECONDS + INITIAL_HEALTH_GRACE_SECONDS))
[[ $expected_health_silence_seconds -eq 240 ]] || {
  printf 'health freshness budget drifted from the expected 240-second contract\n' >&2
  exit 1
}
gate_health_silence_seconds=$MAX_HEALTH_SILENCE_SECONDS
[[ $gate_health_silence_seconds -eq $expected_health_silence_seconds ]] || {
  printf 'shadow gate freshness budget %ss does not cover the 240-second cycle budget\n' \
    "$gate_health_silence_seconds" >&2
  exit 1
}
cutover_health_silence_seconds=$(
  sed -n 's/^readonly MAX_HEALTH_SILENCE_SECONDS=//p' "$CUTOVER"
)
[[ $cutover_health_silence_seconds -eq $expected_health_silence_seconds ]] || {
  printf 'cutover freshness budget %ss does not cover the 240-second cycle budget\n' \
    "$cutover_health_silence_seconds" >&2
  exit 1
}
grep -Fq 'legacy_health_state=$(legacy_health_sample_state' "$GATE"
grep -Fq '"$legacy_health" "$release_control_dir/${LEGACY_HEALTH_POLICY##*/}"' \
  "$GATE"
grep -Fq '    "$baseline_mode")' "$GATE"
grep -Fq 'legacy_health_result=$(legacy_health_transition' "$GATE"
grep -Fq '"$now_uptime" "$MAX_HEALTH_SILENCE_SECONDS")' "$GATE"
grep -Fq 'legacy_health_decision=${legacy_health_result%%:*}' "$GATE"
grep -Fq 'legacy_api_error_started_at=${legacy_health_result#*:}' "$GATE"
legacy_transition_line=$(grep -nF \
  'legacy_health_result=$(legacy_health_transition' "$GATE" | cut -d: -f1)
health_settle_line=$(grep -nF \
  '  if ((elapsed >= HEALTH_SETTLE_SECONDS)); then' "$GATE" | cut -d: -f1)
if ((legacy_transition_line >= health_settle_line)); then
  printf 'legacy health recovery budget starts after the shadow settle delay\n' >&2
  exit 1
fi
grep -Fq 'if [[ $legacy_health_decision == advance ]]; then' "$GATE"
grep -Fq 'observation_deadline=$gate_seconds' "$GATE"
grep -Fq '&& ((observation_deadline < HEALTH_SETTLE_SECONDS)); then' "$GATE"
grep -Fq 'observation_deadline=$HEALTH_SETTLE_SECONDS' "$GATE"
grep -Fq '((elapsed < observation_deadline)) || break' "$GATE"
grep -Fq 'if ((elapsed >= HEALTH_SETTLE_SECONDS)); then' "$GATE"
if grep -Fq 'if ((elapsed >= HEALTH_SETTLE_SECONDS)) || [[ $test_only == true ]]; then' "$GATE"; then
  printf 'short shadow gate bypasses the initial health settle window\n' >&2
  exit 1
fi
grep -Fq 'verify-shadow-parity' "$GATE"
grep -Fq 'parity_args+=(--allow-empty-legacy)' "$GATE"
grep -Fq 'finalization_deferred_overlap' "$GATE"
grep -Fq 'trade_parity_mode' "$GATE"
grep -Fq 'collector_emission_mode' "$GATE"
grep -Fq -- '--max-retained-trade-ids' "$GATE"
grep -Fq 'shadow_finalization_counters' "$GATE"
grep -Fxq 'shadow_state_max_counters=null' "$GATE"
if grep -Eq '^[[:space:]]*shadow_state_max_counters=$' "$GATE"; then
  printf 'Gate initializes the finalization maxima to an empty JSON value\n' >&2
  exit 1
fi
if grep -Fq '&& $LEGACY_RUNTIME_STABILITY_REQUIRED == false ]]; then' "$GATE"; then
  printf 'Gate retains a legacy identity bypass after parity or OSS readback\n' >&2
  exit 1
fi
[[ ! -e "$SCRIPT_DIR/verify-polymarket-shadow-parity.py" ]]
if grep -Fq 'python3 "$PARITY_VERIFIER"' "$GATE"; then
  printf 'shadow gate still invokes the retired legacy parity verifier\n' >&2
  exit 1
fi
grep -Fq 'parity_window_ended_at_unix' "$GATE"
grep -Fq 'common_cutoff' "$GATE"
grep -Fq 'shadow_spool="$shadow_parent/$run_id"' "$GATE"
grep -Fq 'MONDAY_POLYMARKET_SHADOW_SPOOL' \
  "$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
grep -Fq 'crypto_expiry_reference_rust_shadow' "$GATE"
grep -Fq 'crypto_expiry_preflight_${candidate_sha:0:12}_${run_id,,}' "$GATE"
if grep -Fq 'crypto_expiry_market_rust_shadow' "$GATE"; then
  printf 'Gate still substitutes a synthetic market fixture for production input\n' >&2
  exit 1
fi
preflight_line=$(grep -n \
  '^real_market_preflight_json=$(run_budgeted_real_market_preflight' "$GATE" \
  | cut -d: -f1)
gate_start_line=$(grep -n '^started_at_unix=' "$GATE" | cut -d: -f1)
shadow_start_line=$(grep -n '^systemctl start "$shadow_unit"$' "$GATE" | cut -d: -f1)
((preflight_line < gate_start_line && preflight_line < shadow_start_line)) || {
  printf 'real market-segment preflight does not finish before shadow startup\n' >&2
  exit 1
}
grep -Fq '.canonical_uploaded_segments' "$GATE"
grep -Fq 'oss_config_sha256' "$GATE"
grep -Fq 'oss_config_sha256' "$CUTOVER"
grep -Fq 'polymarket-legacy-health-policy.jq' "$GATE"
grep -Fq 'polymarket-legacy-health-policy.jq' "$CUTOVER"
grep -Fq 'polymarket-rust-health-policy.jq' "$GATE"
grep -Fq 'polymarket-rust-health-policy.jq' "$CUTOVER"
grep -Fq 'oss_readback_parity:true' "$GATE"
grep -Fq 'market_oss_readback_parity:true' "$GATE"
grep -Fq 'real_market_segment_preflight:true' "$GATE"
grep -Fq 'ControlGroup' "$GATE"
grep -Fq 'valid_absolute_path "$control_group"' "$GATE"
grep -Fq '[[ $control_group == /system.slice/*' "$GATE"
grep -Fq '${control_group##*/} == "$shadow_unit"' "$GATE"
grep -Fq 'proc_binding == "0::$control_group"' "$GATE"
grep -Fq 'cgroup_dir="/sys/fs/cgroup$control_group"' "$GATE"
grep -Fq 'direct_directory "$cgroup_dir"' "$GATE"
grep -Fq '$(readlink -f -- "$file") == "$file"' "$GATE"
grep -Fq 'stable_memory_events_snapshot' "$GATE"
grep -Fq 'memory_events:{start:$memory_events_start,end:$memory_events_end}' "$GATE"
grep -Fq 'memory_events_stable:true' "$GATE"
grep -Fq 'systemctl freeze "$shadow_unit"' "$GATE"
grep -Fq 'FreezerState' "$GATE"
grep -Fq '[[ $shadow_freezer_state == frozen ]]' "$GATE"
grep -Fq 'systemctl kill --kill-whom=main --signal=SIGTERM "$shadow_unit"' "$GATE"

cleanup_thaw_line=$(grep -n '^    systemctl thaw "$shadow_unit"' "$GATE" | cut -d: -f1)
cleanup_stop_line=$(grep -n '^    systemctl stop "$shadow_unit" >/dev/null' "$GATE" \
  | cut -d: -f1)
[[ $cleanup_thaw_line =~ ^[1-9][0-9]*$ \
  && $cleanup_stop_line =~ ^[1-9][0-9]*$ \
  && $cleanup_thaw_line -lt $cleanup_stop_line ]] || {
  printf 'shadow cleanup does not thaw before stop\n' >&2
  exit 1
}

freeze_line=$(grep -n '^systemctl freeze "$shadow_unit"' "$GATE" | cut -d: -f1)
freezer_state_line=$(grep -n '^shadow_freezer_state=.*FreezerState' "$GATE" | cut -d: -f1)
final_memory_line=$(grep -n '^memory_events_end=$(stable_memory_events_snapshot' "$GATE" \
  | tail -1 | cut -d: -f1)
kill_line=$(grep -n '^systemctl kill --kill-whom=main --signal=SIGTERM' "$GATE" \
  | cut -d: -f1)
final_thaw_line=$(grep -n '^systemctl thaw "$shadow_unit"' "$GATE" \
  | tail -1 | cut -d: -f1 || true)
thawed_state_line=$(grep -n '^shadow_thawed_state=.*FreezerState' "$GATE" \
  | cut -d: -f1 || true)
final_stop_line=$(grep -n '^systemctl stop "$shadow_unit"$' "$GATE" | tail -1 | cut -d: -f1)
finalize_line=$(grep -n '"$release_binary" finalize-reference-tape' "$GATE" \
  | tail -1 | cut -d: -f1 || true)
parity_line=$(grep -n '"$release_binary" "${parity_args\[@\]}"' "$GATE" \
  | tail -1 | cut -d: -f1)
[[ $freeze_line =~ ^[1-9][0-9]*$ \
  && $freezer_state_line =~ ^[1-9][0-9]*$ \
  && $final_memory_line =~ ^[1-9][0-9]*$ \
  && $kill_line =~ ^[1-9][0-9]*$ \
  && $final_thaw_line =~ ^[1-9][0-9]*$ \
  && $thawed_state_line =~ ^[1-9][0-9]*$ \
  && $final_stop_line =~ ^[1-9][0-9]*$ \
  && $finalize_line =~ ^[1-9][0-9]*$ \
  && $parity_line =~ ^[1-9][0-9]*$ \
  && $freeze_line -lt $freezer_state_line \
  && $freezer_state_line -lt $final_memory_line \
  && $final_memory_line -lt $kill_line \
  && $kill_line -lt $final_thaw_line \
  && $final_thaw_line -lt $thawed_state_line \
  && $thawed_state_line -lt $final_stop_line \
  && $kill_line -lt $final_stop_line \
  && $final_stop_line -lt $finalize_line \
  && $finalize_line -lt $parity_line ]] || {
  printf 'shadow final stop/finalize/parity sequence is unsafe\n' >&2
  exit 1
}
grep -Fq '[[ $shadow_thawed_state == running ]]' "$GATE"
grep -Fq 'runuser -u hftcollector -- env HOME=/var/lib/hft-collector' "$GATE"
finalizer_path_contract="$tmp_dir/finalizer-path-contract.sh"
sed -n \
  -e '/^valid_absolute_path() {$/,/^}$/p' \
  -e '/^valid_finalized_reference_tape_path() {$/,/^}$/p' "$GATE" \
  >"$finalizer_path_contract"
# shellcheck disable=SC1090
source "$finalizer_path_contract"
finalizer_spool="$tmp_dir/finalizer-spool"
mkdir -p "$finalizer_spool/market-updates.bad"
direct_finalized="$finalizer_spool/market-updates.20260730T120000000000.ndjson"
nested_finalized="$finalizer_spool/market-updates.bad/market-updates.20260730T120000000000.ndjson"
: >"$direct_finalized"
: >"$nested_finalized"
valid_finalized_reference_tape_path "$direct_finalized" "$finalizer_spool" || {
  printf 'Gate rejected a direct finalized reference tape\n' >&2
  exit 1
}
if valid_finalized_reference_tape_path "$nested_finalized" "$finalizer_spool"; then
  printf 'Gate accepted a nested finalized reference tape\n' >&2
  exit 1
fi

validator_functions="$tmp_dir/control-group-validator.sh"
sed -n '/^valid_absolute_path() {$/,/^}$/p' "$GATE" >"$validator_functions"
sed -n '/^valid_shadow_control_group() {$/,/^}$/p' "$GATE" >>"$validator_functions"
grep -Fq 'valid_absolute_path()' "$validator_functions"
grep -Fq 'valid_shadow_control_group()' "$validator_functions"
# shellcheck disable=SC1090
source "$validator_functions"
shadow_unit="polymarket-reference-collector-shadow@${candidate}.service"
valid_shadow_control_group \
  "/system.slice/system-polymarket\\x2dreference\\x2dcollector\\x2dshadow.slice/$shadow_unit"
for invalid_control_group in \
  "/system.slice/system-polymarket.slice/not-$shadow_unit" \
  "/system.slice/../$shadow_unit" \
  "/system.slice//nested/$shadow_unit"; do
  if valid_shadow_control_group "$invalid_control_group"; then
    printf 'control-group validator accepted unsafe path: %s\n' \
      "$invalid_control_group" >&2
    exit 1
  fi
done
grep -Fq '.missing_target_symbols == []' "$RUST_HEALTH_POLICY"
grep -Fq 'systemctl restart "$COLLECTOR_UNIT"' "$CUTOVER"
grep -Fq 'clear_health_before_restart "$evidence_dir" pre-cutover' "$CUTOVER"
grep -Fq 'readlink -f "/proc/$pid/exe"' "$CUTOVER"
grep -Fq 'FragmentPath' "$CUTOVER"
grep -Fq 'DropInPaths' "$CUTOVER"
grep -Fq 'NRestarts' "$CUTOVER"
[[ $(grep -Fc '[[ $invocation_id == "$expected_invocation_id" ]]' "$GATE") -eq 2 ]]
[[ $(grep -Fc '[[ $invocation_id == "$expected_invocation_id" ]]' "$CUTOVER") -eq 3 ]]
grep -Fq 'invocation_id:$rust_invocation_id' "$CUTOVER"
grep -Fq 'verify_shadow_identity' "$GATE"
grep -Fq 'verify_contained_bootstrap_recovery "$recovery_json" "$candidate_sha"' "$CUTOVER"
grep -Fq 'contained recovery rollback would restart a saved baseline unit' "$CUTOVER"
grep -Fq 'contained recovery rollback restarted a collector or uploader' "$CUTOVER"
contained_recovery_guard_line=$(grep -nF \
  'contained recovery rollback would restart a saved baseline unit' "$CUTOVER" \
  | head -1 | cut -d: -f1 || true)
contained_recovery_health_label_line=$(grep -nF \
  '"pre-contained-recovery-rollback-' \
  "$CUTOVER" | head -1 | cut -d: -f1 || true)
restore_stop_line=$(grep -nF \
  'systemctl stop "$REFERENCE_UPLOAD_TIMER" "$MARKET_UPLOAD_TIMER"' "$CUTOVER" \
  | head -1 | cut -d: -f1 || true)
[[ $contained_recovery_guard_line =~ ^[1-9][0-9]*$ \
  && $contained_recovery_health_label_line =~ ^[1-9][0-9]*$ \
  && $restore_stop_line =~ ^[1-9][0-9]*$ \
  && $contained_recovery_guard_line -lt $contained_recovery_health_label_line \
  && $contained_recovery_health_label_line -lt $restore_stop_line ]] || {
  printf 'contained recovery rollback mutates before validating or clearing health\n' >&2
  exit 1
}
[[ $(sed -n "$((contained_recovery_health_label_line - 1))p" "$CUTOVER") \
  == *'clear_health_before_restart "$evidence_dir"'* ]] || {
  printf 'contained recovery rollback does not clear the candidate health file\n' >&2
  exit 1
}
grep -Fq 'health_advanced=true' "$CUTOVER"
grep -Fq 'updated_epoch >= started_epoch' "$CUTOVER"
grep -Fq 'gate_legacy_pid' "$CUTOVER"
grep -Fq 'pinned_upload_env' "$GATE"
grep -Fq 'pinned_upload_env' "$CUTOVER"
grep -Fq 'EnvironmentFile=$pinned_upload_env' "$CUTOVER"
grep -Fq 'journalctl --unit "$COLLECTOR_UNIT"' "$CUTOVER"
grep -Fq 'restore_legacy "$evidence_dir"' "$CUTOVER"
grep -Fq 'restore_status=$?' "$CUTOVER"
grep -Fq 'control/polymarket-legacy-health-policy.jq' "$CUTOVER"
if grep -Fq 'if ! restore_legacy' "$CUTOVER"; then
  printf 'automatic rollback still invokes restore_legacy from a negated conditional\n' >&2
  exit 1
fi
grep -Fq 'verify_oneshot_success "$MARKET_UPLOAD_UNIT"' "$CUTOVER"
grep -Fq '/opt/monday/bin/polymarket_reference_collector.py' "$CUTOVER"
grep -Fq 'control-plane bundle changed after the shadow gate' "$CUTOVER"
grep -Fq 'shadow gate evidence is stale or from the future' "$CUTOVER"
grep -Fq 'secure_release_directory "$release_dir"' "$GATE"
grep -Fq 'secure_release_directory "$candidate_release_dir"' "$CUTOVER"
grep -Fxq 'Type=exec' "$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
grep -Fxq 'MemoryHigh=1536M' "$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
grep -Fxq 'MemoryMax=2048M' "$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
grep -Fxq 'MemoryHigh=1536M' "$SCRIPT_DIR/polymarket-reference-collector.service"
grep -Fxq 'MemoryMax=2048M' "$SCRIPT_DIR/polymarket-reference-collector.service"
grep -Fxq 'TimeoutStartSec=0' "$SCRIPT_DIR/polymarket-reference-upload.service"
grep -Fxq 'TimeoutStartSec=0' "$SCRIPT_DIR/polymarket-market-tape-upload.service"

release_chmod_line=$(grep -n '^  chmod 0755 "$staging"$' "$GATE" \
  | cut -d: -f1 || true)
release_move_line=$(grep -n '^  mv "$staging" "$release_dir"$' "$GATE" \
  | cut -d: -f1 || true)
[[ $release_chmod_line =~ ^[1-9][0-9]*$ \
  && $release_move_line =~ ^[1-9][0-9]*$ \
  && $release_chmod_line -lt $release_move_line ]] || {
  printf 'shadow gate does not make the release directory traversable before publish\n' >&2
  exit 1
}

cutover_stop_line=$(grep -n '^[[:space:]]*systemctl stop "$COLLECTOR_UNIT"$' \
  "$CUTOVER" | tail -1 | cut -d: -f1)
cutover_legacy_promotion="$tmp_dir/cutover-legacy-promotion.sh"
sed -n '/^# Cutover depends on the current gate bundle/,/^[[:space:]]*systemctl stop "$COLLECTOR_UNIT"$/p' \
  "$CUTOVER" >"$cutover_legacy_promotion"
cutover_legacy_promotion_joined="$tmp_dir/cutover-legacy-promotion-joined.sh"
join_shell_continuations "$cutover_legacy_promotion" \
  >"$cutover_legacy_promotion_joined"
grep -Fq 'jq -e -f "$POLICY" "$gate_json"' "$cutover_legacy_promotion"
[[ $(grep -Fc \
  'verify_legacy_runtime "$legacy_pid" "$gate_legacy_restarts" "$gate_legacy_invocation_id"' \
  "$cutover_legacy_promotion_joined") -eq 2 ]] || {
  printf 'cutover no longer binds the gated legacy identity before transition and stop\n' >&2
  exit 1
}
if grep -Fq 'verify_legacy_health' "$cutover_legacy_promotion"; then
  printf 'cutover re-admits legacy health after the immutable Gate already passed\n' >&2
  exit 1
fi
cutover_legacy_rollback="$tmp_dir/cutover-legacy-rollback.sh"
sed -n '/^restore_legacy() (/,/^\[\[ ${EUID}/p' "$CUTOVER" \
  >"$cutover_legacy_rollback"
cutover_legacy_rollback_joined="$tmp_dir/cutover-legacy-rollback-joined.sh"
join_shell_continuations "$cutover_legacy_rollback" \
  >"$cutover_legacy_rollback_joined"
[[ $(grep -Fc \
  'verify_legacy_runtime "$rollback_pid" 0 "$rollback_invocation_id"' \
  "$cutover_legacy_rollback_joined") -eq 4 ]] || {
  printf 'rollback no longer verifies the restored legacy identity at every boundary\n' >&2
  exit 1
}
if grep -Fq 'verify_fresh_legacy_runtime' "$cutover_legacy_rollback"; then
  printf 'legacy rollback still waits for a full-cycle health publication\n' >&2
  exit 1
fi
legacy_drain_line=$(grep -n '^[[:space:]]*verify_oneshot_success "$REFERENCE_UPLOAD_UNIT"' "$CUTOVER" \
  | head -1 | cut -d: -f1)
legacy_cursor_line=$(grep -n '^[[:space:]]*legacy_stop_cursor=$(journal_cursor "$COLLECTOR_UNIT")' \
  "$CUTOVER" | cut -d: -f1)
legacy_final_runtime_line=$(grep -n \
  '^[[:space:]]*verify_legacy_runtime "$legacy_pid" "$gate_legacy_restarts" "$gate_legacy_invocation_id"' \
  "$CUTOVER" \
  | tail -1 | cut -d: -f1)
legacy_final_oss_line=$(grep -n \
  'OSS configuration changed during the legacy uploader drain' "$CUTOVER" \
  | cut -d: -f1)
legacy_journal_guard_line=$(grep -n '^[[:space:]]*verify_no_restart_after_cursor' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
legacy_stopped_counter_line=$(grep -n '^[[:space:]]*stopped_legacy_restarts=' "$CUTOVER" \
  | cut -d: -f1)
legacy_stopped_equality_line=$(grep -n \
  '^[[:space:]]*\[\[ \$stopped_legacy_restarts == "\$gate_legacy_restarts" \]\]' "$CUTOVER" \
  | cut -d: -f1)
cutover_clear_line=$(grep -n '^clear_health_before_restart "$evidence_dir" pre-cutover$' \
  "$CUTOVER" | cut -d: -f1)
cutover_reset_line=$(grep -n '^systemctl reset-failed "$COLLECTOR_UNIT"$' "$CUTOVER" \
  | cut -d: -f1)
cutover_restart_line=$(grep -n '^systemctl restart "$COLLECTOR_UNIT"$' "$CUTOVER" \
  | cut -d: -f1)
((legacy_drain_line < legacy_cursor_line \
  && legacy_cursor_line < legacy_final_oss_line \
  && legacy_final_oss_line < legacy_final_runtime_line \
  && legacy_final_runtime_line < cutover_stop_line \
  && cutover_stop_line < legacy_journal_guard_line \
  && legacy_journal_guard_line < legacy_stopped_counter_line \
  && cutover_stop_line < legacy_stopped_counter_line \
  && legacy_stopped_counter_line < legacy_stopped_equality_line \
  && legacy_stopped_equality_line < cutover_clear_line \
  && cutover_clear_line < cutover_reset_line \
  && cutover_reset_line < cutover_restart_line)) || {
  printf 'cutover no longer proves restart-free legacy stop before reset/restart\n' >&2
  exit 1
}
rollback_reload_line=$(grep -n '^  systemctl daemon-reload$' "$CUTOVER" | cut -d: -f1)
rollback_reset_line=$(grep -n '^  systemctl reset-failed "$COLLECTOR_UNIT"$' "$CUTOVER" \
  | cut -d: -f1)
rollback_restart_line=$(grep -n '^  systemctl restart "$COLLECTOR_UNIT"$' "$CUTOVER" \
  | cut -d: -f1)
((rollback_reload_line < rollback_reset_line && rollback_reset_line < rollback_restart_line)) \
  || {
    printf 'rollback does not reset the inherited restart counter before verification\n' >&2
    exit 1
  }
rollback_state_line=$(grep -n '^  verify_saved_unit_state' "$CUTOVER" | cut -d: -f1)
rollback_final_runtime_line=$(grep -n '^[[:space:]]*verify_legacy_runtime "$rollback_pid" 0' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
((rollback_state_line < rollback_final_runtime_line)) || {
  printf 'rollback lacks a final runtime check after restoring unit state\n' >&2
  exit 1
}
rollback_branch_line=$(grep -n '^if \[\[ \$mode == rollback \]\]; then$' "$CUTOVER" \
  | cut -d: -f1)
cutover_policy_line=$(grep -n '^secure_regular_file "$POLICY"$' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
((rollback_branch_line < cutover_policy_line)) || {
  printf 'manual rollback still depends on the current cutover bundle preflight\n' >&2
  exit 1
}
grep -Fq 'shadow_restarts=$(systemctl show --property=NRestarts' "$GATE"
grep -Fq 'legacy_restarts=$(systemctl show --property=NRestarts' "$GATE"
grep -Fq \
  'verify_legacy_identity "$legacy_pid" "$legacy_restarts" "$legacy_invocation_id"' \
  "$GATE"
grep -Fq \
  'verify_legacy_runtime "$legacy_pid" "$gate_legacy_restarts" "$gate_legacy_invocation_id"' \
  "$CUTOVER"

shadow_cursor_line=$(grep -n '^shadow_stop_cursor=$(journal_cursor "$shadow_unit")' "$GATE" \
  | cut -d: -f1)
shadow_final_runtime_line=$(grep -n \
  '^verify_shadow_identity "$initial_shadow_pid" "$shadow_invocation_id"' "$GATE" \
  | tail -1 | cut -d: -f1)
shadow_stop_line=$(grep -n '^systemctl stop "$shadow_unit"$' "$GATE" | tail -1 | cut -d: -f1)
shadow_journal_guard_line=$(grep -n \
  '^verify_no_restart_after_cursor "$shadow_unit" "$shadow_stop_cursor" "$shadow_invocation_id"' \
  "$GATE" | cut -d: -f1)
shadow_stopped_counter_line=$(grep -n '^stopped_shadow_restarts=' "$GATE" | cut -d: -f1)
shadow_stopped_equality_line=$(grep -n '^\[\[ \$stopped_shadow_restarts == 0 \]\]' \
  "$GATE" | cut -d: -f1)
((shadow_cursor_line < shadow_final_runtime_line \
  && shadow_final_runtime_line < shadow_stop_line \
  && shadow_stop_line < shadow_journal_guard_line \
  && shadow_journal_guard_line < shadow_stopped_counter_line \
  && shadow_stopped_counter_line < shadow_stopped_equality_line)) || {
  printf 'shadow lacks exact invocation, journal, and restart-counter stop guards\n' >&2
  exit 1
}

cutover_sync_line=$(grep -n '^sync "$evidence_dir/cutover.json"$' "$CUTOVER" | cut -d: -f1)
cutover_rollback_sync_line=$(grep -n '^sync -f "$rollback_dir"$' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
cutover_release_sync_line=$(grep -n '^sync -f "$candidate_binary"$' "$CUTOVER" \
  | cut -d: -f1)
cutover_final_runtime_line=$(grep -n \
  '^verify_rust_runtime "$candidate_binary" "$started_epoch" "$rust_pid" "$rust_invocation_id"' \
  "$CUTOVER" \
  | tail -1 | cut -d: -f1)
cutover_final_upload_line=$(grep -n '^verify_upload_units "$pinned_upload_env"' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
cutover_final_oss_line=$(grep -n \
  'pinned OSS configuration changed before cutover completion' "$CUTOVER" \
  | cut -d: -f1)
cutover_marker_hash_line=$(grep -n '^  sha256sum cutover.json >' "$CUTOVER" | cut -d: -f1)
cutover_marker_move_line=$(grep -n '^mv -Tf "$success_marker_tmp" "$success_marker"$' \
  "$CUTOVER" | cut -d: -f1)
cutover_marker_sync_line=$(grep -n '^sync "$success_marker"$' "$CUTOVER" | cut -d: -f1)
cutover_marker_dir_sync_line=$(grep -n '^sync -f "$evidence_dir"$' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
cutover_success_line=$(grep -n '^cutover_succeeded=true$' "$CUTOVER" | cut -d: -f1)
cutover_trap_off_line=$(grep -n '^trap - EXIT$' "$CUTOVER" | tail -1 | cut -d: -f1)
((cutover_sync_line < cutover_rollback_sync_line \
  && cutover_rollback_sync_line < cutover_release_sync_line \
  && cutover_release_sync_line < cutover_final_runtime_line \
  && cutover_final_runtime_line < cutover_final_upload_line \
  && cutover_final_upload_line < cutover_final_oss_line \
  && cutover_final_oss_line < cutover_marker_hash_line \
  && cutover_marker_hash_line < cutover_marker_move_line \
  && cutover_marker_move_line < cutover_marker_sync_line \
  && cutover_marker_sync_line < cutover_marker_dir_sync_line \
  && cutover_marker_dir_sync_line < cutover_success_line \
  && cutover_success_line < cutover_trap_off_line)) || {
  printf 'cutover publishes success before final verification or durable marker sync\n' >&2
  exit 1
}

[[ $(grep -c '^systemctl start "$MARKET_UPLOAD_UNIT"$' "$CUTOVER") -eq 0 ]] || {
  printf 'cutover still synchronously drains the complete market backlog\n' >&2
  exit 1
}
[[ $(grep -c '^verify_oneshot_success "$MARKET_UPLOAD_UNIT"' "$CUTOVER") -eq 0 ]] || {
  printf 'cutover still requires terminal success for the complete market backlog\n' >&2
  exit 1
}
grep -Fq 'systemctl start --no-block "$MARKET_UPLOAD_UNIT"' "$CUTOVER"
grep -Fq \
  'verify_deferred_market_upload "$candidate_binary" "$market_upload_invocation_before"' \
  "$CUTOVER"
grep -Fq 'systemctl reset-failed "$MARKET_UPLOAD_UNIT"' "$CUTOVER"
[[ $(grep -c 'systemctl is-failed --quiet "$MARKET_UPLOAD_UNIT"' "$CUTOVER") -ge 3 ]]
grep -Fq 'unit_active "$REFERENCE_UPLOAD_TIMER"' "$CUTOVER"
grep -Fq 'unit_active "$MARKET_UPLOAD_TIMER"' "$CUTOVER"
grep -Fq 'market_upload_gate_verified:true' "$CUTOVER"
grep -Fq 'market_upload_terminal_success_required:false' "$CUTOVER"
grep -Fq 'market_backlog_deferred_to_timer:true' "$CUTOVER"

deferred_upload_functions="$tmp_dir/deferred-upload-functions.sh"
awk '
  /^verify_oneshot_success\(\) \{$/ || /^verify_deferred_market_upload\(\) \{$/ {
    copy=1
  }
  copy {print}
  copy && /^}$/ {copy=0}
' "$CUTOVER" >"$deferred_upload_functions"

run_deferred_upload_case() (
  local test_case=$1
  local expected_binary=/opt/monday/releases/polymarket-raw-ops/candidate/polymarket-raw-ops
  local previous_invocation=11111111111111111111111111111111
  local new_invocation=22222222222222222222222222222222
  local pid_state="$tmp_dir/deferred-upload-pid-$test_case"
  local exe_state="$tmp_dir/deferred-upload-exe-$test_case"
  local preexec_state="$tmp_dir/deferred-upload-preexec-$test_case"
  MARKET_UPLOAD_UNIT=polymarket-market-tape-upload.service
  systemctl() {
    if [[ $1 == is-failed ]]; then
      [[ $test_case == failed ]]
      return
    fi
    case "$2" in
      --property=InvocationID)
        [[ $test_case == stale ]] \
          && printf '%s\n' "$previous_invocation" \
          || printf '%s\n' "$new_invocation"
        ;;
      --property=ActiveState)
        if [[ $test_case == inactive-failed ]]; then
          printf '%s\n' inactive
        elif [[ $test_case == active-mismatched-exe ]]; then
          printf '%s\n' active
        else
          printf '%s\n' activating
        fi
        ;;
      --property=MainPID)
        if [[ $test_case == pid-delayed && ! -e $pid_state ]]; then
          : >"$pid_state"
          printf '%s\n' 0
        else
          printf '%s\n' 42
        fi
        ;;
      --property=Result)
        [[ $test_case == inactive-failed ]] \
          && printf '%s\n' exit-code \
          || printf '%s\n' success
        ;;
      --property=ExecMainStatus)
        [[ $test_case == inactive-failed ]] \
          && printf '%s\n' 1 \
          || printf '%s\n' 0
        ;;
      *) return 2 ;;
    esac
  }
  readlink() {
    if [[ $test_case == pid-delayed && $3 == /proc/0/exe ]]; then
      printf '%s\n' "${expected_binary}.wrong"
      return
    fi
    if [[ $test_case == exe-delayed && ! -e $exe_state ]]; then
      : >"$exe_state"
      return 1
    fi
    if [[ $test_case == preexec-delayed && ! -e $preexec_state ]]; then
      : >"$preexec_state"
      printf '%s\n' /usr/lib/systemd/systemd-executor
      return
    fi
    [[ $test_case == mismatched-exe || $test_case == active-mismatched-exe ]] \
      && printf '%s\n' /opt/monday/releases/polymarket-raw-ops/wrong/polymarket-raw-ops \
      || printf '%s\n' "$expected_binary"
  }
  sleep() { : >"$tmp_dir/deferred-upload-slept-$test_case"; }
  source "$deferred_upload_functions"
  verify_deferred_market_upload "$expected_binary" "$previous_invocation"
)

for rejected_case in stale failed inactive-failed mismatched-exe active-mismatched-exe; do
  if run_deferred_upload_case "$rejected_case"; then
    printf 'deferred market upload accepted counterexample: %s\n' "$rejected_case" >&2
    exit 1
  fi
done
[[ ! -e $tmp_dir/deferred-upload-slept-active-mismatched-exe ]] || {
  printf 'deferred market upload retried an active wrong executable\n' >&2
  exit 1
}
run_deferred_upload_case success
for retry_case in pid-delayed exe-delayed preexec-delayed; do
  if ! run_deferred_upload_case "$retry_case"; then
    printf 'deferred market upload rejected transient visibility: %s\n' \
      "$retry_case" >&2
    exit 1
  fi
done

snapshot_nullglob_line=$(grep -n '^    shopt -s nullglob$' "$CUTOVER" | cut -d: -f1)
snapshot_manifest_line=$(grep -n \
  '^    sha256sum state.json systemd/\* bin/\* config/\* control/\* >manifest.sha256$' \
  "$CUTOVER" | cut -d: -f1)
snapshot_sync_line=$(grep -n '^  sync -f "$rollback_dir"$' "$CUTOVER" | head -1 \
  | cut -d: -f1)
[[ -n $snapshot_nullglob_line && -n $snapshot_manifest_line \
  && $snapshot_nullglob_line -lt $snapshot_manifest_line \
  && $snapshot_manifest_line -lt $snapshot_sync_line ]] || {
  printf 'rollback snapshot is not synchronized after its checksum manifest\n' >&2
  exit 1
}

rollback_pending_move_line=$(grep -n '^    mv -Tf "$marker" "$pending"' "$CUTOVER" \
  | cut -d: -f1)
rollback_pending_file_sync_line=$(grep -n '^    sync "$pending"' "$CUTOVER" \
  | cut -d: -f1)
rollback_pending_dir_sync_line=$(grep -n '^    sync -f "$evidence_dir"' "$CUTOVER" \
  | head -1 | cut -d: -f1)
((rollback_pending_move_line < rollback_pending_file_sync_line \
  && rollback_pending_file_sync_line < rollback_pending_dir_sync_line)) || {
  printf 'rollback can start before PASSED invalidation is durably synchronized\n' >&2
  exit 1
}

restore_manifest_extract_line=$(grep -n \
  "expected_manifest_sha=.*'\\.rollback_manifest_sha256'" "$CUTOVER" | cut -d: -f1)
restore_manifest_compare_line=$(grep -n \
  'actual_manifest_sha == "\$expected_manifest_sha"' "$CUTOVER" | cut -d: -f1)
restore_first_stop_line=$(grep -n '^  systemctl stop "\$REFERENCE_UPLOAD_TIMER" "\$MARKET_UPLOAD_TIMER"$' \
  "$CUTOVER" | cut -d: -f1)
((restore_manifest_extract_line < restore_manifest_compare_line \
  && restore_manifest_compare_line < restore_first_stop_line)) || {
  printf 'restore_legacy can mutate runtime before validating rollback manifest lineage\n' >&2
  exit 1
}

active_target_line=$(grep -n '^    active_target=$(readlink -f -- "\$ACTIVE_BINARY")$' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
active_target_guard_line=$(grep -n \
  'active_target == "\$RELEASE_ROOT"/\*/polymarket-raw-ops' "$CUTOVER" | tail -1 | cut -d: -f1)
active_rm_line=$(grep -n '^    rm -f "\$ACTIVE_BINARY"$' "$CUTOVER" | tail -1 | cut -d: -f1)
active_link_line=$(grep -n '^ln -s "\$candidate_binary" "\$temporary_link"$' "$CUTOVER" \
  | cut -d: -f1)
((active_target_line < active_target_guard_line \
  && active_target_guard_line < active_rm_line \
  && active_rm_line < active_link_line)) || {
  printf 'cutover no longer proves active Rust symlink lineage before replacement\n' >&2
  exit 1
}

if grep -Eq 'rm -[^[:space:]]* .*state\.json|unlink .*state\.json' "$CUTOVER"; then
  printf 'cutover explicitly deletes rollback state JSON\n' >&2
  exit 1
fi
if grep -Eq 'rm -[^[:space:]]* .*/data/monday/spool/polymarket-reference|unlink .*/data/monday/spool/polymarket-reference' \
  "$CUTOVER"; then
  printf 'cutover explicitly deletes the production spool\n' >&2
  exit 1
fi

automatic_prepare_line=$(grep -n \
  '^    prepare_rollback_evidence "$evidence_dir" || {' "$CUTOVER" | cut -d: -f1)
automatic_restore_line=$(grep -n '^    restore_legacy "$evidence_dir" >/dev/null$' "$CUTOVER" \
  | cut -d: -f1)
automatic_finalize_line=$(grep -n \
  '^      finalize_rollback_evidence "$evidence_dir" invalid || status=1$' "$CUTOVER" \
  | cut -d: -f1)
automatic_failure_exit_line=$(grep -n '^  exit "$status"$' "$CUTOVER" | cut -d: -f1)
((automatic_prepare_line < automatic_restore_line \
  && automatic_restore_line < automatic_finalize_line \
  && automatic_finalize_line < automatic_failure_exit_line)) || {
  printf 'automatic rollback does not revoke success before restore and finalize evidence\n' >&2
  exit 1
}

manual_prepare_line=$(grep -n \
  'prepare_rollback_evidence "$rollback_evidence"' "$CUTOVER" | tail -1 | cut -d: -f1)
manual_restore_line=$(grep -n '^  restore_legacy "$rollback_evidence" >/dev/null$' "$CUTOVER" \
  | cut -d: -f1)
manual_finalize_line=$(grep -n \
  '^  finalize_rollback_evidence "$rollback_evidence" rolled-back' "$CUTOVER" \
  | cut -d: -f1)
manual_print_line=$(grep -n '^  printf '\''%s\\n'\'' "$rollback_evidence"$' "$CUTOVER" \
  | cut -d: -f1)
manual_exit_line=$(grep -n '^  exit 0$' "$CUTOVER" | head -1 | cut -d: -f1)
((manual_prepare_line < manual_restore_line \
  && manual_restore_line < manual_finalize_line \
  && manual_finalize_line < manual_print_line \
  && manual_print_line < manual_exit_line)) || {
  printf 'manual rollback does not revoke success before restore and finalize evidence\n' >&2
  exit 1
}

market_upload_line=$(grep -n '^market_upload_json=' "$GATE" | cut -d: -f1)
baseline_final_line=$(grep -n 'verify_baseline_identity' "$GATE" \
  | tail -1 | cut -d: -f1)
oss_final_line=$(grep -n 'verify_current_oss_config' "$GATE" | tail -1 | cut -d: -f1)
((baseline_final_line > market_upload_line && oss_final_line > market_upload_line)) || {
  printf 'gate does not revalidate baseline identity and OSS config after both uploads\n' >&2
  exit 1
}
grep -Fq 'cd artifact' "$WORKFLOW"
if grep -Fq 'sha256sum artifact/polymarket-raw-ops' "$WORKFLOW"; then
  printf 'workflow checksum still embeds the stripped artifact directory\n' >&2
  exit 1
fi
extract_bundle_assets() {
  sed -n '/^[[:space:]]*readonly -a BUNDLE_ASSETS=($/,/^[[:space:]]*)$/p' "$1" \
    | sed '1d;$d;s/^[[:space:]]*//'
}
workflow_assets=$(sed -n \
  '/^[[:space:]]*control_assets=($/,/^[[:space:]]*)$/p' "$WORKFLOW" \
  | sed '1d;$d;s/^[[:space:]]*//')
gate_assets=$(extract_bundle_assets "$GATE")
cutover_assets=$(extract_bundle_assets "$CUTOVER")
[[ -n $workflow_assets && $workflow_assets == "$gate_assets" \
  && $workflow_assets == "$cutover_assets" ]] || {
  printf 'ACR artifact and release-control bundle asset lists differ\n' >&2
  exit 1
}
grep -Fq '@${{ steps.build.outputs.digest }}' "$WORKFLOW"
if grep -F 'IMAGE:' "$WORKFLOW" | grep -Fq ':${{ github.sha }}'; then
  printf 'workflow still extracts the collector from a mutable SHA tag\n' >&2
  exit 1
fi
grep -Fq 'monday.polymarket_raw_ops_release.v1' "$WORKFLOW"
grep -Fq '> polymarket-raw-ops-release.json' "$WORKFLOW"
grep -Fq "jq -er '.source_revision' polymarket-raw-ops-release.json" "$WORKFLOW"
grep -Fq "jq -er '.control_manifest.sha256' polymarket-raw-ops-release.json" "$WORKFLOW"
grep -Fq "jq -er '.control_archive.sha256' polymarket-raw-ops-release.json" "$WORKFLOW"
workflow_candidate_line=$(grep -n '^            candidate_sha=' "$WORKFLOW" | cut -d: -f1)
workflow_manifest_line=$(grep -n '^              > polymarket-raw-ops-release.json$' "$WORKFLOW" \
  | cut -d: -f1)
workflow_sidecar_line=$(grep -n '^              > source-revision.txt$' "$WORKFLOW" \
  | cut -d: -f1)
((workflow_candidate_line < workflow_manifest_line \
  && workflow_manifest_line < workflow_sidecar_line)) || {
  printf 'workflow publishes release sidecars before the immutable manifest exists\n' >&2
  exit 1
}
grep -Fq 'stage_command="$source_tree/deployment/aliyun/polymarket-raw-ops-cutover.sh"' \
  "$README"
grep -Fq 'candidate_dir=$(sudo "$stage_command" stage "$artifact_dir" "$source_revision")' \
  "$README"
grep -Fq 'gate_control="$candidate_dir/polymarket-raw-ops-gate-control.sh"' "$README"
grep -Fq 'sudo "$gate_control" install' "$README"
grep -Fq 'gate_status=$(sudo "$gate_control" start' "$README"
grep -Fq 'gate_terminal=$(sudo "$gate_control" status "$candidate_sha" "$gate_invocation")' \
  "$README"
grep -Fq "jq -e '.terminal_state == \"passed\"'" "$README"
grep -Fq 'pinned_control_dir="/opt/monday/releases/polymarket-raw-ops/$candidate_sha/control"' \
  "$README"
if grep -Fq 'polymarket-raw-ops-shadow-gate.sh" \
  "$artifact_dir/polymarket-raw-ops"' "$README"; then
  printf 'README bypasses the supervised Gate controller\n' >&2
  exit 1
fi
if grep -Fq 'deployment/aliyun/polymarket-reference-collector.service' "$README"; then
  printf 'README installs the production unit from a mutable checkout\n' >&2
  exit 1
fi
if grep -Fq ':(exclude,glob)deployment/aliyun/polymarket-raw-ops-' "$CI_WORKFLOW"; then
  printf 'Rust-only CI excludes an entire migration-control script\n' >&2
  exit 1
fi
grep -Fq -- '--allow-match-regex "${legacy_runtime_reference_allowlist}"' "$CI_WORKFLOW"
grep -Fq 'deployment/aliyun/polymarket-raw-ops-cutover[.]sh:' "$CI_WORKFLOW"
legacy_test_reference_allowlist=$(sed -n \
  "s/^[[:space:]]*legacy_runtime_reference_allowlist='\(.*\)'$/\1/p" "$CI_WORKFLOW")
[[ -n $legacy_test_reference_allowlist ]] || {
  printf 'Rust Fast Gates has no legacy-runtime reference allowlist\n' >&2
  exit 1
}
legacy_test_reference=$(grep -n -Fx \
  "  LEGACY_EXEC='/usr/bin/pyth""on3 /opt/monday/bin/polymarket_reference_collector.py'" "$0")
legacy_test_reference="deployment/aliyun/test-polymarket-raw-ops-control-plane.sh:$legacy_test_reference"
printf '%s\n' \
  "$legacy_test_reference" \
  | grep -E -x "$legacy_test_reference_allowlist" >/dev/null || {
    printf 'Rust Fast Gates rejects the control-plane identity fixture\n' >&2
    exit 1
  }

grep -Fq 'candidate CLI digest differs from the verified release manifest' "$GATE"
grep -Fq 'source CLI revision differs from the verified release manifest' "$GATE"
gate_final_binding_line=$(grep -n '^  verify_release_binding "\$pinned_release_manifest"' "$GATE" \
  | tail -1 | cut -d: -f1)
gate_marker_line=$(grep -n '^  pass_ready_marker="\$evidence_dir/.PASSED.sha256.ready"$' "$GATE" \
  | cut -d: -f1)
gate_marker_sync_line=$(grep -n '^  sync "\$pass_ready_marker"$' "$GATE" | cut -d: -f1)
gate_marker_dir_sync_line=$(grep -n '^  sync -f "\$evidence_dir"$' "$GATE" \
  | tail -1 | cut -d: -f1)
((gate_final_binding_line < gate_marker_line \
  && gate_marker_line < gate_marker_sync_line \
  && gate_marker_sync_line < gate_marker_dir_sync_line)) || {
  printf 'gate stages success before revalidating the immutable release binding\n' >&2
  exit 1
}
cutover_binding_line=$(grep -n '^verify_release_binding "\$RELEASE_MANIFEST"' "$CUTOVER" \
  | head -1 | cut -d: -f1)
cutover_transition_line=$(grep -n '^transition_started=true$' "$CUTOVER" | cut -d: -f1)
((cutover_binding_line < cutover_transition_line)) || {
  printf 'cutover mutates runtime before revalidating the immutable release binding\n' >&2
  exit 1
}
rust_baseline_verify_line=$(grep -n '^  verify_rust_runtime "\$gate_baseline_release_path"' \
  "$CUTOVER" | head -1 | cut -d: -f1)
rust_snapshot_line=$(grep -n '^snapshot_legacy "\$rollback_dir" "\$baseline_mode"' \
  "$CUTOVER" | cut -d: -f1)
((rust_baseline_verify_line < rust_snapshot_line \
  && rust_snapshot_line < cutover_transition_line)) || {
  printf 'Rust cutover snapshots or mutates before proving the gated baseline\n' >&2
  exit 1
}
candidate_link_line=$(grep -n '^ln -s "\$candidate_binary" "\$temporary_link"$' "$CUTOVER" | cut -d: -f1)
candidate_controls_line=$(grep -n '^install_control_release "\$SCRIPT_DIR"$' "$CUTOVER" | cut -d: -f1)
candidate_units_line=$(grep -n '^for asset in "\${UNIT_ASSETS\[@\]}"; do$' "$CUTOVER" | tail -1 | cut -d: -f1)
((candidate_link_line < candidate_controls_line && candidate_controls_line < candidate_units_line)) || {
  printf 'candidate release, controls, and units are not installed in the governed order\n' >&2
  exit 1
}
grep -Fq '.control_modes[$asset]=$mode' "$CUTOVER"
grep -Fq '.active_symlink={target:$path,sha256:$sha}' "$CUTOVER"
grep -Fq 'verify_control_release "$CONTROL_DIR" "$rollback_sha" "$active_target"' "$CUTOVER"
cutover_final_binding_line=$(grep -n '^verify_release_binding "\$RELEASE_MANIFEST"' "$CUTOVER" \
  | tail -1 | cut -d: -f1)
cutover_success_marker_line=$(grep -n '^success_marker="\$evidence_dir/PASSED.sha256"$' \
  "$CUTOVER" | cut -d: -f1)
((cutover_final_binding_line < cutover_success_marker_line)) || {
  printf 'cutover publishes success without final immutable release revalidation\n' >&2
  exit 1
}

gate_upload_env_chain_line=$(grep -n '^  "\$RELEASE_ROOT" /etc/monday ' "$GATE" \
  | head -1 | cut -d: -f1)
gate_upload_env_read_line=$(grep -n '^secure_control_file "\$UPLOAD_ENV"$' "$GATE" \
  | cut -d: -f1)
((gate_upload_env_chain_line < gate_upload_env_read_line)) || {
  printf 'gate reads the upload environment before validating its trusted parent chain\n' >&2
  exit 1
}

printf 'Polymarket raw-ops control-plane tests passed\n'
