#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf '%s\n' \
    'Usage: INSTANCE_ID=i-... ARTIFACT_OSS_URI=oss://... ARTIFACT_SHA256=<64 hex> SOURCE_REVISION=<git sha> deploy-rust-lob-release.sh' \
    '' \
    'Optional: REGION_ID=ap-northeast-1 ALIYUN_LOCAL_PROFILE=default'
}

for command in aliyun base64 git jq tar; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'missing required command: %s\n' "$command" >&2
    exit 2
  fi
done
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
  printf 'missing required SHA-256 command: sha256sum or shasum\n' >&2
  exit 2
fi

: "${INSTANCE_ID:?set INSTANCE_ID}"
: "${ARTIFACT_OSS_URI:?set ARTIFACT_OSS_URI}"
: "${ARTIFACT_SHA256:?set ARTIFACT_SHA256}"
: "${SOURCE_REVISION:?set SOURCE_REVISION}"

REGION_ID=${REGION_ID:-ap-northeast-1}
ALIYUN_LOCAL_PROFILE=${ALIYUN_LOCAL_PROFILE:-default}
BUNDLE_OSS_PREFIX=${BUNDLE_OSS_PREFIX:-oss://monday-lob-apne1-1045353359/releases/binance-lob-archiver}

if [[ "$REGION_ID" != 'ap-northeast-1' ]]; then
  printf 'refusing non-Tokyo region: %s\n' "$REGION_ID" >&2
  exit 2
fi
if [[ ! "$INSTANCE_ID" =~ ^i-[a-z0-9]+$ ]]; then
  usage >&2
  exit 2
fi
if [[ ! "$ARTIFACT_OSS_URI" =~ ^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$ ]]; then
  printf 'ARTIFACT_OSS_URI must be a private OSS object URI without query parameters\n' >&2
  exit 2
fi
if [[ ! "$ARTIFACT_SHA256" =~ ^[A-Fa-f0-9]{64}$ ]]; then
  printf 'ARTIFACT_SHA256 must contain exactly 64 hexadecimal characters\n' >&2
  exit 2
fi
if [[ ! "$SOURCE_REVISION" =~ ^[A-Fa-f0-9]{7,64}$ ]]; then
  printf 'SOURCE_REVISION must be a 7-64 character hexadecimal Git revision\n' >&2
  exit 2
fi
if [[ ! "$BUNDLE_OSS_PREFIX" =~ ^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$ ]]; then
  printf 'BUNDLE_OSS_PREFIX is not a valid OSS prefix\n' >&2
  exit 2
fi

ARTIFACT_SHA256=$(printf '%s' "$ARTIFACT_SHA256" | tr '[:upper:]' '[:lower:]')
SOURCE_REVISION=$(printf '%s' "$SOURCE_REVISION" | tr '[:upper:]' '[:lower:]')
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)
RESOLVED_SOURCE_REVISION=$(git -C "$REPO_ROOT" rev-parse "${SOURCE_REVISION}^{commit}")
HEAD_REVISION=$(git -C "$REPO_ROOT" rev-parse HEAD)
if [[ "$RESOLVED_SOURCE_REVISION" != "$HEAD_REVISION" ]]; then
  printf 'SOURCE_REVISION must resolve to the current HEAD (%s)\n' "$HEAD_REVISION" >&2
  exit 2
fi
if [[ -n $(git -C "$REPO_ROOT" status --porcelain --untracked-files=normal) ]]; then
  printf 'refusing to build a deployment bundle from a dirty working tree\n' >&2
  exit 2
fi
SOURCE_REVISION=$RESOLVED_SOURCE_REVISION
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

assets=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-rust@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-rust-upload@.service
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
  binance-lob-archiver-rust-spot.env
  binance-lob-archiver-rust-usdm.env
  host-rust-lob-shadow-gate.sh
  host-rust-lob-cutover.sh
  rust-lob-control-plane-lib.sh
  rust-lob-runtime-health-policy.jq
  rust-lob-shadow-gate-policy.jq
)
for asset in "${assets[@]}"; do
  if [[ ! -f "$SCRIPT_DIR/$asset" ]]; then
    printf 'missing deployment asset: %s\n' "$SCRIPT_DIR/$asset" >&2
    exit 2
  fi
done

BUNDLE_PATH="$TMP_DIR/deployment.tar"
tar -C "$SCRIPT_DIR" -cf "$BUNDLE_PATH" "${assets[@]}"
if command -v sha256sum >/dev/null 2>&1; then
  BUNDLE_SHA256=$(sha256sum "$BUNDLE_PATH" | awk '{print $1}')
else
  BUNDLE_SHA256=$(shasum -a 256 "$BUNDLE_PATH" | awk '{print $1}')
fi
BUNDLE_OSS_URI="${BUNDLE_OSS_PREFIX%/}/${SOURCE_REVISION}/deployment-${BUNDLE_SHA256}.tar"

aliyun_profile_args=()
if [[ -n "$ALIYUN_LOCAL_PROFILE" ]]; then
  aliyun_profile_args=(--profile "$ALIYUN_LOCAL_PROFILE")
fi

aliyun ossutil cp \
  "$BUNDLE_PATH" \
  "$BUNDLE_OSS_URI" \
  --endpoint oss-ap-northeast-1.aliyuncs.com \
  --region "$REGION_ID" \
  --force \
  "${aliyun_profile_args[@]}"

printf -v remote_variables \
  'artifact_uri=%q\nartifact_sha256=%q\nsource_revision=%q\nbundle_uri=%q\nbundle_sha256=%q\n' \
  "$ARTIFACT_OSS_URI" \
  "$ARTIFACT_SHA256" \
  "$SOURCE_REVISION" \
  "$BUNDLE_OSS_URI" \
  "$BUNDLE_SHA256"

read -r -d '' remote_body <<'REMOTE_SCRIPT' || true
set -euo pipefail
umask 027

install -d -m 0755 /run/lock
exec 9>/run/lock/monday-rust-lob-release.lock
if ! flock -w 30 9; then
  printf 'another Rust collector release operation holds the host lock\n' >&2
  exit 1
fi
if ! mountpoint -q /data; then
  printf '/data must be a mounted filesystem before collector installation\n' >&2
  exit 1
fi
for path in \
  /data/monday \
  /data/monday/spool \
  /data/monday/spool/binance-lob-rust-shadow \
  /data/monday/spool/binance-lob-rust-shadow/spot \
  /data/monday/spool/binance-lob-rust-shadow/usdm; do
  if [[ -L $path ]]; then
    printf 'refusing symlink in shadow spool path: %s\n' "$path" >&2
    exit 1
  fi
done

if systemctl is-active --quiet binance-lob-archiver-rust@spot.service \
  || systemctl is-active --quiet binance-lob-archiver-rust@usdm.service; then
  printf 'refusing to replace the shadow candidate while a shadow unit is active\n' >&2
  exit 1
fi

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
artifact_tmp="$work_dir/binance-lob-archiver"
bundle_tmp="$work_dir/deployment.tar"
bundle_dir="$work_dir/deployment"
mkdir -p "$bundle_dir"

aliyun ossutil cp "$artifact_uri" "$artifact_tmp" \
  --profile ecs-role \
  --endpoint oss-ap-northeast-1-internal.aliyuncs.com \
  --region ap-northeast-1 \
  --force
printf '%s  %s\n' "$artifact_sha256" "$artifact_tmp" | sha256sum --check --strict

aliyun ossutil cp "$bundle_uri" "$bundle_tmp" \
  --profile ecs-role \
  --endpoint oss-ap-northeast-1-internal.aliyuncs.com \
  --region ap-northeast-1 \
  --force
printf '%s  %s\n' "$bundle_sha256" "$bundle_tmp" | sha256sum --check --strict
tar --no-same-owner --no-same-permissions -xf "$bundle_tmp" -C "$bundle_dir"

if ! id hftcollector >/dev/null 2>&1; then
  useradd --system --create-home --home-dir /var/lib/hft-collector \
    --shell /usr/sbin/nologin hftcollector
fi
install -d -m 0755 /opt/monday/bin
install -d -m 0755 "/opt/monday/releases/binance-lob-archiver/$artifact_sha256"
install -d -m 0755 "/opt/monday/releases/binance-lob-archiver/$artifact_sha256/deployment"
install -d -m 0755 /etc/monday
install -d -m 0750 -o hftcollector -g hftcollector \
  /data/monday/spool/binance-lob-rust-shadow/spot \
  /data/monday/spool/binance-lob-rust-shadow/usdm
for path in \
  /data/monday/spool/binance-lob-rust-shadow \
  /data/monday/spool/binance-lob-rust-shadow/spot \
  /data/monday/spool/binance-lob-rust-shadow/usdm; do
  if [[ $(readlink -f "$path") != "$path" ]]; then
    printf 'shadow spool resolved outside its canonical path: %s\n' "$path" >&2
    exit 1
  fi
done

release_binary="/opt/monday/releases/binance-lob-archiver/$artifact_sha256/binance-lob-archiver"
install -m 0755 "$artifact_tmp" "$release_binary"
printf '%s  %s\n' "$artifact_sha256" "$release_binary" | sha256sum --check --strict
"$release_binary" --self-test
"$release_binary" --help | grep -Fq -- '--upload-only'

cp -a "$bundle_dir/." \
  "/opt/monday/releases/binance-lob-archiver/$artifact_sha256/deployment/"
install -m 0644 "$bundle_dir/binance-lob-archiver-rust@.service" \
  /etc/systemd/system/binance-lob-archiver-rust@.service
install -m 0644 "$bundle_dir/binance-lob-archiver-rust-upload@.service" \
  /etc/systemd/system/binance-lob-archiver-rust-upload@.service
install -m 0640 "$bundle_dir/binance-lob-archiver-rust-spot.env" \
  /etc/monday/binance-lob-archiver-rust-spot.env
install -m 0640 "$bundle_dir/binance-lob-archiver-rust-usdm.env" \
  /etc/monday/binance-lob-archiver-rust-usdm.env

ln -sfn "$release_binary" /opt/monday/bin/binance-lob-archiver-shadow
printf '%s  %s\n' "$artifact_sha256" /opt/monday/bin/binance-lob-archiver-shadow \
  | sha256sum --check --strict

metadata_tmp="/opt/monday/releases/binance-lob-archiver/$artifact_sha256/release.json.tmp"
printf '{"artifact_uri":"%s","artifact_sha256":"%s","deployment_source_revision":"%s","deployment_bundle_uri":"%s","deployment_bundle_sha256":"%s"}\n' \
  "$artifact_uri" "$artifact_sha256" "$source_revision" "$bundle_uri" "$bundle_sha256" \
  > "$metadata_tmp"
mv "$metadata_tmp" "/opt/monday/releases/binance-lob-archiver/$artifact_sha256/release.json"
systemctl daemon-reload
printf 'installed Rust collector candidate %s from %s; no service was started\n' \
  "$artifact_sha256" "$source_revision"
REMOTE_SCRIPT

remote_script=$'#!/usr/bin/env bash\n'
remote_script+="$remote_variables"
remote_script+="$remote_body"
command_content=$(printf '%s' "$remote_script" | base64 | tr -d '\n')

run_json=$(aliyun ecs RunCommand \
  --RegionId "$REGION_ID" \
  --InstanceId.1 "$INSTANCE_ID" \
  --Type RunShellScript \
  --ContentEncoding Base64 \
  --CommandContent "$command_content" \
  --KeepCommand false \
  --Name monday-rust-lob-release-install \
  --Timeout 1200 \
  "${aliyun_profile_args[@]}")
invoke_id=$(printf '%s' "$run_json" | jq -er '.InvokeId')
printf 'Cloud Assistant invocation: %s\nDeployment bundle: %s\n' \
  "$invoke_id" "$BUNDLE_OSS_URI"

for _ in $(seq 1 240); do
  if ! result_json=$(aliyun ecs DescribeInvocationResults \
    --RegionId "$REGION_ID" \
    --InvokeId "$invoke_id" \
    --InstanceId "$INSTANCE_ID" \
    "${aliyun_profile_args[@]}"); then
    sleep 5
    continue
  fi
  status=$(printf '%s' "$result_json" \
    | jq -r '[.. | objects | .InvocationStatus? // empty][0] // empty')
  exit_code=$(printf '%s' "$result_json" \
    | jq -r '[.. | objects | .ExitCode? // empty][0] // empty')
  case "$status" in
    Success|Finished)
      if [[ "$exit_code" == '0' ]]; then
        printf 'candidate install completed successfully: %s\n' "$invoke_id"
        exit 0
      fi
      printf '%s\n' "$result_json" >&2
      exit 1
      ;;
    Failed|Stopped|PartialFailed|Timeout)
      printf '%s\n' "$result_json" >&2
      exit 1
      ;;
  esac
  sleep 5
done

printf 'timed out waiting for Cloud Assistant invocation %s\n' "$invoke_id" >&2
aliyun ecs StopInvocation \
  --RegionId "$REGION_ID" \
  --InvokeId "$invoke_id" \
  --InstanceId.1 "$INSTANCE_ID" \
  "${aliyun_profile_args[@]}" >/dev/null || true
for _ in $(seq 1 12); do
  result_json=$(aliyun ecs DescribeInvocationResults \
    --RegionId "$REGION_ID" \
    --InvokeId "$invoke_id" \
    --InstanceId "$INSTANCE_ID" \
    "${aliyun_profile_args[@]}" || true)
  status=$(printf '%s' "$result_json" \
    | jq -r '[.. | objects | .InvocationStatus? // empty][0] // empty')
  case "$status" in
    Success|Finished|Failed|Stopped|PartialFailed|Timeout)
      printf 'invocation reached terminal state after cancellation: %s\n' "$status" >&2
      exit 1
      ;;
  esac
  sleep 5
done
printf 'invocation did not confirm cancellation: %s\n' "$invoke_id" >&2
exit 1
