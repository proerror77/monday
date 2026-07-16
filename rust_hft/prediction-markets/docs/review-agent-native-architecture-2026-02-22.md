# Agent-Native Architecture Review (2026-02-22)

## Scope

Review target: current platform runtime (`Coordinator + agents + sidecar/governance API`) with OpenClaw as control plane.

## Current Layering Verdict

結論：方向是對的，但還有兩個結構性缺口要持續收斂。

- 上層（AI 調度 / OpenClaw）：透過 governance + deployments + system control API 做治理。
- 中層（Execution Plane / Coordinator）：唯一 live 下單入口，負責 gate/queue/risk/execution/audit。
- 下層（Rust adapters/executor）：與 Polymarket/CLOB/DB/WS 互動，純執行與資料管線。

這個分層已經符合「OpenClaw 不逐筆審批，Poly Agent 自主決策」的原則。

## Agent-Native Scorecard

| Principle | Score | Status | Notes |
|---|---:|---|---|
| Action Parity | 82% | ✅ | 控制面核心動作（部署、治理、pause/resume/halt、intent ingress）可 API 化 |
| Tools as Primitives | 81% | ⚠️ | `main.rs` 已拆入口，live primitive 清楚；legacy runtime 尚待進一步拆 bins |
| Context Injection | 76% | ⚠️ | `strategies/control` 已含 lifecycle/version + latest evidence；策略優化 prompt context 仍待集中 |
| Shared Workspace | 90% | ✅ | 控制面與 runtime 共享部署矩陣與 strategy evaluations 證據檔 |
| CRUD Completeness | 87% | ✅ | Deployments/Governance + strategies control 已有讀與定向更新；已補策略 version/lifecycle 契約 |
| UI Integration | 74% | ⚠️ | WebSocket + API 可觀測；控制面全景在 UI 還不完整 |
| Capability Discovery | 80% | ⚠️ | `/api/capabilities` 已揭示 lifecycle gate 與 strategy evaluations surface |
| Prompt-Native Features | 70% | ⚠️ | 已支持 AI sidecar 調度，但策略行為仍大量硬編碼在 Rust agent |

**Overall: 84% (Partial+, architecture is viable for staged production).**

## What Was Fixed In This Pass

1. `CoordinatorHandle::force_close_domain` / `shutdown_domain` 現在會先即時把 domain ingress 設為 `halted`，避免命令傳遞延遲期間繼續吃 BUY intents。
2. `GET /api/governance/status` 擴展為調度友好快照：
   - `domain_ingress_modes[]`
   - `agents[]`（含 heartbeat/exposure/pnl/error）
3. 補上對應回歸測試（domain halt 即時生效 + governance status 新欄位）。
4. `StrategyDeployment` 補上控制面契約欄位：`strategy_version` / `lifecycle_stage` / `product_type` / `last_evaluated_at` / `last_evaluation_score`。
5. `GET/PUT /api/strategies/control*` 可讀寫 lifecycle/version，且 sidecar live ingress 預設只接受 `lifecycle_stage=live`。
6. `src/main.rs` 已拆成薄入口，legacy runtime 移到 `src/main_legacy.rs`，避免入口層持續膨脹。
7. 新增 `strategy_evaluations` 證據層（`GET/POST /api/strategy-evaluations*`），可追溯 backtest/paper/live 評估來源與雜湊。
8. 新增 canonical namespace `crate::agent_system::{ai,runtime,legacy_platform}`，開始收斂 `agent/agents/platform/agents` 混用。

## Remaining High-Impact Gaps

1. `src/main_legacy.rs` 仍大，下一步應按 command domain 再拆 `legacy_*` bins。
2. canonical namespace 已建，但舊 import 路徑仍大量存在，需分批遷移與 lint gate。
3. strategy evaluations 目前先用檔案持久化；下一步應升級 DB schema 與 query index。

## Recommended Next Refactors (ordered)

1. 把 `src/main_legacy.rs` 按 `crypto/sports/politics/analysis` 拆成 `bin/legacy_*`，入口僅保留 dispatcher。
2. 以 `agent_system` 為唯一對外 import，逐步替換舊路徑並加 deny list。
3. 把 strategy evaluations 從 JSON state 升級到 PostgreSQL（含 stage/version/deployment 複合索引）。
