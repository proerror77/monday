# Strategy Plugin Platform Design

Date: 2026-03-07

## Goal

让策略的运营形态从“代码模块 + bootstrap 特判”收敛成“插件定义 + 插件参数 + 插件部署”，使：

- crypto 新 alpha 尽量不再需要新增 Rust strategy 文件
- 策略可以像运营插件一样上架、下架、启用、停用、draining
- event/sports 仍可保留注册型实现，但不能再拥有另一套 runtime
- 账户管理与订单管理成为显式平台平面，而不是散在 strategy/bootstrap 里的功能集合

## User Constraints

本设计以以下约束为前提：

1. `crypto` 同类策略希望做到只改配置即可上线。
2. 新的 crypto alpha 也尽量不新增 Rust strategy，而是由预定义积木组合而成。
3. 仅接受预定义积木组合，不引入脚本语言或自由 DSL。
4. `event` / `sports` 允许额外注册链路，但必须落在同一条 canonical runtime 上。
5. 策略下架默认语义不是强平，而是：
   - 停止新开仓
   - 已有仓位继续由策略自己收尾
   - 不自动强平
6. 所谓“插件”指运营插件，不是动态加载代码插件。

## Why The Current Shape Is Not Enough

当前 layered runtime 已经大幅收敛，但距离“策略像插件一样上下架”还差三个结构层面的 owning plane：

### 1) 没有正式的 Plugin Plane

当前系统已经接近统一 runtime，但策略仍主要通过以下对象被感知：

- `Strategy`
- `runtime_specs`
- deployment config
- bootstrap path selection

这意味着系统知道“怎么启动某个策略”，但不知道“插件是什么、插件如何被安装、插件如何被启停、插件如何进入 draining”。

### 2) Account Plane 还未显式化

账户相关能力分散在：

- `src/config.rs` 的 account config
- `bootstrap` 中的 accounts 表维护
- `src/strategy/claimer.rs`
- API 层的 accounts overview
- coordinator governance policy

这说明系统已经有 account-scoped 数据和动作，但还没有统一的 account owner 来承接：

- strategy budget
- reserved capital
- claim / redeem lifecycle
- deployment-to-account binding
- account-level ledger snapshots

### 3) Order Plane 缺少 lifecycle contract

订单主链路已经基本具备：

- ingress
- risk gate
- queue
- execution
- fill tracking
- cancel / modify
- reconciliation

但它还没有显式承接 deployment lifecycle。特别是 `draining` 需要一个平台级契约来区分：

- 新开仓
- 平仓
- 减仓
- 对冲
- 撤单

如果 execution contract 不知道这些语义，`draining` 就只能停留在 runtime 约定上，不能成为平台规则。

## Recommended Architecture

采用 **Strategy Plugin Platform**，但插件定义为运营插件，而不是动态代码插件。

### Top-Level Planes

建议未来 steady state 由五层组成：

1. **Plugin Registry Plane**
   - 管理“有哪些策略插件存在”
   - 负责 definition / spec / deployment 的加载、校验、查找

2. **Strategy Plane**
   - 承载 canonical `Strategy` runtime
   - 执行策略自己的 market understanding / signal / state machine

3. **Capital / Account Plane**
   - 承载账户、预算、claimer、redeem、deployment-to-account ownership

4. **Execution / Order Plane**
   - 承载统一下单、改单、撤单、fill tracking、reconciliation、recovery

5. **Control / Governance Plane**
   - 承载 deployment controls、pause/resume、allocator、OpenClaw policy projection

## Plugin Model

建议将“插件”拆成三个不同对象，而不是把定义、参数和运行实例混成一份配置。

### 1) Plugin Definition

描述“这是什么能力”。

示例字段：

- `plugin_id`
- `kind`
- `version`
- `domain`
- `schema_ref`
- `capabilities`
- `default_draining_behavior`

这层对应 skills 的“能力目录”。

### 2) Plugin Spec

描述“这个能力如何被参数化”。

对于 `composable_crypto`，spec 主要由 block 组合构成。
对于 `registered_strategy`，spec 主要是注册策略的参数 schema。

### 3) Plugin Deployment

描述“这个插件在某个账户/环境里是否运行”。

示例字段：

- `deployment_id`
- `plugin_id`
- `account_id`
- `execution_mode`
- `state`
- `dry_run/live`
- `budget_profile`
- `tags`

真正被 operator 上下架的是 deployment，不是 definition，也不是代码本身。

## Two Plugin Kinds

### 1) Composable Crypto Plugin

这是本设计的核心。

crypto 新 alpha 不再默认等于新增一个 Rust strategy。相反，系统提供一个统一的 `ComposableCryptoStrategy`，从 `ComposableCryptoSpec` 读取积木组合并执行。

第一版 block catalog 应保持保守，只支持当前高频需求：

- `signals`
  - `momentum`
  - `mean_reversion`
  - `spread_dislocation`
- `filters`
  - `time_window`
  - `volatility_gate`
  - `liquidity_gate`
- `entry`
  - `marketable_limit`
  - `ladder_limit`
- `exit`
  - `trailing_stop`
  - `edge_decay`
  - `time_stop`
- `sizing`
  - `fixed_shares`
  - `fixed_usd_risk`
  - `budget_fraction`
- `risk_budget`
  - `max_daily_loss`
  - `max_open_positions`
  - `max_notional`
  - `cooldown`

这里的目标不是“一切都可配置”，而是让高频 crypto alpha 变成由平台预定义积木拼装出的运营插件。

### 2) Registered Event/Sports Plugin

`event_edge`、`nba_comeback` 这类不适合强行压进 block composition，因此保留为注册型策略实现。

但它们必须满足：

- 有 `PluginDefinition`
- 有 `PluginDeployment`
- 走同一条 canonical runtime
- 走同一套 governance/account/execution planes
- 服从同一套 lifecycle semantics

也就是说，底层实现可以不同，运营面必须统一。

## Lifecycle Semantics

平台必须显式支持以下 deployment states：

- `enabled`
- `draining`
- `disabled`
- `archived`

### Enabled

- 接受全部有效策略动作
- 允许新 entry
- 允许 exit / reduce / hedge / cancel

### Draining

- 禁止新开仓
- 允许已有仓位继续退出
- 允许减仓、对冲、撤单
- 运行状态保留，直到仓位和挂单都清空

### Disabled

- 不再产生新动作
- 只保留必要的状态恢复和读侧可见性

### Archived

- 历史对象，仅供审计和查询
- 不允许运行

## Required Order-Plane Contract Change

为支持上述 lifecycle，canonical strategy output 必须显式包含 intent purpose。

建议在统一 contract 中新增：

- `entry`
- `exit`
- `reduce`
- `hedge`
- `cancel`

这样平台才能写出明确规则：

- `enabled`：全部允许
- `draining`：拒绝 `entry`，放行 `exit/reduce/hedge/cancel`
- `disabled`：不再接受新 intent

没有这个字段，`draining` 就无法成为平台规则。

## Account Plane

建议单独抽出 `src/account/`，承接当前散落的账户相关功能。

推荐子模块：

- `registry.rs`
  - account metadata
  - wallet address
  - labels
- `budget.rs`
  - strategy budgets
  - reserved capital
  - account allocation
- `ledger.rs`
  - account-level realized/unrealized/pending claim snapshots
- `claimer.rs`
  - redeem / claim lifecycle
- `service.rs`
  - 对 plugin deployments / API / governance 暴露统一接口

一个关键重定位是：

- `claimer` 不应继续属于 `strategy` 语义
- 它应属于账户层资产回收能力

## Read-Side API Visibility

为了让 plugin/account lifecycle 成为正式平台能力，而不是内部约定，读侧 API 需要直接暴露这些状态：

- `GET /api/system/capabilities`
  - deployment state counts：`enabled|draining|disabled|archived`
  - scoped deployment state counts（account + dry_run scope）
  - builtin plugin summaries（`plugin_id` / `kind` / `domain` / `version`）
- `GET /api/system/accounts`
  - per-account deployment state counts
  - runtime budget snapshot

这样 operator 才能直接观察：

- 哪些 deployments 正在 `draining`
- `draining` 是 active-but-draining，而不是 disabled
- 当前 runtime 认识哪些 builtin plugins
- runtime account 当前预算快照是什么

## Repo Mapping

### New Modules

- `src/plugins/definition.rs`
- `src/plugins/spec.rs`
- `src/plugins/deployment.rs`
- `src/plugins/registry.rs`
- `src/plugins/projector.rs`
- `src/account/registry.rs`
- `src/account/budget.rs`
- `src/account/ledger.rs`
- `src/account/claimer.rs`
- `src/account/service.rs`

### Existing Modules To Reposition

- `src/coordinator/runtime_specs.rs`
  - 逐步演化为 plugin spec projector
- `src/strategy/*`
  - 只保留 canonical runtime + concrete strategy implementations
- `src/agents/openclaw/*`
  - 继续作为 governance-only capability
- `src/agents/*` 与 `src/platform/agents/*`
  - 旧兼容 runtime 最终退役

## Execution Blueprint

### Phase 1: Add The Plugin Object Model

先添加：

- `PluginDefinition`
- `PluginSpec`
- `PluginDeployment`
- deployment state schema
- registry loader / validator

不改变当前 runtime 行为，仅让系统学会“认识插件”。

### Phase 2: Promote `runtime_specs` Into Plugin Projection

让 runtime projection 的输入逐步从 strategy-specific config 转向：

- `PluginDefinition`
- `PluginSpec`
- `PluginDeployment`

### Phase 3: Extract Account Plane

把 account metadata、budgets、claimer、ledger 和 deployment binding 收口到 `src/account/`。

### Phase 4: Add Order Lifecycle Contract

给统一 execution contract 增加 intent purpose，并正式接入 deployment state gating。

### Phase 5: Build `ComposableCryptoStrategy`

实现统一的 composable crypto runtime，并接入第一版 block catalog。

### Phase 6: Wrap Event/Sports As Registered Plugins

保留代码实现，但用 plugin definitions + deployments 承接运营面。

### Phase 7: Retire Legacy Surfaces

在新 plugin/deployment path 跑通后，再删除 legacy compatibility runtime paths。

## Risks

### 1) 过早追求“全可配置”

如果第一版 block catalog 试图覆盖所有 alpha，会把系统推向 mini-language 或复杂 DSL。第一版应只覆盖高频、稳定、明确的 crypto primitives。

### 2) Account Plane 继续缺席

如果只做 plugin registry 而不补 account plane，deployment-to-account ownership 仍会模糊，claimer/budget/redeem 的语义也会继续散落。

### 3) Draining 仍停留在约定

如果 execution contract 不增加 intent purpose，`draining` 仍然只能依赖策略内部自觉，不是平台规则。

## Acceptance Criteria

1. 新 crypto alpha 不需要新增 Rust strategy 文件。
2. 新 crypto 策略可通过 plugin spec + deployment 上线。
3. 下架默认进入 `draining`，不会开新仓，但允许已有仓位自然退出。
4. `event/sports` 虽仍为注册型实现，但走同一条 canonical runtime。
5. `claimer` 和 account budget 不再挂在 strategy 语义下。
6. order plane 能显式识别 `entry`、`exit`、`reduce`、`hedge`、`cancel`。
7. `bootstrap` 不再承担策略产品形态的决定权，只做 assembly。
