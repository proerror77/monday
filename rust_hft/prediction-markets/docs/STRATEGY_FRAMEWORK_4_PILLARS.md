# 四大策略框架（Repo 對照 + 實作狀態）

本文件把你提出的「四大策略」框架，對照到目前 `ploy` repo 的真實模組、指令入口、配置點，並標記已完成/缺口與下一步。

Status note (2026-05-24): this is a historical strategy taxonomy and should
not be used as the active runtime or promotion runbook. Current runtime work
flows through `new-ployd`, `ployctl`, `new-ploy-runner`, strategy TOML configs,
and the artifact-backed research workflows described in
`docs/runbooks/strategy-research-cicd.md`. Legacy `src/strategy/*` and
`ploy ...` CLI examples below are compatibility references unless a newer
runbook explicitly re-promotes them.

---

## 0) TL;DR

- 你列的四大策略 **在 repo 內都有對應落地**，而且 live runtime 現在已經明確朝 **四層分工** 收斂：
  - Strategy Plane
  - Capital Governance Plane
  - Execution Plane
  - Control Plane
- `src/strategy/*` 已是 canonical live strategy path；`TradingAgent` / `DomainAgent` 已退休。
- 平台啟動面已不再保留 compatibility crypto/sports live runtimes 或對應 env gate。
- 本文件的目標是把「策略分類」變成可追蹤的工程邊界：每一類策略要能回答：
  - 入口在哪（CLI / service / agent）
  - 核心邏輯在哪（module）
  - 資料來源與風控在哪（adapters / coordinator / config）
  - 還缺什麼（backlog）

---

## 1) 四大策略：Repo 對照表

### 1.1 事件驅動策略（Event-Driven）

核心：利用「資訊發布/外部狀態更新」與市場反應的時間差。

- 主要實作
  - `src/strategy/event_edge/mod.rs`
  - `src/strategy/event_edge/core.rs`
  - `src/strategy/event_models/arena_text.rs`（外部資料源：Arena leaderboard）
- 主要入口
  - CLI：`ploy event-edge ...`（Legacy CLI，見 `src/main.rs` 的 `run_event_edge_mode`）
  - 常駐背景 agent：舊單體 runtime 啟動路徑已移除；`[event_edge_agent]` 只保留為歷史配置語境，不是目前預設 runtime surface
  - Canonical runtime：`src/strategy/event_edge/strategy.rs`（registered strategy plugin through the managed strategy runtime）
- 已完成
  - `p_true` 估計 → `edge/EV` → 下單（可 dry-run）
- 缺口/下一步
  - 外部資料源目前幾乎只有 Arena（想覆蓋「選舉/監管事件」需新增 `event_models/*`）
  - 擴充更多事件資料源與 discovery path

### 1.2 套利策略（Arbitrage）

核心：跨市場/跨時間/跨組合定價不一致。

- 主要實作
  - 時間套利（分時對沖）：`src/strategy/split_arb.rs`
  - 兩腿套利狀態機（time-bounded binary）：`src/strategy/engine.rs`
  - 多結果/單調性/拆合套利/EV：`src/strategy/multi_outcome.rs`
  - 波動套利：`src/strategy/volatility_arb.rs`
- 跨市場（傳統博彩）相關
  - Odds 來源：`src/ai_clients/odds_provider.rs`（The Odds API：DraftKings/FanDuel...）
  - 比較/edge：`src/ai_clients/sports_analyst.rs`（含 DK 對照）
- 已完成
  - Polymarket 內部套利（split/two-leg/multi-outcome）能力完整
  - Sportsbook odds 拉取與 bookmaker 間 arb 偵測（The Odds API）
- 缺口/下一步
  - 「Polymarket ↔ Sportsbook」套利目前偏 **偵測/比較**，尚未有可執行的雙邊對沖 execution layer（下單、對帳、風控、失敗補救）

### 1.3 動量策略（Momentum）

核心：新聞/趨勢驅動、突破，利用 lead-lag 或趨勢延續。

- 主要實作（Legacy 高級版）
  - `src/strategy/momentum.rs`（multi-timeframe momentum、vol 調整、OBI、time decay、dynamic sizing）
  - Binance trade feed：`src/adapters/binance_ws.rs`
- 主要實作（StrategyManager/新架構簡化版）
  - `src/strategy/detectors/momentum.rs`（moving average / trend）
  - `src/strategy/strategies/momentum_strat.rs`（Strategy trait 版 Momentum）
- 已完成
  - Moving average / 趨勢偵測（detector）
  - 多時間框架動量（10s/30s/60s 加權）
  - OBI（order book imbalance）確認（見 `src/strategy/momentum.rs`、`src/strategy/volatility.rs` 等）
- 本次實作（從 2026-02-14 起）
  - Binance trade quantity 納入快取 + VWAP 計算（見 `src/adapters/binance_ws.rs`）
  - Momentum 策略加入可選 VWAP 確認條件（見 `src/strategy/momentum.rs`）
  - CLI（legacy momentum）可用參數：
    - `--vwap-confirm`
    - `--vwap-lookback <secs>`（預設 60）
    - `--vwap-min-dev <pct>`（預設 0.0；例如 0.1 代表 0.1%）

範例：

```bash
# 需要 spot 在 VWAP 之上（UP）或之下（DOWN）才允許進場
ploy momentum --symbols BTCUSDT,ETHUSDT --vwap-confirm --vwap-lookback 60 --vwap-min-dev 0.1
```

### 1.4 信息優勢策略（Information Advantage）

核心：Tier 1 一手、Tier 2 專業模型、Tier 3 市場微結構/情緒。

- Tier 1：一手/鏈上/原始流
  - `src/adapters/onchain_indexer.rs`（OrderFilled、whale tracking）
  - Polymarket CLOB/WS adapters（`src/adapters/polymarket_*.rs`）
- Tier 2：專業/模型
  - NBA winprob：`src/strategy/nba_winprob.rs`
  - Odds/博彩：`src/ai_clients/odds_provider.rs`
- Tier 3：市場資訊/情緒
  - Grok/X：`src/ai_clients/grok.rs`
  - 多源資料聚合：`src/ai_clients/sports_data.rs`（品質分數、降級、快取）
  - OBI/深度不平衡：`src/strategy/momentum.rs`、`src/strategy/nba_filters.rs` 等

---

## 2) 執行框架現況（目前的穩態）

目前 repo 的穩態已經不是「3 套框架並存」，而是：

1. `src/strategy/*` + coordinator managed runtime = 正式 live path
2. `src/agents/*` = governance-only surface
3. `src/platform/*` = execution / shared contracts，不再承載另一套 live strategy runtime

具體來說：

- `TradingAgent` / `DomainAgent` 已退休，不再是 live 策略入口
- sports / politics live path 只走 canonical managed runtime
- crypto live path 只走 canonical runtime

所以今天最大的架構現實，不是「策略分散在三套正式 runtime」，而是：
canonical runtime 已經確立，舊 trait / platform-agent 層也已經退休；剩下的是 plugin / account / order plane 的持續產品化。

### 2.1 讀側可見性

現在 API 已經可以直接把 plugin/account lifecycle 暴露給 operator：

- `GET /api/system/capabilities`
  - deployment state counts：`enabled|draining|disabled|archived`
  - scoped deployment state counts（account + dry_run scope）
  - builtin plugin summaries（`plugin_id` / `kind` / `domain` / `version`）
- `GET /api/system/accounts`
  - per-account deployment state counts
  - runtime budget snapshot

這表示 `draining` 不再只是 runtime 內部默契，而是正式可觀測的 control-plane 狀態。

---

## 3) Backlog（建議的下一步工程化）

1. 把四大策略類別寫入 Event Registry 的 `strategy_hint`（或新增欄位），讓 discover→research→monitor→trade 能按策略分類聚合。
2. 將 EventEdge / NBA comeback 變成 StrategyManager 的一級 Strategy（統一啟停、狀態、metrics）。
3. 擴充 event_models：選舉/體育/監管類事件（替代目前偏單一的 Arena）。
4. 跨市場套利（Polymarket ↔ Sportsbook）：先做「機會偵測 + 人工對沖操作手冊」，再評估自動化執行風險與合規。
