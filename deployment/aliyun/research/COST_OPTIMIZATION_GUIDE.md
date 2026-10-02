# ACK Research 成本优化指南

> 2026-09-28 读回：下面的金额是审查时的估算，不是账单读回。DevPod 没有创建出 ESSD，按量云盘下单被余额拒绝。两台 ECS 仍被 campaign operator 和 guardian 使用，节点名是 `ap-northeast-1.172.31.1.69` 与 `ap-northeast-1.172.31.1.68`。释放只走 `scripts/cleanup-temp-nodes.sh`，它在节点上还有非 DaemonSet Pod 时退出。

**日期**: 2026-09-28  
**当前月度成本**: ~956元  
**优化目标**: ~796元（省 160元/月，17% 降幅）

---

## 📊 当前成本分析

### 固定成本（每月）
| 资源 | 规格 | 成本 | 备注 |
|------|------|------|------|
| monday-trade-data-26 | 2C8G + 80G系统盘 + 500G数据盘 | 590元 | 包月，collector ECS |
| ACK system 节点 | ecs.u1-c1m2.large (2C4G) | 63元 | Spot，必须保留 |
| system 磁盘 | 40GB PL0 | 14元 | |
| DevPod workspace PVC | 20Gi ESSD | 24元 | |
| DevPod cargo-cache PVC | 20Gi ESSD | 24元 | |
| NAT 网关 | 小规格 | 70元 | VPC 出网必需 |
| SLB 内网 | 小规格 | 40元 | API Server |
| OSS lake | ~300GB | 18元 | 14-30 天数据 |
| EIP | 1Mbps | 8元 | |
| **小计** | | **851元** | |

### 临时成本（应清理）
| 资源 | 规格 | 成本 | 状态 |
|------|------|------|------|
| 临时 research worker | ecs.u1-c1m4.xlarge (4C16G) | 63元/月 | ⚠️ 手动创建，应释放 |
| 临时 system 节点 | ecs.u1-c1m2.large (2C4G) | 63元/月 | ⚠️ 手动创建，应释放 |
| worker 磁盘 | 40GB PL0 + 100GB PL1 | ~28元/月 | 随节点释放 |
| **小计** | | **154元/月** | **待清理** |

### 按需成本（动态扩缩容）
| 资源 | 单价 | 月度预估 | 备注 |
|------|------|---------|------|
| research worker (Spot) | 0.342元/小时 | ~10元 | 假设每月运行 5 天 × 6 小时 |

---

## 🎯 优化方案

### 方案 A：清理临时节点 + 动态扩缩容（推荐）

**节省**: 126元/月（临时节点成本 - 按需成本）  
**风险**: 低（ACK 自动管理节点生命周期）

#### 1. 检查临时节点状态

```bash
# 列出所有节点
kubectl get nodes -o wide

# 检查临时 worker 节点上的 Pod
kubectl get pods --all-namespaces -o wide | grep i-6we7cllyrp33tzudrv4a

# 检查临时 system 节点上的 Pod
kubectl get pods --all-namespaces -o wide | grep i-6we7cllyrp33txvcnt4i

# 验证节点是否可安全移除（应该只有 DaemonSet Pod）
```

#### 2. 释放临时节点

实例 ID 不是 Kubernetes 节点名。节点上还有 Pod 时下面的脚本会退出，不会调用删除。

```bash
./deployment/aliyun/research/scripts/cleanup-temp-nodes.sh

# 验证实例已释放
aliyun ecs DescribeInstances --InstanceIds '["i-6we7cllyrp33tzudrv4a"]'
```

#### 3. 恢复 research pool 自动扩缩容

Campaign Pod 还在这两台节点上时，不要把节点池 desired 改成 0，也不要打开自动缩容。下面是节点空出来之后的操作记录。

```bash
# 获取集群和节点池 ID
CLUSTER_ID=$(aliyun cs DescribeClustersV1 | jq -r '.clusters[] | select(.name=="monday-research-apne1") | .cluster_id')
NODEPOOL_ID=$(aliyun cs DescribeClusterNodePools --ClusterId $CLUSTER_ID | jq -r '.nodepools[] | select(.nodepool_info.name | contains("research")) | .nodepool_id')

# 恢复自动扩缩容配置
aliyun cs ScaleClusterNodePool \
  --ClusterId $CLUSTER_ID \
  --NodepoolId $NODEPOOL_ID \
  --Count 0

aliyun cs ModifyClusterNodePool \
  --ClusterId $CLUSTER_ID \
  --NodepoolId $NODEPOOL_ID \
  --ScalingGroup.AutoScaling.Enable true \
  --ScalingGroup.DesiredSize 0 \
  --ScalingGroup.MinSize 0 \
  --ScalingGroup.MaxSize 3

# 验证配置
aliyun cs DescribeClusterNodePools --ClusterId $CLUSTER_ID | jq '.nodepools[] | select(.nodepool_info.name | contains("research"))'
```

#### 4. 测试动态扩缩容

```bash
# 创建测试 Pod（应触发节点创建）
kubectl apply -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: test-autoscaler
  namespace: monday-research
spec:
  nodeSelector:
    workload: backtest
  containers:
  - name: test
    image: busybox
    command: ["sleep", "3600"]
EOF

# 等待 2-5 分钟，检查节点是否自动创建
kubectl get nodes -w

# 验证 Pod 调度成功
kubectl -n monday-research get pod test-autoscaler -o wide

# 清理测试 Pod（节点应在 10-15 分钟后自动释放）
kubectl -n monday-research delete pod test-autoscaler

# 观察节点自动缩容
watch kubectl get nodes
```

**预期结果**:
- ✅ 有 Pod 需要调度 → 节点自动创建（2-5 分钟）
- ✅ Pod 完成/删除 → 节点空闲 10-15 分钟后自动释放
- ✅ 月度成本从 ~956元 降到 ~830元

---

### 方案 B：优化 PVC StorageClass（可选）

**节省**: 34元/月（DevPod PVC 成本）  
**风险**: 中（需重建 PVC，会丢失当前编译缓存）

#### 1. 评估当前 I/O 性能

```bash
# 进入 DevPod
kubectl -n monday-research exec -it research-devpod-0 -- bash

# 测试编译性能（作为基线）
cd /workspace
time cargo build -p segment-index-cli --release

# 测试磁盘 I/O（ESSD 基线）
dd if=/dev/zero of=/cargo-cache/test.bin bs=1M count=1000 oflag=direct
# 预期: ESSD ~150-200 MB/s

rm /cargo-cache/test.bin
exit
```

#### 2. 创建效率型云盘 PVC（测试）

```bash
# 创建测试 PVC
kubectl apply -f - <<EOF
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: cargo-cache-test
  namespace: monday-research
spec:
  accessModes:
    - ReadWriteOnce
  storageClassName: alicloud-disk-efficiency
  resources:
    requests:
      storage: 20Gi
EOF

# 创建测试 Pod 挂载新 PVC
kubectl apply -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: test-efficiency-disk
  namespace: monday-research
spec:
  containers:
  - name: test
    image: rust:1.98.1-bookworm
    command: ["sleep", "3600"]
    volumeMounts:
    - name: cargo-cache-test
      mountPath: /cargo-cache
  volumes:
  - name: cargo-cache-test
    persistentVolumeClaim:
      claimName: cargo-cache-test
EOF

# 等待 Pod 启动
kubectl -n monday-research wait --for=condition=Ready pod/test-efficiency-disk --timeout=300s

# 测试效率型云盘 I/O
kubectl -n monday-research exec test-efficiency-disk -- dd if=/dev/zero of=/cargo-cache/test.bin bs=1M count=1000 oflag=direct
# 预期: 效率型 ~50-80 MB/s（慢 2-3 倍，但够用）

# 清理测试资源
kubectl -n monday-research delete pod test-efficiency-disk
kubectl -n monday-research delete pvc cargo-cache-test
```

#### 3. 如果性能可接受，迁移 DevPod PVC

```bash
# ⚠️ 警告：此操作会丢失当前编译缓存（workspace 代码可从 git 恢复）

# 1. 备份 workspace（如有未提交改动）
kubectl -n monday-research exec research-devpod-0 -- bash -c 'cd /workspace && git status'

# 2. 删除 DevPod StatefulSet（保留 PVC）
kubectl -n monday-research delete statefulset research-devpod --cascade=orphan

# 3. 删除旧 PVC
kubectl -n monday-research delete pvc workspace-research-devpod-0 cargo-cache-research-devpod-0

# 4. 修改 StatefulSet YAML
# 将 storageClassName 从 monday-spot-essd-689 改为 alicloud-disk-efficiency

# 5. 重新部署 DevPod
kubectl apply -f deployment/aliyun/research/k8s/research-devpod-statefulset.yaml

# 6. 等待首次编译完成（20-30 分钟，比 ESSD 慢 ~5 分钟）
kubectl -n monday-research logs -f research-devpod-0
```

**成本对比**:
```
ESSD (当前):
  workspace 20Gi: 24元/月
  cargo-cache 20Gi: 24元/月
  总计: 48元/月

效率型云盘:
  workspace 20Gi: 7元/月
  cargo-cache 20Gi: 7元/月
  总计: 14元/月

节省: 34元/月（71% 降幅）
```

**权衡**:
- ✅ 省 34元/月
- ⚠️ 首次编译慢 ~5 分钟（25→30 分钟）
- ⚠️ 增量编译慢 ~10-15 秒（<1分钟 → ~1分钟）
- ✅ 对研究回放无影响（DevPod 不再挂载生产数据）

---

### 方案 C：组合优化（最大节省）

同时执行方案 A + B：

**总节省**: 160元/月  
**最终成本**: 796元/月（相比当前 956元，降幅 17%）

```
当前成本: 956元
├─ 方案 A（清理临时节点）: -126元
└─ 方案 B（优化 PVC）: -34元
= 最终成本: 796元
```

---

## 🔐 成本监控

### 1. 启用阿里云成本分析

```bash
# 安装 bssopenapi
pip install aliyun-python-sdk-bssopenapi

# 设置端点（国际区域）
export ALIYUN_BUSINESS_ENDPOINT=business.aliyuncs.com

# 查询当前账期费用
aliyun bssopenapi QueryAccountBalance --endpoint $ALIYUN_BUSINESS_ENDPOINT

# 查询 ECS 实例账单
aliyun bssopenapi QueryInstanceBill \
  --BillingCycle $(date +%Y-%m) \
  --ProductCode ecs \
  --endpoint $ALIYUN_BUSINESS_ENDPOINT
```

### 2. 监控孤儿资源

```bash
# 列出所有 Stopped 但未释放的 ECS 实例
aliyun ecs DescribeInstances --Status Stopped | jq '.Instances.Instance[] | {id: .InstanceId, name: .InstanceName, status: .Status}'

# 列出未挂载的 PVC
kubectl get pvc --all-namespaces | grep -v Bound

# 列出未使用的 OSS Bucket
aliyun oss ls

# 列出闲置的负载均衡器
aliyun slb DescribeLoadBalancers | jq '.LoadBalancers.LoadBalancer[] | select(.BackendServers.BackendServer | length == 0)'
```

### 3. 设置成本告警

```bash
# 在阿里云控制台创建成本告警：
# 1. 费用中心 → 成本管理 → 预算
# 2. 设置月度预算: 1000元
# 3. 告警阈值: 80%（800元）, 90%（900元）, 100%（1000元）
# 4. 通知方式: 邮件 + 短信
```

---

## 📋 操作检查清单

### 立即执行（省 126元/月）

- [ ] 检查临时节点状态（无关键 Pod）
- [ ] 排空并删除临时 worker 节点 `i-6we7cllyrp33tzudrv4a`
- [ ] 排空并删除临时 system 节点 `i-6we7cllyrp33txvcnt4i`
- [ ] 恢复 research pool 自动扩缩容（max=3）
- [ ] 测试动态扩缩容（创建测试 Pod → 验证节点创建 → 删除 Pod → 验证节点释放）

### 可选执行（省 34元/月）

- [ ] 测试效率型云盘 I/O 性能
- [ ] 评估编译时间增加是否可接受（~5 分钟）
- [ ] 备份 DevPod workspace（如有未提交改动）
- [ ] 删除并重建 DevPod PVC（改用 alicloud-disk-efficiency）
- [ ] 验证首次编译完成

### 持续监控

- [ ] 每月检查账单（费用中心）
- [ ] 每周检查孤儿资源（ECS/PVC/PV）
- [ ] 每月验证自动扩缩容工作正常（Pod 创建 → 节点扩容 → Pod 删除 → 节点缩容）

---

## 🎯 优化效果对比

| 场景 | 月度成本 | 节省 | 备注 |
|------|---------|------|------|
| 当前状态 | 956元 | - | 2 个临时节点持续计费 |
| 方案 A（清理 + 动态扩缩容） | 830元 | 126元 (13%) | 推荐，低风险 |
| 方案 A + B（组合优化） | 796元 | 160元 (17%) | 最大节省，需接受编译慢 ~5 分钟 |

---

## ⚠️ 风险提示

1. **动态扩缩容延迟**: 节点创建需 2-5 分钟，紧急任务需提前触发
2. **Spot 中断**: Spot 实例可能被回收（概率 <3%），Job 需支持重试
3. **PVC 迁移风险**: 改 StorageClass 需重建 PVC，会丢失编译缓存
4. **成本波动**: Spot 价格会浮动（±10%），按需成本可能略高于预估

---

## 📞 支持

如遇问题，检查：
1. 节点释放失败 → 检查是否有 DaemonSet 或 static Pod 阻止排空
2. 自动扩缩容不工作 → 检查 goatscaler 日志：`kubectl -n kube-system logs -l app=goatscaler`
3. PVC 重建失败 → 检查 StorageClass 是否存在：`kubectl get storageclass`
4. 成本异常 → 查询详细账单：`aliyun bssopenapi QueryInstanceBill`
