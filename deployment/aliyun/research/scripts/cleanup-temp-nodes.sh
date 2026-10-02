#!/bin/bash
# 释放两台手动加入的 ACK 节点。
# kubectl 用节点名，ECS API 用实例 ID。Campaign 或 DevPod 还在上面时直接退出。

set -euo pipefail

WORKER_ID="i-6we7cllyrp33tzudrv4a"
SYSTEM_ID="i-6we7cllyrp33txvcnt4i"

node_for() {
  kubectl get nodes -l "alibabacloud.com/ecs-instance-id=$1" -o jsonpath='{.items[0].metadata.name}'
}

blockers() {
  local node="$1"
  kubectl get pods -A --field-selector "spec.nodeName=${node}" -o json | jq -ers '
    if length != 1 then error("expected one PodList") else .[0] end
    | .items
    | if type != "array" then error("expected PodList.items array") else . end
    | map(
        (.metadata.ownerReferences // []) as $owners
        | if ($owners | type) != "array" then error("expected ownerReferences array") else . end
        | select(any($owners[]; .kind == "DaemonSet") | not)
        | select(.metadata.namespace != "kube-system" and .metadata.namespace != "ack-csi-fuse")
        | if (.metadata.namespace | type) != "string" or (.metadata.name | type) != "string"
          then error("expected pod namespace and name")
          else .metadata.namespace + "/" + .metadata.name end
      )
    | join("\n")
  '
}

release_one() {
  local instance_id="$1"
  local node
  node="$(node_for "$instance_id")" || return 1
  if [ -z "$node" ]; then
    echo "实例 ${instance_id} 不在集群里，跳过。"
    return 0
  fi
  local held
  # release_one is called under ||, so errexit alone cannot reject query/parse failures.
  held="$(blockers "$node")" || return 1
  if [ -n "$held" ]; then
    echo "拒绝释放 ${instance_id}（节点 ${node}）。上面还有："
    echo "$held"
    return 1
  fi
  echo "排空 ${node}"
  kubectl drain "$node" --ignore-daemonsets --delete-emptydir-data --timeout=300s
  kubectl delete node "$node"
  aliyun ecs StopInstance --InstanceId "$instance_id"
  aliyun ecs DeleteInstance --InstanceId "$instance_id" --Force true
  echo "已提交释放 ${instance_id}"
}

echo "检查 ${WORKER_ID} 和 ${SYSTEM_ID}"
failed=0
release_one "$WORKER_ID" || failed=1
release_one "$SYSTEM_ID" || failed=1
if [ "$failed" -ne 0 ]; then
  echo "有节点上还有工作负载，没有释放任何被拒绝的实例。"
  exit 1
fi
echo "两台实例都已处理。"
