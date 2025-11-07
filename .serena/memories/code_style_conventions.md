# 代碼風格與約定

## Rust 代碼風格
### 命名約定
- **結構體**: PascalCase (`ExchangeManager`, `HealthMetrics`)
- **函數/變量**: snake_case (`select_best_exchange`, `last_event_time`)
- **常量**: SCREAMING_SNAKE_CASE (`MAX_RECONNECT_ATTEMPTS`)
- **模塊**: snake_case (`event_hub`, `redis_bridge`)

### 文檔約定
- 使用繁體中文註釋，技術術語保持英文
- 模塊文檔以 `//!` 開頭，描述模塊目的和功能
- 公共API都需要文檔註釋

### 錯誤處理
- 使用 `anyhow::Result<T>` 作為返回類型
- 使用 `thiserror` 定義自定義錯誤類型
- 異步函數返回 `Result` 而不是 panic

### 性能考慮
- 熱路徑使用無鎖數據結構 (`dashmap`, `crossbeam`)
- 避免在關鍵路徑中分配內存
- 使用 `Arc<T>` 共享不可變數據
- 使用 `RwLock` 處理讀多寫少場景

### 依賴管理
- 優先使用高性能庫：`simd-json` > `serde_json`
- 分feature切割可選依賴 (`python`, `torchscript`, `ml`)
- dev-dependencies 與生產依賴分離

## 架構模式
- **模塊結構**: `src/{domain}/{module}.rs`
- **抽象**: 使用 trait 定義接口（如 `Exchange` trait）
- **依賴注入**: 通過構造函數傳入依賴
- **配置管理**: 使用 serde + YAML/TOML