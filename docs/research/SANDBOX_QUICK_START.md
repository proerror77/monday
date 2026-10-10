# Monday Research Sandbox - 5 分钟快速启动

## 🎯 目标

在 5 分钟内启动一个可用的研究开发环境，支持：
- ✅ 秒级代码切换
- ✅ 增量编译（< 1 分钟）
- ✅ 状态持久化
- ✅ OSS 数据访问

---

## 📋 前提条件

确认你有：
- [ ] `kubectl` 访问权限到 `monday-research-apne1` 集群
- [ ] 命名空间 `monday-research` 的权限
- [ ] ACR 镜像拉取凭证（`monday-acr` secret）

```bash
# 验证访问
kubectl get nodes
kubectl get ns monday-research
kubectl get secret monday-acr -n monday-research
```

---

## 🚀 快速部署（3 步）

### Step 1: 部署 DevPod（30 秒）

```bash
cd /Users/proerror/Documents/monday
kubectl apply -f deployment/aliyun/research/k8s/research-devpod-statefulset.yaml
```

预期输出：
```
statefulset.apps/research-devpod created
configmap/research-devpod-guide created
```

### Step 2: 等待启动（1-2 分钟）

```bash
kubectl get pod -n monday-research -w
```

等待状态变为 `Running`：
```
NAME                  READY   STATUS    RESTARTS   AGE
research-devpod-0     1/1     Running   0          90s
```

按 `Ctrl+C` 停止监控。

### Step 3: 连接并测试（30 秒）

```bash
kubectl exec -it research-devpod-0 -n monday-research -- bash
```

你会看到：
```
🚀 Research DevPod Starting...
📦 Installing tools...
✅ DevPod Ready!

📂 Workspace: /workspace
🔧 Cargo cache: /cargo-cache
🏗️  Build cache: /workspace/target

Available commands:
  - cargo build -p hft-collector --release
  - cargo test -p hft-collector segment_index
  - duckdb /lake/output/metadata/segments.parquet

Keeping pod alive...
```

---

## ✅ 验证清单

在 DevPod 内运行这些命令：

```bash
# 1. 检查代码已拉取
cd /workspace
git log -1 --oneline
# 应该看到最新的 commit

# 2. 检查 Rust 环境
cargo --version
# 应该是 1.98.1 或更新

# 3. 检查 DuckDB
duckdb --version
# 应该是 v1.1.3 或更新

# 4. 检查 OSS 挂载
ls -la /lake/raw | head -5
ls -la /lake/output | head -5
# 应该看到数据目录

# 5. 快速编译测试（可选，约 30 分钟）
cargo build -p hft-collector --release
```

---

## 🎨 基本工作流

### 切换分支（10 秒）

```bash
# 在 DevPod 内
cd /workspace

# 查看当前分支
git branch --show-current

# 切换到 main
git checkout main
git pull

# 切换到特性分支
git checkout feat/segment-index-and-replay-fix
git pull

# 测试 PR
git fetch origin pull/1240/head:pr-1240
git checkout pr-1240
```

### 增量编译（< 1 分钟）

```bash
# 首次编译（25-40 分钟）
cd /workspace
cargo build -p hft-collector --release

# 修改代码后，增量编译（< 1 分钟）
cargo build -p hft-collector --release

# 只编译特定二进制
cargo build -p hft-collector --bin segment-index-backfill --release
```

### 运行测试

```bash
# 单元测试
cargo test -p hft-collector segment_index

# 运行构建的工具
./target/release/segment-index-backfill --help

# 查询 DuckDB 索引
duckdb /lake/output/metadata/segments.parquet -c "
  SELECT COUNT(*) FROM read_parquet('/lake/output/metadata/segments.parquet')
"
```

---

## 💡 常用场景

### 场景 1: 测试新代码

```bash
# 本地机器
git checkout -b experiment/my-feature
# ... 编辑代码 ...
git commit -am "Add new feature"
git push origin experiment/my-feature

# DevPod 内
cd /workspace
git fetch origin
git checkout experiment/my-feature
git pull
cargo build -p hft-collector --release
cargo test -p hft-collector
```

### 场景 2: 调试问题

```bash
# 在 DevPod 内
cd /workspace
git checkout main
git pull

# 运行有问题的工具
RUST_LOG=debug ./target/release/segment-index-backfill \
  --raw-root /lake/raw \
  --index-output /tmp/debug.parquet \
  --market usdm \
  --start-date 2026-09-01 \
  --end-date 2026-09-02

# 检查输出
ls -lh /tmp/debug.parquet
duckdb /tmp/debug.parquet -c "SELECT * FROM read_parquet('/tmp/debug.parquet') LIMIT 5"
```

### 场景 3: 比较分支

```bash
# 测试 main 分支
cd /workspace
git checkout main
cargo build -p hft-collector --release
./target/release/segment-index-backfill --dry-run

# 测试特性分支
git checkout feat/segment-index-and-replay-fix
cargo build -p hft-collector --release
./target/release/segment-index-backfill --dry-run

# 比较结果
```

---

## 🔧 管理命令

### 重启 DevPod（保留状态）

```bash
# 删除 Pod
kubectl delete pod research-devpod-0 -n monday-research

# StatefulSet 会自动重建
# PVC 保留，代码和缓存不丢失
# 重启时间：< 1 分钟
```

### 查看日志

```bash
# 实时日志
kubectl logs -f research-devpod-0 -n monday-research

# 查看最近 100 行
kubectl logs --tail=100 research-devpod-0 -n monday-research
```

### 扩展多个开发者

```bash
# 创建 3 个独立的 DevPod
kubectl scale statefulset research-devpod --replicas=3 -n monday-research

# 连接到不同的实例
kubectl exec -it research-devpod-0 -n monday-research -- bash  # 开发者 1
kubectl exec -it research-devpod-1 -n monday-research -- bash  # 开发者 2
kubectl exec -it research-devpod-2 -n monday-research -- bash  # 开发者 3
```

### 清理

```bash
# 删除 DevPod（保留 PVC）
kubectl delete statefulset research-devpod -n monday-research

# 完全删除（包括 PVC）
kubectl delete statefulset research-devpod -n monday-research
kubectl delete pvc workspace-research-devpod-0 -n monday-research
kubectl delete pvc cargo-cache-research-devpod-0 -n monday-research
```

---

## 🐛 故障排查

### Pod 启动失败

```bash
# 查看详细状态
kubectl describe pod research-devpod-0 -n monday-research

# 常见问题
# - ImagePullBackOff: 检查 ACR 凭证
# - Pending: 检查节点资源
# - CrashLoopBackOff: 检查日志
```

### Git 同步失败

```bash
# 查看 init container 日志
kubectl logs research-devpod-0 -n monday-research -c git-sync

# 手动修复
kubectl exec -it research-devpod-0 -n monday-research -- bash
cd /workspace
git reset --hard HEAD
git pull origin feat/segment-index-and-replay-fix
```

### 编译失败

```bash
# 清理缓存
cd /workspace
cargo clean

# 检查磁盘空间
df -h /workspace
df -h /cargo-cache

# 重新编译
cargo build -p hft-collector --release
```

---

## 📚 下一步

现在你有了可用的开发环境，可以：

1. **部署段索引**: 参考 `SEGMENT_INDEX_DEPLOYMENT.md`
2. **运行研究任务**: 参考 `CAMPAIGN_WORKFLOW.md`
3. **构建生产镜像**: 使用 `kaniko-build-job.yaml`
4. **查看完整架构**: 阅读 `FAST_SANDBOX_COMPLETE_GUIDE.md`

---

## 🎯 性能对比

| 操作 | 传统方式 | DevPod | 提升 |
|------|---------|--------|------|
| 切换分支 | 60-90 分钟 | 10 秒 | **360-540x** |
| 增量编译 | 60-90 分钟 | < 1 分钟 | **60-90x** |
| 测试迭代 | 每次数小时 | 秒级 | **数千倍** |

---

## ✅ 成功标志

你已经成功部署如果：
- [x] Pod 状态是 `Running`
- [x] 可以进入 Pod 的 bash
- [x] 看到 "DevPod Ready!" 消息
- [x] `/workspace` 目录有代码
- [x] `cargo --version` 可用
- [x] `duckdb --version` 可用
- [x] `/lake/raw` 和 `/lake/output` 可访问

**恭喜！你现在有了一个快速的研究开发环境！** 🎉
