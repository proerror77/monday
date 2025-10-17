# Quick Start: Core Architecture Fix

**Feature**: 004-core-architecture-fix
**Target**: 25μs p99 延遲 HFT 系統重構

---

## Prerequisites

```bash
# Rust toolchain
rustup --version  # 應為 1.75+

# 依賴工具
cargo install cargo-tarpaulin  # 測試覆蓋率
cargo install cargo-flamegraph # 性能分析
cargo install criterion        # 基準測試

# eBPF 工具 (Linux only)
sudo apt install bpftrace      # 網路延遲測量
```

---

## Phase 1: 測量與熱路徑修復 (48h)

### 1.1. 驗證當前性能基線

```bash
# 編譯 benchmark
cargo bench --bench hotpath_latency_p99

# 預期輸出
# StrategyDispatch/Box<dyn>  time: [2.345 μs 2.389 μs 2.431 μs]
```

### 1.2. 實現 WsFrameMetrics

```bash
# 創建 hft-core/src/latency.rs (如果不存在)
cargo check -p hft-core

# 驗證新增的 LatencyStage enum
cargo test -p hft-core --test latency_stage_test
```

### 1.3. 重構 Strategy 為 enum

```bash
# 修改 market-core/engine/src/lib.rs
# 運行對比 benchmark
cargo bench --bench strategy_dispatch

# 預期改進
# StrategyDispatch/enum  time: [45.2 ns 47.1 ns 49.3 ns]  # < 50ns 目標達成
```

### 1.4. 添加 Feature Gates 互斥檢查

```bash
# 在 workspace Cargo.toml 添加檢查後
cargo build --features json-std,json-simd  # 應編譯失敗

# 預期錯誤
# error: Features 'json-std' and 'json-simd' are mutually exclusive
```

### 1.5. 驗證 Phase 1 成功標準

```bash
# SC-001: WS frame 到 parsing 延遲可測量
cargo test --test ws_frame_latency_test

# SC-002: Prometheus metrics lag < 1s
cargo run --bin live --features metrics &
curl http://localhost:9090/metrics | grep latency_sync_lag_ms  # < 1000

# SC-003: Strategy dispatch < 50ns
cargo bench --bench strategy_dispatch | grep "time:"

# SC-004: Feature gates 互斥檢查
cargo build --features json-std,json-simd  # 必須失敗
```

---

## Phase 2: 架構重構 (2w)

### 2.1. 創建新 Crates

```bash
# 創建 4 個核心 crate
cargo new --lib hft-integration
cargo new --lib hft-testing
cargo new --lib hft-data
cargo new --lib hft-execution

# 驗證依賴圖
cargo tree -p market-core-engine
```

### 2.2. 重構 Engine 結構體

```bash
# 檢查 Engine 字段數
rg "pub struct Engine" -A 20 market-core/engine/src/lib.rs | wc -l  # < 10

# 檢查代碼行數
wc -l market-core/engine/src/lib.rs  # < 400
```

### 2.3. 驗證模組分離

```bash
# 單獨測試 IngestionPipeline
cargo test -p market-core-engine --test ingestion_backpressure

# 單獨測試 StrategyRouter
cargo test -p market-core-engine --test venue_filtering

# 單獨測試 RiskGate
cargo test -p market-core-engine --test risk_validation
```

### 2.4. 運行性能基準測試

```bash
# L1/Trades 延遲
cargo bench --bench l1_trades_latency
# 目標: p99 < 0.8ms

# L2/diff 延遲
cargo bench --bench l2_diff_latency
# 目標: p99 < 1.5ms
```

### 2.5. 測試覆蓋率檢查

```bash
# 生成覆蓋率報告
cargo tarpaulin --workspace --out Html

# 檢查報告
open tarpaulin-report.html
# 目標: 總覆蓋率 ≥ 80%
```

---

## Phase 3: 長期優化 (4w)

### 3.1. eBPF 端到端延遲測量

```bash
# 啟動 eBPF 追蹤 (需 root)
sudo bpftrace -e '
kprobe:tcp_sendmsg {
  @start[tid] = nsecs;
}
kretprobe:tcp_sendmsg /@start[tid]/ {
  @latency_ns = hist(nsecs - @start[tid]);
  delete(@start[tid]);
}
'

# 預期分佈
# [16K, 32K)  ████████████  # p50 約 20μs
# [32K, 64K)  ██████        # p99 約 50μs
```

### 3.2. 完整 Benchmark Suite

```bash
# 運行所有 8 個階段的 benchmark
cargo bench --bench hotpath_all_stages

# 檢查直方圖
cat target/criterion/*/base/estimates.json | jq '.mean.point_estimate'
```

### 3.3. 生產環境驗證

```bash
# 運行 7 天穩定性測試
cargo run --release --bin live --features metrics &
PID=$!

# 監控 7 天
for i in {1..168}; do  # 168 小時
  curl -s http://localhost:9090/metrics | grep -E "(exec_latency|pos_dd|uptime)"
  sleep 3600  # 每小時檢查
done

# 檢查是否有 panic 或 crash
kill -0 $PID && echo "✅ 7 天運行無回歸" || echo "❌ 進程已崩潰"
```

---

## 故障排查

### 問題 1: Benchmark 延遲超標

```bash
# 檢查是否使用 release 模式
cargo bench --verbose | grep "profile"  # 應顯示 release

# 檢查 CPU 頻率調度器
cat /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor
# 應全部為 "performance"

# 固定 CPU 頻率
sudo cpupower frequency-set -g performance
```

### 問題 2: Feature Gates 未生效

```bash
# 檢查 Cargo.lock
cargo tree --features json-std -i serde_json
cargo tree --features json-simd -i simd-json

# 確認互斥檢查代碼位置
rg "compile_error.*mutually exclusive" --type rust
```

### 問題 3: 測試覆蓋率不足

```bash
# 找出未覆蓋的模組
cargo tarpaulin --workspace --out Json | jq '.files[] | select(.coverage < 80)'

# 為特定模組補充測試
cargo test -p <crate-name> --test <missing-test>
```

### 問題 4: eBPF 無法啟動

```bash
# 檢查內核版本
uname -r  # 應 ≥ 4.9

# 檢查 BPF 功能
cat /boot/config-$(uname -r) | grep CONFIG_BPF

# 安裝缺失依賴
sudo apt install linux-headers-$(uname -r) bpfcc-tools
```

---

## 關鍵指標監控

### Prometheus 查詢範例

```promql
# 撮合延遲 p99
histogram_quantile(0.99, rate(hft_exec_latency_bucket[1m]))

# Metrics 同步延遲
max_over_time(latency_sync_lag_ms[30s])

# WS frame 到 parsing 延遲
histogram_quantile(0.99, rate(ws_frame_to_parsed_us_bucket[1m]))
```

### Grafana Dashboard Import

```bash
# 導入預設儀表板
curl -X POST http://localhost:3000/api/dashboards/db \
  -H "Content-Type: application/json" \
  -d @grafana/hft-core-dashboard.json
```

---

## 參考資料

- **Spec Document**: specs/004-core-architecture-fix/spec.md
- **Implementation Plan**: specs/004-core-architecture-fix/plan.md
- **Data Model**: specs/004-core-architecture-fix/data-model.md
- **Rust Performance Book**: https://nnethercote.github.io/perf-book/
- **eBPF Guide**: https://ebpf.io/what-is-ebpf/
