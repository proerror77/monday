# Monday Research 部署现状与建议

## 📋 当前情况

### 已完成
1. ✅ **代码开发完成**：1700+ 行代码，3500+ 行文档
2. ✅ **PR 已提交**：#1240 等待审查
3. ✅ **功能验证**：本地逻辑正确
4. ✅ **架构设计**：零部署方案完整

### 遇到的问题
1. ❌ **镜像未构建**：新代码不在现有镜像中
2. ❌ **Pod 环境受限**：无 git、unzip、DuckDB
3. ❌ **启动速度慢**：17 分钟冷启动不可接受
4. ❌ **工具缺失**：现有 Pod 无法直接测试

---

## 🎯 实际可行的部署路径

### 方案 1：通过 PR 合并触发 CI 构建（推荐）

**流程**：
```
1. PR #1240 审查通过
   ↓
2. 合并到 main
   ↓
3. GitHub Actions/CI 自动构建镜像
   ↓
4. 推送到 ACR: research-runner:latest
   ↓
5. 在 ACK 中使用新镜像测试
```

**优势**：
- ✅ 标准化流程
- ✅ 自动化构建
- ✅ 版本可追溯
- ✅ 团队同步

**时间**：
- PR 审查：1-2 天
- CI 构建：30-60 分钟
- 总计：1-2 天

---

### 方案 2：本地构建并推送镜像（快速）

**流程**：
```bash
# 1. 切换到特性分支
cd /Users/proerror/Documents/monday
git checkout feat/segment-index-and-replay-fix

# 2. 本地构建镜像（需要 Docker）
docker build \
  -f rust_hft/deployment/docker/Dockerfile.research \
  --build-arg SOURCE_REVISION=$(git rev-parse HEAD) \
  -t research-runner:segment-index-test \
  .

# 3. 标记并推送到 ACR
docker tag research-runner:segment-index-test \
  crpi-ygobwehhof7qs9m3-vpc.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/research-runner:segment-index-test

docker push crpi-ygobwehhof7qs9m3-vpc.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/research-runner:segment-index-test

# 4. 在 ACK 中使用新镜像
kubectl apply -f deployment/aliyun/research/k8s/segment-index-test-pod.yaml
```

**优势**：
- ✅ 立即可用
- ✅ 完全控制
- ✅ 快速迭代

**劣势**：
- ❌ 需要本地 Docker
- ❌ 需要 ACR 推送权限
- ❌ 镜像构建耗时（60-90 分钟）

**时间**：
- 构建：60-90 分钟
- 推送：5-10 分钟
- 总计：1.5-2 小时

---

### 方案 3：使用 ACK 内的 Kaniko 构建（云端构建）

**创建构建 Job**：
```yaml
apiVersion: batch/v1
kind: Job
metadata:
  name: build-segment-index
  namespace: monday-research
spec:
  template:
    spec:
      containers:
        - name: kaniko
          image: gcr.io/kaniko-project/executor:latest
          args:
            - --context=git://github.com/proerror77/monday.git#feat/segment-index-and-replay-fix
            - --dockerfile=rust_hft/deployment/docker/Dockerfile.research
            - --destination=crpi-ygobwehhof7qs9m3-vpc.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/research-runner:segment-index-test
            - --cache=true
          volumeMounts:
            - name: docker-config
              mountPath: /kaniko/.docker/
      volumes:
        - name: docker-config
          secret:
            secretName: acr-secret
      restartPolicy: Never
```

**优势**：
- ✅ 云端构建，本地无需 Docker
- ✅ 利用集群资源
- ✅ 自动推送到 ACR

**时间**：
- 构建：30-60 分钟（并行）
- 总计：30-60 分钟

---

### 方案 4：先部署基础设施，后续更新代码（渐进式）

**阶段 1：部署 Backfill Job（使用现有代码）**
```bash
# 即使没有新代码，也可以：
# 1. 创建索引 PVC
# 2. 部署健康检查 CronJob
# 3. 准备基础设施
```

**阶段 2：PR 合并后自动更新**
```bash
# PR 合并 → CI 构建 → 自动部署新镜像
```

**优势**：
- ✅ 逐步推进
- ✅ 降低风险
- ✅ 基础设施先行

---

## 📊 方案对比

| 方案 | 时间 | 复杂度 | 推荐度 | 备注 |
|---|---|---|---|---|
| 1. PR 合并 + CI | 1-2 天 | 低 | ⭐⭐⭐⭐⭐ | 最标准 |
| 2. 本地构建 | 1.5-2 小时 | 中 | ⭐⭐⭐⭐ | 需要权限 |
| 3. Kaniko 云端构建 | 30-60 分钟 | 中 | ⭐⭐⭐⭐ | 最快 |
| 4. 渐进式部署 | 立即开始 | 低 | ⭐⭐⭐ | 最安全 |

---

## 🚀 立即可行的行动

### A. 如果你有 ACR 推送权限

```bash
# 检查 Docker 登录
docker login crpi-ygobwehhof7qs9m3-vpc.ap-northeast-1.personal.cr.aliyuncs.com

# 开始构建
cd /Users/proerror/Documents/monday
git checkout feat/segment-index-and-replay-fix
./scripts/build-and-push-image.sh
```

### B. 如果等待 PR 合并

**当前可以做的**：
1. ✅ 审查 PR #1240
2. ✅ 准备部署文档
3. ✅ 创建索引 PVC
4. ✅ 部署监控 CronJob
5. ✅ 准备回滚计划

### C. 如果使用 Kaniko

```bash
# 1. 创建 ACR Secret
kubectl create secret docker-registry acr-secret \
  --docker-server=crpi-ygobwehhof7qs9m3-vpc.ap-northeast-1.personal.cr.aliyuncs.com \
  --docker-username=<username> \
  --docker-password=<password> \
  -n monday-research

# 2. 部署构建 Job
kubectl apply -f deployment/aliyun/research/k8s/kaniko-build-job.yaml

# 3. 监控构建
kubectl logs -f job/build-segment-index -n monday-research
```

---

## 💡 关于启动速度优化

### 为什么现在启动慢？

1. **镜像大小**：Rust 镜像 + 依赖 ≈ 2-3 GB
2. **冷启动**：节点首次拉取镜像
3. **挂载延迟**：OSS FUSE 挂载需要时间
4. **初始化**：容器启动 + 环境设置

### 解决方案

#### 短期（1-2 天）
```yaml
# 使用 Pod 亲和性，复用已有镜像的节点
affinity:
  podAffinity:
    preferredDuringSchedulingIgnoredDuringExecution:
      - weight: 100
        podAffinityTerm:
          topologyKey: kubernetes.io/hostname
          labelSelector:
            matchLabels:
              app: research-runner
```

#### 中期（1 周）
```yaml
# 使用 DaemonSet 预热镜像
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: image-warmer
spec:
  template:
    spec:
      containers:
        - name: warmer
          image: research-runner:latest
          command: ["sleep", "infinity"]
```

#### 长期（类似 Google AX）
```yaml
# 使用 VirtualKubelet + Firecracker
# 或 Kata Containers 实现快速启动
# 启动时间：< 1 秒
```

---

## ✅ 我的建议

### 立即执行（今天）
1. **审查并合并 PR #1240**
2. **等待 CI 构建镜像**（如果有自动化）
3. **或使用 Kaniko 云端构建**（最快）

### 明天
1. **使用新镜像部署 Backfill Job**
2. **运行健康检查验证**
3. **文档化部署流程**

### 本周
1. **全量部署到生产**
2. **监控性能提升**
3. **优化启动速度**

---

## 🤔 你想怎么做？

请告诉我：
1. **你有 ACR 推送权限吗？**
2. **你倾向于哪个方案？**
3. **是否需要我创建 Kaniko 构建 Job？**
4. **PR 什么时候可以合并？**
