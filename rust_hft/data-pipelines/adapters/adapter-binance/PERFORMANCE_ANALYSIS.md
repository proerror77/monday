# Binance Adapter JSON 解析性能分析

**日期**: 2025-01-07
**測試環境**: Release build with optimization
**測試工具**: Criterion 0.5

## 執行摘要

通過 Criterion 基準測試對比了 `serde_json`（默認）和 `simd-json`（啟用 json-simd feature）的性能。

**關鍵發現**: 在當前架構下，SIMD JSON 實際上比默認的 serde_json **慢 30-47%**。

## 詳細測試結果

### 性能數據對比表

| 測試項目 | serde_json (默認) | simd-json | 性能差異 |
|---------|------------------|-----------|---------|
| **parse_depth_update** | 1.71 µs<br/>261.65 MiB/s | 2.30 µs<br/>195.20 MiB/s | +31.8% 更慢<br/>-24.9% 吞吐 |
| **parse_trade_event** | 1.25 µs<br/>165.27 MiB/s | 1.84 µs<br/>112.22 MiB/s | +47.0% 更慢<br/>-32.1% 吞吐 |
| **parse_kline_event** | 3.31 µs<br/>136.24 MiB/s | 4.78 µs<br/>94.31 MiB/s | +39.8% 更慢<br/>-30.8% 吞吐 |
| **parse_mixed_batch** | 6.78 µs<br/>162.94 MiB/s | 9.09 µs<br/>121.61 MiB/s | +39.3% 更慢<br/>-25.4% 吞吐 |

### 統計顯著性

所有測試結果的 p-value < 0.05，性能退化具有統計顯著性。

## 根本原因分析

### 問題定位

性能退化源於 `converter.rs` 中的 `parse_value()` 函數的實現：

```rust
fn parse_value<T: DeserializeOwned>(value: Value) -> HftResult<T> {
    #[cfg(feature = "json-simd")]
    {
        // 步驟 1: 將 OwnedValue 序列化回 JSON 字符串
        let json_str = simd_json::to_string(&value)
            .map_err(|e| HftError::Parse(format!("SIMD 序列化失敗: {}", e)))?;

        // 步驟 2: 將 JSON 字符串重新解析為目標類型
        let mut bytes = json_str.as_bytes().to_vec();
        simd_json::from_slice(&mut bytes)
            .map_err(|e| HftError::Parse(format!("SIMD 反序列化失敗: {}", e)))
    }
    #[cfg(not(feature = "json-simd"))]
    {
        // 直接從 Value 反序列化（高效）
        serde_json::from_value(value)
            .map_err(|e| HftError::Parse(format!("JSON 反序列化失敗: {}", e)))
    }
}
```

### 數據流分析

**當前流程（使用 StreamMessage 包裝器）**:

```
文本消息 (text)
    ↓
parse_json::<StreamMessage>(text)  // 解析為 StreamMessage { stream, data: Value }
    ↓
parse_value(data.clone())          // 從 Value 轉換為目標類型
    ↓
對於 simd-json:
    ├─> simd_json::to_string(&value)     // ❌ 序列化回字符串
    └─> simd_json::from_slice(&mut bytes) // ❌ 重新解析

對於 serde_json:
    └─> serde_json::from_value(value)     // ✅ 直接轉換（無額外開銷）
```

**性能開銷分析**:
- simd-json 路徑: 1 次解析 + 1 次序列化 + 1 次解析 = **3 個操作**
- serde_json 路徑: 1 次解析 + 0 次序列化 + 1 次轉換 = **2 個操作**

雙重序列化/反序列化完全抵消了 SIMD 的性能優勢。

### API 限制

**simd-json 的 API 限制**:
- ✅ 提供: `from_slice(&mut [u8]) -> T` - 從字節切片直接解析
- ✅ 提供: `to_string(&OwnedValue) -> String` - 序列化為字符串
- ❌ 缺失: `from_owned_value(OwnedValue) -> T` - 從 Value 直接反序列化

這個 API 缺口迫使我們使用低效的"重新序列化"變通方案。

## 優化方案

### 方案 1: 保持當前架構，使用默認解析器（推薦）

**決策**: 在當前架構下，禁用 json-simd feature，使用 serde_json

**理由**:
- 性能更優（快 30-47%）
- API 更完整（支持 from_value）
- 生態成熟穩定

**實施**:
```toml
# Cargo.toml
[features]
default = []  # 不包含 json-simd
# json-simd = ["dep:simd-json", "integration/json-simd"]  # 註釋掉
```

### 方案 2: 繞過 StreamMessage 包裝器（架構重構）

**目標**: 在熱路徑中避免 Value 中間層

**實施思路**:

```rust
// 方案 2A: 直接路由解析（當前已有的 parse_direct_message）
pub fn parse_stream_message(text: &str) -> HftResult<Option<MarketEvent>> {
    // 優先嘗試直接解析（繞過 StreamMessage）
    if let Ok(update) = Self::parse_json::<DepthUpdate>(text) {
        let book_update = Self::convert_depth_update(update)?;
        return Ok(Some(MarketEvent::Update(book_update)));
    }

    // ... 其他直接解析路徑

    // 兜底才使用 StreamMessage（僅用於未知格式識別）
    if let Ok(stream_msg) = Self::parse_json::<StreamMessage>(text) {
        return Self::process_stream_data(&stream_msg.stream, &stream_msg.data);
    }

    Ok(None)
}
```

**優勢**:
- 熱路徑使用 `parse_json()` 直接解析（1 次操作）
- 避免 Value 中間層
- SIMD 性能優勢得以發揮

**劣勢**:
- 需要重構消息路由邏輯
- 增加代碼複雜度

### 方案 3: 零拷貝 JSON 切片（高級優化）

**目標**: 使用字符串切片提取 `data` 字段，避免完整解析

**實施思路**:

```rust
fn extract_data_field(text: &str) -> HftResult<&str> {
    // 使用輕量級 JSON 掃描器找到 "data" 字段的起止位置
    // 返回該字段的字符串切片（零拷貝）
    // 然後直接在該切片上調用 parse_json()
}
```

**優勢**:
- 避免解析整個 StreamMessage
- 零拷貝提取
- SIMD 性能完全發揮

**劣勢**:
- 需要實現或集成輕量級 JSON 掃描器
- 增加維護成本
- 可能引入新的解析邊界情況

## 性能測試最佳實踐

### 測試設置

```bash
# 默認配置（serde_json）
cargo bench --bench json_parsing -p hft-data-adapter-binance

# SIMD 配置（simd-json）
cargo bench --bench json_parsing -p hft-data-adapter-binance --features json-simd
```

### 測試樣本

所有測試使用真實的 Binance WebSocket 消息樣本：
- 深度更新（depth update）: 448 bytes
- 交易事件（trade event）: 206 bytes
- K 線事件（kline event）: 451 bytes

### Criterion 配置

- 熱身時間: 3 秒
- 測量樣本: 100 個
- 統計方法: 自舉法（bootstrap）
- 吞吐量: 以 MiB/s 為單位

## 結論與建議

### 短期行動（即刻實施）

1. ✅ **保持默認使用 serde_json**
   - 當前架構下性能最優
   - 無需修改代碼

2. ✅ **優化消息路由邏輯**
   - 優先使用 `parse_direct_message()` 路徑
   - 減少 `StreamMessage` 包裝器的使用頻率

### 中期規劃（架構迭代）

3. 🔄 **重構熱路徑消息處理**
   - 直接解析到目標類型
   - 將 `StreamMessage` 降級為兜底路徑

4. 🔄 **條件化使用 SIMD**
   - 僅在大型消息（>1KB）時啟用
   - 小消息使用標準解析器

### 長期願景（性能極致）

5. 📋 **實現零拷貝 JSON 提取**
   - 集成輕量級 JSON 掃描器
   - 切片級別的字段提取

6. 📋 **建立性能監控體系**
   - CI/CD 集成性能回歸測試
   - 生產環境延遲指標追蹤

## 附錄：完整基準測試日誌

### 默認配置（serde_json）

```
parse_depth_update/parse
    time:   [1.7017 µs 1.7131 µs 1.7264 µs]
    thrpt:  [259.64 MiB/s 261.65 MiB/s 263.40 MiB/s]

parse_trade_event/parse
    time:   [1.2419 µs 1.2464 µs 1.2515 µs]
    thrpt:  [164.59 MiB/s 165.27 MiB/s 165.87 MiB/s]

parse_kline_event/parse
    time:   [3.2892 µs 3.3109 µs 3.3368 µs]
    thrpt:  [135.19 MiB/s 136.24 MiB/s 137.14 MiB/s]

parse_mixed_batch/parse_all
    time:   [6.6560 µs 6.7835 µs 6.9113 µs]
    thrpt:  [159.93 MiB/s 162.94 MiB/s 166.06 MiB/s]
```

### SIMD 配置（simd-json）

```
parse_depth_update/parse
    time:   [2.2735 µs 2.2963 µs 2.3215 µs]
    thrpt:  [193.08 MiB/s 195.20 MiB/s 197.15 MiB/s]
    change: [+30.423% +31.839% +33.234%] (p = 0.00 < 0.05)
    Performance has regressed.

parse_trade_event/parse
    time:   [1.8229 µs 1.8357 µs 1.8477 µs]
    thrpt:  [111.49 MiB/s 112.22 MiB/s 113.00 MiB/s]
    change: [+46.131% +47.031% +47.833%] (p = 0.00 < 0.05)
    Performance has regressed.

parse_kline_event/parse
    time:   [4.7107 µs 4.7829 µs 4.8600 µs]
    thrpt:  [92.817 MiB/s 94.313 MiB/s 95.759 MiB/s]
    change: [+38.085% +39.834% +41.682%] (p = 0.00 < 0.05)
    Performance has regressed.

parse_mixed_batch/parse_all
    time:   [9.0131 µs 9.0888 µs 9.1663 µs]
    thrpt:  [120.58 MiB/s 121.61 MiB/s 122.63 MiB/s]
    change: [+37.495% +39.289% +40.969%] (p = 0.00 < 0.05)
    Performance has regressed.
```

## 相關資源

- [simd-json GitHub](https://github.com/simd-lite/simd-json)
- [serde_json 文檔](https://docs.rs/serde_json)
- [Criterion 用戶指南](https://bheisler.github.io/criterion.rs/book/)
- [Rust 性能優化書籍](https://nnethercote.github.io/perf-book/)
