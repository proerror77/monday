#!/usr/bin/env bash
# shellcheck disable=SC1090,SC2016,SC2034,SC2094,SC2251,SC2317,SC2329
set -Eeuo pipefail

export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
readonly SCRIPT_DIR
readonly HOSTCTL=$SCRIPT_DIR/trading-ecs-hostctl.sh
readonly RUNTIME=$SCRIPT_DIR/trading-ecs-runtime.sh
readonly POLICY=$SCRIPT_DIR/trading-ecs-paper-shadow-policy.jq
readonly UNIT=$SCRIPT_DIR/hft-trading-ecs.service
readonly WORKFLOW=$SCRIPT_DIR/../../.github/workflows/acr-publish.yml

for command in awk chmod cp find grep jq ln mkfifo mktemp rm sed sha256sum shellcheck sort; do
  command -v "$command" >/dev/null 2>&1 \
    || { printf 'missing trading host contract test dependency: %s\n' "$command" >&2; exit 2; }
done

shellcheck "$HOSTCTL" "$RUNTIME" "$0"
bash -n "$HOSTCTL" "$RUNTIME" "$0"
if printf '{}\n' | jq -e -s -f "$POLICY" --argjson policy '[]' \
  >/dev/null 2>&1; then
  printf 'empty policy unexpectedly passed\n' >&2
  exit 1
fi

tmp_dir=$(mktemp -d "$SCRIPT_DIR/.host-contract-test.XXXXXX")
tmp_dir=$(cd -- "$tmp_dir" && pwd -P)
trap 'chmod -R u+w "$tmp_dir" 2>/dev/null || true; rm -rf "$tmp_dir"' EXIT

valid_image="crpi-ygobwehhof7qs9m3-vpc.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/hft-trading@sha256:$(printf 'a%.0s' {1..64})"

(
  source "$HOSTCTL"
  valid_image_reference "$valid_image"
  ! valid_image_reference "${valid_image%@*}:latest"
  ! valid_image_reference "${valid_image%@*}:2b82d590"
  ! valid_image_reference "${valid_image/-vpc/}"
  ! valid_image_reference "${valid_image/ap-northeast-1/ap-southeast-1}"
  ! valid_image_reference "${valid_image/hft-trading/research-runner}"
  ! valid_image_reference "${valid_image%@*}:release@${valid_image##*@}"
)

# A systemd static unit reports success from is-enabled. Accept exactly the
# textual static state, while rejecting enabled, linked, generated, or missing.
(
  source "$HOSTCTL"
  systemctl_state=static
  systemctl() {
    [[ $1 == is-enabled ]] || return 2
    [[ $systemctl_state != missing ]] || return 1
    printf '%s\n' "$systemctl_state"
  }
  assert_service_static
  systemctl_state=enabled
  ! assert_service_static
  systemctl_state=linked
  ! assert_service_static
  systemctl_state=missing
  ! assert_service_static
)

# The selected unit file is insufficient by itself: systemd's loaded fragment,
# drop-ins, and effective command vectors must remain exactly fail-closed.
(
  source "$HOSTCTL"
  unit_path=$tmp_dir/effective.service
  runtime_program=/usr/local/libexec/monday-hft-trading-runtime
  mock_fragment=$unit_path
  mock_dropins=
  mock_restart=no
  mock_start_post=
  mock_start_pre="{ path=$runtime_program ; argv[]=$runtime_program preflight ; ignore_errors=no ; }"
  mock_start="{ path=$runtime_program ; argv[]=$runtime_program run ; ignore_errors=no ; }"
  mock_stop="{ path=$runtime_program ; argv[]=$runtime_program stop ; ignore_errors=no ; }"
  mock_stop_post="{ path=$runtime_program ; argv[]=$runtime_program ensure-stopped ; ignore_errors=no ; }"
  systemctl() {
    case $1 in
      is-enabled) printf 'static\n' ;;
      show)
        case ${3#--property=} in
          FragmentPath) printf '%s\n' "$mock_fragment" ;;
          DropInPaths) printf '%s\n' "$mock_dropins" ;;
          Restart) printf '%s\n' "$mock_restart" ;;
          ExecStartPost) printf '%s\n' "$mock_start_post" ;;
          ExecStartPre) printf '%s\n' "$mock_start_pre" ;;
          ExecStart) printf '%s\n' "$mock_start" ;;
          ExecStop) printf '%s\n' "$mock_stop" ;;
          ExecStopPost) printf '%s\n' "$mock_stop_post" ;;
          *) return 2 ;;
        esac
        ;;
      *) return 2 ;;
    esac
  }
  assert_effective_service_contract "$unit_path" "$runtime_program"
  mock_start_pre="{ path=$runtime_program ; argv[]=$runtime_program preflight ; ignore_errors=yes ; }"
  ! assert_effective_service_contract "$unit_path" "$runtime_program"
  mock_start_pre="{ path=$runtime_program ; argv[]=$runtime_program preflight ; ignore_errors=no ; }"
  mock_dropins=/etc/systemd/system/monday-hft-trading.service.d/stale.conf
  mock_start="{ path=/bin/false ; argv[]=/bin/false ; ignore_errors=no ; }"
  ! assert_effective_service_contract "$unit_path" "$runtime_program"
  mock_dropins=
  ! assert_effective_service_contract "$unit_path" "$runtime_program"
  mock_start="{ path=$runtime_program ; argv[]=$runtime_program run ; ignore_errors=no ; }"
  mock_start_post="{ path=/bin/false ; argv[]=/bin/false ; ignore_errors=no ; }"
  ! assert_effective_service_contract "$unit_path" "$runtime_program"
  mock_start_post=
  mock_restart=always
  ! assert_effective_service_contract "$unit_path" "$runtime_program"
  mock_restart=no
  mock_fragment=/run/systemd/system/monday-hft-trading.service
  ! assert_effective_service_contract "$unit_path" "$runtime_program"
  dropin_root=$tmp_dir/dropin-root
  mkdir -p "$dropin_root/$SERVICE.d"
  ! assert_no_service_dropin_directories "$dropin_root"
)

# Runtime carries the same effective-unit check because it is the actual
# ExecStartPre boundary, not merely a staging-time assertion.
(
  source "$RUNTIME"
  unit_path=/etc/systemd/system/monday-hft-trading.service
  runtime_program=/usr/local/libexec/monday-hft-trading-runtime
  mock_dropins=
  systemctl() {
    case $1 in
      is-enabled) printf 'static\n' ;;
      show)
        case ${3#--property=} in
          FragmentPath) printf '%s\n' "$unit_path" ;;
          DropInPaths) printf '%s\n' "$mock_dropins" ;;
          Restart) printf 'no\n' ;;
          ExecStartPost) printf '\n' ;;
          ExecStartPre|ExecStart|ExecStop|ExecStopPost)
            case ${3#--property=} in
              ExecStartPre) argument=preflight ;;
              ExecStart) argument=run ;;
              ExecStop) argument=stop ;;
              ExecStopPost) argument=ensure-stopped ;;
            esac
            printf '{ path=%s ; argv[]=%s %s ; ignore_errors=no ; }\n' \
              "$runtime_program" "$runtime_program" "$argument"
            ;;
          *) return 2 ;;
        esac
        ;;
      *) return 2 ;;
    esac
  }
  assert_effective_service_contract "$unit_path" "$runtime_program"
  mock_dropins=/run/systemd/system/monday-hft-trading.service.d/stale.conf
  ! assert_effective_service_contract "$unit_path" "$runtime_program"
)

(
  source "$HOSTCTL"
  unit_path=$tmp_dir/pre-stage.service
  service_state=static
  service_status=0
  systemctl() {
    [[ $1 == is-enabled ]] || return 2
    [[ -z $service_state ]] || printf '%s\n' "$service_state"
    return "$service_status"
  }
  assert_service_boot_disabled_before_stage "$unit_path"
  service_state=disabled
  service_status=1
  assert_service_boot_disabled_before_stage "$unit_path"
  service_state=not-found
  service_status=1
  assert_service_boot_disabled_before_stage "$unit_path"
  service_state=enabled
  service_status=0
  ! assert_service_boot_disabled_before_stage "$unit_path"
  service_state=linked
  ! assert_service_boot_disabled_before_stage "$unit_path"
  service_state=
  service_status=1
  : >"$unit_path"
  ! assert_service_boot_disabled_before_stage "$unit_path"
  rm "$unit_path"
  ! assert_service_boot_disabled_before_stage "$unit_path"
  service_state=static
  ! assert_service_boot_disabled_before_stage "$unit_path"
  service_state=disabled
  service_status=0
  ! assert_service_boot_disabled_before_stage "$unit_path"
)

(
  source "$HOSTCTL"
  docker_state=healthy
  docker_names=
  docker() {
    case "${1:-} ${2:-}" in
      'info ') [[ $docker_state == healthy ]] ;;
      'container ls')
        [[ $docker_state != list-error ]] || return 1
        [[ $3 == --all && $4 == --filter \
          && $5 == 'name=^/monday-hft-trading$' && $6 == --format \
          && $7 == '{{.Names}}' ]] || return 2
        printf '%s\n' "$docker_names"
        ;;
      *) return 2 ;;
    esac
  }
  assert_no_orphan_container
  docker_names=monday-hft-trading
  ! assert_no_orphan_container
  docker_names=
  docker_state=list-error
  ! assert_no_orphan_container
  docker_state=down
  ! assert_no_orphan_container
)

(
  source "$RUNTIME"
  valid_image_reference "$valid_image"
  [[ $(image_digest_hex "$valid_image") == "$(printf 'a%.0s' {1..64})" ]]
  ! valid_image_reference "${valid_image%@*}:latest"
  validate_bare_host_identity_values ubuntu 26.04 amd64 ap-northeast-1 \
    MondayTradingEcsRole i-example123
  ! validate_bare_host_identity_values ubuntu 24.04 amd64 ap-northeast-1 \
    MondayTradingEcsRole i-example123
  ! validate_bare_host_identity_values ubuntu 26.04 amd64 ap-northeast-1 \
    WrongRole i-example123
)

# Exercise the complete start-time host check with metadata-v2 and systemd
# behavior mocked at their command boundaries.
(
  source "$RUNTIME"
  mock_os=ubuntu
  mock_version=26.04
  mock_arch=amd64
  mock_region=ap-northeast-1
  mock_role=MondayTradingEcsRole
  mock_instance=i-example123
  kubelet_active=false
  source() {
    [[ $1 == /etc/os-release ]] || return 2
    ID=$mock_os
    VERSION_ID=$mock_version
  }
  systemctl() {
    [[ $1 == is-active && $2 == --quiet && $3 == kubelet ]] || return 2
    [[ $kubelet_active == true ]]
  }
  dpkg() {
    [[ $1 == --print-architecture ]] || return 2
    printf '%s\n' "$mock_arch"
  }
  metadata_token() { printf 'metadata-v2-token\n'; }
  metadata_get() {
    case $2 in
      region-id) printf '%s\n' "$mock_region" ;;
      ram/security-credentials/) printf '%s\n' "$mock_role" ;;
      instance-id) printf '%s\n' "$mock_instance" ;;
      *) return 2 ;;
    esac
  }
  verify_bare_tokyo_host
  mock_region=ap-southeast-1
  ! verify_bare_tokyo_host
  mock_region=ap-northeast-1
  mock_role=WrongRole
  ! verify_bare_tokyo_host
  mock_role=MondayTradingEcsRole
  mock_version=24.04
  ! verify_bare_tokyo_host
  mock_version=26.04
  mock_arch=arm64
  ! verify_bare_tokyo_host
  mock_arch=amd64
  kubelet_active=true
  ! verify_bare_tokyo_host
)

# Absence is accepted only with an independently healthy Docker daemon. A
# daemon error, a stopped orphan, or an exact-name listing residue all fail.
(
  source "$RUNTIME"
  docker_scenario=daemon_down
  docker() {
    if [[ $1 == container && $2 == inspect ]]; then
      if [[ $docker_scenario == stopped ]]; then
        printf 'false\n'
        return 0
      fi
      return 1
    fi
    if [[ $1 == info ]]; then
      [[ $docker_scenario != daemon_down ]]
      return
    fi
    if [[ $1 == container && $2 == ls ]]; then
      [[ $docker_scenario == stale_listing ]] && printf '%s\n' "$CONTAINER_NAME"
      return 0
    fi
    return 2
  }
  ! assert_stopped
  docker_scenario=stopped
  ! assert_stopped
  docker_scenario=stale_listing
  ! assert_stopped
  docker_scenario=absent
  assert_stopped
)

acr_auth_root=$tmp_dir/acr-auth
mkdir "$acr_auth_root"
printf '%s\n' example_registry_password >"$acr_auth_root/password"
chmod 0700 "$acr_auth_root"
chmod 0400 "$acr_auth_root/password"
(
  source "$HOSTCTL"
  EXPECTED_ROOT_UID=$(id -u)
  ACR_AUTH_ROOT=$acr_auth_root
  findmnt() { printf '%s\n' tmpfs; }
  verify_acr_password_file "$acr_auth_root/password"
  chmod 0600 "$acr_auth_root/password"
  printf '\n' >"$acr_auth_root/password"
  chmod 0400 "$acr_auth_root/password"
  ! (verify_acr_password_file "$acr_auth_root/password" 2>/dev/null)
  chmod 0600 "$acr_auth_root/password"
  printf '%s\n' example_registry_password >"$acr_auth_root/password"
  chmod 0400 "$acr_auth_root/password"
  ! (verify_acr_password_file \
    "$acr_auth_root/../${acr_auth_root##*/}/password" 2>/dev/null)
  findmnt() { printf '%s\n' ext4; }
  ! (verify_acr_password_file "$acr_auth_root/password" 2>/dev/null)
  findmnt() {
    local argument target=
    for argument in "$@"; do target=$argument; done
    if [[ $target == "$acr_auth_root/password" ]]; then
      printf '%s\n' ext4
    else
      printf '%s\n' tmpfs
    fi
  }
  ! (verify_acr_password_file "$acr_auth_root/password" 2>/dev/null)
)

activation_dir=$tmp_dir/activation
mkdir -p "$activation_dir/config" "$activation_dir/deployment"
cat >"$activation_dir/config/system.yaml" <<'YAML'
version: "2.0"
venues: []
YAML
printf '%s\n' '{"bundle_id":"example"}' >"$activation_dir/deployment/bundle.json"
cat >"$activation_dir/deployment/envelope.json" <<'JSON'
{"envelope":{"allowed_intent_types":["LoadFactor","StartShadow"],"approval_class":"Shadow"},"key_id":"example-key","signature":"example-signature"}
JSON
cat >"$activation_dir/deployment/policy.json" <<'JSON'
{"account_id":"example","venue":"binance","allowed_instruments":["BTCUSDT"],"allowed_intent_types":["LoadFactor","StartShadow"],"runtime_paused":false,"approvals":[{"approval_class":"Shadow"}]}
JSON
printf '%s\n' '{"example-key":"example-public-key"}' \
  >"$activation_dir/deployment/trusted-keys.json"
(
  cd "$activation_dir"
  find . -type f ! -path ./activation.sha256 -print \
    | sed 's#^./##' | LC_ALL=C sort \
    | while IFS= read -r path; do sha256sum "$path"; done \
    >activation.sha256
)
chmod 0444 "$activation_dir/activation.sha256"

# The cutover-owned candidate snapshot must remain root-readable by runtime
# preflight while exposing no group/world bits. This crosses the hostctl writer
# and runtime reader rather than testing either mode rule in isolation.
candidate_mode_file=$tmp_dir/candidate-mode.env
(
  source "$HOSTCTL"
  write_current_file "$candidate_mode_file" "$valid_image" \
    "$(printf 'b%.0s' {1..64})" "$activation_dir" \
    "$(printf 'c%.0s' {1..40})"
  chmod 0400 "$candidate_mode_file"
)
(
  source "$RUNTIME"
  EXPECTED_ROOT_UID=$(id -u)
  HFT_CURRENT_FILE=$candidate_mode_file
  load_current
  [[ $HFT_TRADING_IMAGE == "$valid_image" ]]
  [[ $HFT_ACTIVATION_DIR == "$activation_dir" ]]
)
chmod 0444 "$candidate_mode_file"
if (
  source "$RUNTIME"
  EXPECTED_ROOT_UID=$(id -u)
  HFT_CURRENT_FILE=$candidate_mode_file
  load_current
) >/dev/null 2>&1; then
  printf 'group/world-readable candidate pointer unexpectedly passed\n' >&2
  exit 1
fi

control_root=$tmp_dir/control
mkdir "$control_root"
cp "$POLICY" "$control_root/"

(
  source "$RUNTIME"
  EXPECTED_ROOT_UID=0
  stat_uid() { printf '0\n'; }
  ACTIVATION_ROOT=$tmp_dir
  CONTROL_ROOT=$control_root
  validate_activation_manifest "$activation_dir"
  validate_paper_shadow_authority "$activation_dir"
  path_root=$tmp_dir/activation-path-components
  unsafe_parent=$path_root/world-writable
  mkdir -p "$unsafe_parent"
  cp -R "$activation_dir" "$unsafe_parent/candidate"
  chmod 0777 "$unsafe_parent"
  ACTIVATION_ROOT=$path_root
  ! validate_activation_manifest "$unsafe_parent/candidate"
  real_parent=$path_root/real-parent
  mkdir -p "$real_parent"
  cp -R "$activation_dir" "$real_parent/candidate"
  ln -s "$real_parent" "$path_root/symlink-parent"
  ! validate_activation_manifest "$path_root/symlink-parent/candidate"
  ACTIVATION_ROOT=$tmp_dir
  mkdir "$activation_dir/nested"
  printf 'nested checksum input\n' >"$activation_dir/nested/activation.sha256"
  ! validate_activation_manifest "$activation_dir"
  chmod 0644 "$activation_dir/activation.sha256"
  (
    cd "$activation_dir"
    find . -type f ! -path ./activation.sha256 -print \
      | sed 's#^./##' | LC_ALL=C sort \
      | while IFS= read -r path; do sha256sum "$path"; done \
      >activation.sha256
  )
  chmod 0444 "$activation_dir/activation.sha256"
  validate_activation_manifest "$activation_dir"
  cp "$activation_dir/deployment/envelope.json" "$tmp_dir/valid-envelope.json"
  {
    cat "$tmp_dir/valid-envelope.json"
    cat "$tmp_dir/valid-envelope.json"
  } >"$activation_dir/deployment/envelope.json"
  ! validate_paper_shadow_authority "$activation_dir"
  cp "$tmp_dir/valid-envelope.json" "$activation_dir/deployment/envelope.json"
  jq '.envelope.allowed_intent_types = ["LoadFactor","StartLiveSmall"] |
      .envelope.approval_class = "HumanApprovedLiveSmall"' \
    "$activation_dir/deployment/envelope.json" >"$tmp_dir/live-envelope.json"
  cp "$tmp_dir/live-envelope.json" "$activation_dir/deployment/envelope.json"
  ! validate_paper_shadow_authority "$activation_dir"
)

# Restore Shadow authority, then prove policy-side live permission also fails.
cat >"$activation_dir/deployment/envelope.json" <<'JSON'
{"envelope":{"allowed_intent_types":["LoadFactor","StartShadow"],"approval_class":"Shadow"},"key_id":"example-key","signature":"example-signature"}
JSON
(
  source "$RUNTIME"
  CONTROL_ROOT=$control_root
  jq '.envelope.allowed_intent_types += ["StartWithdraw"]' \
    "$activation_dir/deployment/envelope.json" >"$tmp_dir/unknown-start-envelope.json"
  cp "$tmp_dir/unknown-start-envelope.json" "$activation_dir/deployment/envelope.json"
  ! validate_paper_shadow_authority "$activation_dir"
)
cat >"$activation_dir/deployment/envelope.json" <<'JSON'
{"envelope":{"allowed_intent_types":["LoadFactor","StartShadow"],"approval_class":"Shadow"},"key_id":"example-key","signature":"example-signature"}
JSON
jq '.allowed_intent_types = ["LoadFactor","StartPaper"]' \
  "$activation_dir/deployment/policy.json" >"$tmp_dir/wrong-start-policy.json"
cp "$tmp_dir/wrong-start-policy.json" "$activation_dir/deployment/policy.json"
(
  source "$RUNTIME"
  CONTROL_ROOT=$control_root
  ! validate_paper_shadow_authority "$activation_dir"
)
cat >"$activation_dir/deployment/policy.json" <<'JSON'
{"account_id":"example","venue":"binance","allowed_instruments":["BTCUSDT"],"allowed_intent_types":["LoadFactor","StartShadow"],"runtime_paused":false,"approvals":[{"approval_class":"Shadow"}]}
JSON
jq '.envelope.allowed_intent_types += ["LoadAllocatorPolicy"]' \
  "$activation_dir/deployment/envelope.json" >"$tmp_dir/allocator-envelope.json"
cp "$tmp_dir/allocator-envelope.json" "$activation_dir/deployment/envelope.json"
(
  source "$RUNTIME"
  CONTROL_ROOT=$control_root
  ! validate_paper_shadow_authority "$activation_dir"
)
cat >"$activation_dir/deployment/envelope.json" <<'JSON'
{"envelope":{"allowed_intent_types":["LoadFactor","StartShadow"],"approval_class":"Shadow"},"key_id":"example-key","signature":"example-signature"}
JSON
jq '.allowed_intent_types += ["LoadAllocatorPolicy"]' \
  "$activation_dir/deployment/policy.json" >"$tmp_dir/allocator-policy.json"
cp "$tmp_dir/allocator-policy.json" "$activation_dir/deployment/policy.json"
(
  source "$RUNTIME"
  CONTROL_ROOT=$control_root
  ! validate_paper_shadow_authority "$activation_dir"
)
cat >"$activation_dir/deployment/policy.json" <<'JSON'
{"account_id":"example","venue":"binance","allowed_instruments":["BTCUSDT"],"allowed_intent_types":["LoadFactor","StartShadow"],"runtime_paused":false,"approvals":[{"approval_class":"Shadow"}]}
JSON
jq '.envelope.allowed_intent_types = ["LoadModel","StartShadow"]' \
  "$activation_dir/deployment/envelope.json" >"$tmp_dir/model-envelope.json"
cp "$tmp_dir/model-envelope.json" "$activation_dir/deployment/envelope.json"
jq '.allowed_intent_types = ["LoadModel","StartShadow"]' \
  "$activation_dir/deployment/policy.json" >"$tmp_dir/model-policy.json"
cp "$tmp_dir/model-policy.json" "$activation_dir/deployment/policy.json"
(
  source "$RUNTIME"
  CONTROL_ROOT=$control_root
  ! validate_paper_shadow_authority "$activation_dir"
)
cat >"$activation_dir/deployment/envelope.json" <<'JSON'
{"envelope":{"allowed_intent_types":["LoadFactor","StartShadow"],"approval_class":"Shadow"},"key_id":"example-key","signature":"example-signature"}
JSON
cat >"$activation_dir/deployment/policy.json" <<'JSON'
{"account_id":"example","venue":"binance","allowed_instruments":["BTCUSDT"],"allowed_intent_types":["LoadFactor","StartShadow"],"runtime_paused":false,"approvals":[{"approval_class":"Shadow"}]}
JSON
jq '.allowed_intent_types += ["StartLiveSmall"]' \
  "$activation_dir/deployment/policy.json" >"$tmp_dir/live-policy.json"
cp "$tmp_dir/live-policy.json" "$activation_dir/deployment/policy.json"
(
  source "$RUNTIME"
  CONTROL_ROOT=$control_root
  ! validate_paper_shadow_authority "$activation_dir"
)

secret_root=$tmp_dir/secrets
mkdir "$secret_root"
cat >"$secret_root/runtime.env" <<'ENV'
HFT_GRPC_AUTH_TOKEN=example_token_with_at_least_32_chars
ENV
printf '%s=%s\n' HFT_SECRET_BINANCE_ACCOUNT_JSON \
  "$(jq -cn --arg runtime_account_id binance_main --arg api_key example_key \
    --arg secret example_secret '{$runtime_account_id,$api_key,$secret}')" \
  >>"$secret_root/runtime.env"
printf '%s\n' "$(printf 'b%.0s' {1..64})" >"$secret_root/feedback-signing-key.hex"
chmod 0750 "$secret_root"
chmod 0440 "$secret_root/runtime.env"
chmod 0440 "$secret_root/feedback-signing-key.hex"
(
  source "$RUNTIME"
  EXPECTED_ROOT_UID=$(id -u)
  getent() {
    case $1 in
      passwd) printf 'mondayhft:x:991:991::/nonexistent:/usr/sbin/nologin\n' ;;
      group) printf 'mondayhft:x:991:\n' ;;
      *) return 2 ;;
    esac
  }
  id() {
    [[ $1 == -g && $2 == mondayhft ]] || return 2
    printf '991\n'
  }
  stat_gid() { printf '991\n'; }
  SECRET_ROOT=$secret_root
  findmnt() { printf '%s\n' tmpfs; }
  validate_runtime_secrets
  binding_activation=$tmp_dir/binding-activation
  mkdir -p "$binding_activation/deployment"
  printf '%s\n' '{"account_id":"wrong-account"}' \
    >"$binding_activation/deployment/policy.json"
  ! validate_runtime_account_binding "$binding_activation"
  printf '%s\n' '{"account_id":"binance_main"}' \
    >"$binding_activation/deployment/policy.json"
  validate_runtime_account_binding "$binding_activation"
  chmod 0640 "$secret_root/runtime.env"
  sed -i.bak \
    's/example_token_with_at_least_32_chars/short_token/' \
    "$secret_root/runtime.env"
  rm "$secret_root/runtime.env.bak"
  chmod 0440 "$secret_root/runtime.env"
  ! validate_runtime_secrets
  chmod 0640 "$secret_root/runtime.env"
  sed -i.bak \
    's/short_token/example_token_with_at_least_32_chars/' \
    "$secret_root/runtime.env"
  rm "$secret_root/runtime.env.bak"
  chmod 0440 "$secret_root/runtime.env"
  validate_runtime_secrets
  chmod 0640 "$secret_root/runtime.env"
  sed -i.bak 's/"runtime_account_id":"binance_main"/"runtime_account_id":""/' \
    "$secret_root/runtime.env"
  rm "$secret_root/runtime.env.bak"
  chmod 0440 "$secret_root/runtime.env"
  ! validate_runtime_secrets
  chmod 0640 "$secret_root/runtime.env"
  sed -i.bak 's/"runtime_account_id":""/"runtime_account_id":"binance_main"/' \
    "$secret_root/runtime.env"
  rm "$secret_root/runtime.env.bak"
  chmod 0440 "$secret_root/runtime.env"
  validate_runtime_secrets
  chmod 0600 "$secret_root/runtime.env"
  printf '%s\n' 'HFT_EXECUTION_MODE=live' >>"$secret_root/runtime.env"
  chmod 0440 "$secret_root/runtime.env"
  ! validate_runtime_secrets
  chmod 0600 "$secret_root/runtime.env"
  sed -i.bak '/^HFT_EXECUTION_MODE=/d' "$secret_root/runtime.env"
  rm "$secret_root/runtime.env.bak"
  chmod 0440 "$secret_root/runtime.env"
  SECRET_ROOT="$tmp_dir/../${tmp_dir##*/}/secrets"
  ! validate_runtime_secrets
  SECRET_ROOT=$secret_root
  findmnt() {
    local argument target=
    for argument in "$@"; do target=$argument; done
    if [[ $target == "$secret_root/runtime.env" ]]; then
      printf '%s\n' ext4
    else
      printf '%s\n' tmpfs
    fi
  }
  ! validate_runtime_secrets
  findmnt() { printf '%s\n' tmpfs; }
  rm "$secret_root/runtime.env"
  ! validate_runtime_secrets
)

# The container identity must be a dedicated, non-login system account. UID or
# GID 1000 (the ordinary Ubuntu login account) is never accepted.
(
  source "$RUNTIME"
  account_uid=991
  account_gid=991
  account_members=
  duplicate_passwd=
  duplicate_group=
  getent() {
    case $1 in
      passwd)
        printf 'mondayhft:x:%s:%s::/nonexistent:/usr/sbin/nologin\n' \
          "$account_uid" "$account_gid"
        if [[ -z ${2:-} && -n $duplicate_passwd ]]; then
          printf '%s\n' "$duplicate_passwd"
        fi
        ;;
      group)
        printf 'mondayhft:x:%s:%s\n' "$account_gid" "$account_members"
        if [[ -z ${2:-} && -n $duplicate_group ]]; then
          printf '%s\n' "$duplicate_group"
        fi
        ;;
      *) return 2 ;;
    esac
  }
  id() {
    [[ $1 == -g && $2 == mondayhft ]] || return 2
    printf '%s\n' "$account_gid"
  }
  account_ids=$(runtime_account_ids)
  [[ $account_ids == $'991\n991' ]]
  account_uid=1000
  ! runtime_account_ids >/dev/null
  account_uid=991
  account_gid=1000
  ! runtime_account_ids >/dev/null
  account_gid=991
  account_members=ubuntu
  ! runtime_account_ids >/dev/null
  account_members=
  duplicate_passwd='ubuntu:x:991:1000::/home/ubuntu:/bin/bash'
  ! runtime_account_ids >/dev/null
  duplicate_passwd='other:x:992:991::/nonexistent:/usr/sbin/nologin'
  ! runtime_account_ids >/dev/null
  duplicate_passwd=
  duplicate_group='ubuntu-runtime:x:991:ubuntu'
  ! runtime_account_ids >/dev/null
)

(
  source "$HOSTCTL"
  account_uid=991
  account_gid=991
  account_members=
  duplicate_passwd=
  duplicate_group=
  getent() {
    case $1 in
      passwd)
        printf 'mondayhft:x:%s:%s::/nonexistent:/usr/sbin/nologin\n' \
          "$account_uid" "$account_gid"
        if [[ -z ${2:-} && -n $duplicate_passwd ]]; then
          printf '%s\n' "$duplicate_passwd"
        fi
        ;;
      group)
        printf 'mondayhft:x:%s:%s\n' "$account_gid" "$account_members"
        if [[ -z ${2:-} && -n $duplicate_group ]]; then
          printf '%s\n' "$duplicate_group"
        fi
        ;;
      *) return 2 ;;
    esac
  }
  groupadd() { return 2; }
  useradd() { return 2; }
  id() {
    [[ $1 == -g && $2 == mondayhft ]] || return 2
    printf '%s\n' "$account_gid"
  }
  ensure_runtime_account
  account_members=ubuntu
  ! ensure_runtime_account
  account_members=
  account_uid=1000
  ! ensure_runtime_account
  account_uid=991
  duplicate_passwd='ubuntu:x:991:1000::/home/ubuntu:/bin/bash'
  ! ensure_runtime_account
  duplicate_passwd=
  duplicate_group='ubuntu-runtime:x:991:ubuntu'
  ! ensure_runtime_account
)

artifact_dir=$tmp_dir/artifact
mkdir "$artifact_dir"
control_assets=(
  hft-trading-ecs.service
  trading-ecs-hostctl.sh
  trading-ecs-paper-shadow-policy.jq
  trading-ecs-runtime.sh
)
(
  cd "$SCRIPT_DIR"
  sha256sum "${control_assets[@]}" \
    >"$artifact_dir/trading-ecs-control-assets.sha256"
  tar -czf "$artifact_dir/trading-ecs-control.tar.gz" "${control_assets[@]}"
)
control_manifest_sha=$(sha256sum "$artifact_dir/trading-ecs-control-assets.sha256" \
  | awk '{print $1}')
control_archive_sha=$(sha256sum "$artifact_dir/trading-ecs-control.tar.gz" \
  | awk '{print $1}')

selected_control_dir=$tmp_dir/selected-control
mkdir "$selected_control_dir"
cp "$UNIT" "$selected_control_dir/hft-trading-ecs.service"
cp "$HOSTCTL" "$selected_control_dir/trading-ecs-hostctl.sh"
cp "$POLICY" "$selected_control_dir/trading-ecs-paper-shadow-policy.jq"
cp "$RUNTIME" "$selected_control_dir/trading-ecs-runtime.sh"
chmod 0444 "$selected_control_dir/hft-trading-ecs.service" \
  "$selected_control_dir/trading-ecs-paper-shadow-policy.jq"
chmod 0555 "$selected_control_dir/trading-ecs-hostctl.sh" \
  "$selected_control_dir/trading-ecs-runtime.sh"
(
  cd "$selected_control_dir"
  sha256sum "${control_assets[@]}" >trading-ecs-control-assets.sha256
)
chmod 0444 "$selected_control_dir/trading-ecs-control-assets.sha256"
(
  source "$RUNTIME"
  EXPECTED_ROOT_UID=$(id -u)
  validate_selected_control_assets \
    "$selected_control_dir/trading-ecs-control-assets.sha256" \
    "$selected_control_dir/trading-ecs-runtime.sh" \
    "$selected_control_dir/trading-ecs-hostctl.sh" \
    "$selected_control_dir/trading-ecs-paper-shadow-policy.jq" \
    "$selected_control_dir/hft-trading-ecs.service"
  chmod 0755 "$selected_control_dir/trading-ecs-hostctl.sh"
  printf '\n' >>"$selected_control_dir/trading-ecs-hostctl.sh"
  chmod 0555 "$selected_control_dir/trading-ecs-hostctl.sh"
  ! validate_selected_control_assets \
    "$selected_control_dir/trading-ecs-control-assets.sha256" \
    "$selected_control_dir/trading-ecs-runtime.sh" \
    "$selected_control_dir/trading-ecs-hostctl.sh" \
    "$selected_control_dir/trading-ecs-paper-shadow-policy.jq" \
    "$selected_control_dir/hft-trading-ecs.service"
  chmod 0755 "$selected_control_dir/trading-ecs-hostctl.sh"
  cp "$HOSTCTL" "$selected_control_dir/trading-ecs-hostctl.sh"
  chmod 0555 "$selected_control_dir/trading-ecs-hostctl.sh"
  chmod 0644 "$selected_control_dir/trading-ecs-control-assets.sha256"
  printf '%s  trading-ecs-runtime.sh\n' "$(printf 'f%.0s' {1..64})" \
    >>"$selected_control_dir/trading-ecs-control-assets.sha256"
  chmod 0444 "$selected_control_dir/trading-ecs-control-assets.sha256"
  ! validate_selected_control_assets \
    "$selected_control_dir/trading-ecs-control-assets.sha256" \
    "$selected_control_dir/trading-ecs-runtime.sh" \
    "$selected_control_dir/trading-ecs-hostctl.sh" \
    "$selected_control_dir/trading-ecs-paper-shadow-policy.jq" \
    "$selected_control_dir/hft-trading-ecs.service"
)
cat >"$artifact_dir/hft-trading-ecs-release.json" <<JSON
{"schema":"monday.hft_trading_ecs_release.v1","source_revision":"$(printf 'c%.0s' {1..40})","image":{"published_repository":"crpi-ygobwehhof7qs9m3.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/hft-trading","repository":"${valid_image%@*}","digest":"${valid_image##*@}","reference":"$valid_image"},"control_manifest":{"file":"trading-ecs-control-assets.sha256","sha256":"$control_manifest_sha"},"control_archive":{"file":"trading-ecs-control.tar.gz","sha256":"$control_archive_sha"},"platform":{"region":"ap-northeast-1","host_os":"ubuntu","host_version":"26.04","architecture":"amd64","orchestrator":"none"}}
JSON
(
  cd "$artifact_dir"
  sha256sum hft-trading-ecs-release.json >hft-trading-ecs-release.json.sha256
)
(
  source "$HOSTCTL"
  EXPECTED_ROOT_UID=$(id -u)
  ARTIFACT_ROOT=$tmp_dir
  verify_release_manifest "$artifact_dir"
  mkdir "$tmp_dir/traversal"
  ! verify_release_manifest "$tmp_dir/traversal/../artifact"
  extract_dir=$tmp_dir/extracted-control
  mkdir "$extract_dir"
  verify_control_bundle "$artifact_dir" "$extract_dir"
  malicious_artifact=$tmp_dir/nonregular-control-artifact
  malicious_source=$tmp_dir/nonregular-control-source
  malicious_extract=$tmp_dir/nonregular-control-extract
  cp -R "$artifact_dir" "$malicious_artifact"
  mkdir "$malicious_source" "$malicious_extract"
  cp "$UNIT" "$malicious_source/hft-trading-ecs.service"
  cp "$HOSTCTL" "$malicious_source/trading-ecs-hostctl.sh"
  cp "$POLICY" "$malicious_source/trading-ecs-paper-shadow-policy.jq"
  mkfifo "$malicious_source/trading-ecs-runtime.sh"
  (
    cd "$malicious_source"
    COPYFILE_DISABLE=1 tar -czf \
      "$malicious_artifact/trading-ecs-control.tar.gz" "${control_assets[@]}"
  )
  malicious_archive_sha=$(sha256sum \
    "$malicious_artifact/trading-ecs-control.tar.gz" | awk '{print $1}')
  jq --arg sha "$malicious_archive_sha" '.control_archive.sha256 = $sha' \
    "$malicious_artifact/hft-trading-ecs-release.json" \
    >"$tmp_dir/nonregular-release.json"
  cp "$tmp_dir/nonregular-release.json" \
    "$malicious_artifact/hft-trading-ecs-release.json"
  ! verify_control_bundle "$malicious_artifact" "$malicious_extract"
  cp "$artifact_dir/hft-trading-ecs-release.json" "$tmp_dir/valid-release.json"
  jq '.image.published_repository = "crpi-different.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/hft-trading"' \
    "$tmp_dir/valid-release.json" >"$artifact_dir/hft-trading-ecs-release.json"
  (
    cd "$artifact_dir"
    sha256sum hft-trading-ecs-release.json >hft-trading-ecs-release.json.sha256
  )
  ! verify_release_manifest "$artifact_dir"
  cp "$tmp_dir/valid-release.json" "$artifact_dir/hft-trading-ecs-release.json"
  (
    cd "$artifact_dir"
    sha256sum hft-trading-ecs-release.json >hft-trading-ecs-release.json.sha256
  )
  jq '.image.reference = .image.published_repository + "@" + .image.digest' \
    "$artifact_dir/hft-trading-ecs-release.json" >"$tmp_dir/public-release.json"
  cp "$tmp_dir/public-release.json" "$artifact_dir/hft-trading-ecs-release.json"
  (
    cd "$artifact_dir"
    sha256sum hft-trading-ecs-release.json >hft-trading-ecs-release.json.sha256
  )
  ! verify_release_manifest "$artifact_dir"
)

# A process/container replacement after a clean health response must invalidate
# the frozen systemd/container identity instead of producing success evidence.
(
  source "$HOSTCTL"
  identity_state=$tmp_dir/identity-state
  curl_count=$tmp_dir/identity-curl-count
  printf '%s\n' original >"$identity_state"
  printf '0\n' >"$curl_count"
  systemctl() {
    case $1 in
      is-active) return 0 ;;
      is-enabled) printf 'static\n'; return 0 ;;
      show)
        case $3 in
          --property=InvocationID) printf '%s\n' "$(printf '1%.0s' {1..32})" ;;
          --property=MainPID) printf '4242\n' ;;
          --property=NRestarts) printf '0\n' ;;
          *) return 2 ;;
        esac
        ;;
      *) return 2 ;;
    esac
  }
  docker() {
    [[ $1 == container && $2 == inspect && $3 == --format ]] || return 2
    case $4 in
      '{{.Id}}')
        if [[ $(<"$identity_state") == original ]]; then
          printf '%s\n' "$(printf '2%.0s' {1..64})"
        else
          printf '%s\n' "$(printf '3%.0s' {1..64})"
        fi
        ;;
      '{{.State.Running}}') printf 'true\n' ;;
      '{{.Config.Image}}') printf '%s\n' "$valid_image" ;;
      '{{with (index (index .NetworkSettings.Ports "9090/tcp") 0)}}{{.HostIp}}:{{.HostPort}}{{end}}')
        printf '127.0.0.1:49152\n'
        ;;
      '{{with (index (index .NetworkSettings.Ports "9092/tcp") 0)}}{{.HostIp}}:{{.HostPort}}{{end}}')
        printf '127.0.0.1:49153\n'
        ;;
      *) return 2 ;;
    esac
  }
  curl() {
    local count
    count=$(<"$curl_count")
    count=$((count + 1))
    printf '%s\n' "$count" >"$curl_count"
    if (( count == 2 )); then
      printf '%s\n' replaced >"$identity_state"
    fi
    return 0
  }
  grpc_endpoint_ready() { return 0; }
  sleep() { return 0; }
  frozen_identity=$(capture_runtime_identity "$valid_image")
  [[ $frozen_identity == *'|127.0.0.1:49152|127.0.0.1:49153' ]]
  ! wait_for_health "$valid_image" "$frozen_identity"
)

# Container creation and random port publication are asynchronous after the
# systemd start succeeds, so identity acquisition is bounded rather than single-shot.
(
  source "$HOSTCTL"
  identity_attempts=$tmp_dir/identity-attempts
  printf '0\n' >"$identity_attempts"
  capture_runtime_identity() {
    local attempts
    attempts=$(<"$identity_attempts")
    attempts=$((attempts + 1))
    printf '%s\n' "$attempts" >"$identity_attempts"
    (( attempts >= 3 )) || return 1
    printf 'ready-identity\n'
  }
  sleep() { return 0; }
  [[ $(wait_for_runtime_identity "$valid_image") == ready-identity ]]
  [[ $(<"$identity_attempts") == 3 ]]
)

# Pointer installation must not let a failed durable write be masked by later
# successful commands when the helper itself is called from an `if !` context.
(
  source "$HOSTCTL"
  install() { return 1; }
  sync() { return 0; }
  mv() { return 0; }
  ! install_current_pointer "$tmp_dir/not-used" "$tmp_dir/current.env"
)

# Failure finalization is the common explicit, EXIT, TERM, and INT cleanup:
# stop the candidate, restore the previous pointer, and commit FAILED.sha256.
(
  source "$HOSTCTL"
  failure_dir=$tmp_dir/failure-evidence
  failure_current=$tmp_dir/failure-current.env
  failure_previous=$failure_dir/previous-current.env
  failure_runtime=$tmp_dir/failure-runtime
  mkdir -m 0700 "$failure_dir"
  printf 'candidate\n' >"$failure_current"
  printf 'previous\n' >"$failure_previous"
  printf 'partial\n' >"$failure_dir/cutover.json.tmp"
  cat >"$failure_runtime" <<'SH'
#!/usr/bin/env bash
[[ ${1:-} == ensure-stopped ]]
SH
  chmod 0755 "$failure_runtime"
  systemctl() { [[ $1 == stop ]]; }
  restore_previous_pointer() { cp "$1" "$2"; }
  mv() {
    case ${1:-} in -T|-Tf|-fT) shift ;; esac
    command mv "$@"
  }
  finalize_cutover_failure "$failure_dir" "$failure_previous" \
    "$failure_current" "$failure_runtime" "$valid_image" \
    "$(printf 'c%.0s' {1..40})" "$(printf 'd%.0s' {1..64})" \
    "$(printf 'e%.0s' {1..64})" interrupted_SIGTERM \
    "$failure_dir/cutover.json.tmp" "$failure_dir/PASSED.sha256.tmp"
  [[ $(<"$failure_current") == previous ]]
  verify_single_checksum_marker "$failure_dir" FAILED.sha256 cutover.failed.json
  jq -e '
    .result == "failed_closed" and
    .failure_reason == "interrupted_SIGTERM" and
    .runtime_stopped == true and
    .previous_pointer_restored == true and
    .previous_runtime_restarted == false
  ' "$failure_dir/cutover.failed.json" >/dev/null
  [[ ! -e $failure_dir/cutover.json.tmp ]]
)

# A stop failure must never mint the canonical fail-closed marker.
(
  source "$HOSTCTL"
  emergency_dir=$tmp_dir/emergency-evidence
  emergency_current=$tmp_dir/emergency-current.env
  emergency_previous=$emergency_dir/previous-current.env
  emergency_runtime=$tmp_dir/emergency-runtime
  mkdir -m 0700 "$emergency_dir"
  printf 'candidate\n' >"$emergency_current"
  printf 'previous\n' >"$emergency_previous"
  cat >"$emergency_runtime" <<'SH'
#!/usr/bin/env bash
exit 1
SH
  chmod 0755 "$emergency_runtime"
  systemctl() { [[ $1 == stop ]]; }
  restore_previous_pointer() { cp "$1" "$2"; }
  mv() {
    [[ ${1:-} != -T ]] || shift
    command mv "$@"
  }
  ! finalize_cutover_failure "$emergency_dir" "$emergency_previous" \
    "$emergency_current" "$emergency_runtime" "$valid_image" \
    "$(printf 'c%.0s' {1..40})" "$(printf 'd%.0s' {1..64})" \
    "$(printf 'e%.0s' {1..64})" health_gate_failed '' ''
  [[ ! -e $emergency_dir/FAILED.sha256 ]]
  verify_single_checksum_marker "$emergency_dir" \
    EMERGENCY_FAILED_OPEN.sha256 cutover.emergency.json
  jq -e '
    .result == "emergency_failed_open" and
    .runtime_stopped == false and
    .trading_authority_blocked == true
  ' "$emergency_dir/cutover.emergency.json" >/dev/null
)

# Rollback authority is bound to immutable pointer snapshots and the exact
# active systemd/container identity. Legacy or stale evidence cannot stop a
# newer runtime.
rollback_evidence=$tmp_dir/rollback-lineage
rollback_current=$tmp_dir/rollback-current.env
mkdir -m 0700 "$rollback_evidence"
printf 'candidate pointer\n' >"$rollback_evidence/candidate-current.env"
printf 'previous pointer\n' >"$rollback_evidence/previous-current.env"
cp "$rollback_evidence/candidate-current.env" "$rollback_current"
chmod 0400 "$rollback_evidence/candidate-current.env" \
  "$rollback_evidence/previous-current.env"
chmod 0600 "$rollback_current"
rollback_candidate_sha=$(sha256sum "$rollback_evidence/candidate-current.env" \
  | awk '{print $1}')
rollback_previous_sha=$(sha256sum "$rollback_evidence/previous-current.env" \
  | awk '{print $1}')
jq -S -n --arg image "$valid_image" \
  --arg source "$(printf 'c%.0s' {1..40})" \
  --arg release "$(printf 'd%.0s' {1..64})" \
  --arg activation "$(printf 'e%.0s' {1..64})" \
  --arg invocation "$(printf '1%.0s' {1..32})" \
  --arg container "$(printf '2%.0s' {1..64})" \
  --arg candidate "$rollback_candidate_sha" \
  --arg previous "$rollback_previous_sha" '
  {schema:"monday.hft_trading_ecs_cutover.v1",result:"passed",
    mode_boundary:"paper_or_shadow_only",image_reference:$image,
    source_revision:$source,release_manifest_sha256:$release,
    activation_manifest_sha256:$activation,
    systemd_invocation_id:$invocation,main_pid:4242,
    container_id:$container,health_endpoint:"127.0.0.1:49152",
    grpc_endpoint:"127.0.0.1:49153",nrestarts:0,
    health_samples:2,grpc_connect_samples:2,
    candidate_pointer_file:"candidate-current.env",
    candidate_pointer_sha256:$candidate,current_pointer_sha256:$candidate,
    previous_pointer_present:true,
    previous_pointer_file:"previous-current.env",
    previous_pointer_sha256:$previous,
    service_enabled:false,live_small_enabled:false}
' >"$rollback_evidence/cutover.json"
cp "$rollback_evidence/cutover.json" "$tmp_dir/valid-lineage-cutover.json"
(
  cd "$rollback_evidence"
  sha256sum cutover.json >PASSED.sha256
)
chmod 0444 "$rollback_evidence/cutover.json" "$rollback_evidence/PASSED.sha256"
(
  ROLLBACK_IMAGE=
  ROLLBACK_EXPECTED_IDENTITY=
  ROLLBACK_CANDIDATE_SHA=
  ROLLBACK_PREVIOUS_SHA=
  source "$HOSTCTL"
  EXPECTED_ROOT_UID=$(id -u)
  verify_single_checksum_marker "$rollback_evidence" PASSED.sha256 cutover.json
  verify_rollback_lineage "$rollback_evidence" "$rollback_current"
  [[ $ROLLBACK_IMAGE == "$valid_image" ]]
  [[ $ROLLBACK_CANDIDATE_SHA == "$rollback_candidate_sha" ]]
  [[ $ROLLBACK_PREVIOUS_SHA == "$rollback_previous_sha" ]]
  [[ $ROLLBACK_EXPECTED_IDENTITY == \
    "$(printf '1%.0s' {1..32})|4242|0|$(printf '2%.0s' {1..64})|127.0.0.1:49152|127.0.0.1:49153" ]]
  chmod 0644 "$rollback_evidence/candidate-current.env"
  printf 'tampered candidate\n' >"$rollback_evidence/candidate-current.env"
  chmod 0400 "$rollback_evidence/candidate-current.env"
  ! verify_rollback_lineage "$rollback_evidence" "$rollback_current"
  chmod 0644 "$rollback_evidence/candidate-current.env"
  cp "$rollback_current" "$rollback_evidence/candidate-current.env"
  chmod 0400 "$rollback_evidence/candidate-current.env"
  chmod 0644 "$rollback_evidence/previous-current.env"
  printf 'tampered previous\n' >"$rollback_evidence/previous-current.env"
  chmod 0400 "$rollback_evidence/previous-current.env"
  ! verify_rollback_lineage "$rollback_evidence" "$rollback_current"
  chmod 0644 "$rollback_evidence/previous-current.env"
  printf 'previous pointer\n' >"$rollback_evidence/previous-current.env"
  chmod 0400 "$rollback_evidence/previous-current.env"
  chmod 0644 "$rollback_current"
  printf 'newer pointer\n' >"$rollback_current"
  chmod 0600 "$rollback_current"
  ! verify_rollback_lineage "$rollback_evidence" "$rollback_current"
  cp "$rollback_evidence/candidate-current.env" "$rollback_current"
  chmod 0600 "$rollback_current"
  chmod 0644 "$rollback_evidence/cutover.json" "$rollback_evidence/PASSED.sha256"
  jq 'del(.candidate_pointer_file,.candidate_pointer_sha256,
    .current_pointer_sha256,.previous_pointer_present,
    .previous_pointer_file,.previous_pointer_sha256)' \
    "$tmp_dir/valid-lineage-cutover.json" >"$rollback_evidence/cutover.json"
  (
    cd "$rollback_evidence"
    sha256sum cutover.json >PASSED.sha256
  )
  chmod 0444 "$rollback_evidence/cutover.json" "$rollback_evidence/PASSED.sha256"
  ! verify_rollback_lineage "$rollback_evidence" "$rollback_current"
)
chmod 0644 "$rollback_evidence/cutover.json" "$rollback_evidence/PASSED.sha256"
cp "$tmp_dir/valid-lineage-cutover.json" "$rollback_evidence/cutover.json"
(
  cd "$rollback_evidence"
  sha256sum cutover.json >PASSED.sha256
)
chmod 0444 "$rollback_evidence/cutover.json" "$rollback_evidence/PASSED.sha256"
stale_stop_log=$tmp_dir/stale-rollback-stop.log
rollback_test_uid=$(id -u)
if (
  source "$HOSTCTL"
  EXPECTED_ROOT_UID=$rollback_test_uid
  CURRENT_FILE=$rollback_current
  id() {
    [[ $1 == -u ]] || return 2
    printf '0\n'
  }
  runtime_identity_matches() { return 1; }
  systemctl() {
    if [[ $1 == stop ]]; then
      printf 'unsafe stop\n' >"$stale_stop_log"
    fi
    return 0
  }
  rollback_cutover "$rollback_evidence"
) >/dev/null 2>&1; then
  printf 'stale rollback evidence unexpectedly succeeded\n' >&2
  exit 1
fi
[[ ! -e $stale_stop_log ]]

rollback_second_evidence=$tmp_dir/rollback-second-recheck
rollback_second_current=$tmp_dir/rollback-second-current.env
cp -R "$rollback_evidence" "$rollback_second_evidence"
cp "$rollback_current" "$rollback_second_current"
chmod 0700 "$rollback_second_evidence"
chmod 0600 "$rollback_second_current"
second_stop_log=$tmp_dir/second-rollback-stop.log
if (
  source "$HOSTCTL"
  EXPECTED_ROOT_UID=$rollback_test_uid
  CURRENT_FILE=$rollback_second_current
  identity_checks=0
  id() {
    [[ $1 == -u ]] || return 2
    printf '0\n'
  }
  runtime_identity_matches() {
    identity_checks=$((identity_checks + 1))
    [[ $identity_checks -eq 1 ]]
  }
  systemctl() {
    if [[ $1 == stop ]]; then
      printf 'unsafe stop\n' >"$second_stop_log"
    fi
    return 0
  }
  mv() {
    [[ ${1:-} != -T ]] || shift
    command mv "$@"
  }
  rollback_cutover "$rollback_second_evidence"
) >/dev/null 2>&1; then
  printf 'changed identity after rollback intent unexpectedly passed\n' >&2
  exit 1
fi
[[ ! -e $second_stop_log ]]
[[ -f $rollback_second_evidence/PASSED.sha256 ]]
[[ -f $rollback_second_evidence/PASSED.rollback-pending.sha256 ]]
(
  cd "$rollback_second_evidence"
  sha256sum --check --strict PASSED.sha256 >/dev/null
  sha256sum --check --strict PASSED.rollback-pending.sha256 >/dev/null
)

rollback_success_evidence=$tmp_dir/rollback-success
rollback_success_current=$tmp_dir/rollback-success-current.env
rollback_runtime=$tmp_dir/rollback-runtime
cp -R "$rollback_evidence" "$rollback_success_evidence"
cp "$rollback_current" "$rollback_success_current"
chmod 0700 "$rollback_success_evidence"
chmod 0600 "$rollback_success_current"
printf '#!/usr/bin/env bash\nexit 0\n' >"$rollback_runtime"
chmod 0700 "$rollback_runtime"
(
  source "$HOSTCTL"
  EXPECTED_ROOT_UID=$rollback_test_uid
  CURRENT_FILE=$rollback_success_current
  RUNTIME_PROGRAM=$rollback_runtime
  id() { printf '0\n'; }
  runtime_identity_matches() { return 0; }
  systemctl() { return 0; }
  install() {
    local -a arguments=()
    while (( $# > 0 )); do
      case $1 in
        -o|-g) shift 2 ;;
        *) arguments+=("$1"); shift ;;
      esac
    done
    command install "${arguments[@]}"
  }
  mv() {
    case ${1:-} in -T|-Tf|-fT) shift ;; esac
    command mv "$@"
  }
  rollback_cutover "$rollback_success_evidence" >/dev/null
)
cmp "$rollback_evidence/PASSED.sha256" "$rollback_success_evidence/PASSED.sha256"
(
  cd "$rollback_success_evidence"
  sha256sum --check --strict PASSED.sha256 >/dev/null
  sha256sum --check --strict PASSED.rollback-pending.sha256 >/dev/null
  sha256sum --check --strict PASSED.rolled-back.sha256 >/dev/null
  sha256sum --check --strict ROLLED_BACK.sha256 >/dev/null
)

grep -Fq \
  'uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5' \
  "$WORKFLOW"
grep -Fq \
  'uses: docker/setup-buildx-action@8d2750c68a42422c14e847fe6c8ac0403b4cbd6f' \
  "$WORKFLOW"
grep -Fq \
  'uses: docker/login-action@c94ce9fb468520275223c153574b00df6fe4bcc9' \
  "$WORKFLOW"
grep -Fq \
  'uses: docker/build-push-action@10e90e3645eae34f1e60eeb005ba3a3d33f178e8' \
  "$WORKFLOW"
[[ $(grep -Fc \
  'uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02' \
  "$WORKFLOW") -eq 4 ]]
if grep -Eq \
  'uses: (actions/(checkout|upload-artifact)|docker/(setup-buildx-action|login-action|build-push-action))@v[0-9]' \
  "$WORKFLOW"; then
  printf 'workflow actions must be pinned to full commit SHAs\n' >&2
  exit 1
fi
grep -Fq 'docker logout "$ACR_REGISTRY"' "$WORKFLOW"
grep -Fq 'rm -f -- "$docker_config_root/config.json"' "$WORKFLOW"
awk '
  /- name: Remove ACR credentials/ {
    if (getline <= 0 || $0 !~ /^[[:space:]]+if: always\(\)$/) exit 1
    found = 1
  }
  END { if (!found) exit 1 }
' "$WORKFLOW"
grep -Fq 'hft-trading-ecs-linux-amd64-${{ github.sha }}' "$WORKFLOW"
grep -Fq 'IMAGE_DIGEST: ${{ steps.build.outputs.digest }}' "$WORKFLOW"
grep -Fq ':run-${{ github.run_id }}-${{ github.run_attempt }}' "$WORKFLOW"
grep -Fq 'reference:$image_reference' "$WORKFLOW"
grep -Fq 'orchestrator:"none"' "$WORKFLOW"
grep -Fq -- '--pull never' "$RUNTIME"
grep -Fq -- '--publish 127.0.0.1::9090/tcp' "$RUNTIME"
grep -Fq -- '--publish 127.0.0.1::9092/tcp' "$RUNTIME"
grep -Fq -- '--stop-signal SIGINT' "$RUNTIME"
grep -Fq -- '--stop-timeout 60' "$RUNTIME"
grep -Fq -- '--ulimit core=0:0' "$RUNTIME"
grep -Fq -- '--entrypoint /bin/sh' "$RUNTIME"
grep -Fq '/run/secrets/hft/runtime.env' "$RUNTIME"
if grep -Fq -- '--env-file' "$RUNTIME"; then
  printf 'Docker daemon metadata must not contain runtime secret values\n' >&2
  exit 1
fi
if grep -Fq -- '--user 1000:1000' "$RUNTIME"; then
  printf 'ordinary Ubuntu UID/GID 1000 must not run the trading container\n' >&2
  exit 1
fi
grep -Fq 'StartLiveSmall' "$POLICY"
grep -Fq 'live_small_enabled:false' "$HOSTCTL"
grep -Fq 'service_enabled:false' "$HOSTCTL"
grep -Fq 'previous_runtime_restarted:false' "$HOSTCTL"
grep -Fq 'new signed envelope and nonce before restart' "$HOSTCTL"
grep -Fq 'PASSED.rolled-back.sha256' "$HOSTCTL"
grep -Fq 'PASSED.rollback-pending.sha256' "$HOSTCTL"
grep -Fq 'PASSED.sha256 is the commit point' "$HOSTCTL"
grep -Fq 'cutover.unconfirmed.json' "$HOSTCTL"
grep -Fq 'trap - EXIT; trap "" HUP INT TERM' "$HOSTCTL"
grep -Fq 'failure_reason=interrupted_SIGHUP' "$HOSTCTL"
grep -Fq 'failure_reason=interrupted_SIGINT' "$HOSTCTL"
grep -Fq 'failure_reason=interrupted_SIGTERM' "$HOSTCTL"
grep -Fq 'finalize_cutover_failure' "$HOSTCTL"
grep -Fq 'result=emergency_failed_open' "$HOSTCTL"
grep -Fq 'wait_for_runtime_identity' "$HOSTCTL"
grep -Fq 'grpc_endpoint_ready' "$HOSTCTL"
grep -Fq 'verify_rollback_lineage' "$HOSTCTL"
grep -Fq 'candidate_pointer_sha256' "$HOSTCTL"
grep -Fq 'flock -n 9' "$HOSTCTL"
grep -Fq "mkdir -m 0700 \"\$evidence_dir\"" "$HOSTCTL"
grep -Fq 'docker info >/dev/null 2>&1' "$HOSTCTL"
grep -Fq 'docker info >/dev/null 2>&1' "$RUNTIME"
grep -Fq 'DOCKER_CONFIG=$docker_config docker login' "$HOSTCTL"
grep -Fq 'assert_service_static' "$HOSTCTL"
grep -Fq 'assert_service_boot_disabled_before_stage' "$HOSTCTL"
grep -Fq 'useradd --system' "$HOSTCTL"
grep -Fq '[[ $state == absent ]]' "$RUNTIME"
grep -Fq 'verify_bare_tokyo_host || die' "$RUNTIME"
grep -Fq 'validate_selected_control_assets' "$RUNTIME"
grep -Fq 'assert_effective_service_contract "$UNIT_PATH" "${BASH_SOURCE[0]}"' "$RUNTIME"
grep -Fq 'assert_effective_service_contract "$unit_path" "$runtime_program"' "$HOSTCTL"
grep -Fq 'secure_directory_chain "$activation_dir"' "$RUNTIME"
grep -Fq '! -path ./activation.sha256' "$RUNTIME"
grep -Fq 'ExecStartPre=/usr/local/libexec/monday-hft-trading-runtime preflight' "$UNIT"
grep -Fq 'ExecStopPost=/usr/local/libexec/monday-hft-trading-runtime ensure-stopped' "$UNIT"
grep -Fq 'assert-stopped' "$HOSTCTL"
grep -Fq 'Restart=no' "$UNIT"
grep -Fq 'LimitCORE=0' "$UNIT"
if grep -Eq '^\[Install\]$' "$UNIT"; then
  printf 'trading service must remain static and boot-disabled\n' >&2
  exit 1
fi
if grep -Eq 'systemctl[[:space:]]+enable' "$HOSTCTL"; then
  printf 'host contract must never enable the trading service\n' >&2
  exit 1
fi
if grep -Eq 'hft-trading:(latest|[$][{]?[A-Za-z_])' "$HOSTCTL" "$RUNTIME" "$UNIT"; then
  printf 'mutable trading image reference detected\n' >&2
  exit 1
fi

printf 'trading ECS host contract tests passed\n'
