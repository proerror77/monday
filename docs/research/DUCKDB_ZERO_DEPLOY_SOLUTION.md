# DuckDB 索引方案 - 基于现有 ACK 架构

## 🎯 核心思路：无需额外服务

你已经有：
- ✅ ACK 集群 + Spot worker 节点
- ✅ OSS (raw + reference + output)
- ✅ ClickHouse（可选，当前未用于 manifest 索引）
- ✅ DuckDB 聚合器（用于 lineage 和 approval state）

**方案：复用现有 DuckDB + OSS，无需新增服务**

---

## 📐 架构设计（零额外部署）

### 当前流程
```
Collector ECS 写入 manifest.json
    ↓
OSS /lake/raw/venue=binance_usdm/date=YYYY-MM-DD/hour=HH/manifest.json
    ↓
准备 Job 启动 → 扫描 OSS 目录（慢）
    ↓
读取每个 manifest.json → 校验标志位
    ↓
选择文件 → 物化
```

### 优化方案（利用现有组件）
```
Collector ECS 写入 manifest.json
    ↓
OSS /lake/raw/.../manifest.json
    ↓
同时写入 OSS 索引文件：
    /lake/output/metadata/segments.parquet  ← DuckDB 可直接查询！
    ↓
准备 Job 启动 → 查询 Parquet（快）
    ↓
直接获取文件列表 → 物化
```

**关键**：
1. ✅ 不需要独立的 DuckDB 服务
2. ✅ Parquet 文件存在 OSS 上
3. ✅ 准备 Job 内嵌 DuckDB（已有依赖）
4. ✅ 查询本地或远程 Parquet（DuckDB 原生支持）

---

## 🚀 实现方案

### 方案 A：Parquet 索引文件（推荐，最简单）

#### 1. Collector 同步写索引

```rust
// rust_hft/tools/collector/src/bin/binance-lob-archiver.rs

use arrow::array::*;
use arrow::datatypes::*;
use parquet::arrow::ArrowWriter;

struct SegmentIndexWriter {
    oss_path: String,  // oss://monday-research/lake/output/metadata/segments.parquet
    buffer: Vec<SegmentMetadata>,
}

impl SegmentIndexWriter {
    fn flush(&mut self) -> Result<()> {
        // 构造 Arrow Schema
        let schema = Schema::new(vec![
            Field::new("segment_id", DataType::Utf8, false),
            Field::new("market", DataType::Utf8, false),
            Field::new("symbol", DataType::Utf8, false),
            Field::new("date", DataType::Date32, false),
            Field::new("hour", DataType::Int32, false),
            Field::new("start_ns", DataType::Int64, false),
            Field::new("end_ns", DataType::Int64, false),
            Field::new("replay_safe", DataType::Boolean, false),  // 关键！
            Field::new("manifest_path", DataType::Utf8, false),
            Field::new("tape_path", DataType::Utf8, false),
            Field::new("verified_bytes", DataType::Int64, false),
        ]);
        
        // 写入 Parquet
        let file = File::create(&self.oss_path)?;
        let mut writer = ArrowWriter::try_new(file, Arc::new(schema), None)?;
        
        let batch = create_record_batch(&self.buffer)?;
        writer.write(&batch)?;
        writer.close()?;
        
        Ok(())
    }
}

// 在 close_segment 时追加
fn close_segment(segment: &Segment) -> Result<()> {
    // 1. 写入 manifest.json（现有）
    segment.write_manifest()?;
    
    // 2. 追加到 Parquet 索引（新增）
    let index_writer = SegmentIndexWriter::open(
        "oss://monday-research/lake/output/metadata/segments.parquet"
    )?;
    
    index_writer.append(SegmentMetadata {
        segment_id: segment.id.clone(),
        market: segment.market.clone(),
        symbol: segment.symbol.clone(),
        date: segment.date,
        hour: segment.hour,
        start_ns: segment.start_ns,
        end_ns: segment.end_ns,
        replay_safe: segment.is_replay_safe(),  // 直接计算
        manifest_path: segment.manifest_path.clone(),
        tape_path: segment.tape_path.clone(),
        verified_bytes: segment.verified_bytes,
    })?;
    
    Ok(())
}
```

#### 2. 准备 Job 查询 Parquet

```rust
// rust_hft/tools/collector/src/research_inventory.rs

use duckdb::Connection;

pub fn select_segments_from_parquet(
    request: &FreshWindowRequest,
) -> Result<FreshWindowSelection> {
    // DuckDB 在内存中打开（无需服务）
    let conn = Connection::open_in_memory()?;
    
    // 直接查询 OSS 上的 Parquet（DuckDB 原生支持 S3）
    conn.execute(
        r#"
        INSTALL httpfs;
        LOAD httpfs;
        SET s3_region='ap-northeast-1';
        SET s3_access_key_id='YOUR_KEY';
        SET s3_secret_access_key='YOUR_SECRET';
        "#,
        [],
    )?;
    
    // 查询可重放段（<1秒）
    let mut stmt = conn.prepare(
        r#"
        SELECT manifest_path, tape_path, tape_sha256, verified_bytes
        FROM read_parquet('s3://monday-research/lake/output/metadata/segments.parquet')
        WHERE market = ?
          AND symbol = ?
          AND start_ns <= ?
          AND end_ns >= ?
          AND replay_safe = TRUE  -- 关键过滤！
        ORDER BY start_ns
        "#
    )?;
    
    let segments: Vec<SegmentInfo> = stmt
        .query_map(
            params![
                request.market.as_str(),
                &request.symbol,
                request.end_ns,
                request.start_ns,
            ],
            |row| {
                Ok(SegmentInfo {
                    manifest_path: row.get(0)?,
                    tape_path: row.get(1)?,
                    tape_sha256: row.get(2)?,
                    verified_bytes: row.get(3)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    
    if segments.is_empty() {
        // 诊断：为什么没有可用段
        let (total, safe) = conn.query_row(
            r#"
            SELECT 
                COUNT(*) as total,
                SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe
            FROM read_parquet('s3://...')
            WHERE market = ? AND symbol = ?
              AND start_ns <= ? AND end_ns >= ?
            "#,
            params![request.market.as_str(), &request.symbol, request.end_ns, request.start_ns],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        )?;
        
        bail!(
            "No replayable segments: {} total in range, {} safe",
            total, safe
        );
    }
    
    Ok(FreshWindowSelection {
        raw: segments,
        verified_bytes: segments.iter().map(|s| s.verified_bytes).sum(),
        inventory_eligible: true,
        materialized_pit_admitted: true,
    })
}
```

---

### 方案 B：利用 ClickHouse（如果已部署）

如果 ClickHouse 已在运行，可以直接用它：

```sql
-- 在 ClickHouse 中创建表
CREATE TABLE market_segments (
    segment_id String,
    market String,
    symbol String,
    date Date,
    hour UInt8,
    start_ns Int64,
    end_ns Int64,
    has_replay_safe_checkpoint Bool,
    all_symbols_bridged Bool,
    all_stream_coverage_verified Bool,
    venue_depth_complete Bool,
    replay_safe Bool MATERIALIZED (
        has_replay_safe_checkpoint AND
        all_symbols_bridged AND
        all_stream_coverage_verified AND
        NOT venue_depth_complete
    ),
    manifest_path String,
    tape_path String,
    verified_bytes UInt64,
    ingested_at DateTime DEFAULT now()
) ENGINE = MergeTree()
PARTITION BY toYYYYMM(date)
ORDER BY (symbol, date, hour);

-- Collector 写入（通过 HTTP API）
INSERT INTO market_segments VALUES (...);

-- 准备 Job 查询
SELECT manifest_path, tape_path
FROM market_segments
WHERE symbol = 'SOLUSDT'
  AND start_ns <= ?
  AND end_ns >= ?
  AND replay_safe = TRUE;
```

**优点**：
- 实时查询
- 自动分区
- 自带监控

**缺点**：
- 需要 ClickHouse 已部署
- 多一个依赖

---

### 方案 C：本地 DuckDB 文件（最轻量）

利用现有的 DuckDB 聚合器，直接存储在 PVC：

```yaml
# 在 cex-materialization-job.yaml 中
volumes:
  - name: metadata
    persistentVolumeClaim:
      claimName: monday-research-metadata  # 新增小容量 PVC（1GB即可）
```

```rust
// Collector 写入
fn update_index(segment: &Segment) -> Result<()> {
    // 挂载 /metadata/segments.db
    let conn = Connection::open("/metadata/segments.db")?;
    
    conn.execute(
        "INSERT OR REPLACE INTO segments (...) VALUES (...)",
        params![...],
    )?;
    
    Ok(())
}

// 准备 Job 查询（同样的 /metadata/segments.db）
let conn = Connection::open_with_flags(
    "/metadata/segments.db",
    duckdb::Config::default().access_mode(duckdb::AccessMode::ReadOnly)?
)?;
```

**优点**：
- 无需 OSS 访问凭证
- 单文件，简单
- 读写快（本地磁盘）

**缺点**：
- Collector 和 ACK Job 需要共享 PVC
- 需要处理并发写入

---

## 🎯 推荐方案对比

| 方案 | 复杂度 | 性能 | 依赖 | 推荐度 |
|---|---|---|---|---|
| **A: Parquet 索引** | 低 | 极快（<1s） | 仅 DuckDB（已有） | ⭐⭐⭐⭐⭐ |
| B: ClickHouse | 中 | 快 | 需要 ClickHouse | ⭐⭐⭐ |
| C: 本地 DuckDB | 低 | 最快 | 需要共享 PVC | ⭐⭐⭐⭐ |

**最佳选择：方案 A（Parquet 索引）**

理由：
1. ✅ 零额外部署
2. ✅ DuckDB 已在依赖中
3. ✅ OSS 已存在
4. ✅ 查询极快
5. ✅ 可并行读取
6. ✅ 自带版本控制（Parquet 可追加）

---

## 📋 实施步骤

### 第一步：扩展 Collector（1天）

```rust
// 修改 binance-lob-archiver
// 每次 close_segment 时追加到 segments.parquet
```

### 第二步：回填历史（半天）

```bash
# 运行一次性 Job 扫描现有 manifest → 写入 Parquet
kubectl apply -f k8s/parquet-index-backfill-job.yaml
```

### 第三步：修改准备逻辑（1天）

```rust
// 修改 research_inventory.rs
// 从扫描目录 → 查询 Parquet
```

### 第四步：验证（半天）

```bash
# 运行首小时准备测试
# 验证：扫描时间从 5分钟 → <1秒
```

---

## 💰 成本分析

### 额外成本
- Parquet 索引文件：**约 10-50 MB**（数千个段）
- OSS 存储：**< CNY 0.5/月**
- 查询成本：**0**（DuckDB 在 Pod 内存中）

### 节省成本
- Worker 时间：20 小时 → 10 分钟
- Worker 费用：CNY 6.84 → CNY 0.06
- **节省：99%**

---

## 🔧 现在如何快速修复当前问题

### 临时方案（1小时内）

```bash
# 1. 让 Luna 读取错误日志，确认失败的 manifest 路径
# 例如：/lake/raw/venue=binance_usdm/date=2026-09-02/hour=01/manifest.json

# 2. 手动检查该 manifest
kubectl run -it --rm debug --image=alpine --restart=Never -- sh
apk add curl jq
curl <oss_manifest_url> | jq '{
  date, hour, 
  has_replay_safe_checkpoint,
  all_symbols_bridged,
  all_stream_coverage_verified,
  venue_depth_complete
}'

# 3. 确认问题后，调整 Campaign 配置排除该小时
# 参考我之前创建的脚本

# 4. 重新提交
```

### 中期方案（本周）

实现 Parquet 索引，参考上面的代码。

---

## 📞 下一步

我可以帮你：

1. **立即**：生成排除 2026-09-02T01:00 的具体命令
2. **本周**：编写完整的 Parquet 索引代码
3. **现在**：创建 backfill Job YAML

你想先做哪个？
