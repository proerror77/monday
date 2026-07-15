#!/usr/bin/env bash
set -Eeuo pipefail

export LC_ALL=C

readonly CURRENT_FILE_DEFAULT=/etc/monday/hft-trading-current.env
readonly ACTIVATION_ROOT_DEFAULT=/opt/monday/activations
readonly RELEASE_ROOT_DEFAULT=/opt/monday/releases/hft-trading
readonly CONTROL_ROOT_DEFAULT=/opt/monday/control/hft-trading
readonly STATE_ROOT_DEFAULT=/var/lib/monday/hft-trading
readonly SECRET_ROOT_DEFAULT=/run/monday/trading-secrets
readonly CONTAINER_NAME=monday-hft-trading
readonly EXPECTED_ROOT_UID_DEFAULT=0
readonly HFT_RUNTIME_USER_DEFAULT=mondayhft
readonly HFT_RUNTIME_GROUP_DEFAULT=mondayhft
readonly EXPECTED_REGION=ap-northeast-1
readonly EXPECTED_HOST_OS=ubuntu
readonly EXPECTED_HOST_VERSION=26.04
readonly EXPECTED_ARCH=amd64
readonly EXPECTED_RAM_ROLE=MondayTradingEcsRole
readonly HOSTCTL_PROGRAM=/usr/local/sbin/monday-hft-trading-hostctl
readonly UNIT_PATH=/etc/systemd/system/monday-hft-trading.service
readonly SERVICE=monday-hft-trading.service

die() {
  printf 'hft-trading runtime preflight failed: %s\n' "$*" >&2
  exit 1
}

stat_uid() {
  if [[ $(uname -s) == Darwin ]]; then
    stat -f %u -- "$1"
  else
    stat -c %u -- "$1"
  fi
}

stat_gid() {
  if [[ $(uname -s) == Darwin ]]; then
    stat -f %g -- "$1"
  else
    stat -c %g -- "$1"
  fi
}

stat_mode() {
  if [[ $(uname -s) == Darwin ]]; then
    stat -f %Lp -- "$1"
  else
    stat -c %a -- "$1"
  fi
}

secure_regular_file() {
  local path=$1 max_mode=$2 expected_uid=${EXPECTED_ROOT_UID:-$EXPECTED_ROOT_UID_DEFAULT}
  local mode
  [[ -f $path && ! -L $path ]] || return 1
  [[ $(stat_uid "$path") == "$expected_uid" ]] || return 1
  mode=$(stat_mode "$path") || return 1
  (( (8#$mode & ~8#$max_mode) == 0 ))
}

secure_directory() {
  local path=$1 expected_uid=${EXPECTED_ROOT_UID:-$EXPECTED_ROOT_UID_DEFAULT}
  local mode
  [[ -d $path && ! -L $path ]] || return 1
  [[ $(stat_uid "$path") == "$expected_uid" ]] || return 1
  mode=$(stat_mode "$path") || return 1
  (( (8#$mode & 8#022) == 0 ))
}

canonical_directory() {
  local path=$1 canonical
  [[ $path == /* ]] || return 1
  canonical=$(cd -- "$path" 2>/dev/null && pwd -P) || return 1
  [[ $path == "$canonical" ]] || return 1
  printf '%s\n' "$canonical"
}

secure_directory_chain() {
  local path=$1 current=/ component
  local -a components
  [[ $path == /* ]] || return 1
  secure_directory / || return 1
  [[ $path == / ]] && return 0
  IFS=/ read -r -a components <<<"${path#/}"
  for component in "${components[@]}"; do
    [[ -n $component && $component != . && $component != .. ]] || return 1
    current=${current%/}/$component
    secure_directory "$current" || return 1
  done
}

assert_no_service_dropin_directories() {
  local root
  for root in "$@"; do
    [[ ! -e $root/$SERVICE.d ]] || return 1
  done
}

effective_exec_matches() {
  local property=$1 program=$2 argument=$3 value after_open before_close
  value=$(systemctl show "$SERVICE" --property="$property" --value) || return 1
  [[ $value == "{ path=$program ; argv[]=$program $argument ;"* \
    && $value == *'ignore_errors=no'* && $value == *' }' ]] || return 1
  after_open=${value#*\{}
  before_close=${value%\}}
  [[ $after_open != "$value" && $after_open != *'{'* \
    && $before_close != "$value" && $before_close != *'}'* ]]
}

assert_service_static() {
  local state
  state=$(systemctl is-enabled "$SERVICE" 2>/dev/null) || return 1
  [[ $state == static ]]
}

assert_effective_service_contract() {
  local unit_path=$1 runtime_program=$2 fragment dropins restart start_post
  assert_no_service_dropin_directories \
    /etc/systemd/system /run/systemd/system /usr/local/lib/systemd/system \
    /usr/lib/systemd/system /lib/systemd/system || return 1
  fragment=$(systemctl show "$SERVICE" --property=FragmentPath --value) || return 1
  dropins=$(systemctl show "$SERVICE" --property=DropInPaths --value) || return 1
  restart=$(systemctl show "$SERVICE" --property=Restart --value) || return 1
  start_post=$(systemctl show "$SERVICE" --property=ExecStartPost --value) || return 1
  [[ $fragment == "$unit_path" && -z $dropins && $restart == no \
    && -z $start_post ]] || return 1
  assert_service_static || return 1
  effective_exec_matches ExecStartPre "$runtime_program" preflight || return 1
  effective_exec_matches ExecStart "$runtime_program" run || return 1
  effective_exec_matches ExecStop "$runtime_program" stop || return 1
  effective_exec_matches ExecStopPost "$runtime_program" ensure-stopped
}

validate_bare_host_identity_values() {
  local host_os=$1 host_version=$2 architecture=$3 region=$4 role=$5 instance_id=$6
  [[ $host_os == "$EXPECTED_HOST_OS" \
    && $host_version == "$EXPECTED_HOST_VERSION" \
    && $architecture == "$EXPECTED_ARCH" \
    && $region == "$EXPECTED_REGION" \
    && $role == "$EXPECTED_RAM_ROLE" \
    && $instance_id =~ ^i-[a-z0-9]+$ ]]
}

metadata_token() {
  curl -fsS --connect-timeout 2 --max-time 5 -X PUT \
    -H 'X-aliyun-ecs-metadata-token-ttl-seconds: 300' \
    http://100.100.100.200/latest/api/token
}

metadata_get() {
  local token=$1 path=$2
  curl -fsS --connect-timeout 2 --max-time 5 \
    -H "X-aliyun-ecs-metadata-token: $token" \
    "http://100.100.100.200/latest/meta-data/$path"
}

verify_bare_tokyo_host() {
  local token role region instance_id architecture
  # shellcheck disable=SC1091
  source /etc/os-release
  systemctl is-active --quiet kubelet && return 1
  [[ ! -e /var/lib/kubelet/kubeconfig ]] || return 1
  architecture=$(dpkg --print-architecture) || return 1
  token=$(metadata_token) || return 1
  region=$(metadata_get "$token" region-id) || return 1
  role=$(metadata_get "$token" ram/security-credentials/) || return 1
  role=${role%%$'\n'*}
  instance_id=$(metadata_get "$token" instance-id) || return 1
  validate_bare_host_identity_values "${ID:-}" "${VERSION_ID:-}" \
    "$architecture" "$region" "$role" "$instance_id"
}

validate_selected_control_assets() {
  local manifest=$1 runtime_path=$2 hostctl_path=$3 policy_path=$4 unit_path=$5
  local control_names runtime_sha hostctl_sha policy_sha unit_sha
  secure_regular_file "$manifest" 0444 || return 1
  secure_regular_file "$runtime_path" 0555 || return 1
  secure_regular_file "$hostctl_path" 0555 || return 1
  secure_regular_file "$policy_path" 0444 || return 1
  secure_regular_file "$unit_path" 0444 || return 1
  [[ $(stat_mode "$runtime_path") == 555 \
    && $(stat_mode "$hostctl_path") == 555 \
    && $(stat_mode "$policy_path") == 444 \
    && $(stat_mode "$unit_path") == 444 ]] || return 1
  control_names=$(awk '
    NF != 2 || $1 !~ /^[0-9a-f]{64}$/ || $2 !~ /^[A-Za-z0-9._-]+$/ { exit 2 }
    { print $2 }
  ' "$manifest") || return 1
  [[ $control_names == $'hft-trading-ecs.service\ntrading-ecs-hostctl.sh\ntrading-ecs-paper-shadow-policy.jq\ntrading-ecs-runtime.sh' ]] \
    || return 1
  runtime_sha=$(awk '$2 == "trading-ecs-runtime.sh" { print $1 }' "$manifest")
  hostctl_sha=$(awk '$2 == "trading-ecs-hostctl.sh" { print $1 }' "$manifest")
  policy_sha=$(awk '$2 == "trading-ecs-paper-shadow-policy.jq" { print $1 }' "$manifest")
  unit_sha=$(awk '$2 == "hft-trading-ecs.service" { print $1 }' "$manifest")
  [[ $runtime_sha =~ ^[0-9a-f]{64}$ \
    && $hostctl_sha =~ ^[0-9a-f]{64}$ \
    && $policy_sha =~ ^[0-9a-f]{64}$ \
    && $unit_sha =~ ^[0-9a-f]{64}$ ]] || return 1
  [[ $(sha256sum "$runtime_path" | awk '{print $1}') == "$runtime_sha" \
    && $(sha256sum "$hostctl_path" | awk '{print $1}') == "$hostctl_sha" \
    && $(sha256sum "$policy_path" | awk '{print $1}') == "$policy_sha" \
    && $(sha256sum "$unit_path" | awk '{print $1}') == "$unit_sha" ]]
}

runtime_account_ids() {
  local runtime_user=$HFT_RUNTIME_USER_DEFAULT
  local runtime_group=$HFT_RUNTIME_GROUP_DEFAULT
  local passwd_record group_record all_passwd all_groups uid gid home shell
  local group_gid group_members other_name other_uid other_gid
  passwd_record=$(getent passwd "$runtime_user") || return 1
  group_record=$(getent group "$runtime_group") || return 1
  IFS=: read -r _ _ uid gid _ home shell <<<"$passwd_record"
  IFS=: read -r _ _ group_gid group_members <<<"$group_record"
  [[ $uid =~ ^[0-9]+$ && $gid =~ ^[0-9]+$ ]] || return 1
  [[ $group_gid =~ ^[0-9]+$ && $group_gid == "$gid" ]] || return 1
  [[ $uid != 0 && $uid != 1000 && $gid != 0 && $gid != 1000 ]] || return 1
  [[ $home == /nonexistent ]] || return 1
  [[ $shell == /usr/sbin/nologin || $shell == /bin/false ]] || return 1
  [[ ${group_record%%:*} == "$runtime_group" && -z $group_members ]] || return 1
  [[ $(id -g "$runtime_user") == "$gid" ]] || return 1
  all_passwd=$(getent passwd) || return 1
  while IFS=: read -r other_name _ other_uid other_gid _; do
    [[ $other_name == "$runtime_user" ]] && continue
    [[ $other_uid != "$uid" && $other_gid != "$gid" ]] || return 1
  done <<<"$all_passwd"
  all_groups=$(getent group) || return 1
  while IFS=: read -r other_name _ other_gid _; do
    [[ $other_name == "$runtime_group" ]] && continue
    [[ $other_gid != "$gid" ]] || return 1
  done <<<"$all_groups"
  printf '%s\n%s\n' "$uid" "$gid"
}

valid_image_reference() {
  local image=$1
  [[ $image =~ ^crpi-[a-z0-9]+-vpc[.]ap-northeast-1[.]personal[.]cr[.]aliyuncs[.]com/wildcard0923/hft-trading@sha256:[0-9a-f]{64}$ ]]
}

image_digest_hex() {
  valid_image_reference "$1" || return 1
  printf '%s\n' "${1##*@sha256:}"
}

validate_activation_manifest() {
  local activation_dir=$1
  local manifest=$activation_dir/activation.sha256
  local activation_root=${ACTIVATION_ROOT:-$ACTIVATION_ROOT_DEFAULT}
  local actual_names manifest_names required path expected_uid mode
  local canonical_root canonical_activation
  expected_uid=${EXPECTED_ROOT_UID:-$EXPECTED_ROOT_UID_DEFAULT}
  canonical_root=$(canonical_directory "$activation_root") || return 1
  canonical_activation=$(canonical_directory "$activation_dir") || return 1
  [[ $canonical_root == "$activation_root" \
    && $canonical_activation == "$activation_dir" \
    && $activation_dir == "$activation_root"/* ]] || return 1
  secure_directory_chain "$activation_dir" || return 1
  secure_regular_file "$manifest" 0444 || return 1
  [[ -z $(find "$activation_dir" -type l -print -quit) ]] || return 1
  while IFS= read -r -d '' path; do
    [[ $(stat_uid "$path") == "$expected_uid" ]] || return 1
    mode=$(stat_mode "$path") || return 1
    (( (8#$mode & 8#022) == 0 )) || return 1
  done < <(find "$activation_dir" \( -type f -o -type d \) -print0)

  manifest_names=$(awk '
    NF != 2 || $1 !~ /^[0-9a-f]{64}$/ || $2 !~ /^[A-Za-z0-9._\/-]+$/ ||
      $2 ~ /^\// || $2 ~ /(^|\/)\.\.?(\/|$)/ { exit 2 }
    { print $2 }
  ' "$manifest") || return 1
  [[ -n $manifest_names ]] || return 1
  [[ $(printf '%s\n' "$manifest_names" | LC_ALL=C sort -u) == "$manifest_names" ]] \
    || return 1
  actual_names=$(cd "$activation_dir" && find . -type f \
    ! -path ./activation.sha256 -print | sed 's#^./##' | LC_ALL=C sort) || return 1
  [[ $actual_names == "$manifest_names" ]] || return 1
  for required in \
    config/system.yaml \
    deployment/bundle.json \
    deployment/envelope.json \
    deployment/policy.json \
    deployment/trusted-keys.json; do
    printf '%s\n' "$manifest_names" | grep -Fxq "$required" || return 1
  done
  while IFS= read -r path; do
    [[ -f $activation_dir/$path && ! -L $activation_dir/$path ]] || return 1
  done <<<"$manifest_names"
  (cd "$activation_dir" && sha256sum --check --strict activation.sha256 >/dev/null) \
    || return 1
}

validate_paper_shadow_authority() {
  local activation_dir=$1 control_root=${CONTROL_ROOT:-$CONTROL_ROOT_DEFAULT}
  jq -e -s --slurpfile policy "$activation_dir/deployment/policy.json" \
    -f "$control_root/trading-ecs-paper-shadow-policy.jq" \
    "$activation_dir/deployment/envelope.json" >/dev/null
}

validate_runtime_secrets() {
  local secret_root=${SECRET_ROOT:-$SECRET_ROOT_DEFAULT}
  local runtime_env=$secret_root/runtime.env
  local feedback_key=$secret_root/feedback-signing-key.hex
  local fs_type runtime_env_fs feedback_key_fs api_prefixes secret_prefixes grpc_token
  local expected_uid runtime_uid runtime_gid
  local runtime_ids
  expected_uid=${EXPECTED_ROOT_UID:-$EXPECTED_ROOT_UID_DEFAULT}
  canonical_directory "$secret_root" >/dev/null || return 1
  secure_directory "$secret_root" || return 1
  [[ $(stat_mode "$secret_root") == 750 ]] || return 1
  runtime_ids=$(runtime_account_ids) || return 1
  [[ $runtime_ids == *$'\n'* && ${runtime_ids#*$'\n'} != *$'\n'* ]] || return 1
  runtime_uid=${runtime_ids%%$'\n'*}
  runtime_gid=${runtime_ids#*$'\n'}
  [[ $runtime_uid =~ ^[0-9]+$ && $runtime_gid =~ ^[0-9]+$ ]] || return 1
  [[ $runtime_uid != 0 && $runtime_uid != 1000 \
    && $runtime_gid != 0 && $runtime_gid != 1000 ]] || return 1
  [[ $(stat_uid "$secret_root") == "$expected_uid" ]] || return 1
  [[ $(stat_gid "$secret_root") == "$runtime_gid" ]] || return 1
  fs_type=$(findmnt -n -o FSTYPE --target "$secret_root") || return 1
  [[ $fs_type == tmpfs ]] || return 1
  runtime_env_fs=$(findmnt -n -o FSTYPE --target "$runtime_env") || return 1
  feedback_key_fs=$(findmnt -n -o FSTYPE --target "$feedback_key") || return 1
  [[ $runtime_env_fs == tmpfs && $feedback_key_fs == tmpfs ]] || return 1
  secure_regular_file "$runtime_env" 0440 || return 1
  secure_regular_file "$feedback_key" 0440 || return 1
  [[ ${runtime_env%/*} == "$secret_root" && ${feedback_key%/*} == "$secret_root" ]] \
    || return 1
  [[ $(stat_mode "$runtime_env") == 440 ]] || return 1
  [[ $(stat_mode "$feedback_key") == 440 ]] || return 1
  [[ $(stat_gid "$runtime_env") == "$runtime_gid" ]] || return 1
  [[ $(stat_gid "$feedback_key") == "$runtime_gid" ]] || return 1
  [[ $(wc -l <"$feedback_key") -eq 1 ]] || return 1
  grep -Eq '^[0-9a-f]{64}$' "$feedback_key" || return 1
  grep -Eq '^HFT_GRPC_AUTH_TOKEN=.+$' "$runtime_env" || return 1
  grep -Eq '^HFT_SECRET_[A-Z0-9_]+_API_KEY=.+$' "$runtime_env" || return 1
  grep -Eq '^HFT_SECRET_[A-Z0-9_]+_SECRET=.+$' "$runtime_env" || return 1
  if ! awk -F= '
    !/^[A-Z_][A-Z0-9_]*=.+$/ { exit 1 }
    seen[$1]++ { exit 1 }
    $1 == "HFT_GRPC_AUTH_TOKEN" { grpc++; next }
    $1 ~ /^HFT_SECRET_[A-Z0-9][A-Z0-9_]*_(API_KEY|SECRET)$/ { next }
    { exit 1 }
    END { if (grpc != 1) exit 1 }
  ' "$runtime_env"; then
    return 1
  fi
  api_prefixes=$(sed -n 's/^HFT_SECRET_\([A-Z0-9_]*\)_API_KEY=.*/\1/p' \
    "$runtime_env" | LC_ALL=C sort -u) || return 1
  secret_prefixes=$(sed -n 's/^HFT_SECRET_\([A-Z0-9_]*\)_SECRET=.*/\1/p' \
    "$runtime_env" | LC_ALL=C sort -u) || return 1
  [[ -n $api_prefixes && $api_prefixes == "$secret_prefixes" ]] || return 1
  if grep -Eq '^[A-Za-z_][A-Za-z0-9_]*=$|^[[:space:]]|[[:space:]]$' "$runtime_env"; then
    return 1
  fi
  grpc_token=$(sed -n 's/^HFT_GRPC_AUTH_TOKEN=//p' "$runtime_env") || return 1
  [[ ${#grpc_token} -ge 32 \
    && ! $grpc_token =~ ^[[:space:]] \
    && ! $grpc_token =~ [[:space:]]$ ]] || return 1
}

load_current() {
  local current_file=${HFT_CURRENT_FILE:-$CURRENT_FILE_DEFAULT}
  secure_regular_file "$current_file" 0600 || die "invalid current release file"
  # This file is root-owned, contains identifiers only, and is generated by hostctl.
  # shellcheck disable=SC1090
  source "$current_file"
  : "${HFT_TRADING_IMAGE:?missing HFT_TRADING_IMAGE}"
  : "${HFT_RELEASE_MANIFEST_SHA256:?missing HFT_RELEASE_MANIFEST_SHA256}"
  : "${HFT_ACTIVATION_DIR:?missing HFT_ACTIVATION_DIR}"
  : "${HFT_ACTIVATION_SHA256:?missing HFT_ACTIVATION_SHA256}"
  : "${HFT_SOURCE_REVISION:?missing HFT_SOURCE_REVISION}"
}

preflight() {
  local release_root=${RELEASE_ROOT:-$RELEASE_ROOT_DEFAULT}
  local control_root=${CONTROL_ROOT:-$CONTROL_ROOT_DEFAULT}
  local digest_hex release_file release_dir activation_sha
  local control_manifest expected_control_manifest
  assert_effective_service_contract "$UNIT_PATH" "${BASH_SOURCE[0]}" \
    || die "effective systemd service no longer matches the reviewed static unit"
  verify_bare_tokyo_host || die "host identity no longer matches the reviewed Tokyo ECS contract"
  load_current
  valid_image_reference "$HFT_TRADING_IMAGE" || die "image is not the Tokyo VPC ACR hft-trading digest reference"
  [[ $HFT_SOURCE_REVISION =~ ^[0-9a-f]{40}$ ]] || die "invalid source revision"
  [[ $HFT_RELEASE_MANIFEST_SHA256 =~ ^[0-9a-f]{64}$ ]] || die "invalid release manifest identity"
  digest_hex=$(image_digest_hex "$HFT_TRADING_IMAGE")
  release_file=$release_root/$digest_hex/$HFT_RELEASE_MANIFEST_SHA256/hft-trading-ecs-release.json
  release_dir=${release_file%/*}
  secure_regular_file "$release_file" 0444 || die "missing immutable staged release"
  secure_regular_file "$release_file.sha256" 0444 || die "missing staged release checksum"
  [[ $(wc -l <"$release_file.sha256") -eq 1 ]] || die "invalid staged release checksum"
  [[ $(sha256sum "$release_file" | awk '{print $1}') == "$HFT_RELEASE_MANIFEST_SHA256" ]] \
    || die "staged release identity mismatch"
  [[ $(<"$release_file.sha256") == "$HFT_RELEASE_MANIFEST_SHA256  hft-trading-ecs-release.json" ]] \
    || die "staged release checksum mismatch"
  jq -e --arg image "$HFT_TRADING_IMAGE" --arg source "$HFT_SOURCE_REVISION" '
    .schema == "monday.hft_trading_ecs_release.v1" and
    .image.reference == $image and .source_revision == $source
  ' "$release_file" >/dev/null || die "staged release identity mismatch"
  control_manifest=$release_dir/trading-ecs-control-assets.sha256
  secure_regular_file "$control_manifest" 0444 || die "missing staged control manifest"
  expected_control_manifest=$(jq -er '.control_manifest.sha256' "$release_file") \
    || die "release has no control manifest identity"
  [[ $(sha256sum "$control_manifest" | awk '{print $1}') == "$expected_control_manifest" ]] \
    || die "staged control manifest mismatch"
  validate_selected_control_assets "$control_manifest" "${BASH_SOURCE[0]}" \
    "$HOSTCTL_PROGRAM" "$control_root/trading-ecs-paper-shadow-policy.jq" \
    "$UNIT_PATH" || die "installed control assets do not match the selected release"
  validate_activation_manifest "$HFT_ACTIVATION_DIR" || die "activation manifest is invalid"
  activation_sha=$(sha256sum "$HFT_ACTIVATION_DIR/activation.sha256" | awk '{print $1}')
  [[ $activation_sha == "$HFT_ACTIVATION_SHA256" ]] || die "activation identity mismatch"
  validate_paper_shadow_authority "$HFT_ACTIVATION_DIR" || die "activation is not fail-closed Paper/Shadow"
  validate_runtime_secrets || die "RAM-role injected tmpfs secrets are absent or unsafe"
  docker image inspect "$HFT_TRADING_IMAGE" --format '{{json .RepoDigests}}' \
    | jq -e --arg image "$HFT_TRADING_IMAGE" 'index($image) != null' >/dev/null \
    || die "digest-pinned image is not staged locally"
}

run_container() {
  local state_root=${STATE_ROOT:-$STATE_ROOT_DEFAULT}
  local secret_root=${SECRET_ROOT:-$SECRET_ROOT_DEFAULT}
  local activation_id state_dir runtime_uid runtime_gid runtime_ids
  preflight
  runtime_ids=$(runtime_account_ids) \
    || die "dedicated runtime account is absent or unsafe"
  [[ $runtime_ids == *$'\n'* && ${runtime_ids#*$'\n'} != *$'\n'* ]] \
    || die "dedicated runtime account is invalid"
  runtime_uid=${runtime_ids%%$'\n'*}
  runtime_gid=${runtime_ids#*$'\n'}
  activation_id=$HFT_ACTIVATION_SHA256
  state_dir=$state_root/$activation_id
  install -d -o root -g root -m 0750 "$state_root"
  install -d -o "$runtime_uid" -g "$runtime_gid" -m 0700 "$state_dir"
  exec docker run --rm --pull never \
    --name "$CONTAINER_NAME" \
    --publish 127.0.0.1::9090/tcp \
    --publish 127.0.0.1::9092/tcp \
    --read-only \
    --user "$runtime_uid:$runtime_gid" \
    --cap-drop ALL \
    --security-opt no-new-privileges:true \
    --pids-limit 512 \
    --ulimit nofile=65536:65536 \
    --ulimit core=0:0 \
    --stop-signal SIGINT \
    --stop-timeout 60 \
    --tmpfs "/tmp:rw,noexec,nosuid,nodev,size=64m,mode=0700,uid=$runtime_uid,gid=$runtime_gid" \
    --log-driver journald \
    --env HFT_ENV=production \
    --mount "type=bind,src=$HFT_ACTIVATION_DIR,dst=/activation,readonly" \
    --mount "type=bind,src=$state_dir,dst=/app/state" \
    --mount "type=bind,src=$secret_root/runtime.env,dst=/run/secrets/hft/runtime.env,readonly" \
    --mount "type=bind,src=$secret_root/feedback-signing-key.hex,dst=/run/secrets/hft/feedback-signing-key.hex,readonly" \
    --entrypoint /bin/sh \
    "$HFT_TRADING_IMAGE" \
    -euc 'while IFS= read -r secret; do export "$secret"; done < /run/secrets/hft/runtime.env; unset secret; exec /usr/local/bin/hft-live "$@"' \
    hft-live \
    --config /activation/config/system.yaml \
    --deployment-envelope /activation/deployment/envelope.json \
    --strategy-bundle /activation/deployment/bundle.json \
    --deployment-policy /activation/deployment/policy.json \
    --deployment-trusted-keys /activation/deployment/trusted-keys.json \
    --deployment-nonce-ledger /app/state/nonces.jsonl \
    --deployment-audit-log /app/state/audit.jsonl \
    --deployment-feedback-log /app/state/feedback.jsonl \
    --deployment-feedback-signing-key /run/secrets/hft/feedback-signing-key.hex \
    --deployment-feedback-key-id runtime-feedback-1 \
    --metrics-port 9090
}

container_state() {
  local running names
  if running=$(docker container inspect --format '{{.State.Running}}' \
    "$CONTAINER_NAME" 2>/dev/null); then
    [[ $running == true || $running == false ]] || return 1
    printf '%s\n' "$running"
    return 0
  fi

  # An inspect failure is safe only when the daemon is healthy and an exact-name
  # listing independently proves that no stale container exists.
  docker info >/dev/null 2>&1 || return 1
  names=$(docker container ls --all --filter "name=^/${CONTAINER_NAME}$" \
    --format '{{.Names}}') || return 1
  [[ -z $names ]] || return 1
  printf 'absent\n'
}

assert_stopped() {
  local state
  state=$(container_state) || return 1
  # Cutover preflight requires absence, not merely a stopped orphan.
  [[ $state == absent ]]
}

stop_container() {
  local state
  state=$(container_state) || return 1
  case $state in
    absent) return 0 ;;
    true) docker stop --time 60 "$CONTAINER_NAME" >/dev/null || return 1 ;;
    false) ;;
    *) return 1 ;;
  esac
  # docker run --rm normally removes the container after stop. ExecStopPost is
  # still the fail-closed cleanup backstop if removal has not completed yet.
  state=$(container_state) || return 1
  [[ $state == absent || $state == false ]]
}

ensure_stopped() {
  local state
  state=$(container_state) || return 1
  if [[ $state != absent ]]; then
    docker rm --force "$CONTAINER_NAME" >/dev/null || return 1
  fi
  assert_stopped
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  case ${1:-} in
    preflight) preflight ;;
    run) run_container ;;
    stop) stop_container ;;
    ensure-stopped) ensure_stopped ;;
    assert-stopped) assert_stopped ;;
    *) die "usage: $0 preflight|run|stop|ensure-stopped|assert-stopped" ;;
  esac
fi
