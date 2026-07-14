# Trading Platform Refactor Design

Date: 2026-03-20

## Goal

把 `ploy` 重构成一个真正的 `trading platform`：

- `v1` 运行在单机
- 支持多策略、多 deployment
- 外部部署保持极简
- 控制面以 API 为唯一语义中心
- research / backtesting 不再污染 live platform 主路径

## Decisions Locked

本设计基于以下已经确认的约束：

- `v1` operating model: 单机、多策略、多 deployment
- 允许多进程，但外部部署必须轻
- canonical operator surface: `API-first`
- deployment unit: 一个 deployment 是一个 `strategy bundle`
- backtesting / research: 完全平台外置
- target runtime shape: `Platform Daemon + Deployment Workers`
- Rust workspace: 从大单体 crate 收敛成真正的 multi-crate workspace

## Why Change

当前 repo 的问题不是功能不够，而是系统身份不清晰：

- `src/strategy` 混合了策略定义、runtime、execution、position、reconciliation、backtest、research
- `bootstrap` 既做装配又做 runtime orchestration，还承担太多历史兼容路径
- CLI、API、TUI、frontend、sidecar 都持有部分业务语义
- deployment 更像 heuristic projection，而不是平台资源对象
- 订单、成交、仓位、风控没有唯一生命周期真相源

结果是：系统既不像交易平台，也不像研究平台，而是多条历史路径叠在一起。

## Architecture Decision

采用 `Platform Daemon + Deployment Workers`：

- `ployd` 是唯一 control plane
- 每个 deployment 运行成一个由 `ployd` 托管的 worker
- 所有 operator surface 都降级成 control-plane client
- trading lifecycle 在平台内部统一
- research / backtesting 从 live platform 主路径完全剥离

不采用：

- `Single Host Service Mesh`
  原因：模块边界会更清楚，但部署更重，违背 `v1` 目标。
- `In-Process Plugin Host`
  原因：部署最轻，但隔离最差，不适合 deployment = strategy bundle 的模型。

## Platform Spine

平台核心只保留 4 个能力域。

### 1. Control Plane

由 `ployd` 统一负责：

- API
- deployment registry
- lifecycle control
- account capability
- global risk policy
- system state
- audit trail

### 2. Execution Core

平台内部只允许一条 canonical trading lifecycle：

`strategy signal -> intent -> order -> exchange ack -> fill -> position update -> pnl update -> risk update`

### 3. Deployment Runtime

每个 deployment 是一个策略组合包的运行单元，由 `ployd` 托管：

- start
- pause
- stop
- restart
- upgrade
- observe

### 4. Market / Exchange Connectivity

市场数据与交易所接入是共享平台设施，不属于单个策略。

## Explicit Non-Goals

以下能力不属于 `v1 trading platform` 核心：

- backtesting
- research lab
- parameter search
- simulation
- notebook-style exploratory tooling
- sidecar 自己定义风险规则
- TUI / frontend 持有独立业务状态机

这些能力可以继续存在于同 repo，但必须从 live platform 主路径断开。

## Deployment Model

`deployment` 是平台的一等资源对象，而不是配置文件推导结果。

### Canonical Deployment Shape

- `deployment_id`
- `bundle_id`
- `account_profile`
- `market_scope`
- `risk_profile`
- `execution_profile`
- `runtime_mode`
- `desired_state`
- `observed_state`

### Semantics

- 一个 deployment 不是单条策略，而是一个 `strategy bundle`
- 同一个 bundle 可以部署到不同账户、风险配置或 market scope
- `ployd` 托管 deployment worker，但 worker 不拥有平台真相

### Worker Responsibilities

worker 只负责：

- 接收 deployment config
- 消费标准化 market feed
- 执行 strategy bundle
- 产出 intents
- 上报 health、metrics、events

worker 不负责：

- 持有订单真相
- 持有仓位真相
- 持有全局风控语义
- 自己定义 operator-facing lifecycle

## Canonical Trading Lifecycle

策略只能产出 `intent`，不能直接拥有订单和仓位真相。

### Flow

1. strategy bundle 产生 `intent`
2. execution core 把 intent 规范化成 order
3. venue adapter 负责提交、取消、替换和 venue translation
4. fill ledger 驱动 position / pnl 变化
5. risk engine 基于 intent、active order、fill、position、exposure 做决策
6. control plane 对外暴露完整 audit trail

### Hard Rules

- 仓位只能由 fill ledger 改变
- order lifecycle 只能由 execution core 维护
- risk 是主链路组成部分，不是旁路系统
- 任意 operator surface 查询到的 order / fill / position / risk 必须来自同一套平台状态

## Operator Surfaces

平台只认一个语义中心：`control-plane API`。

### API

唯一 canonical surface，定义：

- system lifecycle
- deployment CRUD
- account capability
- order / fill / position / pnl / risk read models
- live event stream
- worker health and audit

### CLI

`ployctl` 只做 operator shell：

- 启平台
- 调 control-plane API
- 不再直接持有 runtime orchestration 逻辑

### TUI

实时 operations console：

- 读 API snapshot
- 订阅 event stream
- 发 control-plane actions
- 不再维护 demo state

### Frontend

远程 control console：

- 和 TUI 同语义
- 不再定义独立文案和状态机

### Sidecar

agent operator client：

- 调用与人类操作员一致的 API
- 不再是单策略特例系统
- 不再持有独立风险层

## Repository Shape

repo 应以平台职责组织，而不是按历史模块堆叠。

### Target Workspace

- `apps/ployd`
- `apps/ployctl`
- `crates/ploy-platform`
- `crates/ploy-trading`
- `crates/ploy-connectivity`
- `crates/ploy-deployments`
- `crates/ploy-operator-contracts`
- `crates/ploy-strategy-bundles`
- `crates/ploy-research`（可选，且不得反向污染平台）

### Responsibility Boundaries

`ploy-platform`
- control plane
- deployment registry
- lifecycle
- audit
- accounts
- worker supervisor

`ploy-trading`
- intent
- order
- fill
- position
- pnl
- risk

`ploy-connectivity`
- market data
- venue adapters
- signing

`ploy-deployments`
- worker runtime
- bundle loading
- worker protocol

`ploy-operator-contracts`
- API DTOs
- event schema
- websocket contract

`ploy-strategy-bundles`
- strategy traits
- strategy composition
- signal generation

`ploy-research`
- backtesting
- replay
- simulation
- parameter search

### Boundary Rule

平台核心 crate 不允许依赖 `ploy-research`。

## Simple Deployment Contract

部署要简单，必须具体化成 operator 真正面对的对象。

### External Dependencies

`v1` 只要求：

- `Postgres`
- `ployd`

以下都不是必需基础设施：

- Redis
- sidecar
- frontend
- TUI

### Process Model

- `ployd` 是唯一长期主进程
- deployment worker 由 `ployd` 本机拉起、监控、重启、回收
- operator 只管理 deployment 的 `desired_state`
- 平台负责把 `desired_state` 收敛成真实运行状态

### Config Model

只保留两层配置：

1. `platform config`
2. `deployment manifests`

`platform config` 只包含：

- database
- API bind
- credential references
- global defaults
- worker supervisor defaults

`deployment manifest` 只包含：

- `bundle_id`
- `account_profile`
- `market_scope`
- `risk_profile`
- `execution_profile`
- `runtime_mode`
- `desired_state`

### Operator Contract

日常操作只需要：

- 启平台
- 查看 deployment 列表
- apply deployment manifest
- pause / resume / stop deployment
- inspect deployment 的 orders / fills / positions / risk / logs
- 查看系统总 health

## Migration Principles

重构必须遵守：

- 先建立新平台主干，再逐步把旧路径挂接进去
- compatibility shim 允许短期存在，但不得成为新默认路径
- research / backtesting 先断依赖，再挪目录
- operator surface 先统一 contract，再替换 UI / CLI
- 任何阶段都要能回答：
  - 哪些 deployment 在跑
  - 每个 deployment 当前的风险和仓位是什么
  - 每一笔订单经历了什么生命周期

## Success Criteria

重构完成后，`v1` 至少满足：

- 通过一个 `ployd` 实例托管多个 deployment worker
- 所有 operator surface 只通过 control-plane API 交互
- 平台可以对任意 deployment 返回完整 order / fill / position / risk 视图
- deployment lifecycle 有显式资源模型，不再依赖 heuristic config projection
- Rust workspace 边界与平台职责一致
- live platform 不依赖 research / backtesting 模块
