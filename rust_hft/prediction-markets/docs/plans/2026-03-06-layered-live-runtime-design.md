# Layered Live Runtime Refactor Design

Date: 2026-03-06

## Goal

把 `ploy` 的 live trading 架構收斂成明確四層：

- Strategy Plane：策略自己做進出場與持倉決策
- Capital Governance Plane：agentic 資金治理，負責預算、限額、節流、暫停
- Execution Plane：唯一 live 下單入口與執行/對賬/恢復
- Control Plane：部署、配置、版本、觀測，不進逐筆交易鏈路

## Why This Change

目前 repo 的主要問題不是缺少分層概念，而是分層 ownership 沒有真正落到模組和接口：

- 同時存在 `Strategy`、`TradingAgent`、`DomainAgent` 三套 live runtime 契約
- `bootstrap.rs` 同時做策略分類、runtime 裝配、資料面編排、特判與治理接線
- 策略層、治理層、資料面、控制面責任交叉
- 新策略加入時，工程師沒有唯一答案知道應該掛在哪條 runtime

這會讓 duplicate strategy paths、治理越界、bootstrap 膨脹成為常態。

## Architecture Decision

採用「分層統一」而不是「全部 agent 化」：

1. 策略決定「想做什麼」。
2. 治理層決定「現在准不准做、能做多大」。
3. 執行層決定「如何安全地做」。
4. 控制面只決定「哪些 deployment 可存在、用什麼配置啟動」。

關鍵約束：

- agentic 能力保留在資金治理層，不進策略逐筆方向判斷
- 所有 live order 都只能經過 coordinator ingestion path
- 策略不得直接擁有全局資金治理權
- bootstrap 不再知道單個策略的特殊啟動語義

## Current-State Review

### 1) Strategy Plane 不唯一

現況同時維護三套主要路徑：

- `src/strategy/*`：`Strategy` trait + `StrategyManager` + adapters
- `src/agents/*`：pull-based `TradingAgent`
- `src/platform/agents/*`：push-based `DomainAgent`

結果是同一個策略概念可能同時以：

- legacy engine
- adapter-wrapped strategy
- platform agent
- pull-based trading agent

等不同形式存在。

### 2) Control Plane 嚴重越界

`src/coordinator/bootstrap.rs` 目前承擔了不屬於 control plane 的責任：

- 策略名稱分類與 alias 映射
- crypto market/series 解析
- strategy-specific TOML render
- 各策略 runtime 分支啟動
- data plane / collector / persistence 編排
- split-arb 類策略的特殊 execution observability

這使 bootstrap 成為「總黏合層 + 特判中心」。

### 3) Capital Governance Plane 沒有被獨立

治理能力現在分散在多個 runtime 內部：

- 各類 agent 自帶 `AgentRiskParams`
- 多個 agent 因連續失敗自行 pause
- OpenClaw 仍以另一套 trading runtime 形式接入

這與目標架構衝突。治理層應是策略之上的 policy owner，而不是 runtime 內的局部自保邏輯。

## Target Boundaries

### 1) Strategy Plane

保留在這一層的職責：

- 特徵解讀、訊號生成
- 市場選擇、進場/出場邏輯
- 策略自己的狀態機
- 策略內在持倉邏輯
- 靜態資料需求宣告
- 統一交易意圖輸出

不允許留在這一層的職責：

- 全局資金上限
- 跨策略預算分配
- deployment enable/disable
- pause/resume policy
- 動態資料 feed 控制
- 全局風控閘門決策

### 2) Capital Governance Plane

這層應由 agentic policy 驅動，但權限被明確限制為：

- 策略預算分配
- 域別/策略級限額
- throttle / de-risk / pause / resume
- deployment-level policy projection
- 全局或域別 block new intents

這層不能做的事：

- 替策略決定買哪邊
- 改寫策略 fair value / confidence / edge
- 直接產生交易方向訊號
- 繞過 coordinator 直接下單

### 3) Execution Plane

這層應成為唯一正式 live 路徑：

`StrategyIntent -> Governance Gate -> Risk Gate -> Queue -> Executor -> Reconciliation`

保留在這一層的責任：

- ingress validation
- idempotency
- order routing
- fills / partial fills / retries
- reconciliation
- crash recovery
- position aggregation
- execution audit trail

### 4) Control Plane

這層只做：

- deployment matrix
- config 與版本投影
- strategy enable/disable
- health / observability
- rollout / stop / restart / lifecycle stage

這層不再做：

- strategy classification logic
- strategy-specific runtime config rendering
- market subscription 特判
- 逐筆交易執行語義

## Canonical Interfaces

### Strategy Contract

`src/strategy/traits.rs` 應成為唯一 Strategy Plane 契約來源。

這個契約需要瘦身成純決策層接口：

- 保留 market / order / execution updates
- 保留 strategy state / positions
- 保留 static data requirements
- 保留 strategy shutdown semantics
- 移出 `UpdateRisk`
- 移出 `SubscribeFeed`
- 移出 `UnsubscribeFeed`

### Governance Contract

需要新增一個顯式治理接口，給 OpenClaw 類 policy agent 使用。它只操作：

- strategy budget
- strategy limit overrides
- strategy pause/resume
- domain/global ingress policy

不暴露 `submit_order`.

### Execution Contract

`CoordinatorHandle::submit_order` 對應的 ingestion path 應繼續成為唯一 live order entry。

長期目標是讓 Strategy Plane 的輸出在進入 execution 前只剩一種標準形式，不再同時維護多套下單語義。

## Module Repositioning

### `src/strategy`

重新定位為唯一 Strategy Plane 與 canonical strategy runtime 所在地。

應包含：

- strategy traits
- strategy registry/factory
- strategy runtime
- concrete strategies
- strategy-scoped adapters（僅過渡期）

不應再成為 legacy/backtest/live/utility 混合入口的總出口。

### `src/agents`

重新定位為治理層與過渡兼容層。

長期保留：

- OpenClaw / allocator / governance policy agents

長期移出或刪除：

- `CryptoTradingAgent`
- `SportsTradingAgent`
- `PoliticsTradingAgent`
- 其他直接產生 live order 的 pull-based trading runtimes

### `src/platform`

重新定位為 execution/shared contracts supporting layer，而不是另一套 live strategy runtime。

長期保留：

- risk gate
- queue / router / position aggregation
- core order/domain types（若仍為 canonical）

長期退役：

- `platform/agents/*`
- `DomainAgent` 作為新 live strategy extension point 的角色

### `src/coordinator/bootstrap.rs`

重新定位為純裝配層。

長期保留：

- 讀 config / deployment
- 建立 coordinator/runtime/persistence wiring
- 啟停與 shutdown orchestration

長期移除：

- strategy-name classification
- strategy-specific runtime config generation
- strategy-specific spawn branches
- strategy-specific observability special cases

## Migration Strategy

### Phase 1: Freeze Boundaries

- 宣布 `Strategy` + coordinator path 為 canonical live path
- 停止新增 `TradingAgent` / `DomainAgent` live strategy
- 在 docs 與 module comments 中標記 transitional surfaces

### Phase 2: Extract Canonical Strategy Runtime

- 把 `run_managed_strategy_runtime` 從 bootstrap 移到正式 runtime 模組
- 讓 bootstrap 改為透過 registry 啟動 strategy runtime
- 不再內嵌 strategy-specific spawn logic

### Phase 3: Separate Governance Plane

- 把 OpenClaw 從 trading runtime 身份改成治理層
- 新增 governance-only context/interface
- 把策略自帶 pause/risk ownership 移出到治理層 policy

### Phase 4: Migrate Existing Live Strategies

優先順序：

1. `split_arb` / `staggered_arb`
2. `momentum`
3. `pattern_memory`
4. `lob_ml`
5. `rl_policy`
6. `event_edge`
7. `nba_comeback`

前五個先證明 canonical runtime 可以涵蓋 deterministic 與 model-driven 策略；後兩個再證明事件驅動策略也能被同一路徑吸收。

### Phase 5: Retire Parallel Runtimes

- 刪除或降級 `src/platform/agents/*`
- 刪除或降級 `src/agents/{crypto,sports,politics,...}`
- 刪除 bootstrap 內與策略名稱綁定的 special-case branches

## Non-Goals

本次重構不追求：

- 一次重寫所有策略邏輯
- 先優化單個策略 alpha
- 把 OpenClaw 變成逐筆交易審批器
- 在重構初期大改 execution engine 核心成交邏輯

## Risks

### 1) 遷移期雙軌風險

如果 canonical runtime 沒有先宣布，團隊會在重構期間繼續往舊接口加碼，造成第四套接口或更多橋接層。

### 2) Governance 與 Strategy 再次混層

如果只是把 OpenClaw 從一個模組搬到另一個模組，但仍保留 `submit_order`，那只是換名字，不是分層。

### 3) Bootstrap 回潮

如果 strategy registry/factory 沒有真正吸收策略分類與 runtime wiring，`bootstrap.rs` 很快又會長回特判中心。

## Acceptance Criteria

1. 所有新 live 策略只實作 canonical strategy contract。
2. 所有 live orders 都只經過 coordinator ingestion path。
3. OpenClaw 類治理 agent 不再直接提交交易訂單。
4. `bootstrap.rs` 不再按策略名分支啟動不同 runtime。
5. 策略接口不再擁有治理與 feed lifecycle 權限。
6. `event_edge` 與 `nba_comeback` 也能落入同一條 canonical strategy runtime。
