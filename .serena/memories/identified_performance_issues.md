# 已識別的性能問題

## High-Impact 問題

### 1. ExchangeManager 鎖粒度/await 問題
**文件**: `rust_hft/src/exchanges/mod.rs`
**位置**: `ExchangeManager::select_best_exchange` 方法 (Line 301)

**問題描述**:
- 持有 `instances.write().await` 寫鎖時，在迭代中多次調用 await 操作
- 在選擇算法中可能調用 `exchange.get_info()/is_healthy()` 等異步方法
- 易造成阻塞甚至潛在死鎖，放大寫鎖搶佔時間

**修復方案**:
1. 先用 `read` 鎖快照候選引用
2. 釋放鎖後再 await 採樣
3. 選擇完成後用一次短暫 `write` 鎖更新 `last_selected`
4. 或使用 `DashMap` 拆分降低大鎖粒度

### 2. Event Hub 廣播架構與熱路徑鎖
**文件**: `rust_hft/src/exchanges/event_hub.rs`

**問題描述**:
- 為每個訂閱者建立 `broadcast::Sender` 並遍歷廣播
- 每次事件更新都用 `Mutex` 更新統計，包含 `Instant` 讀寫
- 屬於熱路徑重鎖問題

**關鍵 Bug**:
- Line 301, 409: `stats_guard.last_event_time = Some(Instant::now())`
- 但 `last_event_time` 型別為 `Option<u64>`，造成型別不匹配

**修復方案**:
1. 單一 `broadcast::Sender<Arc<Event>>` 做一次 fan-out
2. 統計改為 `AtomicU64` 與取樣更新
3. 修正時間欄位型別不一致問題
4. 避免在熱路徑打 `info!/warn!` 日誌

### 3. RedisBridge 連線重複建立與序列化成本
**文件**: `rust_hft/src/integrations/redis_bridge.rs`

**問題描述**:
- Lines 39, 70, 92, 118: 每次 `publish_*` 都調用 `get_multiplexed_async_connection().await`
- Lines 41, 72, 94, 120: 使用 `serde_json::json!` 建構字串，包含大量 `to_string()`

**修復方案**:
1. 在 `RedisBridge` 持有長壽命 `MultiplexedConnection`
2. 所有 publish 復用連線
3. JSON 序列化採事先分配緩衝或 `serde_json::to_writer`
4. 使用 pipeline 批次發送

## Medium-Impact 問題

### 4. OMS 訂單與風控共享結構鎖競爭
**文件**: `rust_hft/src/engine/complete_oms.rs`

**問題描述**:
- `orders`、`pending_orders`、`risk_managers` 皆為 `RwLock<HashMap<...>>`
- 多任務更新時易競爭，部分流程中頻繁 acquire `write()`

**修復方案**:
1. 熱路徑改用 `DashMap`
2. 事件合併/統計用 `Atomic*` 或批次聚合
3. `uuid`、`chrono` 生成移出熱路徑

### 5. 時間 API 選擇優化
**問題描述**:
- 熱路徑中過度使用 `SystemTime`/`chrono` 進行時間轉換

**修復方案**:
- 熱路徑用 `std::time::Instant` 做相對時延與指標
- 只在對外序列化時再轉 `SystemTime`/epoch
- 避免 `chrono`/字串格式化在關鍵迴圈中

## 優先修復順序

1. **修正 event_hub 時間欄位型別** - 阻止編譯錯誤
2. **重構 ExchangeManager 鎖/await 模式** - 穩定性改善
3. **RedisBridge 復用連線** - 立即性能提升
4. **EventHub 統計改為 Atomic** - 熱路徑優化
5. **OMS 改用 DashMap** - 併發性能提升