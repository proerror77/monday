# DuckDB 驱动的研究数据索引方案

## 核心思想

**将所有行情段的元数据持久化到 DuckDB，准备时直接 SQL 查询**

```
采集器写入 manifest → 同步写入 DuckDB → 准备时 SQL 查询 → 直接获取文件列表
```

---

## 架构设计

### 1. 元数据表结构

```sql
CREATE TABLE market_segments (
    segment_id VARCHAR PRIMARY KEY,
    market VARCHAR NOT NULL,           -- usdm, spot
    dataset VARCHAR NOT NULL,          -- usdm_perpetual_top100_lob
    symbol VARCHAR NOT NULL,           -- SOLUSDT
    date DATE NOT NULL,
    hour INTEGER NOT NULL,
    start_received_at_ns BIGINT NOT NULL,
    end_received_at_ns BIGINT NOT NULL,
    
    -- 重放标志（关键！）
    has_replay_safe_checkpoint BOOLEAN NOT NULL,
    all_symbols_bridged BOOLEAN NOT NULL,
    all_stream_coverage_verified BOOLEAN NOT NULL,
    venue_depth_complete BOOLEAN NOT NULL,
    replay_safe BOOLEAN GENERATED ALWAYS AS (
        has_replay_safe_checkpoint = TRUE AND
        all_symbols_bridged = TRUE AND
        all_stream_coverage_verified = TRUE AND
        venue_depth_complete = FALSE
    ) STORED,
    
    -- 文件路径
    manifest_path VARCHAR NOT NULL,
    tape_path VARCHAR NOT NULL,
    tape_sha256 VARCHAR,
    verified_bytes BIGINT,
    
    -- 额外信息
    snapshot_only_symbols VARCHAR[],
    raw_trade_incomplete_symbols VARCHAR[],
    stream_types VARCHAR[],
    
    -- 审计
    ingested_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    last_verified_at TIMESTAMP
);

-- 关键索引
CREATE INDEX idx_segments_time ON market_segments(date, hour);
CREATE INDEX idx_segments_symbol ON market_segments(symbol);
CREATE INDEX idx_segments_replay ON market_segments(replay_safe);
CREATE INDEX idx_segments_range ON market_segments(start_received_at_ns, end_received_at_ns);
```

### 2. 准备时的查询

**当前方式**（文件扫描）：
```bash
# 57 分钟 → 5-10 分钟（优化后）
for hour in date_range:
    list_directory(oss_path/date/hour)
    read_manifest()
    validate_flags()
    select_files()
```

**DuckDB 方式**（SQL 查询）：
```sql
-- 查询某个时间窗口的所有可重放段（<1秒）
SELECT 
    manifest_path,
    tape_path,
    tape_sha256,
    verified_bytes
FROM market_segments
WHERE market = 'usdm'
  AND symbol = 'SOLUSDT'
  AND start_received_at_ns <= :window_end
  AND end_received_at_ns >= :window_start
  AND replay_safe = TRUE  -- 直接过滤！
ORDER BY start_received_at_ns;

-- 查找不可重放的段（诊断）
SELECT 
    date, hour, symbol,
    has_replay_safe_checkpoint,
    all_symbols_bridged,
    all_stream_coverage_verified,
    venue_depth_complete
FROM market_segments
WHERE market = 'usdm'
  AND date BETWEEN '2026-09-01' AND '2026-09-03'
  AND replay_safe = FALSE;
```

**性能对比**：
- 文件扫描（优化后）：5-10 分钟
- DuckDB 查询：**< 1 秒**
- 加速比：**300-600 倍**

---

## 实现方案

### 阶段 1：采集器同步写入 DuckDB

```rust
// 在 rust_hft/tools/collector/src/bin/binance-lob-archiver.rs

fn close_segment(segment: &Segment) -> Result<()> {
    // 1. 写入 manifest.json（现有）
    segment.write_manifest()?;
    
    // 2. 同步写入 DuckDB（新增）
    let conn = get_duckdb_connection()?;
    conn.execute(
        r#"INSERT INTO market_segments (
            segment_id, market, dataset, symbol, date, hour,
            start_received_at_ns, end_received_at_ns,
            has_replay_safe_checkpoint, all_symbols_bridged,
            all_stream_coverage_verified, venue_depth_complete,
            manifest_path, tape_path, tape_sha256, verified_bytes
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        params![
            segment.id,
            segment.market,
            segment.dataset,
            segment.symbol,
            segment.date,
            segment.hour,
            segment.start_ns,
            segment.end_ns,
            segment.has_replay_safe_checkpoint(),
            segment.all_symbols_bridged(),
            segment.all_stream_coverage_verified(),
            !segment.venue_depth_complete(),
            segment.manifest_path,
            segment.tape_path,
            segment.tape_sha256,
            segment.verified_bytes,
        ],
    )?;
    
    Ok(())
}
```

### 阶段 2：准备时查询 DuckDB

```rust
// 在 rust_hft/tools/collector/src/research_inventory.rs

pub fn select_segments_duckdb(
    request: &FreshWindowRequest,
) -> Result<FreshWindowSelection> {
    let conn = duckdb::Connection::open(&request.duckdb_path)?;
    
    // 直接查询可重放的段
    let segments: Vec<SegmentInfo> = conn.prepare(
        r#"
        SELECT manifest_path, tape_path, tape_sha256, verified_bytes
        FROM market_segments
        WHERE market = ?
          AND symbol = ?
          AND start_received_at_ns <= ?
          AND end_received_at_ns >= ?
          AND replay_safe = TRUE
        ORDER BY start_received_at_ns
        "#
    )?
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
    
    // 不可重放的段会被自动过滤掉！
    if segments.is_empty() {
        // 诊断：查找为什么没有可用段
        let all_segments = conn.query_row(
            r#"
            SELECT COUNT(*), 
                   SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe_count
            FROM market_segments
            WHERE market = ? AND symbol = ?
              AND start_received_at_ns <= ?
              AND end_received_at_ns >= ?
            "#,
            params![request.market.as_str(), &request.symbol, request.end_ns, request.start_ns],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        )?;
        
        bail!(
            "No replayable segments found: {} total, {} safe",
            all_segments.0,
            all_segments.1
        );
    }
    
    Ok(FreshWindowSelection {
        raw: segments,
        verified_bytes: segments.iter().map(|s| s.verified_bytes).sum(),
        inventory_eligible: true,
        materialized_pit_admitted: true,
        ..Default::default()
    })
}
```

### 阶段 3：历史数据回填

```rust
// 一次性回填脚本
fn backfill_duckdb(raw_root: &Path, db_path: &Path) -> Result<()> {
    let conn = duckdb::Connection::open(db_path)?;
    
    // 扫描所有现有 manifest
    for manifest_path in discover_manifests(raw_root)? {
        let manifest: Value = read_json(&manifest_path)?;
        
        // 提取并插入
        conn.execute(
            "INSERT OR REPLACE INTO market_segments (...) VALUES (...)",
            params![...],
        )?;
    }
    
    println!("Backfilled {} segments", count);
    Ok(())
}
```

---

## 性能对比

| 操作 | 文件扫描（旧） | 文件扫描（优化） | DuckDB |
|---|---|---|---|
| 首小时准备 | 57 分钟 | 5-10 分钟 | **< 1 秒** |
| 后续小时 | 57 分钟 | 2-3 分钟 | **< 1 秒** |
| 385 小时总计 | ~366 小时 | ~15-20 小时 | **< 10 分钟** |
| 诊断不可重放段 | 扫描全目录 | 扫描全目录 | **1 条 SQL** |
| 并行准备 | 串行扫描 | 串行扫描 | **完全并行** |

---

## 额外优势

### 1. 立即诊断问题段

```sql
-- 找出所有不可重放的段
SELECT date, hour, symbol,
       CASE 
           WHEN NOT has_replay_safe_checkpoint THEN 'missing_checkpoint'
           WHEN NOT all_symbols_bridged THEN 'symbols_not_bridged'
           WHEN NOT all_stream_coverage_verified THEN 'coverage_unverified'
           WHEN venue_depth_complete != FALSE THEN 'depth_complete_flag_wrong'
       END as issue
FROM market_segments
WHERE replay_safe = FALSE
  AND date BETWEEN '2026-09-01' AND '2026-09-03';
```

### 2. 数据质量监控

```sql
-- 每日可重放比例
SELECT 
    date,
    COUNT(*) as total_segments,
    SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) as safe_segments,
    100.0 * SUM(CASE WHEN replay_safe THEN 1 ELSE 0 END) / COUNT(*) as safe_percentage
FROM market_segments
GROUP BY date
ORDER BY date DESC
LIMIT 30;
```

### 3. 自动排除问题段

```sql
-- 生成最大连续可重放窗口
WITH safe_hours AS (
    SELECT date, hour
    FROM market_segments
    WHERE symbol = 'SOLUSDT' AND replay_safe = TRUE
    ORDER BY date, hour
),
gaps AS (
    SELECT 
        date, hour,
        LAG(hour, 1, hour - 1) OVER (ORDER BY date, hour) as prev_hour
    FROM safe_hours
)
SELECT 
    MIN(date) as start_date, MIN(hour) as start_hour,
    MAX(date) as end_date, MAX(hour) as end_hour,
    COUNT(*) as hours
FROM gaps
WHERE hour = prev_hour + 1  -- 连续
GROUP BY (SELECT COUNT(*) FROM gaps g2 WHERE g2.date <= gaps.date AND g2.hour < gaps.hour AND g2.hour != g2.prev_hour + 1)
ORDER BY hours DESC
LIMIT 1;
```

---

## 实施建议

### 短期（解决当前问题）：
1. ✅ 使用优化后的文件扫描（已完成）
2. ✅ 手动排除 2026-09-02T01:00（用我的脚本）
3. ⏱️ 预计 1 小时内恢复

### 中期（根本优化）：
1. 📊 实现 DuckDB 索引（1-2 天）
2. 🔄 回填历史 manifest（几小时）
3. 🚀 切换到 DuckDB 查询（<1 天）
4. ⏱️ 单次准备从 5 分钟 → <1 秒

### 长期（架构改进）：
1. 📡 采集器实时写入 DuckDB
2. 📈 数据质量仪表板
3. 🤖 自动化问题段处理
4. 🎯 Campaign 自动选择最优窗口

---

## 投资回报分析

**一次性成本**：
- 开发 DuckDB 集成：2-3 天
- 回填历史数据：4-8 小时
- 测试验证：1 天

**持续收益**：
- 每次准备节省：5-10 分钟 → 300-600 倍加速
- 385 小时批量准备：20 小时 → 10 分钟
- 诊断时间：扫描目录 → 1 条 SQL（秒级）
- 自动排除问题段：手动 → 自动

**ROI**：第一次批量准备就回本

---

## 立即可行的折中方案

如果不想马上实现完整的 DuckDB 方案，可以先做：

### 预计算索引文件（JSON/Parquet）

```rust
// 生成索引文件（一次性）
fn build_index(raw_root: &Path, index_path: &Path) -> Result<()> {
    let segments: Vec<SegmentMetadata> = discover_all_manifests(raw_root)?
        .into_iter()
        .map(|m| extract_metadata(&m))
        .collect();
    
    // 写入 Parquet（DuckDB 可直接查询）
    write_parquet(index_path, &segments)?;
    
    Ok(())
}

// 准备时查询 Parquet
fn select_from_parquet(index_path: &Path, request: &WindowRequest) -> Result<Vec<Segment>> {
    let conn = duckdb::Connection::open_in_memory()?;
    
    // DuckDB 可以直接查询 Parquet
    let segments = conn.query_map(
        r#"
        SELECT * FROM read_parquet(?)
        WHERE symbol = ?
          AND start_ns <= ?
          AND end_ns >= ?
          AND replay_safe = TRUE
        "#,
        params![index_path, request.symbol, request.end_ns, request.start_ns],
        |row| Ok(...)
    )?;
    
    Ok(segments)
}
```

**优点**：
- 无需修改采集器
- 仍然是秒级查询
- 索引文件可以定期重建

**缺点**：
- 需要定期重建索引
- 不是实时的
