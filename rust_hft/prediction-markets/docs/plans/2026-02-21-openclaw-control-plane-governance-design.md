# OpenClaw Control-Plane Governance Design

Date: 2026-02-21

## Goal

建立清晰分層，讓高頻交易不依賴 OpenClaw 逐筆決策：

- Poly Agent：策略判斷與下單意圖生成
- Ploy Coordinator：唯一交易入口、資金與風控閘門、執行與審計
- OpenClaw：控制面治理（資金配置、部署啟停、全局熔斷）

## Why This Change

現況存在策略層、平台層、協調層責任重疊，造成：

- 下單責任邊界不清楚
- 資金治理沒有唯一權威入口
- 容易把 OpenClaw 放進每筆交易同步路徑，拉高延遲與耦合風險

## Architecture Decision

採用「控制面闸门」而非「逐筆審批」：

1. OpenClaw 不進每筆 order 的同步鏈路。
2. OpenClaw 下發全局治理策略（policy），由 Ploy 本地強制執行。
3. Poly Agent 按策略自主下單（intent），但必須經過 Coordinator gate 才能執行。

## Target Boundaries

### 1) Poly Agent (Strategy Plane)

- 負責：方向、時機、價格、倉位建議（策略層）
- 不負責：全局資金上限、跨策略總風險、全局熔斷

### 2) Ploy Coordinator (Execution Plane)

- 唯一 ingestion：`OrderIntent -> RiskGate -> Queue -> Executor`
- 強制 gate：
  - deployment gate（已有）
  - global governance policy gate（新增）
- 審計：所有 block/pass/execute 都可追蹤

### 3) OpenClaw (Control Plane)

- 負責：
  - 全局資金治理（總曝險、單筆上限、域別封鎖）
  - 部署控制（enable/disable）
  - 熔斷（block new intents）
- 不負責：
  - 策略內逐筆進出場判斷

## Phase Plan

### Phase 1 (this PR)

- 引入 `Global Governance Policy` 到 Coordinator
- 新增 API 供 OpenClaw 查詢/更新 policy
- 在 intent ingestion 路徑實施 policy gate（block before risk queue）

### Phase 2

- 加入 Agent Base 資金台帳（deployment/account 維度）
- 提供 governance 狀態與資金占用觀測 API

### Phase 3

- 清理 legacy 雙軌路徑，收斂到單一 execution path
- 補齊 dashboard（資金配額、熔斷狀態、策略健康）

## Acceptance Criteria

1. OpenClaw 可在不進逐筆鏈路前提下控制全局風險。
2. 任一 intent 被擋下時可給出明確 policy reason。
3. Poly Agent 在 policy 允許範圍內保持自主高頻執行。
4. Coordinator 成為唯一可審計的 live 下單入口。

## CI Note

GitHub Actions 的 Rust toolchain step 請使用 `dtolnay/rust-toolchain`（`dtolnay/rust-action` 已不可用）。
