# Polymarket 5m Binary Options 数据收集完整修复方案

> **Historical standalone PLOY operations.** Host, systemd, secret, and nested
> workflow commands below are retained as migration evidence only. They do not
> define a current Monday deployment or execution path.

> Historical note (2026-05-24): this document is an April 2026 incident plan,
> not the current implementation runbook. The old `apps/ploy-runner/*`
> ownership path has been retired. Current data collection ownership lives in
> `crates/ploy-market-data`, `crates/ploy-runner-host`,
> `.github/workflows/deploy-tango-1-1.yml`, and
> `docs/COLLECTOR_RUNBOOK.md`. Current research evidence must flow through
> `docs/runbooks/strategy-research-cicd.md`.

## 问题诊断（2026-04-01）

### 用户需求
每个 5 分钟 BTC/ETH/SOL 预测市场需要完整的生命周期数据：
1. **开盘价 (price_to_beat)**: 事件 start_time 时刻的 Chainlink 价格
2. **持续报价 (CLOB quotes)**: 从开始到结束的每个 tick 的 bid/ask
3. **结算价 (settlement)**: 事件 end_time 时刻的实际价格和 outcome

### 当前数据库状态
```sql
-- Market metadata: 22,638 个 5m 市场
-- ✅ 有 token IDs: 17,491 (77%)
-- ❌ 有 price_to_beat: 5,147 (23%) ← 严重缺失

-- CLOB quotes: 20,149 条
-- ❌ 只有 234/45,276 tokens 有数据 (0.5%) ← 几乎全部缺失
-- 最早数据: 2026-03-25 (只有 7 天)

-- Token settlements: 18,420 条
-- ✅ 结算数据完整
```

### 根本问题
1. **Quote Collector 从未成功运行**
   - SQL 参数绑定 bug: `LIKE` pattern 缺少结尾 `%`
   - 架构设计错误: 周期性断开重连会丢失数据

2. **Market Scanner 不完整**
   - 没有在 `start_time` 捕获 Chainlink price
   - 77% 市场缺少 price_to_beat

3. **回测不可能**
   - 没有开盘价 → 无法计算 edge
   - 没有持续报价 → 无法模拟交易
   - 即使有结算价也没用

## 修复方案

### Phase 1: 修复 Quote Collector ✅ (已完成 90%)

**问题**:
- SQL: `LIKE '%-updown-5m-'` 应该是 `LIKE '%-updown-5m-%'`
- 架构: 每 5 分钟断开重连会丢失中间数据

**修复**:
```rust
// 1. 修复 SQL pattern
let pattern = format!("%-updown-{}-%", self.config.timeframe);

// 2. 改为长连接架构（待实现）
// - WebSocket 保持连接
// - 后台定时刷新订阅列表
// - 动态 subscribe/unsubscribe
```

**当前状态**:
- ✅ SQL 修复完成
- ✅ 服务启动成功，订阅 36 tokens
- ⏳ 等待验证数据是否真的在写入

### Phase 2: 补充 Price to Beat 采集 (今天)

**目标**: 在每个市场的 `start_time` 捕获 Chainlink 价格

**历史实现位置**: `apps/ploy-runner/src/scanner.rs`（已退休；当前实现应从
`crates/ploy-market-data` / `crates/ploy-runner-host` 查找）

**当前代码**:
```rust
let price_to_beat = {
    let chainlink_symbol = symbol.trim_end_matches("USDT").to_lowercase() + "/usd";
    let cache_guard = chainlink_cache.read().await;
    cache_guard.get(&chainlink_symbol).map(|(price, _ts)| *price)
        .or_else(|| usable_metadata_threshold(market.group_item_threshold.as_deref()))
};
```

**问题**:
- 只在发现市场时读取缓存
- 如果 `start_time` 还没到，缓存里的价格不是开盘价

**修复方案**:
```rust
// 方案 A: 延迟写入 (推荐)
// 1. 发现市场时先不写 price_to_beat
// 2. 在 start_time 时刻捕获 Chainlink 价格
// 3. UPDATE pm_market_metadata SET price_to_beat = $1 WHERE market_slug = $2

// 方案 B: 实时采集
// 1. 持续订阅 Chainlink WebSocket
// 2. 在 start_time ±5s 窗口内捕获价格
// 3. 立即写入数据库
```

**实现步骤**:
1. 添加 `PriceToBeatCollector` 结构
2. 查询即将开始的市场 (start_time 在未来 5 分钟内)
3. 在 start_time 时刻读取 Chainlink 缓存
4. UPDATE price_to_beat

### Phase 3: 改为长连接架构 (明天)

**当前问题**: 每 5 分钟断开重连
```rust
loop {
    refresh_subscriptions().await;  // 查询数据库
    subscribe_orderbook(tokens).await;  // 创建新 WebSocket
    listen_for_5_minutes().await;  // 监听
    // ← 断开，丢失数据
}
```

**目标架构**: 长连接 + 动态订阅
```rust
// 主循环: 保持 WebSocket 连接
let client = ClobWsClient::default();
let mut stream = client.subscribe_orderbook(initial_tokens).await;

// 后台任务: 定时刷新订阅
tokio::spawn(async move {
    loop {
        sleep(30s).await;
        let new_tokens = query_active_markets().await;
        let added = new_tokens - current_tokens;
        let removed = current_tokens - new_tokens;

        if !added.is_empty() {
            client.subscribe_orderbook(&added).await;  // 动态添加
        }
        if !removed.is_empty() {
            client.unsubscribe_orderbook(&removed).await;  // 动态移除
        }
    }
});

// 主循环: 持续监听
while let Some(book) = stream.next().await {
    insert_quote(book).await;
}
```

**技术验证**:
- ✅ SDK 提供 `unsubscribe_orderbook()` API
- ✅ 可以在同一个 client 上动态订阅/取消订阅
- ⏳ 需要验证 stream 是否会收到新订阅的数据

### Phase 4: 数据完整性验证 (明天)

**验证清单**:
```sql
-- 1. 每个市场都有 price_to_beat
SELECT COUNT(*) FROM pm_market_metadata
WHERE market_slug LIKE '%-updown-5m-%'
  AND price_to_beat IS NULL;
-- 期望: 0

-- 2. 每个市场都有持续的 quotes
SELECT
    m.market_slug,
    COUNT(DISTINCT q.token_id) as tokens_with_quotes,
    COUNT(q.*) as total_quotes,
    MIN(q.received_at) as first_quote,
    MAX(q.received_at) as last_quote
FROM pm_market_metadata m
LEFT JOIN clob_quote_ticks q ON (
    q.token_id IN (
        ((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb->0)::text,
        ((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb->1)::text
    )
    AND q.received_at BETWEEN m.start_time AND m.end_time
)
WHERE m.market_slug LIKE '%-updown-5m-%'
  AND m.end_time < NOW()
  AND m.end_time > NOW() - INTERVAL '24 hours'
GROUP BY m.market_slug
HAVING COUNT(DISTINCT q.token_id) < 2 OR COUNT(q.*) < 10;
-- 期望: 0 rows (所有市场都有完整数据)

-- 3. 每个市场都有 settlement
SELECT COUNT(*) FROM pm_market_metadata m
LEFT JOIN pm_token_settlements s ON (
    s.token_id IN (
        ((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb->0)::text,
        ((m.raw_market->'markets'->0->>'clobTokenIds')::jsonb->1)::text
    )
)
WHERE m.market_slug LIKE '%-updown-5m-%'
  AND m.end_time < NOW()
  AND s.token_id IS NULL;
-- 期望: 0
```

## 时间表

- **今天 (04-01)**:
  - ✅ 修复 Quote Collector SQL bug
  - ✅ 验证 quotes 是否在写入 (74,479 quotes/hour, 106 tokens)
  - ✅ 实现 Price to Beat 采集 (自动更新 + Binance回填)
  - ✅ 添加 XRPUSDT 币种
  - ✅ 用 Binance 数据回填历史 price_to_beat (6,809 markets, 83% coverage)
  - ⏳ 运行 24 小时测试

- **明天 (04-02)**:
  - 🔲 改为长连接架构
  - 🔲 数据完整性验证
  - 🔲 回测验证

## 成功标准

1. **数据完整性**: 100% 市场有完整数据链
2. **实时性**: Quote 延迟 < 1 秒
3. **可靠性**: 7x24 运行无中断
4. **回测可行**: 能用历史数据跑完整回测

## 风险

1. **Polymarket API 限制**: 可能有订阅数量限制
2. **WebSocket 稳定性**: 长连接可能断开
3. **数据库性能**: 高频写入可能影响性能
4. **历史数据缺失**: 过去的数据无法补救

## 备选方案

如果 WebSocket 不稳定:
- 方案 A: REST API 轮询 (每 5 秒)
- 方案 B: 混合模式 (WebSocket + REST 补漏)
- 方案 C: 使用 Polymarket 官方数据源
