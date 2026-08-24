#!/usr/bin/env bash
# shellcheck disable=SC2016 # Stub scripts intentionally expand only when executed.
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
CUTOVER="$SCRIPT_DIR/binance-fee-cutover.sh"
WORKFLOW="$SCRIPT_DIR/../../.github/workflows/acr-publish.yml"
CI_WORKFLOW="$SCRIPT_DIR/../../.github/workflows/ci.yml"
TAR_BIN=$(command -v gtar || command -v tar)
if "$TAR_BIN" --help 2>/dev/null | grep -q -- '--sort'; then
  TAR_CREATE_OPTS=(--sort=name --mtime='UTC 1970-01-01' --owner=0 --group=0 --numeric-owner)
else
  TAR_CREATE_OPTS=()
fi

for command in awk bash cmp date find grep id install jq mktemp readlink rm sed sha256sum stat tar; do
  command -v "$command" >/dev/null 2>&1 \
    || { printf 'missing test dependency: %s\n' "$command" >&2; exit 2; }
done

tmp_dir=$(readlink -f "$(mktemp -d)")
cleanup() {
  chmod -R u+w "$tmp_dir" 2>/dev/null || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

grep -Fq 'monday.binance_fee_cutover.v1' "$CUTOVER"
grep -Fq 'binance-fee-production-control-assets.sha256' "$CUTOVER"
grep -Fq 'FAILED.sha256' "$CUTOVER"
grep -Fq 'credential JSON must contain exactly runtime_account_id/api_key/secret' "$CUTOVER"
grep -Fq 'binance-fee-cutover.sh' "$WORKFLOW"
grep -Fq 'test-binance-fee-cutover.sh' "$CI_WORKFLOW"

sha_file() {
  sha256sum "$1" | awk '{print $1}'
}

file_uid() {
  local path=$1
  case $(/usr/bin/uname -s) in
    Darwin) /usr/bin/stat -f %u "$path" ;;
    *) /usr/bin/stat -c %u "$path" ;;
  esac
}

write_file() {
  local path=$1
  shift
  install -d -m 0755 "${path%/*}"
  cat >"$path" <<EOF
$*
EOF
}

map_test_path() {
  local path=$1 root=$2
  case "$path" in
    /opt/monday/*) printf '%s\n' "$root${path}" ;;
    /etc/monday/*) printf '%s\n' "$root${path}" ;;
    /data/*) printf '%s\n' "$root${path}" ;;
    *) printf '%s\n' "$path" ;;
  esac
}

create_stub_commands() {
  local bin_dir=$1
  install -d -m 0755 "$bin_dir"

  write_file "$bin_dir/id" '#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == -u ]]; then
  printf "0\n"
  exit 0
fi
exec /usr/bin/id "$@"
'
  chmod 0755 "$bin_dir/id"

  write_file "$bin_dir/flock" '#!/usr/bin/env bash
set -euo pipefail
if [[ ${1:-} == -n ]]; then
  shift
fi
if [[ $# -gt 0 ]]; then
  shift
fi
if [[ $# -gt 0 ]]; then
  exec "$@"
fi
exit 0
'
  chmod 0755 "$bin_dir/flock"

  write_file "$bin_dir/mv" '#!/usr/bin/env bash
set -euo pipefail
args=()
destination=
for arg in "$@"; do
  destination=$arg
  [[ $arg == -T || $arg == -Tf ]] || args+=("$arg")
done
if [[ -n ${MONDAY_TEST_FAIL_MV_DEST:-} && $destination == *"$MONDAY_TEST_FAIL_MV_DEST" ]]; then
  exit 1
fi
exec /bin/mv "${args[@]}"
'
  chmod 0755 "$bin_dir/mv"

  write_file "$bin_dir/mountpoint" '#!/usr/bin/env bash
set -euo pipefail
if [[ ${MONDAY_TEST_MOUNTPOINT_FAIL:-0} == 1 ]]; then
  exit 1
fi
if [[ ${1:-} == -q && ${2:-} == "$MONDAY_ROOT_PREFIX/data" ]]; then
  exit 0
fi
exit 1
'
  chmod 0755 "$bin_dir/mountpoint"

  write_file "$bin_dir/stat" '#!/usr/bin/env bash
set -euo pipefail
if [[ $(/usr/bin/uname -s) == Darwin && ${1:-} == -c ]]; then
  format=$2
  shift 2
  [[ ${1:-} != -- ]] || shift
  case "$format" in
    %u) /usr/bin/stat -f %u "$1" ;;
    %a) /usr/bin/stat -f %Lp "$1" ;;
    *) exit 2 ;;
  esac
  exit 0
fi
exec /usr/bin/stat "$@"
'
  chmod 0755 "$bin_dir/stat"

  write_file "$bin_dir/systemd-tmpfiles" '#!/usr/bin/env bash
set -euo pipefail
[[ ${1:-} == --create ]] || exit 2
conf=$2
[[ -f $conf ]] || exit 1
target=$(awk "NR == 1 {print \$2}" "$conf")
target="${MONDAY_ROOT_PREFIX}${target}"
mkdir -p "$target"
'
  chmod 0755 "$bin_dir/systemd-tmpfiles"

  write_file "$bin_dir/aliyun" '#!/usr/bin/env bash
set -euo pipefail
[[ ${1:-} == ossutil && ${2:-} == cp ]] || exit 2
src=$3
dst=$4
if [[ -n ${MONDAY_TEST_FAIL_OSS_CP_URI_FRAGMENT:-} \
  && "$src $dst" == *"$MONDAY_TEST_FAIL_OSS_CP_URI_FRAGMENT"* ]]; then
  exit 1
fi
if [[ $src == oss://* ]]; then
  remote="${FAKE_OSS_ROOT}/${src#oss://}"
  if [[ -n ${MONDAY_TEST_CORRUPT_URI_FRAGMENT:-} && $src == *"$MONDAY_TEST_CORRUPT_URI_FRAGMENT"* && ! -e ${FAKE_OSS_ROOT}/.corrupted ]]; then
    printf "corrupted\n" >"$dst"
    : >"${FAKE_OSS_ROOT}/.corrupted"
    exit 0
  fi
  cp "$remote" "$dst"
else
  remote="${FAKE_OSS_ROOT}/${dst#oss://}"
  mkdir -p "${remote%/*}"
  cp "$src" "$remote"
fi
'
  chmod 0755 "$bin_dir/aliyun"

  write_file "$bin_dir/tar" '#!/usr/bin/env bash
set -euo pipefail
if [[ -n ${MONDAY_TEST_MUTATE_ORIGINAL_ARTIFACT:-} \
  && ! -e ${MONDAY_TEST_MUTATE_ORIGINAL_ARTIFACT}/.mutated ]]; then
  printf "replaced after validation staging\n" \
    >"${MONDAY_TEST_MUTATE_ORIGINAL_ARTIFACT}/binance-fee-snapshot"
  : >"${MONDAY_TEST_MUTATE_ORIGINAL_ARTIFACT}/.mutated"
fi
exec /usr/bin/tar "$@"
'
  chmod 0755 "$bin_dir/tar"

  write_file "$bin_dir/systemctl" '#!/usr/bin/env bash
set -euo pipefail
state_root="$MONDAY_ROOT_PREFIX/state/systemd"
mkdir -p "$state_root"

unit_path() {
  printf "%s/%s\n" "$state_root" "$1"
}

get_state() {
  local unit=$1 key=$2 file
  file="$(unit_path "$unit").$key"
  [[ -f $file ]] && cat "$file" || true
}

set_state() {
  local unit=$1 key=$2 value=$3
  printf "%s\n" "$value" >"$(unit_path "$unit").$key"
}

map_path() {
  local value=$1 cred_dir=$2
  value=${value//%d/$cred_dir}
  value=${value//\/opt\/monday/$MONDAY_ROOT_PREFIX\/opt\/monday}
  value=${value//\/etc\/monday/$MONDAY_ROOT_PREFIX\/etc\/monday}
  value=${value//\/data/$MONDAY_ROOT_PREFIX\/data}
  printf "%s\n" "$value"
}

run_service() {
  local unit=$1 unit_file exec_line cred_line cred_dir env_line raw_path path status
  unit_file="$MONDAY_ROOT_PREFIX/etc/systemd/system/$unit"
  [[ -f $unit_file ]] || exit 1
  if [[ ${MONDAY_TEST_FAIL_UNIT:-} == "$unit" ]]; then
    set_state "$unit" result failed
    set_state "$unit" exec 1
    set_state "$unit" active inactive
    exit 1
  fi
  cred_line=$(awk -F= "/^LoadCredential=/{print \$2}" "$unit_file")
  cred_dir="$MONDAY_ROOT_PREFIX/run/credentials/${unit%.service}"
  mkdir -p "$cred_dir"
  if [[ -n $cred_line ]]; then
    raw_path=${cred_line#*:}
    cp "$(map_path "$raw_path" "$cred_dir")" "$cred_dir/binance-account.json"
  fi
  while IFS= read -r env_line; do
    [[ -n $env_line ]] || continue
    case "$env_line" in
      EnvironmentFile=*)
        path=$(map_path "${env_line#EnvironmentFile=}" "$cred_dir")
        set -a
        . "$path"
        set +a
        ;;
      Environment=*)
        export "${env_line#Environment=}"
        ;;
    esac
  done < <(grep -E "^(Environment|EnvironmentFile)=" "$unit_file" || true)
  exec_line=$(awk -F= "/^ExecStart=/{print \$2}" "$unit_file")
  exec_line=$(map_path "$exec_line" "$cred_dir")
  if bash -lc "$exec_line"; then
    status=0
    set_state "$unit" result success
  else
    status=$?
    set_state "$unit" result failed
  fi
  set_state "$unit" exec "$status"
  set_state "$unit" active inactive
  exit "$status"
}

cmd=$1
shift
case "$cmd" in
  daemon-reload)
    if [[ ${MONDAY_TEST_FAIL_DAEMON_RELOAD_ONCE:-0} == 1 \
      && ! -e $state_root/.daemon-reload-failed ]]; then
      : >"$state_root/.daemon-reload-failed"
      exit 1
    fi
    exit 0
    ;;
  stop)
    for unit in "$@"; do
      if [[ ${MONDAY_TEST_FAIL_STOP_UNIT:-} == "$unit" ]]; then
        set_state "$unit" active active
        exit 1
      fi
      set_state "$unit" active inactive
    done
    exit 0
    ;;
  disable)
    for unit in "$@"; do
      set_state "$unit" enabled disabled
      set_state "$unit" active inactive
    done
    exit 0
    ;;
  enable)
    start_now=0
    if [[ ${1:-} == --now ]]; then
      start_now=1
      shift
    fi
    for unit in "$@"; do
      if [[ ${MONDAY_TEST_DISABLED_TIMER:-} == "$unit" ]]; then
        set_state "$unit" enabled disabled
      else
        set_state "$unit" enabled enabled
      fi
      if (( start_now )); then
        if [[ ${MONDAY_TEST_INACTIVE_TIMER:-} == "$unit" ]]; then
          set_state "$unit" active inactive
        else
          set_state "$unit" active active
        fi
      fi
    done
    exit 0
    ;;
  start)
    unit=$1
    if [[ $unit == *.timer ]]; then
      set_state "$unit" active active
      exit 0
    fi
    run_service "$unit"
    ;;
  is-active)
    quiet=0
    if [[ ${1:-} == --quiet ]]; then
      quiet=1
      shift
    fi
    unit=$1
    if [[ $(get_state "$unit" active) == active ]]; then
      (( quiet )) || printf "active\n"
      exit 0
    fi
    (( quiet )) || printf "inactive\n"
    exit 3
    ;;
  is-enabled)
    quiet=0
    if [[ ${1:-} == --quiet ]]; then
      quiet=1
      shift
    fi
    unit=$1
    if [[ $(get_state "$unit" enabled) == enabled ]]; then
      (( quiet )) || printf "enabled\n"
      exit 0
    fi
    (( quiet )) || printf "disabled\n"
    exit 1
    ;;
  show)
    unit=$1
    shift
    [[ ${1:-} == --property=* ]] || exit 2
    property=${1#--property=}
    [[ ${2:-} == --value ]] || exit 2
    case "$property" in
      Result) printf "%s\n" "$(get_state "$unit" result)" ;;
      ExecMainStatus) printf "%s\n" "$(get_state "$unit" exec)" ;;
      *) exit 2 ;;
    esac
    ;;
  *) exit 2 ;;
esac
'
  chmod 0755 "$bin_dir/systemctl"
}

create_snapshot_binary() {
  local path=$1
  install -d -m 0755 "${path%/*}"
  cat >"$path" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
market=
symbol=
output_root=
credential=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --market) market=$2; shift 2 ;;
    --symbol) symbol=$2; shift 2 ;;
    --output-root) output_root=$2; shift 2 ;;
    --account-secret-file) credential=$2; shift 2 ;;
    *) shift ;;
  esac
done
api_key=$(jq -r .api_key "$credential")
runtime_account_id=$(jq -r .runtime_account_id "$credential")
account=$(printf "%s" "$api_key" | sha256sum | awk "{print \$1}")
case "$market" in
  spot) batch=1111111111111111111 ;;
  usdm) batch=2222222222222222222 ;;
  *) exit 2 ;;
esac
dir="$output_root/lake/raw/venue=binance_${market}/dataset=fee/account=${account}/date=2026-08-24/hour=00/batch=${batch}"
mkdir -p "$dir"
jq -n \
  --arg market "$market" \
  --arg symbol "$symbol" \
  --arg runtime_account_id "$runtime_account_id" \
  --arg account_fingerprint "$account" \
  '{
     schema:"binance.fee-snapshot.v2",
     venue:"binance",
     market:$market,
     symbol:$symbol,
     runtime_account_id:$runtime_account_id,
     account_fingerprint:$account_fingerprint,
     maker_fee_bps:{buy:"1",sell:"1"},
     taker_fee_bps:{buy:"2",sell:"2"},
     calculation:"test",
     source_endpoint:"/test",
     instrument_rules:null,
     rules_source_endpoint:null,
     requested_at:"2026-08-24T00:00:00Z",
     received_at:"2026-08-24T00:00:00Z"
   }' >"$dir/fee.json"
data_sha=$(sha256sum "$dir/fee.json" | awk "{print \$1}")
bytes=$(wc -c <"$dir/fee.json" | tr -d "[:space:]")
jq -n \
  --arg market "$market" \
  --arg symbol "$symbol" \
  --arg runtime_account_id "$runtime_account_id" \
  --arg account_fingerprint "$account" \
  --arg data_sha "$data_sha" \
  --argjson bytes "$bytes" \
  '{
     schema:"binance.fee-artifact-manifest.v2",
     data_schema:"binance.fee-snapshot.v2",
     venue:"binance",
     market:$market,
     symbol:$symbol,
     runtime_account_id:$runtime_account_id,
     account_fingerprint:$account_fingerprint,
     file:"fee.json",
     bytes:$bytes,
     sha256:$data_sha,
     received_at:"2026-08-24T00:00:00Z"
   }' >"$dir/fee.json.manifest.json"
printf "%s\n" "$data_sha" >"$dir/fee.json._SUCCESS"
EOF
  chmod 0755 "$path"
}

create_upload_binary() {
  local path=$1
  install -d -m 0755 "${path%/*}"
  cat >"$path" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output_root=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --output-root) output_root=$2; shift 2 ;;
    *) shift ;;
  esac
done
last_prefix=
last_data=
last_manifest=
while IFS= read -r data_file; do
  dir=${data_file%/fee.json}
  prefix=${dir#"$output_root"/}
  remote="$FAKE_OSS_ROOT/$OSS_BUCKET/$prefix"
  mkdir -p "$remote"
  cp "$dir/fee.json" "$remote/fee.json"
  cp "$dir/fee.json.manifest.json" "$remote/fee.json.manifest.json"
  cp "$dir/fee.json._SUCCESS" "$remote/fee.json._SUCCESS"
  last_prefix=$prefix
  last_data=$(sha256sum "$dir/fee.json" | awk "{print \$1}")
  last_manifest=$(sha256sum "$dir/fee.json.manifest.json" | awk "{print \$1}")
done < <(find "$output_root/lake/raw" -type f -name fee.json | sort)
jq -n \
  --arg now "2026-08-24T00:01:00Z" \
  --arg prefix "$last_prefix" \
  --arg data_sha "$last_data" \
  --arg manifest_sha "$last_manifest" \
  '{
     updated_at:$now,
     last_success_at:$now,
     last_error_at:null,
     last_error:null,
     failure_count:0,
     pending_batches:0,
     uploaded_batches:2,
     retried_batches:0,
     failed_batches:[],
     discovery_failed:false,
     last_uploaded_triplet:{
       object_prefix:$prefix,
       data_sha256:$data_sha,
       manifest_sha256:$manifest_sha,
       success_sha256:$data_sha
     }
   }' >"$output_root/upload-status.json"
EOF
  chmod 0755 "$path"
}

create_artifact_bundle() {
  local artifact_dir=$1
  local source_revision=$2
  local control_dir="$artifact_dir/control"
  install -d -m 0755 "$artifact_dir" "$control_dir"
  create_snapshot_binary "$artifact_dir/binance-fee-snapshot"
  create_upload_binary "$artifact_dir/binance-fee-snapshot-upload"
  (
    cd "$artifact_dir"
    sha256sum binance-fee-snapshot >binance-fee-snapshot.sha256
    sha256sum binance-fee-snapshot-upload >binance-fee-snapshot-upload.sha256
  )
  for asset in \
    binance-fee-cutover.sh \
    binance-fee-snapshot-spot.service \
    binance-fee-snapshot-spot.timer \
    binance-fee-snapshot-usdm.service \
    binance-fee-snapshot-usdm.timer \
    binance-fee-upload.service \
    binance-fee-upload.timer \
    binance-fee-upload.env \
    binance-fee.conf \
    monday-collector-health.sh; do
    cp "$SCRIPT_DIR/$asset" "$control_dir/$asset"
  done
  (
    cd "$control_dir"
    sha256sum \
      binance-fee-cutover.sh \
      binance-fee-snapshot-spot.service \
      binance-fee-snapshot-spot.timer \
      binance-fee-snapshot-usdm.service \
      binance-fee-snapshot-usdm.timer \
      binance-fee-upload.service \
      binance-fee-upload.timer \
      binance-fee-upload.env \
      binance-fee.conf \
      monday-collector-health.sh >"$artifact_dir/binance-fee-production-control-assets.sha256"
    "$TAR_BIN" "${TAR_CREATE_OPTS[@]}" \
      -czf "$artifact_dir/binance-fee-production-control.tar.gz" \
      binance-fee-cutover.sh \
      binance-fee-snapshot-spot.service \
      binance-fee-snapshot-spot.timer \
      binance-fee-snapshot-usdm.service \
      binance-fee-snapshot-usdm.timer \
      binance-fee-upload.service \
      binance-fee-upload.timer \
      binance-fee-upload.env \
      binance-fee.conf \
      monday-collector-health.sh
  )
  (
    cd "$artifact_dir"
    sha256sum binance-fee-production-control.tar.gz >binance-fee-production-control.tar.gz.sha256
    jq -n \
      --arg source_revision "$source_revision" \
      --arg candidate_sha256 "$(awk "NR == 1 {print \$1}" binance-fee-snapshot.sha256)" \
      --arg uploader_sha256 "$(awk "NR == 1 {print \$1}" binance-fee-snapshot-upload.sha256)" \
      --arg control_manifest_sha256 "$(sha256sum binance-fee-production-control-assets.sha256 | awk "{print \$1}")" \
      --arg control_archive_sha256 "$(awk "NR == 1 {print \$1}" binance-fee-production-control.tar.gz.sha256)" \
      '{
         schema:"monday.binance_fee_release.v1",
         source_revision:$source_revision,
         candidate:{file:"binance-fee-snapshot",sha256:$candidate_sha256},
         uploader:{file:"binance-fee-snapshot-upload",sha256:$uploader_sha256},
         control_manifest:{file:"binance-fee-production-control-assets.sha256",sha256:$control_manifest_sha256},
         control_archive:{file:"binance-fee-production-control.tar.gz",sha256:$control_archive_sha256}
       }' >binance-fee-release.json
    sha256sum binance-fee-release.json >binance-fee-release.json.sha256
  )
}

create_root_layout() {
  local fixture=$1
  install -d -m 0755 \
    "$fixture/root/data" \
    "$fixture/root/etc/monday/credentials" \
    "$fixture/root/etc/systemd/system" \
    "$fixture/root/etc/tmpfiles.d" \
    "$fixture/root/opt/monday/bin" \
    "$fixture/root/opt/monday/releases/binance-fee" \
    "$fixture/oss"
  printf '%s\n' '{"runtime_account_id":"desk/main","api_key":"key","secret":"secret"}' \
    >"$fixture/root/etc/monday/credentials/binance-account.json"
  chmod 0600 "$fixture/root/etc/monday/credentials/binance-account.json"
}

make_partial_baseline() {
  local fixture=$1
  local release="$fixture/root/opt/monday/releases/binance-fee/old"
  install -d -m 0755 "$release"
  printf '#!/usr/bin/env bash\nexit 0\n' >"$release/binance-fee-snapshot"
  printf '#!/usr/bin/env bash\nexit 0\n' >"$release/binance-fee-snapshot-upload"
  chmod 0755 "$release/binance-fee-snapshot" "$release/binance-fee-snapshot-upload"
  ln -s "$release/binance-fee-snapshot" "$fixture/root/opt/monday/bin/binance-fee-snapshot"
  ln -s "$release/binance-fee-snapshot-upload" "$fixture/root/opt/monday/bin/binance-fee-snapshot-upload"
  cp "$SCRIPT_DIR/binance-fee-snapshot-spot.service" "$fixture/root/etc/systemd/system/binance-fee-snapshot-spot.service"
  cp "$SCRIPT_DIR/binance-fee-snapshot-spot.timer" "$fixture/root/etc/systemd/system/binance-fee-snapshot-spot.timer"
  cp "$SCRIPT_DIR/binance-fee-snapshot-usdm.service" "$fixture/root/etc/systemd/system/binance-fee-snapshot-usdm.service"
  cp "$SCRIPT_DIR/binance-fee-snapshot-usdm.timer" "$fixture/root/etc/systemd/system/binance-fee-snapshot-usdm.timer"
  cp "$SCRIPT_DIR/binance-fee-upload.service" "$fixture/root/etc/systemd/system/binance-fee-upload.service"
  cp "$SCRIPT_DIR/binance-fee-upload.timer" "$fixture/root/etc/systemd/system/binance-fee-upload.timer"
  cp "$SCRIPT_DIR/binance-fee-upload.env" "$fixture/root/etc/monday/binance-fee-upload.env"
  chmod 0640 "$fixture/root/etc/monday/binance-fee-upload.env"
  cp "$SCRIPT_DIR/binance-fee.conf" "$fixture/root/etc/tmpfiles.d/binance-fee.conf"
}

latest_evidence_dir() {
  local fixture=$1
  find "$fixture/root/data/monday/evidence/binance-fee-cutovers" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort | tail -n 1
}

run_cutover() {
  local fixture=$1
  shift
  PATH="$fixture/bin:$PATH" \
  MONDAY_ROOT_PREFIX="$fixture/root" \
  MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
  FAKE_OSS_ROOT="$fixture/oss" \
  "$CUTOVER" "$@"
}

assert_success_receipt() {
  local fixture=$1 expected_baseline=$2 evidence candidate_sha
  evidence=$(latest_evidence_dir "$fixture")
  if [[ -z $evidence ]]; then
    printf 'missing success evidence dir\n' >&2
    cat "$fixture/err" >&2
    exit 1
  fi
  candidate_sha=$(awk 'NR == 1 {print $1}' "$fixture/artifact/binance-fee-snapshot.sha256")
  jq -e --arg baseline "$expected_baseline" --arg candidate "$candidate_sha" '
    .success == true
    and .baseline_mode == $baseline
    and .controller == "fee-test"
    and .candidate_sha256 == $candidate
    and (.local_triplets | length == 2)
    and (.remote_triplets | length == 2)
  ' \
    "$evidence/cutover.json" >/dev/null
  [[ -f $evidence/PASSED.sha256 ]]
}

assert_failure_receipt() {
  local fixture=$1 expected=$2 evidence
  evidence=$(latest_evidence_dir "$fixture")
  if [[ -z $evidence ]]; then
    printf 'missing failure evidence dir\n' >&2
    cat "$fixture/err" >&2
    exit 1
  fi
  jq -e --arg expected "$expected" '
    .success == false and (.failure_reason | tostring | contains($expected))
  ' "$evidence/cutover.json" >/dev/null || {
    cat "$evidence/cutover.json" >&2
    cat "$fixture/err" >&2
    exit 1
  }
  [[ -f $evidence/FAILED.sha256 ]]
}

assert_timers_contained() {
  local fixture=$1 timer enabled active
  for timer in \
    binance-fee-snapshot-spot.timer \
    binance-fee-snapshot-usdm.timer \
    binance-fee-upload.timer; do
    enabled="$fixture/root/state/systemd/$timer.enabled"
    active="$fixture/root/state/systemd/$timer.active"
    [[ ! -f $enabled || $(<"$enabled") == disabled ]]
    [[ ! -f $active || $(<"$active") == inactive ]]
  done
}

test_bad_credential() {
  local fixture="$tmp_dir/bad-credential"
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 1111111111111111111111111111111111111111
  printf '%s\n' '{"runtime_account_id":"desk/main","api_key":"key"}' \
    >"$fixture/root/etc/monday/credentials/binance-account.json"
  chmod 0600 "$fixture/root/etc/monday/credentials/binance-account.json"
  if run_cutover "$fixture" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover accepted an incomplete credential JSON\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'credential JSON must contain exactly runtime_account_id/api_key/secret'
}

test_digest_drift() {
  local fixture="$tmp_dir/digest-drift"
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 2222222222222222222222222222222222222222
  printf '\ncorrupt\n' >>"$fixture/artifact/binance-fee-production-control-assets.sha256"
  if run_cutover "$fixture" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover accepted a drifted control manifest\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'release manifest does not bind the candidate, uploader, and fee control bundle'
}

test_preflight_failure_preserves_reason() {
  local fixture="$tmp_dir/preflight-failure"
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 2929292929292929292929292929292929292929
  if PATH="$fixture/bin:$PATH" \
    MONDAY_ROOT_PREFIX="$fixture/root" \
    MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
    FAKE_OSS_ROOT="$fixture/oss" \
    MONDAY_TEST_MOUNTPOINT_FAIL=1 \
    "$CUTOVER" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover ignored a missing data mount\n' >&2
    exit 1
  fi
  grep -Fq 'must be a mounted filesystem' "$fixture/err"
  ! grep -Fq 'unbound variable' "$fixture/err"
}

test_install_failure_restores_absent_baseline() {
  local fixture="$tmp_dir/install-failure" evidence
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 3030303030303030303030303030303030303031
  if PATH="$fixture/bin:$PATH" \
    MONDAY_ROOT_PREFIX="$fixture/root" \
    MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
    FAKE_OSS_ROOT="$fixture/oss" \
    MONDAY_TEST_FAIL_DAEMON_RELOAD_ONCE=1 \
    "$CUTOVER" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover ignored an install-time daemon-reload failure\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'unhandled command failed'
  [[ ! -e $fixture/root/etc/systemd/system/binance-fee-upload.timer ]]
  [[ ! -L $fixture/root/opt/monday/bin/binance-fee-snapshot ]]
  evidence=$(latest_evidence_dir "$fixture")
  jq -e '.rollback_result == "baseline-restored"' "$evidence/cutover.json" >/dev/null
}

test_absent_success_enables_timers() {
  local fixture="$tmp_dir/absent-success"
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 3030303030303030303030303030303030303030
  run_cutover "$fixture" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err" || {
    cat "$fixture/err" >&2
    exit 1
  }
  [[ $(cat "$fixture/root/state/systemd/binance-fee-snapshot-spot.timer.enabled") == enabled ]]
  [[ $(cat "$fixture/root/state/systemd/binance-fee-snapshot-usdm.timer.enabled") == enabled ]]
  [[ $(cat "$fixture/root/state/systemd/binance-fee-upload.timer.enabled") == enabled ]]
  assert_success_receipt "$fixture" absent
}

test_success_requires_enabled_active_timers() {
  local fixture="$tmp_dir/timer-state-failure"
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 3030303030303030303030303030303030303032
  if PATH="$fixture/bin:$PATH" \
    MONDAY_ROOT_PREFIX="$fixture/root" \
    MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
    FAKE_OSS_ROOT="$fixture/oss" \
    MONDAY_TEST_INACTIVE_TIMER=binance-fee-upload.timer \
    "$CUTOVER" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover accepted an enabled but inactive timer\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'upload timer is not enabled and active'
  assert_timers_contained "$fixture"
}

test_receipt_failure_rolls_back_enabled_timers() {
  local fixture="$tmp_dir/receipt-failure" evidence
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 3030303030303030303030303030303030303033
  if PATH="$fixture/bin:$PATH" \
    MONDAY_ROOT_PREFIX="$fixture/root" \
    MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
    FAKE_OSS_ROOT="$fixture/oss" \
    MONDAY_TEST_FAIL_MV_DEST=PASSED.sha256 \
    "$CUTOVER" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover stayed enabled after its success marker failed\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'terminal success receipt persistence failed'
  assert_timers_contained "$fixture"
  evidence=$(latest_evidence_dir "$fixture")
  [[ ! -e $evidence/PASSED.sha256 ]]
}

test_rollback_stop_failure_is_not_reported_as_restored() {
  local fixture="$tmp_dir/rollback-stop-failure" evidence
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 3030303030303030303030303030303030303034
  if PATH="$fixture/bin:$PATH" \
    MONDAY_ROOT_PREFIX="$fixture/root" \
    MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
    FAKE_OSS_ROOT="$fixture/oss" \
    MONDAY_TEST_FAIL_MV_DEST=PASSED.sha256 \
    MONDAY_TEST_FAIL_STOP_UNIT=binance-fee-snapshot-spot.timer \
    "$CUTOVER" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover ignored a rollback containment failure\n' >&2
    exit 1
  fi
  evidence=$(latest_evidence_dir "$fixture")
  jq -e '.success == false and .rollback_result == "restore-failed"' \
    "$evidence/cutover.json" >/dev/null
  [[ $(<"$fixture/root/state/systemd/binance-fee-snapshot-spot.timer.active") == active ]]
}

test_artifact_swap_after_freeze_does_not_change_release() {
  local fixture="$tmp_dir/artifact-swap" candidate_sha release_sha
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 3131313131313131313131313131313131313131
  candidate_sha=$(awk 'NR == 1 {print $1}' "$fixture/artifact/binance-fee-snapshot.sha256")
  release_sha=$(awk 'NR == 1 {print $1}' "$fixture/artifact/binance-fee-release.json.sha256")
  if ! PATH="$fixture/bin:$PATH" \
    MONDAY_ROOT_PREFIX="$fixture/root" \
    MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
    FAKE_OSS_ROOT="$fixture/oss" \
    MONDAY_TEST_MUTATE_ORIGINAL_ARTIFACT="$fixture/artifact" \
    "$CUTOVER" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    cat "$fixture/err" >&2
    exit 1
  fi
  [[ -f $fixture/artifact/.mutated ]]
  [[ $(sha256sum "$fixture/artifact/binance-fee-snapshot" | awk '{print $1}') != "$candidate_sha" ]]
  [[ $(sha256sum "$fixture/root/opt/monday/releases/binance-fee/$release_sha/binance-fee-snapshot" \
    | awk '{print $1}') == "$candidate_sha" ]]
  assert_success_receipt "$fixture" absent
}

test_market_failure_rolls_back_absent() {
  local fixture="$tmp_dir/market-failure" evidence old_data old_dir
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 3333333333333333333333333333333333333333
  if PATH="$fixture/bin:$PATH" \
    MONDAY_ROOT_PREFIX="$fixture/root" \
    MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
    FAKE_OSS_ROOT="$fixture/oss" \
    MONDAY_TEST_FAIL_UNIT=binance-fee-snapshot-usdm.service \
    "$CUTOVER" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover ignored a USD-M snapshot failure\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'usdm fee snapshot start failed'
  evidence=$(latest_evidence_dir "$fixture")
  jq -e '.rollback_result == "baseline-restored-with-pending-data"' \
    "$evidence/cutover.json" >/dev/null
  [[ ! -e $fixture/root/etc/systemd/system/binance-fee-snapshot-usdm.service ]]
  [[ ! -L $fixture/root/opt/monday/bin/binance-fee-snapshot ]]
  old_data=$(find "$fixture/root/data/monday/spool/binance-fee/lake/raw" \
    -type f -name fee.json -print -quit)
  [[ -n $old_data ]]
  old_dir=${old_data%/fee.json}
  mv "$old_dir" "${old_dir%/*}/batch=0000000000000000001"
  if run_cutover "$fixture" "$fixture/artifact" fee-test \
    >"$fixture/retry.out" 2>"$fixture/retry.err"; then
    printf 'retry uploaded a triplet left by the failed attempt\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'canonical fee spool contains pending triplets before cutover'
  [[ -z $(find "$fixture/oss" -type f -name fee.json -print -quit) ]]
}

test_preexisting_triplet_blocks_uploader() {
  local fixture="$tmp_dir/preexisting-triplet" data_dir old_dir
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 3333333333333333333333333333333333333334
  "$fixture/artifact/binance-fee-snapshot" \
    --market spot \
    --symbol BTCUSDT \
    --output-root "$fixture/root/data/monday/spool/binance-fee" \
    --account-secret-file "$fixture/root/etc/monday/credentials/binance-account.json"
  data_dir=$(find "$fixture/root/data/monday/spool/binance-fee/lake/raw" \
    -type f -name fee.json -print -quit)
  data_dir=${data_dir%/fee.json}
  old_dir="${data_dir%/*}/batch=0000000000000000000"
  mv "$data_dir" "$old_dir"
  if run_cutover "$fixture" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover uploaded a triplet outside the current attempt\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'canonical fee spool contains pending triplets before cutover'
  [[ -f $old_dir/fee.json ]]
  [[ -z $(find "$fixture/oss" -type f -name fee.json -print -quit) ]]
}

test_readback_failure_rolls_back_absent() {
  local fixture="$tmp_dir/readback-failure"
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 4444444444444444444444444444444444444444
  if PATH="$fixture/bin:$PATH" \
    MONDAY_ROOT_PREFIX="$fixture/root" \
    MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
    FAKE_OSS_ROOT="$fixture/oss" \
    MONDAY_TEST_CORRUPT_URI_FRAGMENT='fee.json._SUCCESS' \
    "$CUTOVER" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover accepted a corrupted remote readback\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'remote fee success marker drifted'
  [[ ! -e $fixture/root/etc/monday/binance-fee-upload.env ]]
}

test_unhandled_readback_failure_records_reason() {
  local fixture="$tmp_dir/unhandled-readback-failure"
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 4444444444444444444444444444444444444445
  if PATH="$fixture/bin:$PATH" \
    MONDAY_ROOT_PREFIX="$fixture/root" \
    MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
    FAKE_OSS_ROOT="$fixture/oss" \
    MONDAY_TEST_FAIL_OSS_CP_URI_FRAGMENT=fee.json.manifest.json \
    "$CUTOVER" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover ignored an unhandled OSS readback failure\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'unhandled command failed'
}

test_market_failure_restores_partial_contained() {
  local fixture="$tmp_dir/partial-failure" old_snapshot old_uploader
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 4545454545454545454545454545454545454545
  make_partial_baseline "$fixture"
  old_snapshot=$(readlink "$fixture/root/opt/monday/bin/binance-fee-snapshot")
  old_uploader=$(readlink "$fixture/root/opt/monday/bin/binance-fee-snapshot-upload")
  if PATH="$fixture/bin:$PATH" \
    MONDAY_ROOT_PREFIX="$fixture/root" \
    MONDAY_EXPECTED_ROOT_UID="$(file_uid "$fixture/root/etc/monday/credentials/binance-account.json")" \
    FAKE_OSS_ROOT="$fixture/oss" \
    MONDAY_TEST_FAIL_UNIT=binance-fee-snapshot-usdm.service \
    "$CUTOVER" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err"; then
    printf 'cutover ignored a USD-M snapshot failure on partial baseline\n' >&2
    exit 1
  fi
  assert_failure_receipt "$fixture" 'usdm fee snapshot start failed'
  [[ $(readlink "$fixture/root/opt/monday/bin/binance-fee-snapshot") == "$old_snapshot" ]]
  [[ $(readlink "$fixture/root/opt/monday/bin/binance-fee-snapshot-upload") == "$old_uploader" ]]
  local evidence
  evidence=$(latest_evidence_dir "$fixture")
  jq -e '.baseline_mode == "partial-contained"
    and .rollback_result == "baseline-restored-with-pending-data"' \
    "$evidence/cutover.json" >/dev/null
}

test_partial_contained_success_enables_timers() {
  local fixture="$tmp_dir/partial-success"
  local release_sha
  create_root_layout "$fixture"
  create_stub_commands "$fixture/bin"
  create_artifact_bundle "$fixture/artifact" 5555555555555555555555555555555555555555
  make_partial_baseline "$fixture"
  run_cutover "$fixture" "$fixture/artifact" fee-test >"$fixture/out" 2>"$fixture/err" || {
    cat "$fixture/err" >&2
    exit 1
  }
  release_sha=$(awk 'NR == 1 {print $1}' "$fixture/artifact/binance-fee-release.json.sha256")
  [[ $(readlink "$fixture/root/opt/monday/bin/binance-fee-snapshot") == \
    "$fixture/root/opt/monday/releases/binance-fee/$release_sha/binance-fee-snapshot" ]]
  [[ $(cat "$fixture/root/state/systemd/binance-fee-snapshot-spot.timer.enabled") == enabled ]]
  [[ $(cat "$fixture/root/state/systemd/binance-fee-snapshot-spot.timer.active") == active ]]
  [[ $(cat "$fixture/root/state/systemd/binance-fee-snapshot-usdm.timer.enabled") == enabled ]]
  [[ $(cat "$fixture/root/state/systemd/binance-fee-upload.timer.enabled") == enabled ]]
  assert_success_receipt "$fixture" partial-contained
}

run_case() {
  local name=$1 fn=$2
  if [[ -z ${MONDAY_TEST_CASE_FILTER:-} || ${MONDAY_TEST_CASE_FILTER} == "$name" ]]; then
    "$fn"
  fi
}

run_case bad_credential test_bad_credential
run_case digest_drift test_digest_drift
run_case preflight_failure test_preflight_failure_preserves_reason
run_case install_failure test_install_failure_restores_absent_baseline
run_case absent_success test_absent_success_enables_timers
run_case timer_state_failure test_success_requires_enabled_active_timers
run_case receipt_failure test_receipt_failure_rolls_back_enabled_timers
run_case rollback_stop_failure test_rollback_stop_failure_is_not_reported_as_restored
run_case artifact_swap test_artifact_swap_after_freeze_does_not_change_release
run_case market_failure test_market_failure_rolls_back_absent
run_case preexisting_triplet test_preexisting_triplet_blocks_uploader
run_case readback_failure test_readback_failure_rolls_back_absent
run_case unhandled_readback test_unhandled_readback_failure_records_reason
run_case partial_failure test_market_failure_restores_partial_contained
run_case partial_success test_partial_contained_success_enables_timers

printf '%s\n' 'Binance fee cutover tests passed'
