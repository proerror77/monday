# Feature Gates 指南

本指南概述了 workspace 各 crate 的 feature 设计、命名规范以及推荐的特性组合，帮助在不同运行场景下快速启用合适的能力，并降低特性矩阵的复杂度风险。

## 命名与分组原则

- 作用域前缀：`adapter-*`、`strategy-*`、`infrastructure`（逐步统一为 `metrics`/`clickhouse`/`redis` 等）、`simd-*`、`profile-*`
- 转发而不重命名：上层 crate 仅做特性转发，不改变下游特性语义
- 小而清晰：避免“超级特性”聚合过多子特性（如 `full` 仅用于开发便捷）

## 核心 crates 特性

### hft-engine

- `metrics`: 启用引擎侧 Prometheus 指标埋点（依赖 `hft-infra-metrics`）

### hft-integration

- `json-simd`: 启用 `simd-json`，用于更快的 JSON 解析

### hft-runtime

- 适配器特性：`adapter-*-data`、`adapter-*-execution`
- 策略特性：`strategy-*`
- 基础设施：`clickhouse`、`redis`、`metrics`、`infra-ipc`、`recovery`
- 便捷聚合：`bitget`、`binance`、`bybit`、`mock`、`all-strategies`、`full`

注意：`metrics` 会转发到 `engine/metrics` 并引入 `hft-infra-metrics`。如需开启 HTTP 暴露端口，请在应用层额外启用 `infra-metrics/http-server` 特性（runtime 的 `metrics` 不会自动打开 http-server）。

## 应用层特性组合

### hft-live（生产）

默认特性：`bitget`、`trend-strategy`、`metrics`

- `metrics` 组合包含：
  - `runtime/metrics`（转发 `engine/metrics` + 引入 infra 库）
  - 如需 HTTP 暴露：在应用层再加 `hft-infra-metrics/http-server`

常用构建：

```bash
cargo build -p hft-live --features "binance,all-strategies,metrics,hft-infra-metrics/http-server"
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
- 顶层启用指标：优先使用统一的 `metrics` 特性，由 runtime 串联 engine 与指标服务；需要 HTTP 暴露再在应用层开启 `infra-metrics/http-server`

## Metrics 使用（示例）

- 仅启用指标收集（不启动 HTTP）：
  - `cargo run -p hft-paper --features "metrics"`
- 启用指标并通过 HTTP 暴露（Axum）：
  - `cargo run -p hft-paper --features "metrics,hft-infra-metrics/http-server"`
- Engine 端可观测项（节选）：
  - 直方图：`hft_latency_*_microseconds`（ingestion/aggregation/strategy/risk/execution/end_to_end）
  - 计数器：`hft_orders_*_total`, `hft_intents_dropped_total`, `hft_snapshot_publish_failed_total`
  - Gauges：`hft_engine_*`（cycle_count、exec_events_processed、orders_* 当前快照）
- 文档即代码：新增特性务必在本文件补充说明与示例

## 新增：Lighter DEX（行情）

- 适配器特性：`adapter-lighter-data`（公有 WS 行情 `/stream`）
- 便捷聚合：`lighter` 仅包含行情（执行集成需接入官方 signer 库，后续提供）

示例：

```bash
cargo run -p hft-paper --features "lighter"
```

## 后续计划（提案）

- 添加 `profile-live`、`profile-paper`、`profile-replay` 三个场景特性，封装稳定组合，进一步简化特性矩阵
