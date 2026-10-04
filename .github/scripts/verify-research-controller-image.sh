#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
[[ $# == 3 ]] || { echo 'usage: verify-research-controller-image.sh IMAGE SOURCE_SHA VERIFIED_BIN_DIR' >&2; exit 2; }
image=$1 source=$2 binaries=$3
[[ -n $image && $source =~ ^[0-9a-f]{40}$ && -d $binaries ]] || exit 2
test "$(docker image inspect --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}' "$image")" = "$source"
docker image inspect --format '{{json .Config.Entrypoint}}' "$image" | jq -e \
  '. == ["/usr/bin/tini","--","/bin/bash","/opt/monday/deployment/aliyun/research/scripts/campaign-cycle-controller.sh"]' >/dev/null
test "$(docker image inspect --format '{{.Config.User}}' "$image")" = research
test "$(docker image inspect --format '{{.Config.WorkingDir}}' "$image")" = /work

# Run only local packaging probes. Never invoke the Campaign entrypoint or
# forward host credentials; even the client version probes have no network.
docker run --rm --network none --entrypoint /bin/bash "$image" -ceu '
  for tool in bash curl jq tini; do test -x "/usr/bin/$tool"; done
  for tool in aliyun kubectl alpha-harness binance-market-tape-slicer lob-pit-materializer binance-replay-parquet-materializer; do
    test -x "/usr/local/bin/$tool"
  done
  test -x /usr/local/bin/cex-materialization-entrypoint.sh
  test -x /opt/monday/deployment/aliyun/research/scripts/campaign-cycle-controller.sh
  test -r /opt/monday/deployment/aliyun/research/scripts/campaign-job-watch.sh
  test -r /opt/monday/deployment/aliyun/research/k8s/campaign-cycle-controller-job.example.yaml
  bash -n /usr/local/bin/cex-materialization-entrypoint.sh
  bash -n /opt/monday/deployment/aliyun/research/scripts/campaign-cycle-controller.sh
  bash -n /opt/monday/deployment/aliyun/research/scripts/campaign-job-watch.sh
  test "$(/usr/local/bin/alpha-harness --version)" = "alpha-harness $1"
  /usr/local/bin/aliyun version
  /usr/local/bin/kubectl version --client=true
' -- "$source"

readback=$(mktemp -d)
container_id=
cleanup() {
  if [[ -n $container_id ]]; then docker rm -f "$container_id" >/dev/null 2>&1 || true; fi
  rm -rf "$readback"
}
trap cleanup EXIT
container_id=$(docker create "$image")
for binary in alpha-harness binance-market-tape-slicer lob-pit-materializer binance-replay-parquet-materializer; do
  docker cp "$container_id:/usr/local/bin/$binary" "$readback/$binary"
  cmp "$readback/$binary" "$binaries/$binary"
done
for asset in \
  'scripts/cex-materialization-entrypoint.sh|/usr/local/bin/cex-materialization-entrypoint.sh' \
  'scripts/campaign-cycle-controller.sh|/opt/monday/deployment/aliyun/research/scripts/campaign-cycle-controller.sh' \
  'scripts/campaign-job-watch.sh|/opt/monday/deployment/aliyun/research/scripts/campaign-job-watch.sh' \
  'k8s/campaign-cycle-controller-job.example.yaml|/opt/monday/deployment/aliyun/research/k8s/campaign-cycle-controller-job.example.yaml'; do
  IFS='|' read -r source_path image_path <<<"$asset"
  docker cp "$container_id:$image_path" "$readback/asset"
  cmp "$readback/asset" "$root/deployment/aliyun/research/$source_path"
done
echo 'PASS: controller entrypoint, offline probes and published asset bytes'
