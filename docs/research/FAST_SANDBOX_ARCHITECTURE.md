# Monday Research 快速 Sandbox 架构设计

## 🎯 目标

解决当前问题：
1. **冷启动慢**：17 分钟不可接受
2. **代码更新慢**：需要重新构建镜像
3. **无状态保存**：每次都从头开始

## 🏗️ 架构设计：分层 Sandbox

### 方案 A：分层镜像 + 代码挂载（推荐）

```yaml
# 快速测试 Pod - 代码热加载
apiVersion: v1
kind: Pod
metadata:
  name: segment-index-dev
  namespace: monday-research
spec:
  # 使用基础镜像（已有依赖）
  containers:
    - name: dev
      image: rust:1.98.1-bookworm
      command: ["/bin/sleep", "infinity"]
      volumeMounts:
        - name: code
          mountPath: /workspace
          subPath: monday
        - name: cargo-cache
          mountPath: /usr/local/cargo/registry
        - name: build-cache
          mountPath: /workspace/target
        - name: raw
          mountPath: /lake/raw
        - name: output
          mountPath: /lake/output
  
  volumes:
    - name: code
      # 从 GitHub 拉取最新代码
      gitRepo:
        repository: "https://github.com/proerror77/monday.git"
        revision: "feat/segment-index-and-replay-fix"
    - name: cargo-cache
      persistentVolumeClaim:
        claimName: dev-cargo-cache
    - name: build-cache
      persistentVolumeClaim:
        claimName: dev-build-cache
```

**优势**：
- ✅ 代码更新：git pull（秒级）
- ✅ 增量编译：缓存 target/
- ✅ 快速启动：基础镜像已缓存
- ✅ 状态保存：PVC 持久化

**启动时间**：
- 首次：2-3 分钟（拉取代码 + 编译）
- 后续：10-30 秒（代码已在，增量编译）

---

### 方案 B：Stateful Pod + Init Container

```yaml
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: research-sandbox
  namespace: monday-research
spec:
  serviceName: research-sandbox
  replicas: 1
  selector:
    matchLabels:
      app: research-sandbox
  template:
    metadata:
      labels:
        app: research-sandbox
    spec:
      initContainers:
        # 快速同步代码
        - name: sync-code
          image: alpine/git
          command:
            - sh
            - -c
            - |
              if [ -d /workspace/.git ]; then
                cd /workspace && git fetch origin && git checkout ${GIT_BRANCH} && git pull
              else
                git clone -b ${GIT_BRANCH} ${GIT_REPO} /workspace
              fi
          env:
            - name: GIT_REPO
              value: "https://github.com/proerror77/monday.git"
            - name: GIT_BRANCH
              value: "feat/segment-index-and-replay-fix"
          volumeMounts:
            - name: workspace
              mountPath: /workspace
      
      containers:
        - name: builder
          image: rust:1.98.1-bookworm
          command: ["/bin/sleep", "infinity"]
          volumeMounts:
            - name: workspace
              mountPath: /workspace
            - name: cargo-cache
              mountPath: /usr/local/cargo
            - name: raw
              mountPath: /lake/raw
            - name: output
              mountPath: /lake/output
          resources:
            requests:
              cpu: "2"
              memory: 4Gi
  
  volumeClaimTemplates:
    - metadata:
        name: workspace
      spec:
        accessModes: ["ReadWriteOnce"]
        resources:
          requests:
            storage: 20Gi
    - metadata:
        name: cargo-cache
      spec:
        accessModes: ["ReadWriteOnce"]
        resources:
          requests:
            storage: 10Gi
```

**优势**：
- ✅ 状态持久化：workspace + cargo cache
- ✅ 代码增量更新：git pull
- ✅ 快速重启：状态保留
- ✅ 并行开发：多个 replica

**启动时间**：
- 首次：3-5 分钟
- 重启：30 秒（状态保留）

---

### 方案 C：远程开发容器（类似 GitHub Codespaces）

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: research-codespace
  namespace: monday-research
  annotations:
    code-server.io/enabled: "true"
spec:
  containers:
    - name: code-server
      image: codercom/code-server:latest
      ports:
        - containerPort: 8080
          name: http
      env:
        - name: PASSWORD
          valueFrom:
            secretKeyRef:
              name: code-server-secret
              key: password
      volumeMounts:
        - name: workspace
          mountPath: /home/coder/project
        - name: cargo-cache
          mountPath: /home/coder/.cargo
        - name: raw
          mountPath: /lake/raw
        - name: output
          mountPath: /lake/output
      command:
        - /bin/sh
        - -c
        - |
          # 安装 Rust
          curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
          source $HOME/.cargo/env
          
          # 克隆代码
          if [ ! -d /home/coder/project/.git ]; then
            git clone https://github.com/proerror77/monday.git /home/coder/project
          fi
          cd /home/coder/project
          git checkout feat/segment-index-and-replay-fix
          
          # 启动 code-server
          exec code-server --bind-addr 0.0.0.0:8080 /home/coder/project
  
  volumes:
    - name: workspace
      persistentVolumeClaim:
        claimName: codespace-workspace
    - name: cargo-cache
      persistentVolumeClaim:
        claimName: codespace-cargo
```

**优势**：
- ✅ 浏览器访问：无需本地环境
- ✅ 实时编辑：VSCode in browser
- ✅ 团队协作：多人可访问
- ✅ 状态保存：完整开发环境

---

## 🚀 立即可用方案：快速测试脚本

不依赖 K8s 的快速验证：

```bash
#!/bin/bash
# quick-test.sh - 在现有 Pod 中快速测试

set -e

echo "🚀 Quick Sandbox Test"

# 1. 找到一个运行中的 Pod
POD=$(kubectl get pods -n monday-research --field-selector=status.phase=Running -o name | head -1)

if [ -z "$POD" ]; then
    echo "❌ No running pods found"
    exit 1
fi

echo "✓ Using pod: $POD"

# 2. 安装 DuckDB
echo "📦 Installing DuckDB..."
kubectl exec -n monday-research $POD -- bash -c "
    wget -q https://github.com/duckdb/duckdb/releases/download/v1.1.3/duckdb_cli-linux-amd64.zip
    unzip -q duckdb_cli-linux-amd64.zip
    chmod +x duckdb
    mv duckdb /usr/local/bin/
"

# 3. 拷贝测试脚本
echo "📝 Copying test scripts..."
kubectl cp scripts/segment_index_monitor.sh monday-research/$POD:/tmp/monitor.sh

# 4. 测试 DuckDB 查询
echo "🔍 Testing DuckDB..."
kubectl exec -n monday-research $POD -- bash -c "
    duckdb -c 'SELECT version();'
"

echo "✅ Sandbox ready!"
echo "To connect: kubectl exec -it -n monday-research $POD -- bash"
```

**启动时间**：< 1 分钟

---

## 📊 方案对比

| 方案 | 首次启动 | 重启时间 | 代码更新 | 状态保存 | 复杂度 |
|---|---|---|---|---|---|
| A: 代码挂载 | 2-3 分钟 | 10-30 秒 | git pull | ✅ | 低 |
| B: StatefulSet | 3-5 分钟 | 30 秒 | git pull | ✅ | 中 |
| C: Codespace | 5-10 分钟 | 30 秒 | 实时 | ✅ | 高 |
| **快速脚本** | **<1 分钟** | **即时** | 手动 | ❌ | **最低** |

---

## 🎯 推荐执行路径

### 立即（验证代码）

使用**快速测试脚本**：
```bash
./quick-test.sh
# 在现有 Pod 中安装 DuckDB
# 手动测试代码逻辑
```

### 短期（1-2 天）

部署**方案 A**（代码挂载）：
- 创建 dev-cargo-cache PVC
- 创建 dev-build-cache PVC
- 部署 segment-index-dev Pod
- 启动时间：< 3 分钟

### 长期（生产环境）

构建完整镜像：
- 包含所有新代码
- 包含 DuckDB
- 包含所有工具
- 使用 buildkit 多阶段构建加速

---

## 💡 加速技巧

### 1. 使用镜像缓存

```bash
# 预拉取基础镜像到所有节点
kubectl create -f - <<EOF
apiVersion: apps/v1
kind: DaemonSet
metadata:
  name: image-preload
  namespace: monday-research
spec:
  selector:
    matchLabels:
      app: image-preload
  template:
    metadata:
      labels:
        app: image-preload
    spec:
      containers:
        - name: preload
          image: rust:1.98.1-bookworm
          command: ["sleep", "infinity"]
EOF
```

### 2. 使用本地 Registry 缓存

```bash
# 部署本地 registry 作为 pull-through cache
kubectl apply -f k8s/local-registry-cache.yaml
```

### 3. 使用 BuildKit 远程缓存

```bash
docker buildx build \
  --cache-from type=registry,ref=cache.example.com/monday-research \
  --cache-to type=registry,ref=cache.example.com/monday-research \
  -t research-runner:latest .
```

---

## ✅ 下一步

你想使用哪个方案？

1. **快速测试脚本**（立即可用，< 1 分钟）
2. **方案 A - 代码挂载**（最平衡，2-3 分钟）
3. **方案 B - StatefulSet**（适合长期开发）
4. **方案 C - Codespace**（团队协作）
