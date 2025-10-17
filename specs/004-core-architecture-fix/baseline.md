# Phase 1 Baseline (T001-T002)

## 工具環境 (T001)

| 工具 | 版本 | 備註 |
|------|------|------|
| `rustc` | `1.88.0 (6b00bc388 2025-06-23)` | `rustup` 1.28.2 管理 |
| `cargo-tarpaulin` | `0.33.0` | `cargo install cargo-tarpaulin` |
| `cargo-flamegraph` | `0.6.9` | 由 `flamegraph` crate 提供 |
| `cargo-criterion` | `1.1.0` | Criterion CLI (替代 `cargo install criterion`) |
| `bpftrace` | N/A | macOS 環境無原生套件，後續需於 Linux 節點驗證 |

## 性能基線 (T002)

### 1. 市場熱路徑基準
- 指令：`cargo test --release --bench hotpath_latency_p99 -- --nocapture`
- 結果：`端到端 p99 ≈ 93.1 ms`（16383 樣本）
  - P50 45.2 ms / P95 88.4 ms / P99 93.1 ms
  - Idle spins: 0，Tick 數 16,383

### 2. 策略動態派發
- 新增 `market-core/engine/benches/strategy_dispatch.rs`
- 指令：`cargo bench -p hft-engine --bench strategy_dispatch -- --quick`
- Criterion 報告：`strategy_dispatch_box_dyn` 時間介於 **7.99 µs ~ 8.21 µs**（32 支策略，每回合呼叫 32 次）
  - 換算單次 dispatch 約 **0.25 µs**
  - 結果低於規劃預估的 2–5 µs，推測因測試資料僅含 `Disconnect` 事件與極簡策略，後續 enum 化後需於真實負載再驗證

### 3. Engine 體積
- 指令：`wc -l market-core/engine/src/lib.rs`
  - 行數：**1381 行**
- 指令：自定腳本計算欄位數
  - 結果：`Engine` 結構目前 **26 個欄位**

## 待辦
- Linux 節點安裝 `bpftrace`（或於 CI 中覆核）
- 待 Phase 2/US2 完成後，於相同基準重新測量 enum dispatch 與熱路徑延遲
