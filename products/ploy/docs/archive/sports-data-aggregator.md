# 多源数据聚合系统 - 配置和使用指南

## 概述

新的 `SportsDataAggregator` 提供了更稳固的数据获取方式，具有以下特性：

### 核心优势

1. **多源数据聚合** - 从多个 API 并行获取数据
2. **智能降级** - 主源失败时自动切换备用源
3. **数据质量评分** - 自动评估和选择最佳数据
4. **缓存机制** - 减少 API 调用，提高响应速度
5. **可靠性追踪** - 监控每个数据源的成功率

## 支持的数据源

### 优先级排序（高到低）

| 数据源 | 优先级 | 数据类型 | 可靠性 | 配置要求 |
|--------|--------|----------|--------|----------|
| **NBA Official API** | 10 | 官方统计、比分 | 99% | 无需 API key |
| **ESPN API** | 9 | 球员数据、新闻 | 95% | `ESPN_API_KEY` |
| **SportsRadar** | 8 | 专业数据、分析 | 95% | `SPORTSRADAR_API_KEY` |
| **The Odds API** | 7 | 博彩赔率 | 95% | `THE_ODDS_API_KEY` |
| **Grok (X/Twitter)** | 6 | 实时新闻、情绪 | 80% | `GROK_API_KEY` |
| **Polymarket** | 5 | 市场赔率 | 90% | 无需 API key |
| **Cache** | 1 | 缓存数据 | 100% | 自动 |

## 数据质量评分系统

### 评分维度

```rust
pub struct DataQualityMetrics {
    pub completeness: f64,    // 数据完整度 (30% 权重)
    pub freshness: f64,       // 数据新鲜度 (25% 权重)
    pub reliability: f64,     // 来源可靠性 (25% 权重)
    pub consistency: f64,     // 格式一致性 (20% 权重)
    pub overall_score: f64,   // 总分 (0.0-1.0)
}
```

### 评分标准

- **完整度**: 检查 7 个数据部分是否齐全
  - 球员数据
  - 博彩赔率
  - 市场情绪
  - 突发新闻
  - 历史交锋
  - 球队统计
  - 高级分析

- **新鲜度**: 数据获取时间
  - < 1 分钟: 1.0
  - < 5 分钟: 0.9
  - < 15 分钟: 0.7
  - > 15 分钟: 0.5

- **可靠性**: 基于历史成功率
  - NBA API: 0.99
  - ESPN: 0.95
  - Grok: 0.80

- **一致性**: JSON 格式稳定性
  - 官方 API: 1.0
  - Grok: 0.9

## 使用方法

### 基础用法

```rust
use ploy::agent::sports_data_aggregator::SportsDataAggregator;
use ploy::agent::grok::GrokClient;

// 创建聚合器
let grok = GrokClient::from_env()?;
let aggregator = SportsDataAggregator::new(grok);

// 获取数据
let result = aggregator.fetch_game_data(
    "Philadelphia 76ers",
    "Dallas Mavericks",
    "NBA"
).await?;

// 检查数据质量
println!("Data quality: {:.2}", result.quality.overall_score);
println!("Sources used: {}", result.source_names());
println!("Completeness: {:.2}", result.quality.completeness);
```

### 高级配置

```rust
// 自定义缓存 TTL
let mut aggregator = SportsDataAggregator::new(grok);
aggregator.cache_ttl = Duration::minutes(10);

// 检查数据质量是否可接受
if result.is_acceptable(0.7) {
    println!("Data quality is good!");
} else {
    println!("Warning: Low data quality");
}

// 获取可靠性统计
let stats = aggregator.get_reliability_stats().await;
for (source, rate) in stats {
    println!("{}: {:.1}% success rate", source.name(), rate * 100.0);
}
```

## 数据聚合策略

### 并行获取

```
开始获取
    ↓
并行调用所有配置的数据源
    ├─→ Grok (X/Twitter)
    ├─→ The Odds API
    ├─→ ESPN API
    └─→ NBA Official API
    ↓
等待所有响应（最多 30 秒）
    ↓
按质量分数排序
    ↓
使用最高质量源作为基础
    ↓
合并其他源的补充数据
    ↓
返回聚合结果
```

### 数据合并规则

1. **基础数据**: 使用质量最高的源
2. **博彩赔率**: 优先使用 The Odds API
3. **球员数据**: 选择最完整的数据集
4. **新闻**: 合并所有源的新闻
5. **统计数据**: 优先使用官方 API

## 降级策略

### 场景 1: 主源失败

```
Grok 失败
    ↓
尝试 The Odds API
    ↓
尝试 ESPN API
    ↓
使用缓存（如果可用）
    ↓
返回部分数据或错误
```

### 场景 2: 部分源失败

```
3 个源成功，1 个失败
    ↓
使用成功的 3 个源
    ↓
合并数据
    ↓
标记数据质量分数
```

### 场景 3: 所有源失败

```
所有 API 失败
    ↓
检查缓存
    ↓
如果缓存可用（< 15 分钟）
    ↓
返回缓存数据 + 警告
    ↓
否则返回错误
```

## 缓存机制

### 缓存策略

- **默认 TTL**: 5 分钟
- **缓存键**: `{league}-{team1}-{team2}`
- **存储**: 内存 HashMap（可扩展到 Redis）
- **清理**: 自动过期

### 缓存命中率优化

```rust
// 预热缓存（比赛开始前）
for game in todays_games {
    aggregator.fetch_game_data(&game.team1, &game.team2, &game.league).await?;
}

// 后续请求将使用缓存
let result = aggregator.fetch_game_data("76ers", "Mavericks", "NBA").await?;
// ✓ Cache hit! (age: 45s)
```

## 可靠性监控

### 实时统计

```rust
let stats = aggregator.get_reliability_stats().await;

// 输出示例:
// Grok (X/Twitter): 85.2% success rate
// The Odds API: 98.5% success rate
// ESPN API: 92.1% success rate
// NBA Official API: 99.8% success rate
```

### 自动调整

系统会根据历史成功率自动调整：
- 成功率 > 90%: 正常优先级
- 成功率 70-90%: 降低优先级
- 成功率 < 70%: 标记为不可靠

## 环境变量配置

### 必需

```bash
export GROK_API_KEY="your-grok-key"
export ANTHROPIC_API_KEY="your-claude-key"
```

### 可选（推荐）

```bash
# 博彩赔率数据
export THE_ODDS_API_KEY="your-odds-api-key"

# ESPN 数据
export ESPN_API_KEY="your-espn-key"

# SportsRadar 专业数据
export SPORTSRADAR_API_KEY="your-sportsradar-key"
```

## 性能优化

### 并行请求

- 所有数据源并行调用
- 超时设置: 30 秒
- 最快响应优先使用

### 缓存优化

- 内存缓存: O(1) 查找
- 自动过期: 无需手动清理
- 可扩展到 Redis: 跨实例共享

### 速率限制

```rust
// 自动速率限制（每个源独立）
// Grok: 100 req/min
// The Odds API: 500 req/day (免费版)
// ESPN: 1000 req/hour
```

## 错误处理

### 错误类型

1. **所有源失败**: 返回错误
2. **部分源失败**: 继续使用成功的源
3. **数据质量低**: 返回警告但继续
4. **缓存过期**: 强制刷新

### 错误恢复

```rust
match aggregator.fetch_game_data(team1, team2, league).await {
    Ok(result) => {
        if result.quality.overall_score < 0.5 {
            warn!("Low quality data: {:.2}", result.quality.overall_score);
        }
        // 使用数据
    }
    Err(e) => {
        error!("All sources failed: {}", e);
        // 使用默认值或跳过
    }
}
```

## 监控和日志

### 日志级别

```bash
# 详细日志
RUST_LOG=ploy::agent::sports_data_aggregator=debug

# 输出示例:
# [INFO] Fetching fresh data from multiple sources...
# [INFO] ✓ Grok (X/Twitter) succeeded (quality: 0.85)
# [INFO] ✓ The Odds API succeeded (quality: 0.92)
# [WARN] ✗ ESPN API failed: timeout
# [INFO] Using The Odds API as primary source (quality: 0.92)
# [INFO] Merging data from Grok (X/Twitter) (quality: 0.85)
```

### Prometheus 指标

```rust
// TODO: 添加 Prometheus 指标
// - data_source_requests_total{source="grok", status="success"}
// - data_source_response_time_seconds{source="grok"}
// - data_quality_score{source="grok"}
// - cache_hit_rate
```

## 最佳实践

### 1. 配置多个数据源

```bash
# 至少配置 2-3 个数据源以确保可靠性
export GROK_API_KEY="..."
export THE_ODDS_API_KEY="..."
export ESPN_API_KEY="..."
```

### 2. 监控数据质量

```rust
if result.quality.overall_score < 0.7 {
    // 发送告警
    send_alert("Low data quality detected");
}
```

### 3. 预热缓存

```rust
// 在比赛开始前 30 分钟预热
for game in upcoming_games {
    aggregator.fetch_game_data(&game.team1, &game.team2, &game.league).await?;
}
```

### 4. 定期检查可靠性

```rust
// 每小时检查一次
tokio::spawn(async move {
    loop {
        let stats = aggregator.get_reliability_stats().await;
        log_reliability_stats(stats);
        tokio::time::sleep(Duration::hours(1)).await;
    }
});
```

## 与现有代码集成

### 替换 SportsDataFetcher

```rust
// 旧代码
let fetcher = SportsDataFetcher::new(grok);
let data = fetcher.fetch_game_data(team1, team2, league).await?;

// 新代码
let aggregator = SportsDataAggregator::new(grok);
let result = aggregator.fetch_game_data(team1, team2, league).await?;
let data = result.data; // 使用聚合后的数据
```

### 在 SportsAnalyst 中使用

```rust
impl SportsAnalyst {
    pub fn new_with_aggregator(grok: GrokClient, claude: ClaudeAgentClient) -> Self {
        let aggregator = SportsDataAggregator::new(grok);
        Self { aggregator, claude }
    }

    pub async fn analyze_event(&self, event_url: &str) -> Result<SportsAnalysis> {
        // 使用聚合器获取数据
        let result = self.aggregator.fetch_game_data(team1, team2, league).await?;

        // 检查数据质量
        if !result.is_acceptable(0.6) {
            warn!("Data quality below threshold: {:.2}", result.quality.overall_score);
        }

        // 继续分析...
    }
}
```

## 未来扩展

### 计划中的功能

1. **Redis 缓存**: 跨实例共享缓存
2. **实时数据流**: WebSocket 连接到实时数据源
3. **机器学习**: 预测数据源可靠性
4. **自动降级**: 基于成本和质量的智能选择
5. **数据验证**: 交叉验证多个源的数据一致性

### API 扩展

```rust
// 批量获取
aggregator.fetch_multiple_games(games).await?;

// 实时更新
aggregator.subscribe_to_game(game_id).await?;

// 历史数据
aggregator.fetch_historical_data(team, date_range).await?;
```
