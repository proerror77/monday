#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$script_dir/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
source_sha=1111111111111111111111111111111111111111
mkdir -p "$work/bin" "$work/release"
cat >"$work/bin/docker" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%q ' "$@" >>"$MOCK_CONTROLLER_LOG"
printf '\n' >>"$MOCK_CONTROLLER_LOG"
case "$1" in
  image)
    [[ $2 == inspect && $3 == --format && $5 == fixture-image ]]
    case "$4" in
      '{{ index .Config.Labels "org.opencontainers.image.revision" }}') jq -r .source "$MOCK_CONTROLLER_FS/config.json" ;;
      '{{json .Config.Entrypoint}}') jq -c .entrypoint "$MOCK_CONTROLLER_FS/config.json" ;;
      '{{.Config.User}}') jq -r .user "$MOCK_CONTROLLER_FS/config.json" ;;
      '{{.Config.WorkingDir}}') jq -r .workdir "$MOCK_CONTROLLER_FS/config.json" ;;
      *) exit 2 ;;
    esac ;;
  run)
    [[ $2 == --rm && $3 == --network && $4 == none && $5 == --entrypoint && $6 == /bin/bash && $7 == fixture-image && $8 == -ceu ]]
    script=$9
    shift 9
    # Execute the real local probes against an isolated image filesystem stand-in.
    # No production verifier options or fake-success branch are introduced.
    script=${script//\/usr\//$MOCK_CONTROLLER_FS/usr/}
    script=${script//\/opt\//$MOCK_CONTROLLER_FS/opt/}
    /bin/bash -ceu "$script" "$@" ;;
  create) [[ $2 == fixture-image ]]; printf 'fixture-container\n' ;;
  cp)
    [[ $2 == fixture-container:/* ]]
    cp "$MOCK_CONTROLLER_FS/${2#fixture-container:/}" "$3" ;;
  rm) [[ $2 == -f && $3 == fixture-container ]] ;;
  *) exit 2 ;;
esac
MOCK
chmod 0755 "$work/bin/docker"
for binary in alpha-harness binance-market-tape-slicer lob-pit-materializer binance-replay-parquet-materializer; do
  if [[ $binary == alpha-harness ]]; then
    printf '#!/usr/bin/env bash\nprintf "alpha-harness %s\\n"\n' "$source_sha" >"$work/release/$binary"
  else
    printf '#!/usr/bin/env bash\nexit 0\n' >"$work/release/$binary"
  fi
  chmod 0755 "$work/release/$binary"
done
initialize_image() {
  rm -rf "$work/image"
  mkdir -p "$work/image/usr/bin" "$work/image/usr/local/bin" \
    "$work/image/opt/monday/deployment/aliyun/research/scripts" \
    "$work/image/opt/monday/deployment/aliyun/research/k8s"
  jq -cn --arg source "$source_sha" \
    '{source:$source,user:"research",workdir:"/work",entrypoint:["/usr/bin/tini","--","/bin/bash","/opt/monday/deployment/aliyun/research/scripts/campaign-cycle-controller.sh"]}' >"$work/image/config.json"
  for tool in bash curl jq tini; do
    printf '#!/usr/bin/env bash\nexit 0\n' >"$work/image/usr/bin/$tool"
    chmod 0755 "$work/image/usr/bin/$tool"
  done
  for tool in aliyun kubectl; do
    printf '#!/usr/bin/env bash\nprintf "offline client version\\n"\n' >"$work/image/usr/local/bin/$tool"
    chmod 0755 "$work/image/usr/local/bin/$tool"
  done
  cp "$work/release/"* "$work/image/usr/local/bin/"
  cp "$root/deployment/aliyun/research/scripts/cex-materialization-entrypoint.sh" "$work/image/usr/local/bin/"
  cp "$root/deployment/aliyun/research/scripts/campaign-cycle-controller.sh" \
    "$root/deployment/aliyun/research/scripts/campaign-job-watch.sh" \
    "$work/image/opt/monday/deployment/aliyun/research/scripts/"
  cp "$root/deployment/aliyun/research/k8s/campaign-cycle-controller-job.example.yaml" \
    "$work/image/opt/monday/deployment/aliyun/research/k8s/"
  chmod 0755 "$work/image/usr/local/bin/cex-materialization-entrypoint.sh" \
    "$work/image/opt/monday/deployment/aliyun/research/scripts/campaign-cycle-controller.sh"
  : >"$work/docker.log"
}
verify_image() {
  MOCK_CONTROLLER_FS="$work/image" MOCK_CONTROLLER_LOG="$work/docker.log" PATH="$work/bin:$PATH" \
    bash "$script_dir/verify-research-controller-image.sh" fixture-image "$source_sha" "$work/release"
}
reject_image() {
  if verify_image >"$work/rejected.log" 2>&1; then
    printf 'controller image accepted %s\n' "$1" >&2; exit 1
  fi
}
initialize_image
verify_image
grep -Fq 'run --rm --network none --entrypoint /bin/bash' "$work/docker.log"
grep -Fqx 'rm -f fixture-container ' "$work/docker.log"
for failure in source entrypoint user tool script-missing script-syntax script-bytes template binary; do
  initialize_image
  case "$failure" in
    source|entrypoint|user)
      case "$failure" in
        source) change='.source="2222222222222222222222222222222222222222"' ;;
        entrypoint) change='.entrypoint=["/bin/true"]' ;;
        user) change='.user="root"' ;;
      esac
      jq "$change" "$work/image/config.json" >"$work/config.next"
      mv "$work/config.next" "$work/image/config.json" ;;
    tool) rm "$work/image/usr/bin/jq" ;;
    script-missing) rm "$work/image/opt/monday/deployment/aliyun/research/scripts/campaign-cycle-controller.sh" ;;
    script-syntax) printf '\nif then\n' >>"$work/image/opt/monday/deployment/aliyun/research/scripts/campaign-job-watch.sh" ;;
    script-bytes) printf '\n# changed image asset\n' >>"$work/image/usr/local/bin/cex-materialization-entrypoint.sh" ;;
    template) printf '\n# changed Job template\n' >>"$work/image/opt/monday/deployment/aliyun/research/k8s/campaign-cycle-controller-job.example.yaml" ;;
    binary) printf '\n# changed executable\n' >>"$work/image/usr/local/bin/lob-pit-materializer" ;;
  esac
  reject_image "$failure"
done
echo 'PASS: offline controller checks reject wrong source/config, missing tools/assets, syntax errors and changed bytes'
