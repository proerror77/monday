#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(git -C "$script_dir" rev-parse --show-toplevel)
cargo_toml="$repo_root/rust_hft/tools/collector/Cargo.toml"
bin_source="$repo_root/rust_hft/tools/collector/src/bin/bybit-options-archiver.rs"

[[ -f $bin_source ]] || {
  printf 'missing Bybit Options collector source: %s\n' "$bin_source" >&2
  exit 1
}

grep -Fq -- '[[bin]]' "$cargo_toml"
awk '/^name = "bybit-options-archiver"$/{found=1} found && /^path = "src\/bin\/bybit-options-archiver.rs"$/{exit 0} found && /^\[\[bin\]\]$/{exit 1}' "$cargo_toml" \
  || {
    printf 'Cargo.toml does not bind the bybit-options-archiver bin\n' >&2
    exit 1
  }

# The canonical ACR image and publish workflow must carry the bybit binary
# alongside the other collector bins.
dockerfile="$repo_root/rust_hft/deployment/docker/Dockerfile.binance-lob-archiver"
acr_workflow="$repo_root/.github/workflows/acr-publish.yml"
grep -F -- '--bin bybit-options-archiver' "$dockerfile" >/dev/null
grep -F -- '/out/bin/bybit-options-archiver' "$dockerfile" >/dev/null
grep -F -- '/usr/local/bin/bybit-options-archiver' "$dockerfile" >/dev/null
grep -F -- 'artifact/bybit-options-archiver' "$acr_workflow" >/dev/null
grep -F -- 'bybit-options-archiver.sha256' "$acr_workflow" >/dev/null
# shellcheck disable=SC2016 # literal workflow expression, must not expand
grep -F -- 'bybit-options-archiver ${{ needs.selector.outputs.source_sha }}' \
  "$acr_workflow" >/dev/null

# Defect 1: fail-closed disk/spool gates must be wired into Config and the
# writer loop, and surfaced into health.json.
grep -Fq 'min_free_gb' "$bin_source"
grep -Fq '"MIN_FREE_GB"' "$bin_source"
grep -Fq 'spool_max_bytes' "$bin_source"
grep -Fq '"BYBIT_OPTIONS_SPOOL_MAX_BYTES"' "$bin_source"
grep -Fq 'disk_free_gb(path' "$bin_source"
grep -Fq 'spool_usage_bytes(spool' "$bin_source"
grep -Fq 'fail-closed disk gate' "$bin_source"
grep -Fq 'fail-closed spool gate' "$bin_source"
grep -Fq 'disk_gate_ok(&config)?' "$bin_source"
grep -Fq 'disk_warning' "$bin_source"
grep -Fq 'spool_warning' "$bin_source"

# Defect 2: the uploader must recycle the source .ndjson only after verified
# readback, idempotently, and keep the .zst fallback bounded by retention.
grep -Fq 'cleanup_verified_uploaded' "$bin_source"
grep -Fq '.uploaded.json' "$bin_source"
grep -Fq 'sweep_expired_zst' "$bin_source"
grep -Fq 'BYBIT_OPTIONS_LOCAL_ZST_RETENTION_SECONDS' "$bin_source"
grep -Fq 'remove_file(data)' "$bin_source"

# Defect 3: the WS handshake must carry a User-Agent/Origin/app_id and the
# reconnect delay must be bounded exponential backoff.
grep -Fq 'build_ws_request' "$bin_source"
grep -Fq 'connect_async(build_ws_request' "$bin_source"
grep -Fq 'monday-bybit-options-archiver' "$bin_source"
grep -Fq 'MAX_BACKOFF_SECS' "$bin_source"
grep -Fq 'current * 2' "$bin_source"

# The governed unit files must enforce the fail-closed runtime contract.
archiver_unit="$script_dir/bybit-options-archiver.service"
upload_unit="$script_dir/bybit-options-upload.service"
timer="$script_dir/bybit-options-upload.timer"
for unit in "$archiver_unit" "$upload_unit"; do
  grep -Fq 'AssertPathIsMountPoint=/data' "$unit"
  grep -Fq 'Environment=BYBIT_OPTIONS_SPOOL_DIR=/data/monday/spool/bybit-options' "$unit"
  grep -Fq 'Environment=MIN_FREE_GB=20.0' "$unit"
  grep -Fq 'Environment=BYBIT_OPTIONS_SPOOL_MAX_BYTES=53687091200' "$unit"
  grep -Fq 'Environment=OSS_BUCKET=monday-lob-apne1-1045353359' "$unit"
  grep -Fq 'Environment=OSS_ENDPOINT=oss-ap-northeast-1-internal.aliyuncs.com' "$unit"
  grep -Fq 'Environment=OSS_REGION=ap-northeast-1' "$unit"
  grep -Fq 'Environment=ALIYUN_PROFILE=ecs-role' "$unit"
done
grep -Fq 'RuntimeMaxSec=21600' "$archiver_unit"
grep -Fq 'ExecStart=' "$archiver_unit"
grep -Fq -- '--upload-only' "$upload_unit"
grep -Fq 'Unit=bybit-options-upload.service' "$timer"

# The deploy lane must reject a release whose units drop the governed env.
grep -Fq 'REQUIRED_ENV_KEYS=(MIN_FREE_GB BYBIT_OPTIONS_SPOOL_MAX_BYTES)' \
  "$script_dir/bybit-options-archiver-deploy.sh"
# shellcheck disable=SC2016 # literal contract assertion, $key must not expand
grep -Fq 'Environment=$key=' "$script_dir/bybit-options-archiver-deploy.sh"
grep -Fq 'DEPLOYMENT_BUNDLE.sha256' "$script_dir/bybit-options-archiver-deploy.sh"
grep -Fq 'release.json' "$script_dir/bybit-options-archiver-deploy.sh"
grep -Fq 'does NOT touch systemd' "$script_dir/bybit-options-archiver-deploy.sh"
grep -Fq 'only writer of' "$script_dir/bybit-options-archiver-deploy.sh"
grep -Fq 'host-bybit-options-cutover.sh' "$script_dir/bybit-options-archiver-deploy.sh"

# The deploy script must survive shell startup: a no-argument invocation must
# reach its usage contract (exit 2), not die on a shell error first.
usage_rc=0
usage_output=$("$script_dir/bybit-options-archiver-deploy.sh" 2>&1) || usage_rc=$?
[[ $usage_rc == 2 ]] || {
  printf 'deploy script no-arg invocation must exit 2 (usage), got %s: %s\n' \
    "$usage_rc" "$usage_output" >&2
  exit 1
}
grep -Fq 'install <artifact-dir> <source-revision>' <<<"$usage_output"

# Host gate + cutover must exist and be executable.
for script in \
  "$script_dir/host-bybit-options-shadow-gate.sh" \
  "$script_dir/host-bybit-options-cutover.sh" \
  "$script_dir/bybit-options-archiver-deploy.sh"; do
  [[ -f $script && -x $script ]] || {
    printf 'host script must exist and be executable: %s\n' "$script" >&2
    exit 1
  }
done

printf '%s\n' 'Bybit Options release contract tests passed'
