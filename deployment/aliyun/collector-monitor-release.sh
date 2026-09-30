#!/usr/bin/env bash
# One immutable monitor release. No writer stop, Shadow Gate or active
# collector-controller replacement is part of this operation.
set -Eeuo pipefail
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
repo=$(cd -- "$script_dir/../.." && pwd)
if [[ ${1:-} == status ]]; then
  [[ $# == 2 && $2 =~ ^t-[a-z0-9]+$ ]] || exit 2
  aliyun ecs DescribeInvocationResults --RegionId ap-northeast-1 --profile default \
    --InvokeId "$2" --InstanceId i-6we6afeqsvv8uo1ixmyo
  exit
fi
[[ $# == 3 ]] || { echo 'usage: INSTANCE UNIT_PREIMAGE CONTROLLER_SHA' >&2; exit 2; }
instance=$1 preimage=$2 controller=$3
[[ $instance == i-6we6afeqsvv8uo1ixmyo && $preimage =~ ^[a-f0-9]{64}$ && $controller =~ ^[a-f0-9]{64}$ ]]
[[ -z $(git -C "$repo" status --porcelain) ]]
source_revision=$(git -C "$repo" rev-parse HEAD)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
gh api --paginate --slurp "repos/proerror77/monday/commits/$source_revision/check-runs?filter=latest&per_page=100" >"$work/checks.json"
"$repo/.github/scripts/read-release-required-checks.sh" "$work/checks.json" "$work/checks"
for check in monorepo prediction security; do grep -Fqx "${check}_conclusion=success" "$work/checks"; done
asset_names=(monday-collector-health.sh collector-monitor-retained.sh collector-monitor.service.template
  host-collector-health-unit-release.sh host-collector-monitor-install.sh)
for asset in "${asset_names[@]}"; do
  cp -- "$script_dir/$asset" "$work/$asset"
  sha=$(sha256sum "$work/$asset" | awk '{print $1}')
  printf '%s  %s\n' "$sha" "$asset" >>"$work/assets.sha256"
done
assets=$(jq -Rn '[inputs | split("  ") | {(.[1]):.[0]}] | add' <"$work/assets.sha256")
jq -cnS --arg source "$source_revision" --argjson assets "$assets" \
  '{schema:"monday.collector_monitor_release.v1",source_revision:$source,assets:$assets}' >"$work/release.json"
monitor=$(sha256sum "$work/release.json" | awk '{print $1}')
tar -C "$work" -cf "$work/package.tar" release.json assets.sha256 "${asset_names[@]}"
package_sha=$(sha256sum "$work/package.tar" | awk '{print $1}')
uri="oss://monday-lob-apne1-1045353359/releases/collector-monitor/$source_revision/$monitor/package.tar"
aliyun ossutil cp "$work/package.tar" "$uri" --endpoint oss-ap-northeast-1.aliyuncs.com \
  --region ap-northeast-1 --profile default
cat >"$work/remote.sh" <<EOF
#!/bin/bash
set -Eeuo pipefail
stage=\$(mktemp -d /root/monday-monitor-stage.XXXXXX)
aliyun ossutil cp '$uri' "\$stage/package.tar" --endpoint oss-ap-northeast-1-internal.aliyuncs.com --region ap-northeast-1 --profile ecs-role
[[ \$(sha256sum "\$stage/package.tar" | awk '{print \$1}') == '$package_sha' ]]
tar -xf "\$stage/package.tar" -C "\$stage"
[[ \$(sha256sum "\$stage/release.json" | awk '{print \$1}') == '$monitor' ]]
(cd "\$stage"; sha256sum --check --strict assets.sha256 >/dev/null)
bash "\$stage/host-collector-monitor-install.sh" "\$stage" '$monitor' '$preimage' '$controller'
EOF
content=$(base64 <"$work/remote.sh" | tr -d '\n')
response=$(aliyun ecs RunCommand --RegionId ap-northeast-1 --profile default --InstanceId.1 "$instance" \
  --Type RunShellScript --ContentEncoding Base64 --CommandContent "$content" \
  --Name monday-collector-monitor-release --KeepCommand false --Timeout 180)
jq -n --argjson response "$response" --arg monitor "$monitor" --arg source "$source_revision" \
  --arg package "$package_sha" --arg uri "$uri" \
  '{operation:"monitor-install",instance:"i-6we6afeqsvv8uo1ixmyo",source_revision:$source,
    monitor_sha256:$monitor,package_sha256:$package,package_uri:$uri,invocation:$response}'
echo 'Keep this InvokeId; query the same operation for terminal result before any retry.' >&2
