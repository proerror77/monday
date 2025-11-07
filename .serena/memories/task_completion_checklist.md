# 任務完成檢查清單

## 代碼修改後的必要步驟

### 1. 代碼質量檢查
```bash
# 格式化代碼
cargo fmt

# 運行 Clippy 檢查
cargo clippy --workspace -- -D warnings

# 檢查編譯
cargo check --workspace
```

### 2. 測試驗證
```bash
# 運行單元測試
cargo test --workspace

# 運行集成測試（如果修改了關鍵模塊）
cargo test --workspace --test integration

# 運行特定模塊測試
cargo test --package rust_hft --lib exchanges
```

### 3. 性能驗證（針對性能關鍵修改）
```bash
# 運行基準測試
cargo bench --bench orderbook_benchmarks

# 運行延遲測試
cargo bench --bench latency_benchmarks

# 比較修改前後的性能
cargo bench -- --save-baseline before_fix
# 修改代碼後
cargo bench -- --baseline before_fix
```

### 4. 端到端測試
```bash
# 運行完整測試套件
./scripts/run_full_test_suite.sh

# 運行數據庫壓力測試
./scripts/run_comprehensive_db_test.sh
```

### 5. 記錄和文檔
- 更新相關文檔（如果修改了公共API）
- 記錄性能改進數據
- 更新 CHANGELOG.md（如果是重要修改）

## 特別注意事項

### 性能關鍵修改
如果修改涉及以下模塊，必須運行性能測試：
- `exchanges/mod.rs` (ExchangeManager)
- `exchanges/event_hub.rs` (EventHub)
- `integrations/redis_bridge.rs` (RedisBridge)
- `core/orderbook.rs` 系列
- `engine/complete_oms.rs`

### 並發安全修改
如果修改涉及以下內容，必須運行並發測試：
- 鎖的使用（RwLock, Mutex）
- 無鎖數據結構（dashmap, crossbeam）
- 異步代碼中的共享狀態

### 網絡相關修改
如果修改涉及網絡組件，測試：
- WebSocket 連接穩定性
- Redis 連接管理
- 重連邏輯