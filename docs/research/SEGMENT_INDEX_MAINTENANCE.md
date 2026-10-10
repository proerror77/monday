# Segment Index 维护和优化指南

## 📋 目录

1. [日常维护](#日常维护)
2. [性能优化](#性能优化)
3. [故障恢复](#故障恢复)
4. [升级指南](#升级指南)
5. [最佳实践](#最佳实践)

---

## 日常维护

### 每日检查

#### 1. 索引健康状态

```bash
# 手动运行健康检查
./scripts/segment_index_monitor.sh

# 或使用 CLI 工具
segment-index-cli --index /lake/output/metadata/segments.parquet info
```

#### 2. 检查最近的数据质量

```bash
segment-index-cli --index /lake/output/metadata/segments.parquet \
  stats --group-by date --limit 7
```

预期输出：
```
DATE            Total      Safe       Safe %      Total GB
-------------------------------------------------------------
2026-09-03      24         24         100.00%     12.45
2026-09-02      24         23         95.83%      12.38
2026-09-01      24         24         100.00%     12.42
```

**行动阈值**：
- Safe % < 95%：调查原因
- Safe % < 90%：立即处理
- Safe % < 80%：紧急情况

### 每周任务

#### 1. 清理重复条目

由于 `append_to_parquet_index` 是真正的追加操作，可能会产生重复：

```bash
# 检查重复
duckdb -c "
SELECT segment_id, COUNT(*) as count
FROM read_parquet('/lake/output/metadata/segments.parquet')
GROUP BY segment_id
HAVING COUNT(*) > 1
ORDER BY count DESC
LIMIT 10
"

# 如果有重复，重建索引
kubectl apply -f deployment/aliyun/research/k8s/segment-index-backfill-job.yaml
kubectl patch job segment-index-backfill -n monday-research \
  --type=json -p='[{"op":"replace","path":"/spec/suspend","value":false}]'
```

#### 2. 归档旧报告

```bash
# 保留最近 30 天的健康报告
find /tmp -name "segment_index_health_*.txt" -mtime +30 -delete
```

### 每月任务

#### 1. 验证索引完整性

```bash
segment-index-cli --index /lake/output/metadata/segments.parquet validate
```

#### 2. 分析趋势

```bash
# 生成月度统计
duckdb -c "
SELECT 
    DATE_TRUNC('week', date::DATE) as week,
    COUNT(*) as total,
    SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe,
    ROUND(100.0 * SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) / COUNT(*), 2) as safe_pct
FROM read_parquet('/lake/output/metadata/segments.parquet')
WHERE date >= CURRENT_DATE - INTERVAL 30 DAYS
GROUP BY week
ORDER BY week
" > /tmp/monthly_trends.txt
```

---

## 性能优化

### 索引大小管理

#### 监控索引增长

```bash
# 检查当前大小
ls -lh /lake/output/metadata/segments.parquet

# 预测未来增长
CURRENT_SIZE=$(stat -f%z /lake/output/metadata/segments.parquet 2>/dev/null || stat -c%s /lake/output/metadata/segments.parquet)
SEGMENTS=$(duckdb -csv -c "SELECT COUNT(*) FROM read_parquet('/lake/output/metadata/segments.parquet')" | tail -1)
BYTES_PER_SEGMENT=$((CURRENT_SIZE / SEGMENTS))

echo "当前大小: $((CURRENT_SIZE / 1024 / 1024)) MB"
echo "段数: $SEGMENTS"
echo "每段平均: $BYTES_PER_SEGMENT 字节"
echo "预计每月增长: $(((24 * 30 * BYTES_PER_SEGMENT) / 1024 / 1024)) MB"
```

#### 压缩优化

如果索引过大（> 100 MB），考虑重建并优化压缩：

```bash
# 使用更高的压缩级别重建
duckdb -c "
COPY (
    SELECT DISTINCT ON (segment_id) *
    FROM read_parquet('/lake/output/metadata/segments.parquet')
    ORDER BY segment_id, ingested_at_ms DESC
) TO '/tmp/segments_optimized.parquet' (
    FORMAT PARQUET,
    COMPRESSION 'ZSTD',
    COMPRESSION_LEVEL 9
)
"

# 验证新索引
segment-index-cli --index /tmp/segments_optimized.parquet validate

# 备份旧索引并替换
cp /lake/output/metadata/segments.parquet /lake/output/metadata/segments.parquet.backup
mv /tmp/segments_optimized.parquet /lake/output/metadata/segments.parquet
```

### 查询性能优化

#### 1. 分区策略

考虑按时间分区索引：

```bash
# 为不同时间范围创建分区索引
duckdb -c "
COPY (
    SELECT * FROM read_parquet('/lake/output/metadata/segments.parquet')
    WHERE date >= '2026-09-01' AND date <= '2026-09-30'
) TO '/lake/output/metadata/segments_2026_09.parquet' (FORMAT PARQUET)
"

# 查询时可以只读取相关分区
segment-index-cli --index /lake/output/metadata/segments_2026_09.parquet \
  query --market usdm --symbol SOLUSDT
```

#### 2. 缓存策略

对于频繁查询的数据，使用本地缓存：

```bash
# 将索引复制到本地高速存储
cp /lake/output/metadata/segments.parquet /tmp/segments_cache.parquet

# 使用缓存版本查询
export SEGMENT_INDEX_PATH=/tmp/segments_cache.parquet
```

---

## 故障恢复

### 场景 1：索引文件损坏

**症状**：
```
Error: Failed to read Parquet file: Invalid magic bytes
```

**恢复步骤**：

```bash
# 1. 检查是否有备份
ls -lt /lake/output/metadata/segments.parquet*

# 2. 如果有备份，恢复
cp /lake/output/metadata/segments.parquet.backup \
   /lake/output/metadata/segments.parquet

# 3. 如果没有备份，重建
kubectl apply -f deployment/aliyun/research/k8s/segment-index-backfill-job.yaml
kubectl patch job segment-index-backfill -n monday-research \
  --type=json -p='[{"op":"replace","path":"/spec/suspend","value":false}]'

# 4. 监控重建进度
kubectl logs -n monday-research -l app.kubernetes.io/name=segment-index-backfill -f
```

### 场景 2：索引查询超时

**症状**：
```
⚠ Parquet index query failed: Connection timeout
```

**解决方案**：

```bash
# 1. 检查索引大小
ls -lh /lake/output/metadata/segments.parquet

# 2. 如果过大（> 500 MB），优化
# 运行上面的压缩优化步骤

# 3. 检查网络延迟（如果是远程 OSS）
time ossutil stat oss://monday-research/lake/output/metadata/segments.parquet

# 4. 考虑使用本地缓存
```

### 场景 3：Collector 未更新索引

**症状**：
```
⚠ No new segments ingested in the last 2 hours
```

**诊断步骤**：

```bash
# 1. 检查 Collector 是否运行
systemctl status binance-lob-archiver  # 或 kubectl get pods

# 2. 检查环境变量
echo $SEGMENT_INDEX_PATH

# 3. 检查 Collector 日志
journalctl -u binance-lob-archiver -n 100 | grep -i "segment index"

# 4. 手动测试写入
segment-index-cli --index /lake/output/metadata/segments.parquet info
```

**修复**：

```bash
# 如果环境变量未设置
export SEGMENT_INDEX_PATH=/lake/output/metadata/segments.parquet

# 重启 Collector
systemctl restart binance-lob-archiver
```

### 场景 4：准备仍然慢

**症状**：
```
准备耗时仍然 > 1 分钟
```

**诊断**：

```bash
# 检查日志是否使用了索引
kubectl logs <pod> | grep -E "Parquet index|filesystem scan"

# 应该看到：
# ✓ Used Parquet index (fast path)
```

**可能原因**：

1. **索引路径未配置**
   ```bash
   kubectl describe pod <pod> | grep SEGMENT_INDEX_PATH
   ```

2. **索引文件不存在**
   ```bash
   kubectl exec <pod> -- ls -l /lake/output/metadata/segments.parquet
   ```

3. **权限问题**
   ```bash
   kubectl exec <pod> -- cat /lake/output/metadata/segments.parquet > /dev/null
   ```

---

## 升级指南

### 从文件扫描迁移到索引

#### 阶段 1：准备（无影响）

```bash
# 1. 构建新镜像
docker build -t research-runner:v1.0-index .
docker push crpi-...aliyuncs.com/wildcard0923/research-runner:v1.0-index

# 2. 运行 backfill
kubectl apply -f deployment/aliyun/research/k8s/segment-index-backfill-job.yaml
kubectl patch job segment-index-backfill -n monday-research \
  --type=json -p='[{"op":"replace","path":"/spec/suspend","value":false}]'

# 3. 验证索引
segment-index-cli --index /lake/output/metadata/segments.parquet validate
```

#### 阶段 2：灰度测试

```bash
# 1. 创建测试 Job（只设置索引路径）
kubectl apply -f test-with-index-job.yaml

# 2. 观察性能
kubectl logs -f <test-pod>

# 3. 对比准备时间
# 旧方式: 5-10 分钟
# 新方式: 应该 < 5 秒
```

#### 阶段 3：全量部署

```bash
# 更新所有准备 Job 的环境变量
kubectl patch deployment cex-materialization -n monday-research \
  --type=json -p='[{
    "op":"add",
    "path":"/spec/template/spec/containers/0/env/-",
    "value":{"name":"SEGMENT_INDEX_PATH","value":"/lake/output/metadata/segments.parquet"}
  }]'
```

#### 阶段 4：更新 Collector

```bash
# 在 ECS 上更新环境变量
sudo tee -a /etc/systemd/system/binance-lob-archiver.service.d/override.conf << EOF
[Service]
Environment="SEGMENT_INDEX_PATH=/mnt/oss-output/metadata/segments.parquet"
EOF

sudo systemctl daemon-reload
sudo systemctl restart binance-lob-archiver
```

### 回滚计划

如果需要回滚到文件扫描：

```bash
# 1. 移除环境变量
kubectl patch deployment cex-materialization -n monday-research \
  --type=json -p='[{
    "op":"remove",
    "path":"/spec/template/spec/containers/0/env/<INDEX_OF_SEGMENT_INDEX_PATH>"
  }]'

# 2. 重启 Pod
kubectl rollout restart deployment cex-materialization -n monday-research

# 3. 验证回退
kubectl logs <new-pod> | grep "filesystem scan"
```

---

## 最佳实践

### 1. 备份策略

```bash
# 每天备份索引
0 2 * * * cp /lake/output/metadata/segments.parquet \
  /lake/output/metadata/backups/segments_$(date +\%Y\%m\%d).parquet

# 保留最近 7 天
0 3 * * * find /lake/output/metadata/backups -name "segments_*.parquet" -mtime +7 -delete
```

### 2. 监控集成

```bash
# Prometheus metrics exporter
cat > /tmp/segment_index_metrics.sh << 'EOF'
#!/bin/bash
while true; do
    STATS=$(segment-index-cli --index /lake/output/metadata/segments.parquet info --json)
    SAFE=$(echo "$STATS" | jq '.safe_segments')
    TOTAL=$(echo "$STATS" | jq '.total_segments')
    
    echo "segment_index_safe_count $SAFE"
    echo "segment_index_total_count $TOTAL"
    
    sleep 60
done
EOF
```

### 3. 自动化清理

```bash
# 定期优化索引（每周日凌晨）
0 1 * * 0 /scripts/optimize_segment_index.sh
```

### 4. 文档化变更

在每次维护操作后记录：

```bash
cat >> /var/log/segment_index_maintenance.log << EOF
Date: $(date -u +"%Y-%m-%dT%H:%M:%SZ")
Action: Index optimization
Result: Size reduced from 250MB to 180MB
Performed by: $(whoami)
---
EOF
```

---

## 🆘 紧急联系

- **索引损坏**：运行 backfill Job 重建
- **查询超慢**：检查索引大小，考虑优化
- **准备失败**：查看 `REPLAY_VALIDATION_FIX.md`
- **Collector 问题**：检查环境变量和日志

---

## 📚 相关文档

- 部署指南：`SEGMENT_INDEX_DEPLOYMENT.md`
- 快速参考：`SEGMENT_INDEX_QUICKREF.md`
- 实施总结：`IMPLEMENTATION_SUMMARY.md`
