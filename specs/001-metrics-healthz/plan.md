# Implementation Plan: Health & Metrics Endpoints

**Branch**: `[001-metrics-healthz]` | **Date**: 2025-10-08 | **Spec**: specs/001-metrics-healthz/spec.md
**Input**: Feature specification from `/specs/001-metrics-healthz/spec.md`

## Summary

在 apps/{live,paper,replay} 暴露 `/healthz` 與 `/metrics`。指標資料由非熱路徑的聚合器匯總，熱路徑只做最小原子累加或 lock-free 計數。遵循憲章命名與品質門檻，避免對引擎延遲造成 >2% 回歸。

## Technical Context

**Language/Version**: Rust stable (工作區與憲章一致)  
**Primary Dependencies**: `tracing`、`prometheus`（或同等）、`tokio`、最小 HTTP 暴露（例如 hyper/axum/mini server，依現有 apps 架構）  
**Storage**: N/A（記憶體聚合，無持久化需求）  
**Testing**: `cargo test`、整合測試（/metrics 正確性）、criterion microbench（熱路徑回歸檢查）  
**Target Platform**: Linux 低延遲部署環境  
**Performance Goals**: `/healthz` < 2ms；/metrics 抓取不引起明顯回歸；熱路徑回歸 ≤ 2%  
**Constraints**: 熱路徑零配置、嚴控鎖競爭；以批次/快照方式輸出  
**Scale/Scope**: 適用於 live/paper/replay 三種應用

## Constitution Check

- MUST：熱路徑零額外配置與嚴控鎖；無阻塞 I/O；遵循 `HftError/HftResult` 與 domain newtypes 邊界；觀測性使用統一命名。
- MUST：fmt/clippy 全通過（核心 crates 零警告）。
- MUST：criterion 基準與主分支對照回歸≤2%。

## Project Structure

### Documentation (this feature)

```
specs/001-metrics-healthz/
├── spec.md
├── plan.md
├── quickstart.md        # 可選：端點測試與抓取範例
├── data-model.md        # 可選：指標 families 與 label 介面
└── tasks.md
```

### Source Code (repository root)

```
infra-services/core/metrics/
├── src/
│   ├── registry.rs         # 指標註冊與聚合介面
│   ├── families.rs         # 指標族群與命名映射
│   └── export.rs           # Prometheus 文本輸出
└── Cargo.toml

apps/live/
├── src/main.rs            # 初始化 tracing、metrics registry、HTTP 端點
└── ...

apps/paper/
apps/replay/

tests/integration/
└── metrics_endpoints.rs   # /metrics 與 /healthz 整合測試
```

**Structure Decision**: 採 infra-services/core/metrics 為單一來源；各 app 僅負責啟動時注入與暴露端點。

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| N/A |  |  |

