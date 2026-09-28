# DuckDB Segment Index 完整部署指南

## 📋 概述

本指南介绍如何将 Parquet 段索引集成到 Monday ACK 研究架构中，实现：
- 准备时间：20小时 → 10分钟（120倍加速）
- 成本节省：99%（CNY 6.84 → CNY 0.06）
- 零额外部署：利用现有 DuckDB + OSS

---

## 🎯 架构对比

### 当前（文件系统扫描）
```
准备 Job 启动
  ↓ 扫描 OSS 目录（5-10分钟，已优化）
  ↓ 读取每个 manifest.json
  ↓ 校验重放标志
  ↓ 选择文件
  ↓ 物化
```

### 优化后（Parquet 索引）
```
Collector 写 manifest → 同步写 segments.parquet
准备 Job 启动
  ↓ DuckDB 查询 Parquet（<1秒）
  ↓ 直接获取文件列表
  ↓ 物化
```

---

## 🚀 部署步骤

### 阶段 1：代码集成（第 1 天）

#### 1.1 添加 DuckDB 依赖

```toml
# rust_hft/tools/collector/Cargo.toml
[dependencies]
duckdb = { version = "1.0", features = ["bundled"] }
arrow = "52.0"
parquet = "52.0"
```

#### 1.2 编译并测试

```bash
cd /Users/proerror/Documents/monday

# 编译新模块
cargo build -p hft-collector --release

# 运行测试
cargo test -p hft-collector segment_index

# 验证二进制
cargo build -p hft-collector --bin segment-index-backfill --release
```

### 阶段 2：回填历史数据（第 1-2 天）

#### 2.1 准备 PVC（如果需要）

```bash
# 如果 output PVC 不存在，创建一个
kubectl apply -f - <<EOF
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: monday-research-output
  namespace: monday-research
spec:
  accessModes:
    - ReadWriteMany
  resources:
    requests:
      storage: 10Gi
  storageClassName: alicloud-nas
EOF
```

#### 2.2 构建镜像

```bash
# 构建包含新二进制的镜像
cd rust_hft/deployment/docker
docker build -f Dockerfile.research -t monday-research-runner:latest .

# 推送到 ACR
docker tag monday-research-runner:latest \
  crpi-ygobwehhof7qs9m3-vpc.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/research-runner:v1.0-index

docker push crpi-ygobwehhof7qs9m3-vpc.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/research-runner:v1.0-index
```

#### 2.3 更新 Job YAML

```bash
# 获取镜像 SHA256
IMAGE_DIGEST=$(docker inspect --format='{{index .RepoDigests 0}}' \
  crpi-ygobwehhof7qs9m3-vpc.ap-northeast-1.personal.cr.aliyuncs.com/wildcard0923/research-runner:v1.0-index | \
  sed 's/.*@sha256://')

# 替换 YAML 中的占位符
cd /Users/proerror/Documents/monday
sed -i.bak \
  -e "s/REPLACE_WITH_DIGEST/$IMAGE_DIGEST/g" \
  -e "s/REPLACE_WITH_RAW_PVC_NAME/monday-raw-oss/g" \
  -e "s/REPLACE_WITH_OUTPUT_PVC_NAME/monday-research-output/g" \
  -e "s/REPLACE_WITH_SOURCE_SHA/$(git rev-parse HEAD)/g" \
  deployment/aliyun/research/k8s/segment-index-backfill-job.yaml
```

#### 2.4 运行 Backfill Job

```bash
# 应用 Job
kubectl apply -f deployment/aliyun/research/k8s/segment-index-backfill-job.yaml

# 启动
kubectl patch job segment-index-backfill -n monday-research \
  --type=json -p='[{"op":"replace","path":"/spec/suspend","value":false}]'

# 监控进度
kubectl logs -n monday-research -l app.kubernetes.io/name=segment-index-backfill -f
```

#### 2.5 验证输出

```bash
# 检查 Parquet 文件是否生成
kubectl exec -n monday-research -it \
  $(kubectl get pod -n monday-research -l app.kubernetes.io/name=segment-index-backfill -o name) \
  -- ls -lh /lake/output/metadata/segments.parquet

# 快速查询测试（从 Pod 内）
kubectl exec -n monday-research -it \
  $(kubectl get pod -n monday-research -l app.kubernetes.io/name=segment-index-backfill -o name) \
  -- /usr/local/bin/duckdb -c \
  "SELECT COUNT(*), 
          SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe_count,
          MIN(date) as first_date,
          MAX(date) as last_date
   FROM read_parquet('/lake/output/metadata/segments.parquet')"
```

预期输出：
```
┌──────────┬────────────┬────────────┬────────────┐
│ count(*) │ safe_count │ first_date │ last_date  │
├──────────┼────────────┼────────────┼────────────┤
│ 1440     │ 1438       │ 2026-09-01 │ 2026-09-30 │
└──────────┴────────────┴────────────┴────────────┘
```

### 阶段 3：更新 Collector（第 2 天）

#### 3.1 修改 binance-lob-archiver

```rust
// rust_hft/tools/collector/src/bin/binance-lob-archiver.rs

use hft_collector::segment_index::{append_to_parquet_index, SegmentMetadata};

fn close_segment(segment: &Segment) -> Result<()> {
    // 1. 写入 manifest.json（现有）
    segment.write_manifest()?;
    
    // 2. 同步写入 Parquet 索引（新增）
    if let Ok(index_path) = std::env::var("SEGMENT_INDEX_PATH") {
        let manifest = segment.read_manifest()?;
        let metadata = SegmentMetadata::from_manifest(
            &manifest,
            &segment.manifest_path(),
            &segment.tape_path(),
        )?;
        
        if let Err(e) = append_to_parquet_index(&PathBuf::from(&index_path), &metadata) {
            log::warn!("Failed to update segment index: {}", e);
            // 不阻塞主流程
        }
    }
    
    Ok(())
}
```

#### 3.2 更新 Collector 部署

在 ECS 上设置环境变量：

```bash
# 在 collector ECS 实例上
export SEGMENT_INDEX_PATH=/mnt/oss-output/metadata/segments.parquet

# 或者在 systemd 服务配置中
[Service]
Environment="SEGMENT_INDEX_PATH=/mnt/oss-output/metadata/segments.parquet"
```

### 阶段 4：更新准备 Job（第 2-3 天）

#### 4.1 修改准备入口

```rust
// rust_hft/alpha-harness/app/src/mission_fresh_inputs.rs

use hft_collector::segment_index_query::select_segments_hybrid;

pub fn prepare_fresh_window(request: &FreshWindowRequest) -> Result<FreshWindowSelection> {
    // 尝试使用索引，失败则回退到文件扫描
    let index_path = std::env::var("SEGMENT_INDEX_PATH")
        .ok()
        .map(PathBuf::from);
    
    select_segments_hybrid(request, index_path.as_deref())
}
```

#### 4.2 更新 Job YAML

```yaml
# deployment/aliyun/research/k8s/cex-materialization-job.example.yaml
spec:
  template:
    spec:
      containers:
        - name: cex-materialization
          env:
            - name: SEGMENT_INDEX_PATH
              value: /lake/output/metadata/segments.parquet
          # ... 其他配置保持不变
```

### 阶段 5：测试验证（第 3 天）

#### 5.1 准备测试 Job

```bash
# 提交一个小范围的测试准备任务
# 使用 1 小时数据验证速度提升

# 记录开始时间
START_TIME=$(date +%s)

# 提交测试 Job（使用你现有的准备流程）
# ...

# 监控日志，应该看到：
# "✓ Used Parquet index (fast path)"

# 记录结束时间
END_TIME=$(date +%s)
DURATION=$((END_TIME - START_TIME))

echo "准备耗时: ${DURATION} 秒"
# 预期：从 300-600 秒 → <10 秒
```

#### 5.2 性能基准测试

```bash
# 测试 1：首小时准备（冷启动）
time kubectl apply -f test-first-hour-job.yaml

# 测试 2：后续小时准备（索引已加载）
time kubectl apply -f test-second-hour-job.yaml

# 测试 3：批量准备（385 小时）
time kubectl apply -f test-full-batch-job.yaml
```

预期结果：
| 测试 | 旧方法 | 新方法 | 加速比 |
|---|---|---|---|
| 首小时 | 5-10 分钟 | <1 秒 | 300-600x |
| 后续小时 | 2-3 分钟 | <1 秒 | 120-180x |
| 385 小时 | 20 小时 | 10 分钟 | 120x |

---

## 🔧 运维指南

### 日常维护

#### 索引增量更新
Collector 自动维护，无需手动操作。

#### 索引重建
```bash
# 如果索引损坏或需要重建
kubectl apply -f deployment/aliyun/research/k8s/segment-index-backfill-job.yaml
kubectl patch job segment-index-backfill -n monday-research \
  --type=json -p='[{"op":"replace","path":"/spec/suspend","value":false}]'
```

#### 监控索引健康度
```bash
# 检查索引大小
kubectl exec -n monday-research deployment/some-pod -- \
  ls -lh /lake/output/metadata/segments.parquet

# 查询统计信息
kubectl exec -n monday-research deployment/some-pod -- \
  duckdb -c "SELECT 
    COUNT(*) as total_segments,
    SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe_segments,
    MIN(date) as first_date,
    MAX(date) as last_date,
    SUM(verified_bytes) / 1024 / 1024 / 1024 as total_gb
  FROM read_parquet('/lake/output/metadata/segments.parquet')"
```

### 故障排查

#### 问题：准备仍然很慢

**检查**：
```bash
# 查看日志，确认使用了索引
kubectl logs -n monday-research <pod> | grep "Parquet index"

# 应该看到：
# ✓ Used Parquet index (fast path)
```

**如果看到**：
```
⚠ Parquet index not found
```

**解决**：
- 检查 SEGMENT_INDEX_PATH 环境变量
- 确认索引文件存在
- 验证 PVC 挂载正确

#### 问题：索引查询失败

**日志示例**：
```
⚠ Parquet index query failed: Failed to query segments
```

**解决**：
1. 检查索引文件完整性
2. 重建索引
3. 查看详细错误日志

#### 问题：找不到可重放段

**日志示例**：
```
No replayable segments found. Found 10 segments, but only 2 are replay-safe.
Issues: 8 missing replay_safe_checkpoint
```

**解决**：
- 这是数据质量问题，不是索引问题
- 参考 `REPLAY_VALIDATION_FIX.md` 排除或修复问题段
- 或者调整时间窗口避开问题时段

---

## 📊 成本分析

### 一次性成本
- 开发集成：3 天工作量
- 回填索引：4-8 小时运行时间
- 测试验证：1 天

### 持续收益（每次 Campaign）
| 项目 | 旧方法 | 新方法 | 节省 |
|---|---|---|---|
| Worker 时间 | 20 小时 | 10 分钟 | 19.83 小时 |
| Worker 成本 | CNY 6.84 | CNY 0.06 | CNY 6.78 |
| 调试时间 | 扫描全目录 | 1 条 SQL | 数小时 |

### ROI
- 第一次批量准备即可回本
- 每月运行 10 次 Campaign：节省 CNY 67.8/月
- 加上调试时间节省：价值难以估量

---

## 📈 监控指标

### 关键指标

```bash
# 准备速度
kubectl logs -n monday-research <pod> | \
  grep -E "Used.*index|elapsed" | \
  tail -20

# 索引覆盖率
duckdb -c "SELECT 
  date_trunc('day', date::DATE) as day,
  COUNT(*) as segments,
  SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe
FROM read_parquet('segments.parquet')
GROUP BY day
ORDER BY day DESC
LIMIT 30"

# 数据质量趋势
duckdb -c "SELECT 
  date_trunc('week', date::DATE) as week,
  100.0 * SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) / COUNT(*) as safety_pct
FROM read_parquet('segments.parquet')
GROUP BY week
ORDER BY week DESC"
```

---

## 🎯 下一步优化

1. **ClickHouse 集成**（可选）
   - 如果 ClickHouse 已部署，可以作为备选索引
   - 提供实时仪表板

2. **自动问题段处理**
   - 基于索引自动选择最大连续可重放窗口
   - 生成 Campaign 建议配置

3. **多区域同步**
   - 如果扩展到其他区域，同步索引文件

---

## ✅ 部署检查清单

- [ ] 代码编译通过
- [ ] 单元测试通过
- [ ] 镜像构建并推送到 ACR
- [ ] PVC 配置正确
- [ ] Backfill Job 成功完成
- [ ] 索引文件生成并可查询
- [ ] Collector 更新并测试
- [ ] 准备 Job 更新并测试
- [ ] 性能基准测试通过
- [ ] 监控指标正常
- [ ] 文档更新完整

---

## 📞 支持

遇到问题时：
1. 查看本文档的故障排查部分
2. 检查 Pod 日志
3. 验证索引文件完整性
4. 参考 `REPLAY_VALIDATION_FIX.md` 处理数据质量问题
