# Feature Specification: Health & Metrics Endpoints

**Feature Branch**: `[001-metrics-healthz]`  
**Created**: 2025-10-08  
**Status**: Draft  
**Input**: User description: "為 apps/{live,paper,replay} 提供 /healthz 與 /metrics（Prometheus）端點，涵蓋引擎延遲、吞吐、回壓/丟棄、訂單與風控事件，與現有憲章一致且不影響熱路徑。"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - 查詢健康狀態（Liveness/Readiness）(Priority: P1)

運維人員或自動化平台可呼叫 `/healthz` 得到即時健康狀態與關鍵子系統（引擎、風控、網路、存儲）可用性，作為部署/回滾與告警依據。

**Why this priority**: 偵測故障與回滾決策的最低需求，必須先具備。

**Independent Test**: 於單機與壓力測試環境分別呼叫 `/healthz`，驗證在子系統異常時返回非 200 與細節原因。

**Acceptance Scenarios**:
1. Given 引擎/風控正常，When GET `/healthz`，Then 200 且 JSON 含 ok=true 與各子系統狀態。
2. Given 交易所連線中斷，When GET `/healthz`，Then 狀態為 Degraded 且原因含 network/exchange。

---

### User Story 2 - 匯出 Prometheus 指標 (Priority: P1)

監控系統以 `/metrics` 抓取延遲、吞吐、回壓/丟棄、訂單/風控事件等統計，對照憲章中的命名與品質門檻。

**Why this priority**: 滿足 SLO/告警與效能回歸監控。

**Independent Test**: 啟動應用後，Prometheus 抓取 `/metrics`，並在 Grafana 上顯示正確的延遲直方圖與吞吐曲線。

**Acceptance Scenarios**:
1. Given 應用啟動，When GET `/metrics`，Then 可取得 `hft_latency_*` 與 `hft_throughput_*`、`hft_drops_total` 指標系列。
2. Given 熱路徑壓測，When 持續抓取 `/metrics`，Then 不引入明顯延遲回歸（和主幹比較≤2%）。

---

### User Story 3 - 熱路徑零額外負擔 (Priority: P2)

指標匯集採用離線聚合/批次方式，避免在引擎熱路徑進行動態配置或重型序列化。

**Why this priority**: 直接關聯低延遲與吞吐目標。

**Independent Test**: microbench 對比開關 Metrics 後之引擎 Hotpath；回歸≤2%。

**Acceptance Scenarios**:
1. Given 熱路徑運作，When 開啟指標，Then 基準測試結果顯示 p99 延遲回歸 ≤2%。
2. Given 高流量，When 指標收集，Then 無大量配置或鎖競爭熱點。

---

### Edge Cases

- 在無 Prometheus 的環境，`/metrics` 應可直接被 curl 檢查且輸出正確格式。
- 系統時間不同步時，延遲與事件時間戳應避免負值/錯亂（以單一時鐘來源計算）。
- 高壓測下 `/metrics` 不應導致 OOM 或顯著 GC/allocator 壓力。

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: 系統 MUST 暴露 `/healthz` 回報 Liveness/Readiness 與子系統（引擎、風控、網路、存儲）狀態。
- **FR-002**: 系統 MUST 暴露 `/metrics`（Prometheus 文本格式）含延遲分佈、吞吐、回壓/丟棄、訂單/風控事件統計。
- **FR-003**: `/metrics` 與 `/healthz` MUST 適用於 `apps/live`, `apps/paper`, `apps/replay`。
- **FR-004**: 指標命名 MUST 與憲章一致（`hft_latency_*`, `hft_throughput_*`, `hft_drops_total`, 等）。
- **FR-005**: 指標收集 MUST 不在熱路徑進行動態配置與鎖競爭；採批次/快照輸出。
- **FR-006**: 當任一關鍵子系統故障，`/healthz` MUST 反映降級狀態與原因碼。

### Key Entities *(include if feature involves data)*

- 指標系列（Metrics Families）: latency、throughput、drops/backpressure、orders、risk。
- 健康檢查項（Health Checks）: engine、risk-control、network/exchange、storage。

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: `/healthz` 平均回應 < 2ms；故障時返回非 200 並含原因。
- **SC-002**: `/metrics` 包含所有憲章要求之核心指標系列，名稱/label 一致。
- **SC-003**: 熱路徑延遲回歸 ≤ 2%（與主分支/基準對照）。
- **SC-004**: 端到端 p99 延遲達成憲章目標；壓測期間無 OOM 與不可接受 GC/allocator 壓力。

