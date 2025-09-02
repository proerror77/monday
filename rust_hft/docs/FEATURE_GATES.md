# Feature Gates 指南

本指南概述了 workspace 各 crate 的 feature 设计、命名规范以及推荐的特性组合，帮助在不同运行场景下快速启用合适的能力，并降低特性矩阵的复杂度风险。

## 命名与分组原则

- 作用域前缀：`adapter-*`、`strategy-*`、`infra-*`、`simd-*`、`profile-*`
- 转发而不重命名：上层 crate 仅做特性转发，不改变下游特性语义
- 小而清晰：避免“超级特性”聚合过多子特性（如 `full` 仅用于开发便捷）

## 核心 crates 特性

### hft-engine

- `infra-metrics`: 启用引擎侧 Prometheus 指标埋点（依赖 `hft-infra-metrics`）

### hft-integration

- `json-simd`: 启用 `simd-json`，用于更快的 JSON 解析

### hft-runtime

- 适配器特性：`adapter-*-data`、`adapter-*-execution`
- 策略特性：`strategy-*`
- 基础设施：`infra-clickhouse`、`infra-redis`、`infra-metrics`、`infra-ipc`、`recovery`
- 便捷聚合：`bitget`、`binance`、`bybit`、`mock`、`all-strategies`、`full`

注意：自 2025-08 起，`hft-runtime` 不再转发 `engine/infra-metrics`，由最上层应用显式启用（见下）。

## 应用层特性组合

### hft-live（生产）

默认特性：`bitget`、`trend-strategy`、`metrics`

- `metrics` 组合包含：
  - `runtime/infra-metrics`
  - `engine/infra-metrics`（重要：驱动引擎内指标）
  - `infra-metrics/http-server` + `prometheus`

常用构建：

```bash
cargo build -p hft-live --features "binance,all-strategies,metrics"
```

### hft-paper（模拟）

默认特性：`mock`

示例：

```bash
cargo run -p hft-paper --features "bitget"
```

### hft-replay（回放）

常用特性：`mock`、`clickhouse`

```bash
cargo run -p hft-replay --features "mock,clickhouse"
```

## 推荐实践

- 明确场景组合：优先以“场景”为单位组合特性（live/paper/replay），尽量避免“all-*”大集成用于生产
- 顶层启用指标：由应用层启用 `engine/infra-metrics`，避免跨 crate 特性耦合
- 文档即代码：新增特性务必在本文件补充说明与示例

## 后续计划（提案）

- 添加 `profile-live`、`profile-paper`、`profile-replay` 三个场景特性，封装稳定组合，进一步简化特性矩阵

