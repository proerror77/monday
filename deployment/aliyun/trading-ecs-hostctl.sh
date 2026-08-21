#!/usr/bin/env bash
set -Eeuo pipefail

export LC_ALL=C
umask 0077

readonly EXPECTED_REGION=ap-northeast-1
readonly EXPECTED_HOST_OS=ubuntu
readonly EXPECTED_HOST_VERSION=26.04
readonly EXPECTED_ARCH=amd64
readonly EXPECTED_RAM_ROLE_DEFAULT=MondayTradingEcsRole
readonly ARTIFACT_ROOT_DEFAULT=/opt/monday/incoming/hft-trading
readonly ACR_AUTH_ROOT_DEFAULT=/run/monday/acr-auth
readonly RELEASE_ROOT_DEFAULT=/opt/monday/releases/hft-trading
readonly CONTROL_ROOT_DEFAULT=/opt/monday/control/hft-trading
readonly EVIDENCE_ROOT_DEFAULT=/var/lib/monday/evidence/hft-trading
readonly STATE_ROOT_DEFAULT=/var/lib/monday/hft-trading
readonly CURRENT_FILE_DEFAULT=/etc/monday/hft-trading-current.env
readonly RUNTIME_PROGRAM_DEFAULT=/usr/local/libexec/monday-hft-trading-runtime
readonly HOSTCTL_PROGRAM_DEFAULT=/usr/local/sbin/monday-hft-trading-hostctl
readonly UNIT_PATH_DEFAULT=/etc/systemd/system/monday-hft-trading.service
readonly SERVICE=monday-hft-trading.service
readonly HOST_LOCK_DEFAULT=/run/monday/hft-trading-host.lock
readonly HFT_RUNTIME_USER_DEFAULT=mondayhft
readonly HFT_RUNTIME_GROUP_DEFAULT=mondayhft

die() {
  printf 'hft-trading host contract failed: %s\n' "$*" >&2
  exit 1
}

require_root() {
  [[ $(id -u) -eq 0 ]] || die 'must run as root on the bare ECS host'
}

stat_uid() {
  if [[ $(uname -s) == Darwin ]]; then stat -f %u -- "$1"; else stat -c %u -- "$1"; fi
}

stat_gid() {
  if [[ $(uname -s) == Darwin ]]; then stat -f %g -- "$1"; else stat -c %g -- "$1"; fi
}

stat_mode() {
  if [[ $(uname -s) == Darwin ]]; then stat -f %Lp -- "$1"; else stat -c %a -- "$1"; fi
}

secure_regular_file() {
  local path=$1 max_mode=$2 expected_uid=${EXPECTED_ROOT_UID:-0} mode
  [[ -f $path && ! -L $path ]] || return 1
  [[ $(stat_uid "$path") == "$expected_uid" ]] || return 1
  mode=$(stat_mode "$path") || return 1
  (( (8#$mode & ~8#$max_mode) == 0 ))
}

secure_directory() {
  local path=$1 expected_uid=${EXPECTED_ROOT_UID:-0} mode
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

assert_service_static() {
  local state
  if ! state=$(systemctl is-enabled "$SERVICE" 2>/dev/null); then
    return 1
  fi
  [[ $state == static ]]
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

assert_service_boot_disabled_before_stage() {
  local unit_path=$1 state='' status=0
  state=$(systemctl is-enabled "$SERVICE" 2>/dev/null) || status=$?
  case $state in
    static) (( status == 0 )) ;;
    disabled) (( status == 1 )) ;;
    not-found) (( status == 1 )) && [[ ! -e $unit_path ]] ;;
    *) return 1 ;;
  esac
}

assert_no_orphan_container() {
  local names
  docker info >/dev/null 2>&1 || return 1
  names=$(docker container ls --all --filter 'name=^/monday-hft-trading$' \
    --format '{{.Names}}') || return 1
  [[ -z $names ]]
}

ensure_runtime_account() {
  local runtime_user=$HFT_RUNTIME_USER_DEFAULT
  local runtime_group=$HFT_RUNTIME_GROUP_DEFAULT
  local passwd_record group_record all_passwd all_groups uid gid home shell
  local group_gid group_members other_name other_uid other_gid
  if ! getent group "$runtime_group" >/dev/null; then
    groupadd --system "$runtime_group" || return 1
  fi
  if ! getent passwd "$runtime_user" >/dev/null; then
    useradd --system --gid "$runtime_group" --home-dir /nonexistent \
      --no-create-home --shell /usr/sbin/nologin "$runtime_user" || return 1
  fi
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
}

valid_image_reference() {
  [[ $1 =~ ^crpi-[a-z0-9]+-vpc[.]ap-northeast-1[.]personal[.]cr[.]aliyuncs[.]com/wildcard0923/hft-trading@sha256:[0-9a-f]{64}$ ]]
}

release_digest_hex() {
  valid_image_reference "$1" || return 1
  printf '%s\n' "${1##*@sha256:}"
}

verify_single_checksum_marker() {
  local directory=$1 marker=$2 expected_file=$3
  local line digest
  [[ -f $directory/$marker && ! -L $directory/$marker ]] || return 1
  [[ $(wc -l <"$directory/$marker") -eq 1 ]] || return 1
  line=$(<"$directory/$marker") || return 1
  digest=${line%% *}
  [[ $digest =~ ^[0-9a-f]{64}$ ]] || return 1
  [[ $line == "$digest  $expected_file" ]] || return 1
  (cd "$directory" && sha256sum --check --strict "$marker" >/dev/null) || return 1
}

verify_release_manifest() {
  local artifact_dir=$1
  local manifest=$artifact_dir/hft-trading-ecs-release.json
  local artifact_root=${ARTIFACT_ROOT:-$ARTIFACT_ROOT_DEFAULT}
  local canonical_root canonical_artifact
  canonical_root=$(canonical_directory "$artifact_root") || return 1
  canonical_artifact=$(canonical_directory "$artifact_dir") || return 1
  [[ $artifact_root == "$canonical_root" && $artifact_dir == "$canonical_artifact" \
    && $canonical_artifact == "$canonical_root"/* \
    && ${canonical_artifact#"$canonical_root/"} != */* ]] || return 1
  secure_directory "$canonical_root" || return 1
  secure_directory "$artifact_dir" || return 1
  secure_regular_file "$manifest" 0644 || return 1
  secure_regular_file "$artifact_dir/hft-trading-ecs-release.json.sha256" 0644 \
    || return 1
  verify_single_checksum_marker "$artifact_dir" \
    hft-trading-ecs-release.json.sha256 hft-trading-ecs-release.json || return 1
  jq -e -s '
    length == 1 and (.[0] |
      .schema == "monday.hft_trading_ecs_release.v1" and
      (keys | sort) == (["control_archive","control_manifest","image",
        "platform","schema","source_revision"] | sort) and
      (.source_revision | test("^[0-9a-f]{40}$")) and
      (.image | keys | sort) == (["digest","published_repository",
        "reference","repository"] | sort) and
      (.image.digest | test("^sha256:[0-9a-f]{64}$")) and
      (.image.repository | test("^crpi-[a-z0-9]+-vpc[.]ap-northeast-1[.]personal[.]cr[.]aliyuncs[.]com/wildcard0923/hft-trading$")) and
      .image.reference == (.image.repository + "@" + .image.digest) and
      (.image.published_repository | test("^crpi-[a-z0-9]+[.]ap-northeast-1[.]personal[.]cr[.]aliyuncs[.]com/wildcard0923/hft-trading$")) and
      ((.image.published_repository |
          capture("^crpi-(?<id>[a-z0-9]+)[.]ap-northeast-1").id) ==
        (.image.repository |
          capture("^crpi-(?<id>[a-z0-9]+)-vpc[.]ap-northeast-1").id)) and
      (.control_manifest | keys | sort) == ["file","sha256"] and
      .control_manifest.file == "trading-ecs-control-assets.sha256" and
      (.control_manifest.sha256 | test("^[0-9a-f]{64}$")) and
      (.control_archive | keys | sort) == ["file","sha256"] and
      .control_archive.file == "trading-ecs-control.tar.gz" and
      (.control_archive.sha256 | test("^[0-9a-f]{64}$")) and
      .platform == {region:"ap-northeast-1",host_os:"ubuntu",
        host_version:"26.04",architecture:"amd64",orchestrator:"none"}
    )
  ' "$manifest" >/dev/null || return 1
}

verify_control_bundle() {
  local artifact_dir=$1 extract_dir=$2
  local manifest_sha archive_sha control_names
  secure_regular_file "$artifact_dir/trading-ecs-control-assets.sha256" 0644 || return 1
  secure_regular_file "$artifact_dir/trading-ecs-control.tar.gz" 0644 || return 1
  manifest_sha=$(jq -er '.control_manifest.sha256' \
    "$artifact_dir/hft-trading-ecs-release.json") || return 1
  archive_sha=$(jq -er '.control_archive.sha256' \
    "$artifact_dir/hft-trading-ecs-release.json") || return 1
  [[ $(sha256sum "$artifact_dir/trading-ecs-control-assets.sha256" | awk '{print $1}') == "$manifest_sha" ]] \
    || return 1
  [[ $(sha256sum "$artifact_dir/trading-ecs-control.tar.gz" | awk '{print $1}') == "$archive_sha" ]] \
    || return 1
  control_names=$(awk '
    NF != 2 || $1 !~ /^[0-9a-f]{64}$/ || $2 !~ /^[A-Za-z0-9._-]+$/ { exit 2 }
    { print $2 }
  ' "$artifact_dir/trading-ecs-control-assets.sha256") || return 1
  [[ $control_names == $'hft-trading-ecs.service\ntrading-ecs-hostctl.sh\ntrading-ecs-paper-shadow-policy.jq\ntrading-ecs-runtime.sh' ]] \
    || return 1
  diff -u \
    <(printf '%s\n' hft-trading-ecs.service trading-ecs-hostctl.sh \
      trading-ecs-paper-shadow-policy.jq trading-ecs-runtime.sh) \
    <(tar -tzf "$artifact_dir/trading-ecs-control.tar.gz" | LC_ALL=C sort) || return 1
  tar -tvzf "$artifact_dir/trading-ecs-control.tar.gz" \
    | awk 'substr($1, 1, 1) != "-" { exit 1 } END { if (NR != 4) exit 1 }' \
    || return 1
  tar --no-same-owner --no-same-permissions \
    -xzf "$artifact_dir/trading-ecs-control.tar.gz" -C "$extract_dir" || return 1
  [[ -z $(find "$extract_dir" -type l -print -quit) ]] || return 1
  (cd "$extract_dir" && sha256sum --check --strict \
    "$artifact_dir/trading-ecs-control-assets.sha256" >/dev/null) || return 1
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
  local token role region instance_id
  # shellcheck disable=SC1091
  source /etc/os-release
  [[ ${ID:-} == "$EXPECTED_HOST_OS" && ${VERSION_ID:-} == "$EXPECTED_HOST_VERSION" ]] \
    || die 'host must be Ubuntu 26.04'
  [[ $(dpkg --print-architecture) == "$EXPECTED_ARCH" ]] || die 'host must be amd64'
  systemctl is-active --quiet kubelet && die 'trading host must not be an ACK node'
  [[ ! -e /var/lib/kubelet/kubeconfig ]] || die 'kubelet state is forbidden on the trading host'
  token=$(metadata_token) || die 'Alibaba ECS metadata v2 is required'
  region=$(metadata_get "$token" region-id) || die 'cannot read ECS region'
  [[ $region == "$EXPECTED_REGION" ]] || die 'trading host must be in Tokyo'
  role=$(metadata_get "$token" ram/security-credentials/) || die 'cannot read ECS RAM role'
  role=${role%%$'\n'*}
  [[ $role == "$EXPECTED_RAM_ROLE_DEFAULT" ]] \
    || die 'unexpected or missing ECS RAM role'
  instance_id=$(metadata_get "$token" instance-id) || die 'cannot read ECS instance id'
  [[ $instance_id =~ ^i-[a-z0-9]+$ ]] || die 'invalid ECS instance identity'
  printf '%s\n%s\n%s\n' "$region" "$role" "$instance_id"
}

verify_acr_password_file() {
  local password_file=$1
  local auth_root=${ACR_AUTH_ROOT:-$ACR_AUTH_ROOT_DEFAULT} fs_type password_fs canonical_root
  local canonical_parent
  canonical_root=$(canonical_directory "$auth_root") \
    || die 'ACR auth root must be an absolute canonical directory'
  secure_directory "$auth_root" || die 'ACR auth root is unsafe'
  [[ $(stat_mode "$auth_root") == 700 ]] || die 'ACR auth root must be mode 0700'
  [[ $password_file == "$canonical_root"/* \
    && ${password_file#"$canonical_root/"} != */* ]] \
    || die 'ACR password file must be a direct child of the ephemeral auth root'
  canonical_parent=$(cd -- "${password_file%/*}" 2>/dev/null && pwd -P) \
    || die 'cannot resolve the ACR password parent directory'
  [[ $canonical_parent == "$canonical_root" \
    && $password_file == "$canonical_parent/${password_file##*/}" ]] \
    || die 'ACR password path traversal is forbidden'
  fs_type=$(findmnt -n -o FSTYPE --target "$auth_root") \
    || die 'cannot verify the ACR auth filesystem'
  [[ $fs_type == tmpfs ]] || die 'ACR auth root must be on tmpfs'
  password_fs=$(findmnt -n -o FSTYPE --target "$password_file") \
    || die 'cannot verify the ACR password filesystem'
  [[ $password_fs == tmpfs ]] || die 'ACR password file must be on tmpfs'
  secure_regular_file "$password_file" 0400 || die 'ACR password file must be root-owned mode 0400'
  [[ $(stat_mode "$password_file") == 400 ]] || die 'ACR password file must be mode 0400'
  [[ -s $password_file && $(wc -l <"$password_file") -eq 1 ]] || die 'ACR password file must contain one non-empty line'
  grep -q '[^[:space:]]' "$password_file" || die 'ACR password file must contain one non-empty line'
}

verify_local_image() {
  local image=$1
  valid_image_reference "$image" || return 1
  docker image inspect "$image" --format '{{json .RepoDigests}}' \
    | jq -e --arg image "$image" 'index($image) != null' >/dev/null
}

stage_release() {
  local artifact_dir=$1 acr_username=$2 password_file=$3
  local release_root=${RELEASE_ROOT:-$RELEASE_ROOT_DEFAULT}
  local control_root=${CONTROL_ROOT:-$CONTROL_ROOT_DEFAULT}
  local runtime_program=${RUNTIME_PROGRAM:-$RUNTIME_PROGRAM_DEFAULT}
  local hostctl_program=${HOSTCTL_PROGRAM:-$HOSTCTL_PROGRAM_DEFAULT}
  local unit_path=${UNIT_PATH:-$UNIT_PATH_DEFAULT}
  local auth_root=${ACR_AUTH_ROOT:-$ACR_AUTH_ROOT_DEFAULT}
  local extract_dir docker_config='' image registry digest_hex release_dir source_revision
  local release_manifest_sha
  local host_values region role instance_id evidence_parent evidence_dir staged_at stage_run_id
  local registry_logged_in=false
  require_root
  for command in curl diff docker dpkg find findmnt getent groupadd id install jq \
    sha256sum systemctl tar timeout useradd; do
    command -v "$command" >/dev/null 2>&1 || die "missing host dependency: $command"
  done
  [[ $artifact_dir == /* ]] || die 'artifact directory must be absolute'
  [[ $acr_username =~ ^[^[:space:]]+$ ]] || die 'ACR username is required'
  systemctl is-active --quiet "$SERVICE" \
    && die 'staging cannot mutate an active trading runtime'
  assert_service_boot_disabled_before_stage "$unit_path" \
    || die 'staging refuses a service that is not explicitly boot-disabled'
  assert_no_service_dropin_directories \
    /etc/systemd/system /run/systemd/system /usr/local/lib/systemd/system \
    /usr/lib/systemd/system /lib/systemd/system \
    || die 'staging refuses trading-service drop-in directories'
  assert_no_orphan_container \
    || die 'Docker daemon is unavailable or an orphan trading container exists'
  verify_release_manifest "$artifact_dir" || die 'release manifest is invalid'
  verify_acr_password_file "$password_file"
  mapfile -t host_values < <(verify_bare_tokyo_host)
  [[ ${#host_values[@]} -eq 3 ]] || die 'bare-host verification did not complete'
  region=${host_values[0]}
  role=${host_values[1]}
  instance_id=${host_values[2]}
  ensure_runtime_account || die 'dedicated non-login runtime account is unsafe'
  image=$(jq -er '.image.reference' "$artifact_dir/hft-trading-ecs-release.json")
  valid_image_reference "$image" || die 'release image is not an immutable Tokyo VPC reference'
  registry=${image%%/*}
  digest_hex=$(release_digest_hex "$image")
  source_revision=$(jq -er '.source_revision' "$artifact_dir/hft-trading-ecs-release.json")
  release_manifest_sha=$(sha256sum "$artifact_dir/hft-trading-ecs-release.json" | awk '{print $1}')
  release_dir=$release_root/$digest_hex/$release_manifest_sha
  extract_dir=$(mktemp -d)
  stage_cleanup() {
    rm -rf -- "$extract_dir" 2>/dev/null || true
    if [[ $registry_logged_in == true && -n $docker_config ]]; then
      DOCKER_CONFIG=$docker_config docker logout "$registry" >/dev/null 2>&1 || true
    fi
    [[ -z $docker_config ]] || rm -rf -- "$docker_config" 2>/dev/null || true
  }
  trap stage_cleanup EXIT
  docker_config=$(mktemp -d "$auth_root/docker-config.XXXXXX")
  chmod 0700 "$docker_config"
  verify_control_bundle "$artifact_dir" "$extract_dir" || die 'control bundle is invalid'

  DOCKER_CONFIG=$docker_config docker login --username "$acr_username" \
    --password-stdin "$registry" <"$password_file" >/dev/null
  registry_logged_in=true
  if ! DOCKER_CONFIG=$docker_config docker pull "$image" >/dev/null; then
    die 'digest-pinned ACR pull failed'
  fi
  DOCKER_CONFIG=$docker_config docker logout "$registry" >/dev/null \
    || die 'ACR logout failed'
  registry_logged_in=false
  verify_local_image "$image" || die 'pulled image RepoDigests do not contain the requested digest'

  install -d -o root -g root -m 0755 "$release_dir" "$control_root" \
    "${runtime_program%/*}" "${hostctl_program%/*}" "${unit_path%/*}"
  install -o root -g root -m 0444 \
    "$artifact_dir/hft-trading-ecs-release.json" \
    "$artifact_dir/hft-trading-ecs-release.json.sha256" \
    "$artifact_dir/trading-ecs-control-assets.sha256" \
    "$artifact_dir/trading-ecs-control.tar.gz" "$release_dir/"
  install -o root -g root -m 0555 "$extract_dir/trading-ecs-runtime.sh" "$runtime_program"
  install -o root -g root -m 0555 "$extract_dir/trading-ecs-hostctl.sh" "$hostctl_program"
  install -o root -g root -m 0444 \
    "$extract_dir/trading-ecs-paper-shadow-policy.jq" "$control_root/"
  install -o root -g root -m 0444 "$extract_dir/hft-trading-ecs.service" "$unit_path"
  systemctl daemon-reload
  systemctl is-active --quiet "$SERVICE" && die 'staged service unexpectedly became active'
  assert_effective_service_contract "$unit_path" "$runtime_program" \
    || die 'effective systemd service does not match the selected static unit'

  evidence_parent=${EVIDENCE_ROOT:-$EVIDENCE_ROOT_DEFAULT}/stage/$digest_hex/$release_manifest_sha/runs
  install -d -o root -g root -m 0700 "$evidence_parent"
  stage_run_id=$(date -u +%Y%m%dT%H%M%S)-$$
  evidence_dir=$evidence_parent/$stage_run_id
  mkdir -m 0700 "$evidence_dir" || die 'stage evidence run already exists'
  staged_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  jq -S -n --arg image "$image" --arg source "$source_revision" \
    --arg release_manifest "$release_manifest_sha" \
    --arg region "$region" --arg role "$role" --arg instance "$instance_id" \
    --arg staged_at "$staged_at" '
    {schema:"monday.hft_trading_ecs_stage.v1",result:"staged",
      image_reference:$image,source_revision:$source,
      release_manifest_sha256:$release_manifest,region:$region,
      ram_role:$role,instance_id:$instance,host_os:"ubuntu",
      host_version:"26.04",architecture:"amd64",orchestrator:"none",
      service_started:false,service_enabled:false,staged_at:$staged_at}
  ' >"$evidence_dir/stage.json"
  (cd "$evidence_dir" && sha256sum stage.json >STAGED.sha256)
  chmod 0444 "$evidence_dir/stage.json" "$evidence_dir/STAGED.sha256"
  sync -f "$evidence_dir"
  printf '%s\n' "$evidence_dir/stage.json"
  stage_cleanup
  trap - EXIT
}

write_current_file() {
  local destination=$1 image=$2 release_manifest_sha=$3 activation_dir=$4 source_revision=$5
  local activation_sha
  activation_sha=$(sha256sum "$activation_dir/activation.sha256" | awk '{print $1}')
  umask 077
  {
    printf 'HFT_TRADING_IMAGE=%q\n' "$image"
    printf 'HFT_RELEASE_MANIFEST_SHA256=%q\n' "$release_manifest_sha"
    printf 'HFT_ACTIVATION_DIR=%q\n' "$activation_dir"
    printf 'HFT_ACTIVATION_SHA256=%q\n' "$activation_sha"
    printf 'HFT_SOURCE_REVISION=%q\n' "$source_revision"
  } >"$destination"
  chmod 0600 "$destination"
}

install_current_pointer() {
  local source=$1 destination=$2
  local temporary=$destination.tmp.$$
  if ! install -o root -g root -m 0600 "$source" "$temporary" \
    || ! sync -f "$temporary" \
    || ! mv -Tf "$temporary" "$destination"; then
    rm -f -- "$temporary" 2>/dev/null || true
    return 1
  fi
  sync -f "${destination%/*}" || return 1
}

capture_runtime_identity() {
  local image=$1 invocation_id main_pid nrestarts container_id
  local container_running container_image health_endpoint grpc_endpoint
  systemctl is-active --quiet "$SERVICE" || return 1
  assert_service_static || return 1
  invocation_id=$(systemctl show "$SERVICE" --property=InvocationID --value) || return 1
  main_pid=$(systemctl show "$SERVICE" --property=MainPID --value) || return 1
  nrestarts=$(systemctl show "$SERVICE" --property=NRestarts --value) || return 1
  container_id=$(docker container inspect --format '{{.Id}}' monday-hft-trading) || return 1
  container_running=$(docker container inspect --format '{{.State.Running}}' \
    monday-hft-trading) || return 1
  container_image=$(docker container inspect --format '{{.Config.Image}}' \
    monday-hft-trading) || return 1
  health_endpoint=$(docker container inspect --format \
    '{{with (index (index .NetworkSettings.Ports "9090/tcp") 0)}}{{.HostIp}}:{{.HostPort}}{{end}}' \
    monday-hft-trading) || return 1
  grpc_endpoint=$(docker container inspect --format \
    '{{with (index (index .NetworkSettings.Ports "9092/tcp") 0)}}{{.HostIp}}:{{.HostPort}}{{end}}' \
    monday-hft-trading) || return 1
  [[ $invocation_id =~ ^[0-9a-f]{32}$ ]] || return 1
  [[ $main_pid =~ ^[1-9][0-9]*$ ]] || return 1
  [[ $nrestarts == 0 ]] || return 1
  [[ $container_id =~ ^[0-9a-f]{64}$ ]] || return 1
  [[ $container_running == true && $container_image == "$image" ]] || return 1
  [[ $health_endpoint =~ ^127[.]0[.]0[.]1:[1-9][0-9]{0,4}$ ]] || return 1
  (( ${health_endpoint##*:} <= 65535 )) || return 1
  [[ $grpc_endpoint =~ ^127[.]0[.]0[.]1:[1-9][0-9]{0,4}$ ]] || return 1
  (( ${grpc_endpoint##*:} <= 65535 )) || return 1
  [[ $grpc_endpoint != "$health_endpoint" ]] || return 1
  printf '%s|%s|%s|%s|%s|%s\n' "$invocation_id" "$main_pid" "$nrestarts" \
    "$container_id" "$health_endpoint" "$grpc_endpoint"
}

wait_for_runtime_identity() {
  local image=$1 identity _
  for _ in {1..30}; do
    if identity=$(capture_runtime_identity "$image"); then
      printf '%s\n' "$identity"
      return 0
    fi
    sleep 1
  done
  return 1
}

runtime_identity_matches() {
  local image=$1 expected=$2 actual
  actual=$(capture_runtime_identity "$image") || return 1
  [[ $actual == "$expected" ]]
}

grpc_endpoint_ready() {
  local endpoint=$1 host=${1%:*} port=${1##*:}
  [[ $endpoint == "$host:$port" && $host == 127.0.0.1 \
    && $port =~ ^[1-9][0-9]{0,4}$ ]] || return 1
  # The positional parameters expand in the bounded child Bash, not here.
  # shellcheck disable=SC2016
  timeout 3 bash -c 'exec 3<>"/dev/tcp/$1/$2"' bash "$host" "$port" \
    >/dev/null 2>&1
}

wait_for_health() {
  local image=$1 expected_identity=$2 _ health_endpoint grpc_endpoint
  local identity_without_grpc
  grpc_endpoint=${expected_identity##*|}
  identity_without_grpc=${expected_identity%|*}
  health_endpoint=${identity_without_grpc##*|}
  [[ $health_endpoint =~ ^127[.]0[.]0[.]1:[1-9][0-9]{0,4}$ ]] || return 1
  [[ $grpc_endpoint =~ ^127[.]0[.]0[.]1:[1-9][0-9]{0,4}$ ]] || return 1
  for _ in {1..24}; do
    runtime_identity_matches "$image" "$expected_identity" || return 1
    if curl -fsS --max-time 3 "http://$health_endpoint/health" >/dev/null \
      && curl -fsS --max-time 3 "http://$health_endpoint/readiness" >/dev/null \
      && grpc_endpoint_ready "$grpc_endpoint"; then
      runtime_identity_matches "$image" "$expected_identity" || return 1
      sleep 10
      runtime_identity_matches "$image" "$expected_identity" || return 1
      if curl -fsS --max-time 3 "http://$health_endpoint/health" >/dev/null \
        && curl -fsS --max-time 3 "http://$health_endpoint/readiness" >/dev/null \
        && grpc_endpoint_ready "$grpc_endpoint" \
        && runtime_identity_matches "$image" "$expected_identity"; then
        return 0
      fi
    fi
    sleep 5
  done
  return 1
}

restore_previous_pointer() {
  local previous_file=$1 current_file=$2
  if [[ -f $previous_file ]]; then
    install_current_pointer "$previous_file" "$current_file" || return 1
  else
    rm -f -- "$current_file" || return 1
    sync -f "${current_file%/*}" || return 1
  fi
}

finalize_cutover_failure() {
  local evidence_dir=$1 previous_file=$2 current_file=$3 runtime_program=$4
  local image=$5 source_revision=$6 release_manifest_sha=$7 activation_sha=$8
  local failure_reason=$9 cutover_tmp=${10:-} marker_tmp=${11:-}
  local runtime_stopped=false previous_pointer_restored=false
  local result evidence_name marker_name operator_action
  local evidence_tmp marker_tmp_failed evidence_sha
  [[ -d $evidence_dir && ! -L $evidence_dir ]] || return 1
  [[ ! -e $evidence_dir/PASSED.sha256 ]] || return 1
  if [[ -e $evidence_dir/FAILED.sha256 ]]; then
    verify_single_checksum_marker "$evidence_dir" FAILED.sha256 cutover.failed.json
    return
  fi
  rm -f -- "$evidence_dir/EMERGENCY_FAILED_OPEN.sha256" 2>/dev/null || true
  if [[ -f $evidence_dir/cutover.emergency.json ]]; then
    mv -T "$evidence_dir/cutover.emergency.json" \
      "$evidence_dir/cutover.emergency.previous.$$.json" 2>/dev/null || true
  fi
  [[ -z $cutover_tmp ]] || rm -f -- "$cutover_tmp" 2>/dev/null || true
  [[ -z $marker_tmp ]] || rm -f -- "$marker_tmp" 2>/dev/null || true
  if [[ -f $evidence_dir/cutover.json ]]; then
    mv -T "$evidence_dir/cutover.json" \
      "$evidence_dir/cutover.unconfirmed.json" 2>/dev/null || true
  fi
  if [[ -f $evidence_dir/cutover.failed.json ]]; then
    mv -T "$evidence_dir/cutover.failed.json" \
      "$evidence_dir/cutover.failed.unconfirmed.$$.json" 2>/dev/null || true
  fi
  systemctl stop "$SERVICE" >/dev/null 2>&1 || true
  if "$runtime_program" ensure-stopped; then
    runtime_stopped=true
  else
    failure_reason=${failure_reason}_and_stop_failed
  fi
  if restore_previous_pointer "$previous_file" "$current_file"; then
    previous_pointer_restored=true
  else
    failure_reason=${failure_reason}_and_pointer_restore_failed
  fi
  if [[ $runtime_stopped == true && $previous_pointer_restored == true ]]; then
    result=failed_closed
    evidence_name=cutover.failed.json
    marker_name=FAILED.sha256
    operator_action='new signed envelope and nonce before restart'
  else
    result=emergency_failed_open
    evidence_name=cutover.emergency.json
    marker_name=EMERGENCY_FAILED_OPEN.sha256
    operator_action='IMMEDIATE MANUAL STOP AND POINTER RECOVERY REQUIRED; trading remains blocked'
  fi
  evidence_tmp=$evidence_dir/$evidence_name.tmp.$$
  marker_tmp_failed=$evidence_dir/$marker_name.tmp.$$
  if ! jq -S -n --arg image "$image" --arg source "$source_revision" \
    --arg release_manifest "$release_manifest_sha" \
    --arg activation "$activation_sha" --arg reason "$failure_reason" \
    --arg result "$result" --arg operator_action "$operator_action" \
    --argjson runtime_stopped "$runtime_stopped" \
    --argjson pointer_restored "$previous_pointer_restored" \
    --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
    {schema:"monday.hft_trading_ecs_cutover.v1",result:$result,
      image_reference:$image,source_revision:$source,
      release_manifest_sha256:$release_manifest,
      activation_manifest_sha256:$activation,failure_reason:$reason,
      runtime_stopped:$runtime_stopped,
      previous_pointer_restored:$pointer_restored,
      previous_runtime_restarted:false,
      trading_authority_blocked:true,
      operator_action_required:$operator_action,
      failed_at:$at}
  ' >"$evidence_tmp"; then
    return 1
  fi
  evidence_sha=$(sha256sum "$evidence_tmp" | awk '{print $1}') || return 1
  [[ $evidence_sha =~ ^[0-9a-f]{64}$ ]] || return 1
  printf '%s  %s\n' "$evidence_sha" "$evidence_name" >"$marker_tmp_failed" \
    || return 1
  chmod 0444 "$evidence_tmp" "$marker_tmp_failed" || return 1
  sync -f "$evidence_tmp" || return 1
  sync -f "$marker_tmp_failed" || return 1
  mv -T "$evidence_tmp" "$evidence_dir/$evidence_name" || return 1
  sync -f "$evidence_dir" || return 1
  mv -T "$marker_tmp_failed" "$evidence_dir/$marker_name" || return 1
  sync -f "$evidence_dir" >/dev/null 2>&1 || true
  [[ $result == failed_closed ]]
}

cutover_release() {
  local image=$1 release_manifest_sha=$2 activation_dir=$3
  local release_root=${RELEASE_ROOT:-$RELEASE_ROOT_DEFAULT}
  local evidence_root=${EVIDENCE_ROOT:-$EVIDENCE_ROOT_DEFAULT}
  local current_file=${CURRENT_FILE:-$CURRENT_FILE_DEFAULT}
  local runtime_program=${RUNTIME_PROGRAM:-$RUNTIME_PROGRAM_DEFAULT}
  local digest_hex release_file source_revision activation_sha run_id evidence_parent evidence_dir
  local candidate_file previous_file invocation_id main_pid nrestarts container_id
  local health_endpoint grpc_endpoint expected_identity cutover_tmp marker_tmp cutover_sha
  local candidate_pointer_sha current_pointer_sha previous_pointer_sha=''
  local previous_pointer_present=false
  local failure_reason=unknown success_ready=false
  local cleanup_armed=false success_committed=false cutover_status
  require_root
  valid_image_reference "$image" || die 'cutover requires an immutable Tokyo VPC ACR digest reference'
  [[ $release_manifest_sha =~ ^[0-9a-f]{64}$ ]] || die 'cutover requires an immutable release manifest digest'
  [[ $activation_dir == /* ]] || die 'activation directory must be absolute'
  verify_bare_tokyo_host >/dev/null
  systemctl is-active --quiet "$SERVICE" && die 'cutover requires the runtime to be stopped'
  assert_service_static || die 'cutover requires a static, boot-disabled trading service'
  digest_hex=$(release_digest_hex "$image")
  release_file=$release_root/$digest_hex/$release_manifest_sha/hft-trading-ecs-release.json
  secure_regular_file "$release_file" 0444 || die 'image was not staged by this contract'
  [[ $(sha256sum "$release_file" | awk '{print $1}') == "$release_manifest_sha" ]] \
    || die 'staged release manifest digest mismatch'
  source_revision=$(jq -er --arg image "$image" \
    'select(.image.reference == $image) | .source_revision' "$release_file") \
    || die 'staged release identity mismatch'
  activation_sha=$(sha256sum "$activation_dir/activation.sha256" | awk '{print $1}')
  run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$-$digest_hex-${release_manifest_sha:0:16}-$activation_sha
  evidence_parent=$evidence_root/cutover
  install -d -o root -g root -m 0700 "$evidence_parent" "${current_file%/*}"
  evidence_dir=$evidence_parent/$run_id
  mkdir -m 0700 "$evidence_dir" || die 'cutover evidence run already exists'
  candidate_file=$evidence_dir/candidate-current.env
  previous_file=$evidence_dir/previous-current.env
  if [[ -e $current_file ]]; then
    secure_regular_file "$current_file" 0600 \
      || die 'existing current pointer is unsafe'
    install -o root -g root -m 0400 "$current_file" "$previous_file"
    sync -f "$previous_file"
    previous_pointer_sha=$(sha256sum "$previous_file" | awk '{print $1}')
    [[ $previous_pointer_sha =~ ^[0-9a-f]{64}$ ]] \
      || die 'cannot bind the previous pointer snapshot'
    previous_pointer_present=true
  fi
  write_current_file "$candidate_file" "$image" "$release_manifest_sha" \
    "$activation_dir" "$source_revision"
  chmod 0400 "$candidate_file"
  sync -f "$candidate_file"
  candidate_pointer_sha=$(sha256sum "$candidate_file" | awk '{print $1}')
  [[ $candidate_pointer_sha =~ ^[0-9a-f]{64}$ ]] \
    || die 'cannot bind the candidate pointer snapshot'
  cleanup_armed=true
  trap 'cutover_status=$?; trap - EXIT; trap "" HUP INT TERM; if [[ $cleanup_armed == true && $success_committed != true ]]; then finalize_cutover_failure "$evidence_dir" "$previous_file" "$current_file" "$runtime_program" "$image" "$source_revision" "$release_manifest_sha" "$activation_sha" "$failure_reason" "${cutover_tmp:-}" "${marker_tmp:-}" >/dev/null 2>&1 || true; fi; exit "$cutover_status"' EXIT
  trap 'failure_reason=interrupted_SIGHUP; exit 129' HUP
  trap 'failure_reason=interrupted_SIGINT; exit 130' INT
  trap 'failure_reason=interrupted_SIGTERM; exit 143' TERM
  if ! "$runtime_program" assert-stopped; then
    failure_reason=orphan_container_detected
  elif ! HFT_CURRENT_FILE="$candidate_file" "$runtime_program" preflight; then
    failure_reason=preflight_failed
  elif ! install_current_pointer "$candidate_file" "$current_file"; then
    failure_reason=current_pointer_install_failed
  elif ! systemctl start "$SERVICE"; then
    failure_reason=systemd_start_failed
  elif ! expected_identity=$(wait_for_runtime_identity "$image"); then
    failure_reason=initial_runtime_identity_failed
  elif ! wait_for_health "$image" "$expected_identity"; then
    failure_reason=health_gate_failed
  elif ! runtime_identity_matches "$image" "$expected_identity"; then
    failure_reason=post_health_runtime_identity_failed
  else
    success_ready=true
  fi

  if [[ $success_ready == true ]]; then
    IFS='|' read -r invocation_id main_pid nrestarts container_id health_endpoint \
      grpc_endpoint <<<"$expected_identity"
    cutover_tmp=$evidence_dir/cutover.json.tmp
    marker_tmp=$evidence_dir/PASSED.sha256.tmp
    if ! current_pointer_sha=$(sha256sum "$current_file" | awk '{print $1}'); then
      failure_reason=current_pointer_hash_failed
    elif [[ $current_pointer_sha != "$candidate_pointer_sha" ]]; then
      failure_reason=current_pointer_lineage_mismatch
    elif ! jq -S -n --arg image "$image" --arg source "$source_revision" \
      --arg release_manifest "$release_manifest_sha" \
      --arg activation "$activation_sha" --arg invocation "$invocation_id" \
      --arg container_id "$container_id" \
      --arg health_endpoint "$health_endpoint" \
      --arg grpc_endpoint "$grpc_endpoint" \
      --arg candidate_pointer_sha "$candidate_pointer_sha" \
      --arg previous_pointer_sha "$previous_pointer_sha" \
      --argjson previous_pointer_present "$previous_pointer_present" \
      --argjson main_pid "$main_pid" --argjson nrestarts "$nrestarts" \
      --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" '
      {schema:"monday.hft_trading_ecs_cutover.v1",result:"passed",
        mode_boundary:"paper_or_shadow_only",image_reference:$image,
        source_revision:$source,release_manifest_sha256:$release_manifest,
        activation_manifest_sha256:$activation,
        systemd_invocation_id:$invocation,main_pid:$main_pid,
        container_id:$container_id,health_endpoint:$health_endpoint,
        grpc_endpoint:$grpc_endpoint,
        nrestarts:$nrestarts,health_samples:2,
        grpc_connect_samples:2,
        candidate_pointer_file:"candidate-current.env",
        candidate_pointer_sha256:$candidate_pointer_sha,
        current_pointer_sha256:$candidate_pointer_sha,
        previous_pointer_present:$previous_pointer_present,
        previous_pointer_file:(if $previous_pointer_present
          then "previous-current.env" else null end),
        previous_pointer_sha256:(if $previous_pointer_present
          then $previous_pointer_sha else null end),
        service_enabled:false,live_small_enabled:false,passed_at:$at}
    ' >"$cutover_tmp"; then
      failure_reason=cutover_evidence_write_failed
    elif ! cutover_sha=$(sha256sum "$cutover_tmp" | awk '{print $1}'); then
      failure_reason=cutover_evidence_hash_failed
    elif [[ ! $cutover_sha =~ ^[0-9a-f]{64}$ ]]; then
      failure_reason=cutover_evidence_hash_invalid
    elif ! printf '%s  cutover.json\n' "$cutover_sha" >"$marker_tmp"; then
      failure_reason=cutover_marker_write_failed
    elif ! chmod 0444 "$cutover_tmp" "$marker_tmp"; then
      failure_reason=cutover_evidence_chmod_failed
    elif ! sync -f "$cutover_tmp" || ! sync -f "$marker_tmp"; then
      failure_reason=cutover_evidence_sync_failed
    elif ! runtime_identity_matches "$image" "$expected_identity"; then
      failure_reason=pre_evidence_runtime_identity_failed
    elif ! mv -T "$cutover_tmp" "$evidence_dir/cutover.json"; then
      failure_reason=cutover_evidence_commit_failed
    elif ! sync -f "$evidence_dir"; then
      failure_reason=cutover_evidence_directory_sync_failed
    elif ! runtime_identity_matches "$image" "$expected_identity"; then
      failure_reason=pre_marker_runtime_identity_failed
    elif ! trap '' HUP INT TERM; then
      failure_reason=cutover_signal_mask_failed
    elif ! mv -T "$marker_tmp" "$evidence_dir/PASSED.sha256"; then
      failure_reason=cutover_marker_commit_failed
    else
      # PASSED.sha256 is the commit point. Losing the best-effort directory sync
      # can remove success after a crash, but can never manufacture success.
      success_committed=true
      cleanup_armed=false
      trap - EXIT HUP INT TERM
      sync -f "$evidence_dir" >/dev/null 2>&1 || true
      printf '%s\n' "$evidence_dir/cutover.json" || true
      return 0
    fi
  fi

  trap '' HUP INT TERM
  if ! finalize_cutover_failure "$evidence_dir" "$previous_file" "$current_file" \
    "$runtime_program" "$image" "$source_revision" "$release_manifest_sha" \
    "$activation_sha" "$failure_reason" "${cutover_tmp:-}" "${marker_tmp:-}"; then
    failure_reason=${failure_reason}_and_failure_evidence_commit_failed
    die "cutover emergency cleanup failed: $failure_reason"
  fi
  cleanup_armed=false
  trap - EXIT HUP INT TERM
  die "cutover failed closed: $failure_reason; evidence=$evidence_dir/cutover.failed.json"
}

current_pointer_matches_sha() {
  local current_file=$1 expected_sha=$2
  secure_regular_file "$current_file" 0600 || return 1
  [[ $expected_sha =~ ^[0-9a-f]{64}$ ]] || return 1
  [[ $(sha256sum "$current_file" | awk '{print $1}') == "$expected_sha" ]]
}

verify_rollback_lineage() {
  local evidence_dir=$1 current_file=$2
  local cutover_file=$evidence_dir/cutover.json
  local candidate_file=$evidence_dir/candidate-current.env
  local previous_file=$evidence_dir/previous-current.env
  local invocation main_pid nrestarts container_id health_endpoint grpc_endpoint
  ROLLBACK_IMAGE=
  ROLLBACK_EXPECTED_IDENTITY=
  ROLLBACK_CANDIDATE_SHA=
  ROLLBACK_PREVIOUS_PRESENT=
  ROLLBACK_PREVIOUS_SHA=
  secure_regular_file "$cutover_file" 0444 || return 1
  [[ $(stat_mode "$cutover_file") == 444 ]] || return 1
  jq -e -s '
    length == 1 and (.[0] |
      .schema == "monday.hft_trading_ecs_cutover.v1" and
      .result == "passed" and
      (.image_reference | type == "string") and
      (.source_revision | test("^[0-9a-f]{40}$")) and
      (.release_manifest_sha256 | test("^[0-9a-f]{64}$")) and
      (.activation_manifest_sha256 | test("^[0-9a-f]{64}$")) and
      (.systemd_invocation_id | test("^[0-9a-f]{32}$")) and
      (.main_pid | type == "number" and . > 0 and floor == .) and
      .nrestarts == 0 and
      (.container_id | test("^[0-9a-f]{64}$")) and
      (.health_endpoint | test("^127[.]0[.]0[.]1:[1-9][0-9]{0,4}$")) and
      (.grpc_endpoint | test("^127[.]0[.]0[.]1:[1-9][0-9]{0,4}$")) and
      .health_endpoint != .grpc_endpoint and
      .health_samples >= 2 and .grpc_connect_samples >= 2 and
      .candidate_pointer_file == "candidate-current.env" and
      (.candidate_pointer_sha256 | test("^[0-9a-f]{64}$")) and
      .current_pointer_sha256 == .candidate_pointer_sha256 and
      (.previous_pointer_present | type == "boolean") and
      (if .previous_pointer_present then
        .previous_pointer_file == "previous-current.env" and
        (.previous_pointer_sha256 | test("^[0-9a-f]{64}$"))
       else
        .previous_pointer_file == null and .previous_pointer_sha256 == null
       end) and
      .service_enabled == false and .live_small_enabled == false
    )
  ' "$cutover_file" >/dev/null || return 1
  ROLLBACK_IMAGE=$(jq -er '.image_reference' "$cutover_file") || return 1
  valid_image_reference "$ROLLBACK_IMAGE" || return 1
  ROLLBACK_CANDIDATE_SHA=$(jq -er '.candidate_pointer_sha256' "$cutover_file") \
    || return 1
  ROLLBACK_PREVIOUS_PRESENT=$(jq -r '.previous_pointer_present' "$cutover_file") \
    || return 1
  secure_regular_file "$candidate_file" 0400 || return 1
  [[ $(stat_mode "$candidate_file") == 400 ]] || return 1
  [[ $(sha256sum "$candidate_file" | awk '{print $1}') == "$ROLLBACK_CANDIDATE_SHA" ]] \
    || return 1
  current_pointer_matches_sha "$current_file" "$ROLLBACK_CANDIDATE_SHA" || return 1
  if [[ $ROLLBACK_PREVIOUS_PRESENT == true ]]; then
    ROLLBACK_PREVIOUS_SHA=$(jq -er '.previous_pointer_sha256' "$cutover_file") \
      || return 1
    secure_regular_file "$previous_file" 0400 || return 1
    [[ $(stat_mode "$previous_file") == 400 ]] || return 1
    [[ $(sha256sum "$previous_file" | awk '{print $1}') == "$ROLLBACK_PREVIOUS_SHA" ]] \
      || return 1
  else
    [[ $ROLLBACK_PREVIOUS_PRESENT == false && ! -e $previous_file ]] || return 1
  fi
  invocation=$(jq -er '.systemd_invocation_id' "$cutover_file") || return 1
  main_pid=$(jq -er '.main_pid | tostring' "$cutover_file") || return 1
  nrestarts=$(jq -er '.nrestarts | tostring' "$cutover_file") || return 1
  container_id=$(jq -er '.container_id' "$cutover_file") || return 1
  health_endpoint=$(jq -er '.health_endpoint' "$cutover_file") || return 1
  grpc_endpoint=$(jq -er '.grpc_endpoint' "$cutover_file") || return 1
  ROLLBACK_EXPECTED_IDENTITY=$invocation'|'$main_pid'|'$nrestarts'|'$container_id'|'$health_endpoint'|'$grpc_endpoint
}

readback_cutover() {
  local evidence_dir=$1 current_file=${CURRENT_FILE:-$CURRENT_FILE_DEFAULT}
  local runtime_program=${RUNTIME_PROGRAM:-$RUNTIME_PROGRAM_DEFAULT}
  local state_root=${STATE_ROOT:-$STATE_ROOT_DEFAULT}
  local cutover_file=$evidence_dir/cutover.json activation_dir state_dir
  local envelope_file nonce_file audit_file feedback_file feedback_line
  local runtime_uid runtime_gid activation_sha source_revision release_manifest_sha
  local deployment_id asset_revision_id promotion_id bundle_id bundle_hash
  local risk_policy_hash nonce account_id venue mode feedback_content_hash
  local cutover_sha envelope_sha policy_sha nonce_sha readback_at
  require_root
  [[ $evidence_dir == /* && -d $evidence_dir && ! -L $evidence_dir ]] \
    || die 'cutover evidence directory must be absolute'
  [[ $(canonical_directory "$evidence_dir") == "$evidence_dir" ]] \
    || die 'cutover evidence directory must be canonical'
  secure_directory "$evidence_dir" || die 'cutover evidence directory is unsafe'
  verify_single_checksum_marker "$evidence_dir" PASSED.sha256 cutover.json \
    || die 'canonical cutover success marker is invalid'
  [[ ! -e $evidence_dir/PASSED.rollback-pending.sha256 \
    && ! -e $evidence_dir/PASSED.rolled-back.sha256 ]] \
    || die 'cutover success has been revoked by rollback'
  verify_rollback_lineage "$evidence_dir" "$current_file" \
    || die 'cutover evidence is stale, legacy, or has invalid pointer lineage'
  runtime_identity_matches "$ROLLBACK_IMAGE" "$ROLLBACK_EXPECTED_IDENTITY" \
    || die 'active runtime does not match this cutover evidence'
  HFT_CURRENT_FILE="$current_file" "$runtime_program" preflight \
    || die 'current runtime no longer passes the reviewed production preflight'

  secure_regular_file "$current_file" 0600 || die 'current pointer is unsafe'
  unset HFT_TRADING_IMAGE HFT_RELEASE_MANIFEST_SHA256 HFT_ACTIVATION_DIR \
    HFT_ACTIVATION_SHA256 HFT_SOURCE_REVISION
  # The file is root-owned, content-addressed by cutover evidence, and generated
  # by write_current_file with identifier-only assignments.
  # shellcheck disable=SC1090
  source "$current_file"
  : "${HFT_TRADING_IMAGE:?missing HFT_TRADING_IMAGE}"
  : "${HFT_RELEASE_MANIFEST_SHA256:?missing HFT_RELEASE_MANIFEST_SHA256}"
  : "${HFT_ACTIVATION_DIR:?missing HFT_ACTIVATION_DIR}"
  : "${HFT_ACTIVATION_SHA256:?missing HFT_ACTIVATION_SHA256}"
  : "${HFT_SOURCE_REVISION:?missing HFT_SOURCE_REVISION}"
  activation_sha=$(jq -er '.activation_manifest_sha256' "$cutover_file") \
    || die 'cutover has no activation identity'
  source_revision=$(jq -er '.source_revision' "$cutover_file") \
    || die 'cutover has no source identity'
  release_manifest_sha=$(jq -er '.release_manifest_sha256' "$cutover_file") \
    || die 'cutover has no release identity'
  [[ $HFT_TRADING_IMAGE == "$ROLLBACK_IMAGE" \
    && $HFT_ACTIVATION_SHA256 == "$activation_sha" \
    && $HFT_SOURCE_REVISION == "$source_revision" \
    && $HFT_RELEASE_MANIFEST_SHA256 == "$release_manifest_sha" ]] \
    || die 'current pointer does not match cutover identities'

  activation_dir=$HFT_ACTIVATION_DIR
  [[ $(canonical_directory "$activation_dir") == "$activation_dir" ]] \
    || die 'activation directory is not canonical'
  envelope_file=$activation_dir/deployment/envelope.json
  jq -e -s '
    length == 1 and (.[0] |
      (.envelope | type == "object") and
      (.envelope.deployment_id | type == "string" and length > 0) and
      (.envelope.asset_revision_id | type == "string" and length > 0) and
      (.envelope.promotion_id | type == "string" and length > 0) and
      (.envelope.bundle_id | type == "string" and length > 0) and
      (.envelope.bundle_hash | test("^[0-9a-f]{64}$")) and
      (.envelope.risk_policy_hash | test("^[0-9a-f]{64}$")) and
      (.envelope.nonce | type == "string" and length > 0) and
      (.envelope.account_id | type == "string" and length > 0) and
      (.envelope.venue | type == "string" and length > 0) and
      (.envelope.allowed_intent_types | type == "array") and
      (([.envelope.allowed_intent_types[] |
        select(. == "StartPaper" or . == "StartShadow")] | length) == 1) and
      (.key_id | type == "string" and length > 0) and
      (.signature_hex | test("^[0-9a-f]{128}$"))
    )
  ' "$envelope_file" >/dev/null || die 'deployment envelope identity is invalid'
  deployment_id=$(jq -er '.envelope.deployment_id' "$envelope_file")
  asset_revision_id=$(jq -er '.envelope.asset_revision_id' "$envelope_file")
  promotion_id=$(jq -er '.envelope.promotion_id' "$envelope_file")
  bundle_id=$(jq -er '.envelope.bundle_id' "$envelope_file")
  bundle_hash=$(jq -er '.envelope.bundle_hash' "$envelope_file")
  risk_policy_hash=$(jq -er '.envelope.risk_policy_hash' "$envelope_file")
  nonce=$(jq -er '.envelope.nonce' "$envelope_file")
  account_id=$(jq -er '.envelope.account_id' "$envelope_file")
  venue=$(jq -er '.envelope.venue' "$envelope_file")
  mode=$(jq -er 'if (.envelope.allowed_intent_types | index("StartPaper"))
    then "Paper" else "Shadow" end' "$envelope_file")

  runtime_uid=${EXPECTED_RUNTIME_UID:-$(id -u "$HFT_RUNTIME_USER_DEFAULT")}
  runtime_gid=${EXPECTED_RUNTIME_GID:-$(id -g "$HFT_RUNTIME_USER_DEFAULT")}
  [[ $runtime_uid =~ ^[0-9]+$ && $runtime_gid =~ ^[0-9]+$ ]] \
    || die 'runtime account identity is invalid'
  [[ $(canonical_directory "$state_root") == "$state_root" ]] \
    || die 'runtime state root is not canonical'
  secure_directory "$state_root" || die 'runtime state root is unsafe'
  state_dir=$state_root/$activation_sha
  [[ -d $state_dir && ! -L $state_dir \
    && $(stat_uid "$state_dir") == "$runtime_uid" \
    && $(stat_gid "$state_dir") == "$runtime_gid" \
    && $(stat_mode "$state_dir") == 700 ]] \
    || die 'runtime activation state directory is unsafe'
  nonce_file=$state_dir/nonces.jsonl
  audit_file=$state_dir/audit.jsonl
  feedback_file=$state_dir/feedback.jsonl
  for state_file in "$nonce_file" "$audit_file" "$feedback_file"; do
    [[ -f $state_file && ! -L $state_file \
      && $(stat_uid "$state_file") == "$runtime_uid" \
      && $(stat_gid "$state_file") == "$runtime_gid" ]] \
      || die 'runtime governance state file is unsafe'
  done
  jq -e -s --arg nonce "$nonce" --arg deployment "$deployment_id" '
    length == 1 and .[0].nonce == $nonce and
    .[0].deployment_id == $deployment and
    (.[0].accepted_at | type == "string" and length > 0)
  ' "$nonce_file" >/dev/null || die 'runtime nonce consumption does not match the envelope'
  jq -e -s --arg deployment "$deployment_id" '
    length >= 3 and all(.[]; .deployment_id == $deployment and
      (.recorded_at | type == "string" and length > 0)) and
    .[-3].phase == "pre_activation" and .[-3].result == "verified" and
      .[-3].reason == null and
    .[-2].phase == "configuration" and .[-2].result == "prepared" and
      .[-2].reason == null and
    .[-1].phase == "runtime" and .[-1].result == "activated" and
      .[-1].reason == null
  ' "$audit_file" >/dev/null || die 'runtime activation audit is incomplete or mismatched'
  feedback_line=$(sed -n '1p' "$feedback_file")
  [[ -n $feedback_line ]] || die 'runtime activation feedback is absent'
  jq -e --arg deployment "$deployment_id" --arg asset "$asset_revision_id" \
    --arg account "$account_id" --arg venue "$venue" --arg mode "$mode" '
    .key_id == "runtime-feedback-1" and
    (.content_hash | test("^[0-9a-f]{64}$")) and
    (.signature_hex | test("^[0-9a-f]{128}$")) and
    .event.event_id == ("activation:" + $deployment) and
    .event.deployment_id == $deployment and
    .event.asset_revision_id == $asset and
    .event.mode == $mode and .event.outcome == "Activated" and
    .event.kind == "Activation" and .event.account_id == $account and
    .event.venue == $venue and .event.reason == null
  ' <<<"$feedback_line" >/dev/null \
    || die 'signed runtime activation feedback is incomplete or mismatched'

  cutover_sha=$(sha256sum "$cutover_file" | awk '{print $1}')
  envelope_sha=$(sha256sum "$envelope_file" | awk '{print $1}')
  policy_sha=$(sha256sum "$activation_dir/deployment/policy.json" | awk '{print $1}')
  nonce_sha=$(printf '%s' "$nonce" | sha256sum | awk '{print $1}')
  feedback_content_hash=$(jq -er '.content_hash' <<<"$feedback_line")
  readback_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  if ! verify_single_checksum_marker "$evidence_dir" PASSED.sha256 cutover.json \
    || [[ -e $evidence_dir/PASSED.rollback-pending.sha256 \
      || -e $evidence_dir/PASSED.rolled-back.sha256 ]] \
    || ! current_pointer_matches_sha "$current_file" "$ROLLBACK_CANDIDATE_SHA" \
    || ! runtime_identity_matches "$ROLLBACK_IMAGE" "$ROLLBACK_EXPECTED_IDENTITY"; then
    die 'runtime identity changed during readback'
  fi
  jq -S -n --arg cutover "$cutover_sha" --arg image "$ROLLBACK_IMAGE" \
    --arg source "$source_revision" --arg release "$release_manifest_sha" \
    --arg activation "$activation_sha" --arg envelope "$envelope_sha" \
    --arg policy "$policy_sha" --arg deployment "$deployment_id" \
    --arg asset "$asset_revision_id" --arg promotion "$promotion_id" \
    --arg bundle "$bundle_id" --arg bundle_hash "$bundle_hash" \
    --arg risk "$risk_policy_hash" --arg nonce "$nonce_sha" \
    --arg feedback "$feedback_content_hash" --arg mode "$mode" \
    --arg at "$readback_at" '
    {schema:"monday.hft_trading_ecs_readback.v1",result:"active_governed",
      cutover_sha256:$cutover,image_reference:$image,source_revision:$source,
      release_manifest_sha256:$release,
      activation_manifest_sha256:$activation,envelope_sha256:$envelope,
      policy_sha256:$policy,deployment_id:$deployment,
      asset_revision_id:$asset,promotion_id:$promotion,bundle_id:$bundle,
      bundle_hash:$bundle_hash,risk_policy_hash:$risk,nonce_sha256:$nonce,
      activation_feedback_content_hash:$feedback,mode:$mode,
      nonce_consumed:true,audit_activated:true,
      activation_feedback_signature_present:true,service_enabled:false,
      automatic_restart_enabled:false,live_small_enabled:false,readback_at:$at}
  '
}

rollback_cutover() {
  local evidence_dir=$1 current_file=${CURRENT_FILE:-$CURRENT_FILE_DEFAULT}
  local runtime_program=${RUNTIME_PROGRAM:-$RUNTIME_PROGRAM_DEFAULT}
  local previous_file=$evidence_dir/previous-current.env
  local marker=$evidence_dir/PASSED.sha256
  local pending_marker=$evidence_dir/PASSED.rollback-pending.sha256
  local rolled_back_marker=$evidence_dir/PASSED.rolled-back.sha256
  local pending_tmp=$evidence_dir/.PASSED.rollback-pending.sha256.tmp.$$
  local rolled_back_tmp=$evidence_dir/.PASSED.rolled-back.sha256.tmp.$$
  local rollback_at cutover_sha
  require_root
  [[ $evidence_dir == /* && -d $evidence_dir && ! -L $evidence_dir ]] \
    || die 'cutover evidence directory must be absolute'
  secure_directory "$evidence_dir" || die 'cutover evidence directory is unsafe'
  verify_single_checksum_marker "$evidence_dir" PASSED.sha256 cutover.json \
    || die 'canonical cutover success marker is invalid'
  [[ ! -e $pending_marker && ! -e $rolled_back_marker ]] \
    || die 'rollback evidence already exists'
  verify_rollback_lineage "$evidence_dir" "$current_file" \
    || die 'cutover evidence is stale, legacy, or has invalid pointer lineage'
  runtime_identity_matches "$ROLLBACK_IMAGE" "$ROLLBACK_EXPECTED_IDENTITY" \
    || die 'active runtime does not match this cutover evidence'
  install -m 0444 "$marker" "$pending_tmp"
  mv -T "$pending_tmp" "$pending_marker"
  sync -f "$evidence_dir"
  if ! current_pointer_matches_sha "$current_file" "$ROLLBACK_CANDIDATE_SHA" \
    || ! runtime_identity_matches "$ROLLBACK_IMAGE" "$ROLLBACK_EXPECTED_IDENTITY"; then
    die "rollback lineage changed; candidate was not stopped; marker held at $pending_marker"
  fi
  systemctl stop "$SERVICE" >/dev/null 2>&1 || true
  "$runtime_program" ensure-stopped \
    || die "rollback stop failed; canonical marker invalidated at $pending_marker"
  if [[ $ROLLBACK_PREVIOUS_PRESENT == true ]]; then
    install_current_pointer "$previous_file" "$current_file"
    current_pointer_matches_sha "$current_file" "$ROLLBACK_PREVIOUS_SHA" \
      || die "rollback pointer verification failed; marker held at $pending_marker"
  else
    rm -f -- "$current_file"
    sync -f "${current_file%/*}"
    [[ ! -e $current_file ]] \
      || die "rollback pointer removal failed; marker held at $pending_marker"
  fi
  install -m 0444 "$pending_marker" "$rolled_back_tmp"
  mv -T "$rolled_back_tmp" "$rolled_back_marker"
  rollback_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  cutover_sha=$(sha256sum "$evidence_dir/cutover.json" | awk '{print $1}')
  jq -S -n --arg cutover "$cutover_sha" \
    --arg candidate "$ROLLBACK_CANDIDATE_SHA" \
    --arg previous "$ROLLBACK_PREVIOUS_SHA" \
    --argjson previous_present "$ROLLBACK_PREVIOUS_PRESENT" \
    --arg at "$rollback_at" '
    {schema:"monday.hft_trading_ecs_rollback.v1",result:"rolled_back_stopped",
      cutover_sha256:$cutover,runtime_stopped:true,
      candidate_pointer_sha256:$candidate,
      previous_pointer_present:$previous_present,
      previous_pointer_sha256:(if $previous_present then $previous else null end),
      previous_pointer_restored:true,previous_runtime_restarted:false,
      operator_action_required:"new signed envelope and nonce before restart",
      rolled_back_at:$at}
  ' >"$evidence_dir/rollback.json"
  (cd "$evidence_dir" && sha256sum rollback.json >ROLLED_BACK.sha256)
  chmod 0444 "$rolled_back_marker" \
    "$evidence_dir/rollback.json" "$evidence_dir/ROLLED_BACK.sha256"
  sync -f "$evidence_dir"
  printf '%s\n' "$evidence_dir/rollback.json"
}

usage() {
  cat <<'USAGE'
Usage:
  trading-ecs-hostctl.sh stage ARTIFACT_DIR ACR_USERNAME ACR_PASSWORD_FILE
  trading-ecs-hostctl.sh cutover IMAGE_REFERENCE RELEASE_MANIFEST_SHA256 ACTIVATION_DIR
  trading-ecs-hostctl.sh readback CUTOVER_EVIDENCE_DIR
  trading-ecs-hostctl.sh rollback CUTOVER_EVIDENCE_DIR

The host must be a Tokyo Ubuntu 26.04 amd64 ECS with MondayTradingEcsRole.
stage never starts or enables the service. cutover accepts Paper/Shadow only.
readback never starts, stops, restarts, or enables the service.
USAGE
}

with_host_lock() {
  local lock_file=${HOST_LOCK:-$HOST_LOCK_DEFAULT}
  require_root
  command -v flock >/dev/null 2>&1 || die 'flock is required for host operations'
  install -d -o root -g root -m 0755 "${lock_file%/*}"
  exec 9>"$lock_file"
  chmod 0600 "$lock_file"
  flock -n 9 || die 'another trading host operation holds the lock'
  "$@"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  case ${1:-} in
    stage)
      [[ $# -eq 4 ]] || { usage; exit 2; }
      with_host_lock stage_release "$2" "$3" "$4"
      ;;
    cutover)
      [[ $# -eq 4 ]] || { usage; exit 2; }
      with_host_lock cutover_release "$2" "$3" "$4"
      ;;
    readback)
      [[ $# -eq 2 ]] || { usage; exit 2; }
      with_host_lock readback_cutover "$2"
      ;;
    rollback)
      [[ $# -eq 2 ]] || { usage; exit 2; }
      with_host_lock rollback_cutover "$2"
      ;;
    *) usage; exit 2 ;;
  esac
fi
