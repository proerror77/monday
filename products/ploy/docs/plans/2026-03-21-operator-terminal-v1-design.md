# Operator Terminal V1 Design

Date: 2026-03-21

## Goal

给 `ploy` 增加一个真正可用的 `operator terminal v1`：

- 保留终端操作体验
- 不新增绕过 coordinator 的 live path
- 所有控制动作都收敛到 API control plane
- 第一版只做运营控制，不做人手下单

## Decisions Locked

本设计基于以下已确认约束：

- product shape: operator terminal
- interaction model: `TUI + API`
- scope: 只做运营控制
- control granularity: `global + domain`
- live safety rule: 任何动作都不得绕过现有 coordinator / governance / risk 链路

## Why This Exists

`ploy` 现在已经有 dashboard、API、governance、claimer 和 deployment control，但 operator surface 仍然偏“分散”：

- TUI 更像监控页，不像操作台
- `/api/system/*`、`/api/governance/*`、deployment control 已存在，但还没有统一的 operator action 语义
- claimer / pause / halt / status / domain control 分散在不同命令和 handler 中
- 远程前端、sidecar、未来 bot 若要复用控制逻辑，当前接口语义还不够集中

所以第一版的目标不是加新能力，而是把已有控制能力整理成一个稳定的 operator control surface。

## Non-Goals

第一版明确不做：

- 人工 `buy/sell`
- 单 market 高频手动交易台
- deployment 级精细控制
- 参数热切换
- 新的独立状态机
- 任何绕过 coordinator 的“terminal 直连下单”

## Product Shape

`operator terminal v1` 由两层组成：

### 1. Operator Control API

作为唯一控制语义中心，负责：

- 查询全局运行状态
- 查询 domain 级运行状态
- 发起全局 / domain 级 pause / resume / force-close / claim-check / claim-run
- 返回统一 action receipt
- 记录审计事件

### 2. Terminal Frontend

基于现有 TUI 扩展成操作台，负责：

- 展示 system / governance / risk / queue / claimer 摘要
- 展示 domain 级状态
- 发起操作动作
- 展示 action result / recent operator events

TUI 只做前端，不直接操作 runtime 内部对象。

## Architecture Decision

采用 `API-first operator terminal`。

### Why

- 符合 repo 已锁定的 control-plane 方向
- 避免把控制逻辑锁死在 TUI
- 后续 frontend / sidecar / bot 都能复用同一控制契约
- 最容易遵守 “Coordinator-only live execution” 约束

### Not Chosen

#### 1. TUI-direct control

让 TUI 直接持有 `CoordinatorHandle` 并发命令。

不采用原因：

- 会把控制语义埋进终端进程
- 不利于后续前端或 bot 复用
- 会继续扩大 operator surface 之间的语义漂移

#### 2. API-only without TUI changes

先只补 API，不落 terminal。

不采用原因：

- 不能立刻改善当前 operator 体验
- 用户想借鉴的是 `polyterminal` 那层操作台体验，而不是单纯再补几个接口

## Canonical Actions

第一版只支持以下动作：

### Global Actions

- `pause_all`
- `resume_all`
- `force_close_all`
- `claim_check`
- `claim_run`

### Domain Actions

- `pause_domain`
- `resume_domain`
- `force_close_domain`

### Explicit Exclusions

- `buy`
- `sell`
- `enable_deployment`
- `disable_deployment`
- `patch_strategy_config`

这些都留给后续版本。

## API Contract

第一版不应再继续散落在多个 ad-hoc endpoint 上，而应补一个统一 operator action contract。

### Read Model

增加一个统一读接口，聚合现有 state：

- system status
- governance status
- domain ingress modes
- risk snapshot
- queue snapshot
- claimer summary
- recent operator events

建议路径：

- `GET /api/operator/status`

它是 operator terminal 的首页数据源。

### Write Model

增加一个统一动作入口：

- `POST /api/operator/actions`

请求体只表达：

- `action`
- `scope`
- `domain`（可选）
- `requested_by`
- `reason`（可选）

返回：

- `accepted`
- `action_id`
- `effective_scope`
- `effective_targets`
- `message`
- `requested_at`

### Why A Unified Action Endpoint

不推荐在 TUI 里分别直接打：

- `/api/system/pause`
- `/api/system/resume`
- `/api/system/halt`
- 未来 claim endpoint

原因是 operator terminal 需要一个统一 action log 和统一确认模型。统一 action endpoint 更适合：

- 权限检查
- 审计记录
- UI 反馈
- 未来 bot / frontend 复用

## Runtime Mapping

operator action 不引入新 runtime 语义，只映射到现有能力：

- `pause_all` -> `CoordinatorControlCommand::PauseAll`
- `resume_all` -> `CoordinatorControlCommand::ResumeAll`
- `force_close_all` -> `CoordinatorControlCommand::ForceCloseAll`
- `pause_domain` -> `CoordinatorControlCommand::PauseDomain(Domain)`
- `resume_domain` -> `CoordinatorControlCommand::ResumeDomain(Domain)`
- `force_close_domain` -> `CoordinatorControlCommand::ForceCloseDomain(Domain)`
- `claim_check` -> account-level claimer read/check path
- `claim_run` -> one-shot claimer execution path

关键原则：

- action handler 只做 orchestration
- runtime 真正语义仍然属于 coordinator / claimer / governance
- operator terminal 不能成为第二个 runtime owner

## Claimer Integration

claimer 需要被提升为 operator-visible capability，而不是隐藏在 CLI 里。

### V1 Behavior

`operator/status` 返回：

- claimer enabled/disabled
- last check time
- pending redeemable count
- pending redeemable notional
- last claim result
- last error

### Actions

- `claim_check`: 只做检查，不提交链上 redeem
- `claim_run`: 触发一次 one-shot claim

### Safety Rule

`claim_run` 必须复用现有 claimer 逻辑，不能在 API handler 内重新实现 redeem 细节。

## TUI Design

第一版 TUI 在现有 dashboard 上扩成“监控 + 控制”模式，不重写整套 UI。

### New Panels

- `Operator Summary`
  - runtime mode
  - dry run
  - system status
  - queue depth
  - risk state
- `Domain Control`
  - 每个 domain 的 ingress mode / paused state / exposure / pnl
- `Claimer`
  - enabled
  - pending redeemable count
  - last check / last run / last error
- `Recent Actions`
  - 最近 operator action 及结果

### Keybindings

第一版建议：

- `P`: pause all
- `R`: resume all
- `X`: force close all
- `1/2/3...`: 选中 domain
- `p`: pause selected domain
- `r`: resume selected domain
- `x`: force close selected domain
- `c`: claim check
- `C`: claim run
- `g`: refresh operator snapshot

所有 destructive action 都必须二次确认。

### UI Principle

保留现有 ratatui 架构，不再做 `polyterminal` 式单脚本清屏输出。`ploy` 已经有更适合扩展的 TUI 模块边界。

## Data Flow

### Read Path

`TUI -> GET /api/operator/status -> AppState / coordinator / governance / claimer adapters -> response`

### Write Path

`TUI -> POST /api/operator/actions -> auth / validation / audit -> coordinator or claimer dispatch -> action receipt`

### Eventing

第一版可以先用轮询 + immediate refresh，不强依赖 websocket 新事件种类。

如果当前 websocket 广播容易接入，可加：

- `operator.action.accepted`
- `operator.action.completed`

但这不是 v1 上线前置条件。

## Security And Permissions

operator action 必须继续走 admin token 权限，不为 terminal 降权限。

### Required Rules

- 所有 write action 都需要 admin auth
- action request 必须写审计日志
- destructive action 必须有 UI confirm
- `claim_run` 必须显示 dry-run / live 上下文
- 若 runtime 不支持对应 capability，返回显式 `not_supported`

## Observability

operator terminal v1 至少要补以下可观测性：

- operator action audit log
- action success / failure counters
- claimer check / claim metrics
- per-domain control action counters

这样后续无论接 TUI、frontend 还是 bot，都能追踪谁在什么时间做了什么动作。

## File Shape

建议按现有结构扩展，而不是新起一套平行目录。

### API

- `src/api/handlers/operator.rs`
- `src/api/types.rs`
- `src/api/routes.rs`
- `src/api/state.rs`

### TUI

- `src/tui/app.rs`
- `src/tui/data.rs`
- `src/tui/event.rs`
- `src/tui/runner.rs`
- `src/tui/ui.rs`
- `src/tui/widgets/*`

### Runtime Orchestration

- 复用 `src/coordinator/command.rs`
- 复用 `src/main_modes/claimer_mode.rs` 对应能力
- 如有必要，抽一个轻量 service 层承接 operator action orchestration

## Migration Strategy

### Phase 1

先补 API contract 和 read model。

### Phase 2

再补统一 action dispatch。

### Phase 3

最后把 TUI 接到新 contract。

这样任何一步都不会引入新的 live path，也能逐步验证：

- API 语义是否稳定
- claimer 能否安全挂入 control plane
- TUI 是否只是薄前端

## Success Criteria

第一版完成后，应满足：

- `ploy dashboard` 可切到 operator 视图
- TUI 能看见全局 + domain 级 operator 状态
- TUI 能触发 pause/resume/force-close/claim-check/claim-run
- 所有动作都通过 API control plane
- 没有新增 direct live order path
- 所有动作都有 audit trail
- claimer 成为 operator-visible capability，而不是孤立 CLI 功能
