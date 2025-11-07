# Rust HFT 项目依赖关系分析报告

**生成日期**: 2025-10-07
**分析工具**: 自定义 Python 依赖分析器
**项目路径**: `/Users/proerror/Documents/monday/rust_hft`

---

## 执行摘要

本报告对 Rust HFT 高频交易系统进行了全面的依赖关系分析。系统采用清晰的分层架构，共包含 **56 个 crate**，平均依赖数为 **2.75**，最大依赖深度为 **8**。

**关键发现**:
- ✅ **零循环依赖** - 依赖图是有向无环图 (DAG)
- ✅ **零架构违规** - 所有依赖方向符合分层设计
- ⚠️ **热路径污染** - `hft-engine` 依赖 `hft-infra-metrics`（需修复）
- ✅ **适配器隔离** - 所有适配器之间无直接依赖

---

## 指标概览

| 指标 | 数值 | 状态 |
|:-----|:-----|:-----|
| 模块总数 | 56 | ✅ |
| 平均依赖数 | 2.75 | ✅ 良好 |
| 最大依赖深度 | 8 | ✅ 可控 |
| 循环依赖 | 0 | ✅ 优秀 |
| 架构违规 | 0 | ✅ 优秀 |
| 热路径污染 | 1 | ⚠️ 需修复 |

---

## 架构分层

系统采用清晰的七层架构设计：

### L0-Foundation (基础层)
**模块数**: 2
**职责**: 零依赖基础类型和快照容器

- `hft-core` - 整数新型别、错误、常量 (零外部依赖)
- `hft-snapshot` - ArcSwap/left-right 快照容器

### L1-Ports (端口层)
**模块数**: 2
**职责**: 稳定接口定义

- `hft-instrument` - 交易所/合约/精度/费率模型
- `hft-ports` - 事件模型 + traits (Never break userspace)

### L2-Core (核心层)
**模块数**: 4
**职责**: 核心业务逻辑

- `hft-engine` - 单 writer 事件循环框架
- `hft-oms-core` - 订单状态机
- `hft-portfolio-core` - 账户视图快照
- `hft-risk` - 事前风控系统

### L3-Adapters (适配器层)
**模块数**: 22
**职责**: 外部系统集成

#### 数据适配器 (11个)
- `hft-data-adapter-bitget`
- `hft-data-adapter-binance`
- `hft-data-adapter-bybit`
- `hft-data-adapter-hyperliquid`
- `hft-data-adapter-asterdex`
- `hft-data-adapter-lighter`
- `hft-data-adapter-backpack`
- `hft-data-adapter-grvt`
- `hft-data-adapter-mock`
- `hft-data-adapter-replay`
- `hft-data-adapters-common`

#### 执行适配器 (9个)
- `hft-execution-adapter-bitget`
- `hft-execution-adapter-binance`
- `hft-execution-adapter-bybit`
- `hft-execution-adapter-hyperliquid`
- `hft-execution-adapter-asterdex`
- `hft-execution-adapter-lighter`
- `hft-execution-adapter-backpack`
- `hft-execution-adapter-okx`
- `hft-execution-adapter-grvt`

#### 其他适配器
- `hft-data` - 公共行情核心
- `hft-execution` - OMS/ExecutionClient

### L4-Runtime (运行时层)
**模块数**: 3
**职责**: 系统组装和配置

- `hft-runtime` - 系统构建器和运行时管理
- `hft-shared-config` - 共享配置模式
- `hft-shared-instrument` - 共享交易品种目录

### L5-Apps (应用层)
**模块数**: 4
**职责**: 最终可执行程序

- `hft-live` - 真盘交易应用
- `hft-paper` - 模拟盘应用
- `hft-replay` - 回放应用
- `hft-all-in-one` - 全功能应用

### L6-Infra (基础设施层)
**模块数**: 6
**职责**: 旁路基础设施服务

- `hft-infra-clickhouse` - ClickHouse 集成
- `hft-infra-redis` - Redis 集成
- `hft-infra-metrics` - Prometheus 指标
- `hft-infra-venue` - 交易所配置
- `hft-ipc` - IPC 控制平面
- `hft-recovery` - 启动恢复系统

---

## 依赖关系可视化

### 核心依赖流

```
Apps Layer (L5)
    └─→ Runtime Layer (L4)
            └─→ Adapters Layer (L3)
                    └─→ Core Layer (L2)
                            └─→ Ports Layer (L1)
                                    └─→ Foundation Layer (L0)

Infra Layer (L6) [旁路，不进入热路径]
```

### 高扇出模块 (依赖最多)

| 模块 | 依赖数 | 分类 |
|:-----|:------|:-----|
| hft-runtime | 35 | 🟡 合理 (组装层) |
| hft-engine | 7 | 🟡 可接受 |
| hft-live | 6 | 🟢 良好 |
| hft-examples | 5 | 🟢 良好 |
| hft-recovery | 5 | 🟢 良好 |

### 高扇入模块 (被依赖最多)

| 模块 | 被依赖数 | 分类 |
|:-----|:--------|:-----|
| hft-core | 39 | 🟢 优秀 (基础层) |
| hft-ports | 37 | 🟢 优秀 (接口层) |
| hft-integration | 18 | 🟢 良好 (网络层) |
| hft-runtime | 5 | 🟢 良好 (运行时) |
| hft-engine | 4 | 🟢 良好 (核心) |

---

## 模块耦合度分析

### 稳定性指标 (Instability Metric)

```
I = fanout / (fanin + fanout)

I = 0.0: 完全稳定 (只被依赖，不依赖其他)
I = 1.0: 完全不稳定 (只依赖其他，不被依赖)
I = 0.3-0.7: 平衡状态
```

### 稳定模块 (I < 0.3) - 13 个

| 模块 | I 值 | Fanin | Fanout | 评价 |
|:-----|:-----|:------|:-------|:-----|
| hft-core | 0.00 | 39 | 0 | 🟢 完美基础 |
| hft-snapshot | 0.00 | 3 | 0 | 🟢 完美基础 |
| hft-integration | 0.00 | 18 | 0 | 🟢 完美基础 |
| hft-infra-clickhouse | 0.00 | 1 | 0 | 🟢 良好隔离 |
| hft-infra-redis | 0.00 | 1 | 0 | 🟢 良好隔离 |

### 平衡模块 (0.3 ≤ I < 0.7) - 19 个

| 模块 | I 值 | Fanin | Fanout | 评价 |
|:-----|:-----|:------|:-------|:-----|
| hft-shared-instrument | 0.33 | 2 | 1 | 🟢 健康 |
| hft-oms-core | 0.50 | 2 | 2 | 🟢 健康 |
| hft-shared-config | 0.50 | 2 | 2 | 🟢 健康 |

### 不稳定模块 (I ≥ 0.7) - 24 个

| 模块 | I 值 | Fanin | Fanout | 评价 |
|:-----|:-----|:------|:-------|:-----|
| hft-live | 1.00 | 0 | 6 | 🟡 合理 (应用层) |
| hft-paper | 1.00 | 0 | 1 | 🟡 合理 (应用层) |
| hft-replay | 1.00 | 0 | 1 | 🟡 合理 (应用层) |
| hft-examples | 1.00 | 0 | 5 | 🟡 合理 (示例) |

**注意**: 应用层的高不稳定性是正常且预期的，因为它们是最终组装点。

---

## 关键问题与建议

### 🔥 CRITICAL - 热路径污染

**问题**: `hft-engine` 依赖 `hft-infra-metrics`

```
hft-engine (L2-Core)
    └─→ hft-infra-metrics (L6-Infra)  ❌ 违反热路径纯度
```

**影响**:
- 破坏核心层的纯净性
- 可能引入额外延迟
- 违反"基础设施不进入热路径"原则

**建议修复方案**:

#### 方案一: Feature Gate 隔离（推荐）
```toml
# hft-engine/Cargo.toml
[dependencies]
infra-metrics = { package = "hft-infra-metrics", optional = true }

[features]
default = []
metrics = ["dep:infra-metrics"]
```

```rust
// hft-engine/src/lib.rs
#[cfg(feature = "metrics")]
use infra_metrics::register_metric;

pub fn on_event(&mut self, event: Event) {
    // 热路径逻辑

    #[cfg(feature = "metrics")]
    register_metric("event_count", 1);
}
```

#### 方案二: 回调注入
```rust
// 在 runtime 层注入指标收集器
pub struct Engine<M: MetricsCollector> {
    metrics: Option<M>,
}

impl<M> Engine<M> {
    pub fn on_event(&mut self, event: Event) {
        // 热路径逻辑

        if let Some(m) = &self.metrics {
            m.record("event_count", 1);
        }
    }
}
```

### ✅ 优势总结

#### 1. 零循环依赖
- 依赖图是严格的有向无环图 (DAG)
- 便于理解和维护
- 支持增量编译

#### 2. 清晰分层
- 七层架构职责明确
- 依赖方向单向向下
- 便于测试和替换

#### 3. 适配器隔离
- 所有数据适配器之间无直接依赖
- 所有执行适配器之间无直接依赖
- 通过 `adapters-common` 共享工具代码

#### 4. Feature Gates
- `hft-runtime` 使用 feature gates 控制可选依赖
- 应用层通过 features 组装最终二进制
- 支持按需编译

### ⚠️ 需要关注的领域

#### 1. Runtime 依赖数量
- `hft-runtime` 依赖 35 个模块
- 作为组装层，这是合理的
- 但需要定期审查是否有冗余

#### 2. Engine 复杂度
- `hft-engine` 依赖 7 个模块
- 包含订单簿、策略路由、风控等多个职责
- 建议考虑进一步拆分

#### 3. 共享模块设计
- `shared-config` 和 `shared-instrument` 正在形成
- 需要明确职责边界
- 避免成为"上帝模块"

---

## 依赖矩阵

核心模块依赖关系矩阵（✓ 表示行依赖列）:

```
模块                  core ports integ runti engin metri snap instr
─────────────────────────────────────────────────────────────────────
hft-core               ·     ·     ·     ·     ·     ·     ·     ·
hft-ports              ✓     ·     ·     ·     ·     ·     ·     ✓
hft-integration        ·     ·     ·     ·     ·     ·     ·     ·
hft-runtime            ✓     ✓     ✓     ·     ✓     ✓     ✓     ·
hft-engine             ✓     ✓     ·     ·     ·     ✓     ✓     ✓
hft-infra-metrics      ✓     ·     ·     ·     ·     ·     ·     ·
hft-snapshot           ·     ·     ·     ·     ·     ·     ·     ·
hft-instrument         ✓     ·     ·     ·     ·     ·     ·     ·
```

---

## 策略框架依赖

所有策略模块都遵循相同的依赖模式：

```
strategy-trend/arbitrage/imbalance/dl/lob-flow-grid
    └─→ hft-ports (接口)
    └─→ hft-core (基础类型)
```

**优势**:
- 策略之间完全隔离
- 通过统一接口集成
- 便于独立开发和测试

---

## 适配器依赖模式

### 数据适配器标准模式
```
data-adapter-{venue}
    └─→ hft-integration (网络层)
    └─→ hft-ports (接口)
    └─→ hft-core (基础类型)
```

### 执行适配器标准模式
```
execution-adapter-{venue}
    └─→ hft-integration (网络层)
    └─→ hft-ports (接口)
    └─→ hft-core (基础类型)
```

**一致性**: 所有适配器遵循相同的依赖模式，便于维护和扩展。

---

## 改进路线图

### 短期（1-2周）

1. **修复热路径污染**
   - 在 `hft-engine` 中使用 feature gate 隔离 `hft-infra-metrics`
   - 确保默认编译不包含指标依赖

2. **文档化依赖规范**
   - 创建 `docs/dependency-guidelines.md`
   - 明确每层允许的依赖范围

### 中期（1个月）

3. **Engine 拆分评估**
   - 考虑将策略路由、风控、订单簿维护分离
   - 保持核心事件循环的简洁性

4. **共享模块治理**
   - 明确 `shared-config` 和 `shared-instrument` 的职责边界
   - 防止成为"垃圾桶"模块

### 长期（持续）

5. **定期审查**
   - 每月运行依赖分析
   - 跟踪关键指标趋势
   - 及时发现和修复违规

6. **自动化检查**
   - 在 CI/CD 中集成依赖检查
   - 阻止引入循环依赖
   - 警告不合理的依赖方向

---

## 结论

Rust HFT 项目的依赖架构整体**优秀**，展现了良好的设计原则：

✅ **零循环依赖** - 依赖图清晰可控
✅ **清晰分层** - 职责边界明确
✅ **适配器隔离** - 插件式架构
✅ **Feature Gates** - 灵活组装

唯一需要修复的问题是 `hft-engine → hft-infra-metrics` 的热路径污染，这可以通过 feature gate 快速解决。

修复该问题后，系统将达到 **生产级依赖纯度**，为高频交易的极致性能目标奠定坚实基础。

---

## 附录：完整模块列表

### Foundation Layer (L0)
1. hft-core
2. hft-snapshot

### Ports Layer (L1)
3. hft-instrument
4. hft-ports

### Core Layer (L2)
5. hft-engine
6. hft-oms-core
7. hft-portfolio-core
8. hft-risk

### Adapters Layer (L3)
9. hft-data
10. hft-execution
11-21. Data adapters (11个)
22-30. Execution adapters (9个)
31. hft-data-adapters-common

### Runtime Layer (L4)
32. hft-runtime
33. hft-shared-config
34. hft-shared-instrument

### Apps Layer (L5)
35. hft-live
36. hft-paper
37. hft-replay
38. hft-all-in-one

### Infra Layer (L6)
39. hft-infra-clickhouse
40. hft-infra-redis
41. hft-infra-metrics
42. hft-infra-venue
43. hft-ipc
44. hft-recovery

### Other
45-56. Tools, examples, strategies (12个)

---

**报告生成器**: Python 自定义依赖分析工具
**分析深度**: 完整工作空间扫描
**数据源**: 所有 Cargo.toml 文件
**验证**: 手动交叉验证核心依赖关系
