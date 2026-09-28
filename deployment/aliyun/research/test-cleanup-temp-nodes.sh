#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cleanup="$script_dir/scripts/cleanup-temp-nodes.sh"
root="$(mktemp -d)"
trap 'rm -rf -- "$root"' EXIT
mkdir "$root/bin"
export FAKE_ROOT="$root"
export REAL_JQ
REAL_JQ="$(command -v jq)"

# Every cloud command is intercepted; these tests never contact ACK or ECS.
cat >"$root/bin/kubectl" <<'EOF'
#!/bin/bash
set -euo pipefail
case "$1 $2" in
  'get nodes')
    [[ "$FAKE_CASE" != node-query-failure ]] || exit 1
    [[ "$FAKE_CASE" != absent-node ]] || exit 0
    case "$4" in
      alibabacloud.com/ecs-instance-id=i-6we7cllyrp33tzudrv4a) echo worker-node ;;
      alibabacloud.com/ecs-instance-id=i-6we7cllyrp33txvcnt4i) echo system-node ;;
      *) exit 99 ;;
    esac
    ;;
  'get pods')
    [[ "$FAKE_CASE" != pod-query-failure ]] || exit 1
    if [[ "$FAKE_CASE" == mixed && "$5" == spec.nodeName=system-node ]]; then
      printf '{"items":[]}\n'
    else
      cat "$FAKE_ROOT/pods.json"
    fi
    [[ "$FAKE_CASE" != pod-query-failure-with-json ]] || exit 1
    ;;
  'drain worker-node'|'drain system-node'|'delete node')
    printf 'kubectl %s\n' "$*" >>"$FAKE_ROOT/mutations"
    ;;
  *) exit 99 ;;
esac
EOF

cat >"$root/bin/aliyun" <<'EOF'
#!/bin/bash
set -euo pipefail
case "$1 $2" in
  'ecs StopInstance'|'ecs DeleteInstance')
    printf 'aliyun %s\n' "$*" >>"$FAKE_ROOT/mutations"
    ;;
  *) exit 99 ;;
esac
EOF

cat >"$root/bin/jq" <<'EOF'
#!/bin/bash
[[ "$FAKE_CASE" != jq-failure ]] || exit 127
exec "$REAL_JQ" "$@"
EOF
chmod +x "$root/bin/"*

expected_release() {
  local node="$1" instance="$2"
  printf '%s\n' \
    "kubectl drain $node --ignore-daemonsets --delete-emptydir-data --timeout=300s" \
    "kubectl delete node $node" \
    "aliyun ecs StopInstance --InstanceId $instance" \
    "aliyun ecs DeleteInstance --InstanceId $instance --Force true"
}

run_case() {
  local name="$1" pods="$2" expected_status="$3" releases="${4:-none}" status=0
  printf '%s' "$pods" >"$root/pods.json"
  : >"$root/mutations"
  : >"$root/expected"
  case "$releases" in
    both)
      expected_release worker-node i-6we7cllyrp33tzudrv4a >>"$root/expected"
      expected_release system-node i-6we7cllyrp33txvcnt4i >>"$root/expected"
      ;;
    system) expected_release system-node i-6we7cllyrp33txvcnt4i >>"$root/expected" ;;
  esac
  PATH="$root/bin:$PATH" FAKE_CASE="$name" bash "$cleanup" >"$root/output" 2>&1 || status=$?
  if [[ "$status" != "$expected_status" ]]; then
    cat "$root/output" >&2
    printf '%s: expected exit %s, got %s\n' "$name" "$expected_status" "$status" >&2
    exit 1
  fi
  diff -u "$root/expected" "$root/mutations"
  printf 'PASS %s\n' "$name"
}

workloads='{"items":[
  {"metadata":{"namespace":"research","name":"campaign","ownerReferences":[{"kind":"Job"}]}},
  {"metadata":{"namespace":"dev","name":"devpod"}},
  {"metadata":{"namespace":"dev","name":"null-owners","ownerReferences":null}},
  {"metadata":{"namespace":"dev","name":"empty-owners","ownerReferences":[]}},
  {"metadata":{"namespace":"research","name":"completed","ownerReferences":[{"kind":"Job"}]},"status":{"phase":"Succeeded"}}
]}'
run_case workloads "$workloads" 1
for held in research/campaign dev/devpod dev/null-owners dev/empty-owners research/completed; do
  grep -Fxq "$held" "$root/output"
done
run_case mixed "$workloads" 1 system
run_case exempt '{"items":[
  {"metadata":{"namespace":"research","name":"agent","ownerReferences":[{"kind":"Job"},{"kind":"DaemonSet"}]}},
  {"metadata":{"namespace":"kube-system","name":"core"}},
  {"metadata":{"namespace":"ack-csi-fuse","name":"fuse","ownerReferences":null}}
]}' 0 both
run_case empty-list '{"items":[]}' 0 both
run_case absent-node '' 0

for failure in node-query-failure pod-query-failure pod-query-failure-with-json jq-failure; do
  run_case "$failure" '{"items":[]}' 1
done
run_case empty-input '' 1
run_case invalid-json '{' 1
run_case missing-items '{}' 1
run_case null-items '{"items":null}' 1
run_case object-items '{"items":{}}' 1
run_case multiple-lists '{"items":[]} {"items":[]}' 1
run_case null-pod '{"items":[null]}' 1
run_case missing-namespace '{"items":[{"metadata":{"name":"unknown"}}]}' 1
run_case missing-name '{"items":[{"metadata":{"namespace":"research"}}]}' 1
run_case invalid-name '{"items":[{"metadata":{"namespace":"research","name":1}}]}' 1
run_case invalid-owners '{"items":[{"metadata":{"namespace":"research","name":"unknown","ownerReferences":{}}}]}' 1
run_case invalid-owner '{"items":[{"metadata":{"namespace":"research","name":"unknown","ownerReferences":[1]}}]}' 1
printf 'cleanup-temp-nodes contract tests passed\n'
