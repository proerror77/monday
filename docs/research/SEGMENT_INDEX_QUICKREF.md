# Segment Index 快速参考

## 🚀 立即使用

### 查询可重放段
```bash
duckdb -c "SELECT date, hour, symbol, replay_safe 
FROM read_parquet('/lake/output/metadata/segments.parquet')
WHERE symbol = 'SOLUSDT' 
  AND date = '2026-09-02'
ORDER BY hour"
```

### 诊断问题时段
```bash
duckdb -c "SELECT date, hour,
  CASE 
    WHEN NOT has_replay_safe_checkpoint THEN 'missing_checkpoint'
    WHEN NOT all_symbols_bridged THEN 'not_bridged'
    WHEN NOT all_stream_coverage_verified THEN 'coverage_unverified'
    WHEN venue_depth_complete THEN 'wrong_depth_flag'
  END as issue
FROM read_parquet('/lake/output/metadata/segments.parquet')
WHERE replay_safe = FALSE
  AND date BETWEEN '2026-09-01' AND '2026-09-03'"
```

### 检查数据质量
```bash
duckdb -c "SELECT 
  date,
  COUNT(*) as total,
  SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe,
  ROUND(100.0 * SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) / COUNT(*), 2) as safety_pct
FROM read_parquet('/lake/output/metadata/segments.parquet')
WHERE symbol = 'SOLUSDT'
GROUP BY date
ORDER BY date DESC
LIMIT 7"
```

### 找最大连续窗口
```bash
duckdb -c "WITH safe_hours AS (
  SELECT date, hour
  FROM read_parquet('/lake/output/metadata/segments.parquet')
  WHERE symbol = 'SOLUSDT' AND replay_safe = TRUE
  ORDER BY date, hour
)
SELECT 
  MIN(date) || 'T' || LPAD(MIN(hour)::VARCHAR, 2, '0') as start,
  MAX(date) || 'T' || LPAD(MAX(hour)::VARCHAR, 2, '0') as end,
  COUNT(*) as hours
FROM safe_hours"
```

---

## 🔧 常用操作

### 重建索引
```bash
kubectl patch job segment-index-backfill -n monday-research \
  --type=json -p='[{"op":"replace","path":"/spec/suspend","value":false}]'
```

### 查看索引大小
```bash
ls -lh /lake/output/metadata/segments.parquet
```

### 验证索引更新
```bash
duckdb -c "SELECT MAX(ingested_at_ms) as last_update
FROM read_parquet('/lake/output/metadata/segments.parquet')"
```

---

## ⚡ 性能对比

| 操作 | 文件扫描 | Parquet 索引 |
|---|---|---|
| 首小时准备 | 5-10 分钟 | <1 秒 |
| 385 小时批量 | 20 小时 | 10 分钟 |
| 诊断问题段 | 扫描全目录 | 1 条 SQL |

---

## 🐛 快速排错

### 准备仍然慢？
```bash
# 检查是否使用了索引
kubectl logs <pod> | grep "Parquet index"
# 应该看到: ✓ Used Parquet index (fast path)
```

### 索引不存在？
```bash
# 检查文件
ls /lake/output/metadata/segments.parquet

# 检查环境变量
echo $SEGMENT_INDEX_PATH
```

### 找不到可重放段？
```bash
# 运行诊断查询（见上方"诊断问题时段"）
# 然后参考 REPLAY_VALIDATION_FIX.md
```

---

## 📖 完整文档

- 部署指南: `docs/research/SEGMENT_INDEX_DEPLOYMENT.md`
- 架构设计: `docs/research/DUCKDB_ZERO_DEPLOY_SOLUTION.md`
- 问题修复: `REPLAY_VALIDATION_FIX.md`
- 紧急修复: `scripts/emergency_fix_2026_09_02.sh`
