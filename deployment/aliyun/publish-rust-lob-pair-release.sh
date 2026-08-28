#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf '%s\n' \
    "Usage: ${0##*/} --instance i-... --artifact-uri oss://... --artifact-sha256 <64 hex> --source-revision <git sha>" \
    '       The command publishes an inactive V2 ControllerRelease and never changes production.' >&2
}

die() { printf '%s\n' "$*" >&2; exit 1; }

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

instance='' artifact_uri='' artifact_sha='' source_revision=''
region=${REGION_ID:-ap-northeast-1}
profile=${ALIYUN_LOCAL_PROFILE:-default}
prefix=${BUNDLE_OSS_PREFIX:-oss://monday-lob-apne1-1045353359/releases/binance-lob-controller}
while (($#)); do
  case $1 in
    --instance) instance=${2:-}; shift 2 ;;
    --artifact-uri|--uri) artifact_uri=${2:-}; shift 2 ;;
    --artifact-sha256|--payload) artifact_sha=${2:-}; shift 2 ;;
    --source-revision|--source) source_revision=${2:-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done
[[ $instance =~ ^i-[a-z0-9]+$ ]] || { usage; exit 2; }
[[ $artifact_uri =~ ^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$ ]] \
  || die 'artifact URI is invalid'
[[ $artifact_sha =~ ^[A-Fa-f0-9]{64}$ ]] || die 'payload digest must be 64 hex characters'
[[ $source_revision =~ ^[A-Fa-f0-9]{40,64}$ ]] || die 'source revision must be a full Git SHA'
[[ $prefix =~ ^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$ ]] \
  || die 'bundle prefix is invalid'
[[ $region == ap-northeast-1 ]] || die 'only the Tokyo region is permitted'
artifact_sha=${artifact_sha,,}
source_revision=${source_revision,,}

repo_root=$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)
resolved_source=$(git -C "$repo_root" rev-parse "${source_revision}^{commit}") \
  || die 'source revision is not present locally'
[[ $resolved_source == "$(git -C "$repo_root" rev-parse HEAD)" ]] \
  || die 'source revision must equal the clean checkout HEAD'
[[ -z $(git -C "$repo_root" status --porcelain --untracked-files=normal) ]] \
  || die 'refusing to publish from a dirty checkout'

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
bundle="$tmp/deployment.tar"
manifest="$tmp/release.json"
assets=()
while IFS= read -r asset; do assets+=("$asset"); done < <(
  { monday_runtime_assets; monday_controller_assets; } | sort -u
)
for asset in "${assets[@]}"; do
  monday_file_direct "$SCRIPT_DIR/$asset" || die "missing release asset: $asset"
done
COPYFILE_DISABLE=1 tar -C "$SCRIPT_DIR" -cf "$bundle" "${assets[@]}"
bundle_sha=$(monday_sha256_file "$bundle")
bundle_uri="${prefix%/}/${source_revision}/deployment-${bundle_sha}.tar"
runtime_sha=$(monday_rust_lob_runtime_contract_sha256 "$SCRIPT_DIR")
jq -cS -n \
  --arg uri "$artifact_uri" --arg sha "$artifact_sha" \
  --arg runtime "$runtime_sha" --arg source "$source_revision" \
  --arg bundle "$bundle_uri" --arg bundle_sha "$bundle_sha" \
  '{schema:"monday.rust_lob_controller_release.v2",control_plane_version:2,
    topology:"stable",artifact_uri:$uri,artifact_sha256:$sha,
    runtime_contract_sha256:$runtime,deployment_source_revision:$source,
    deployment_bundle_uri:$bundle,deployment_bundle_sha256:$bundle_sha}' >"$manifest"
manifest_sha=$(monday_sha256_file "$manifest")
manifest_uri="${prefix%/}/${source_revision}/controller-${manifest_sha}.json"
fixed_publisher="$SCRIPT_DIR/host-rust-lob-controller-release.sh"
fixed_lib="$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
fixed_publisher_sha=$(monday_sha256_file "$fixed_publisher")
fixed_lib_sha=$(monday_sha256_file "$fixed_lib")
command -v gzip >/dev/null 2>&1 || die 'gzip is required to transfer the fixed publisher'
fixed_publisher_b64=$(gzip -c "$fixed_publisher" | base64 | tr -d '\n')
fixed_lib_b64=$(gzip -c "$fixed_lib" | base64 | tr -d '\n')

if [[ ${MONDAY_CONTROL_PLANE_DRY_RUN:-0} == 1 ]]; then
  jq -cn --arg controller "$manifest_sha" --arg payload "$artifact_sha" \
    --arg bundle "$bundle_uri" --arg manifest "$manifest_uri" \
    '{operation:"release",controller:$controller,payload:$payload,
      bundle_uri:$bundle,manifest_uri:$manifest,production_changed:false}'
  exit 0
fi

profile_args=()
[[ -n $profile ]] && profile_args=(--profile "$profile")
aliyun ossutil cp "$bundle" "$bundle_uri" \
  --endpoint oss-ap-northeast-1.aliyuncs.com --region "$region" --force \
  "${profile_args[@]}"
aliyun ossutil cp "$manifest" "$manifest_uri" \
  --endpoint oss-ap-northeast-1.aliyuncs.com --region "$region" --force \
  "${profile_args[@]}"

remote=$(cat <<EOF
set -Eeuo pipefail
tmp=\$(mktemp -d)
trap 'rm -rf "\$tmp"' EXIT
printf '%s' '$fixed_publisher_b64' | base64 --decode | gzip -d >"\$tmp/fixed-publisher.sh"
printf '%s' '$fixed_lib_b64' | base64 --decode | gzip -d >"\$tmp/rust-lob-control-plane-lib.sh"
printf '%s  %s\\n' '$fixed_publisher_sha' "\$tmp/fixed-publisher.sh" | sha256sum --check --strict
printf '%s  %s\\n' '$fixed_lib_sha' "\$tmp/rust-lob-control-plane-lib.sh" | sha256sum --check --strict
chmod 0555 "\$tmp/fixed-publisher.sh" "\$tmp/rust-lob-control-plane-lib.sh"
aliyun ossutil cp '$artifact_uri' "\$tmp/payload" --profile ecs-role --endpoint oss-ap-northeast-1-internal.aliyuncs.com --region ap-northeast-1 --force
printf '%s  %s\\n' '$artifact_sha' "\$tmp/payload" | sha256sum --check --strict
aliyun ossutil cp '$bundle_uri' "\$tmp/deployment.tar" --profile ecs-role --endpoint oss-ap-northeast-1-internal.aliyuncs.com --region ap-northeast-1 --force
printf '%s  %s\\n' '$bundle_sha' "\$tmp/deployment.tar" | sha256sum --check --strict
aliyun ossutil cp '$manifest_uri' "\$tmp/release.json" --profile ecs-role --endpoint oss-ap-northeast-1-internal.aliyuncs.com --region ap-northeast-1 --force
printf '%s  %s\\n' '$manifest_sha' "\$tmp/release.json" | sha256sum --check --strict
mkdir "\$tmp/deployment"
# The candidate archive is data only.  The publisher/verifier is the fixed
# byte-checked copy sent above; candidate helper/lib files are never run.
bash "\$tmp/fixed-publisher.sh" "\$tmp/payload" "\$tmp/deployment.tar" "\$tmp/release.json"
EOF
)
command_content=$(printf '%s' "$remote" | base64 | tr -d '\n')
run_json=$(aliyun ecs RunCommand --RegionId "$region" --InstanceId.1 "$instance" \
  --Type RunShellScript --ContentEncoding Base64 --CommandContent "$command_content" \
  --KeepCommand false --Name monday-rust-lob-controller-release --Timeout 1200 \
  "${profile_args[@]}")
invoke_id=$(printf '%s' "$run_json" | jq -er '.InvokeId')
printf 'Cloud Assistant invocation: %s\ncontroller release: %s\n' "$invoke_id" "$manifest_sha"
polls=${MONDAY_OPERATION_TEST_POLLS:-240}
if [[ -n ${MONDAY_OPERATION_TEST_POLLS:-} ]]; then
  [[ ${MONDAY_ALLOW_SHORT_OPERATION_TEST:-0} == 1 ]] \
    || die 'short polling requires MONDAY_ALLOW_SHORT_OPERATION_TEST=1'
fi
[[ $polls =~ ^[1-9][0-9]*$ ]] || die 'poll count must be a positive integer'
for ((poll = 1; poll <= polls; poll++)); do
  result_json=$(aliyun ecs DescribeInvocationResults --RegionId "$region" \
    --InvokeId "$invoke_id" --InstanceId "$instance" "${profile_args[@]}" 2>/dev/null || true)
  status=$(printf '%s' "$result_json" \
    | jq -r '[.. | objects | .InvocationStatus? // empty][0] // empty' 2>/dev/null || true)
  exit_code=$(printf '%s' "$result_json" \
    | jq -r '[.. | objects | .ExitCode? // empty][0] // empty' 2>/dev/null || true)
  case "$status" in
    Success|Finished)
      output=$(printf '%s' "$result_json" \
        | jq -r '[.. | objects | .Output? // empty][0] // empty')
      if [[ -n $output ]]; then
        printf '%s' "$output" | base64 --decode || true
        printf '\n'
      fi
      [[ $exit_code == 0 ]] || die "controller publication failed: $invoke_id"
      printf 'controller publication completed: %s\n' "$invoke_id"
      exit 0
      ;;
    Failed|Stopped|PartialFailed|Timeout)
      printf '%s\n' "$result_json" >&2
      die "controller publication reached terminal state: $status"
      ;;
  esac
  sleep 5
done
printf 'timed out locally; stopping publication %s\n' "$invoke_id" >&2
aliyun ecs StopInvocation --RegionId "$region" --InvokeId "$invoke_id" \
  --InstanceId.1 "$instance" "${profile_args[@]}" >/dev/null 2>&1 || true
die "controller publication did not reach a terminal state: $invoke_id"
