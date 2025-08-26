# 事件契約 - 穩定介面錨點

> **Never break userspace**: 這些欄位與語義一旦定義，不得隨意變更

## 核心事件模型

### L2 快照 (MarketSnapshot)
```rust
struct MarketSnapshot {
    symbol: Symbol,
    timestamp: Timestamp,        // 事件時間 (非系統時間)
    bids: Vec<BookLevel>,        // 按價格降序
    asks: Vec<BookLevel>,        // 按價格升序  
    sequence: u64,               // 序號，用於檢測缺口
}
```

### L2 增量 (BookUpdate) 
```rust
struct BookUpdate {
    symbol: Symbol,
    timestamp: Timestamp,
    bids: Vec<BookLevel>,        // 變更的檔位
    asks: Vec<BookLevel>,
    sequence: u64,
    is_snapshot: bool,           // true=快照，false=增量
}
```

### 交易事件 (Trade)
```rust
struct Trade {
    symbol: Symbol,
    timestamp: Timestamp,
    price: Price,                // 整數新型別 Price(i64)
    quantity: Quantity,          // 整數新型別 Quantity(i64) 
    side: TradeSide,             // Buy/Sell (市場角度)
    trade_id: String,
}
```

### 私有事件 (執行回報)
```rust
enum ExecutionEvent {
    OrderAck { order_id: OrderId, timestamp: Timestamp },
    Fill { order_id: OrderId, price: Price, quantity: Quantity, timestamp: Timestamp },
    OrderReject { order_id: OrderId, reason: String, timestamp: Timestamp },
    BalanceUpdate { asset: String, balance: Quantity, timestamp: Timestamp },
}
```

## 整數新型別

所有金融數值在熱路徑使用整數，避免浮點運算：

```rust
// Price: 1,000,000 = 1.0 (6 位小數精度)
struct Price(i64);

// Quantity: 1,000,000 = 1.0 (6 位小數精度)  
struct Quantity(i64);

// Bps: 10,000 = 100% (基點精度)
struct Bps(i32);

// Timestamp: 微秒級事件時間
type Timestamp = u64;
```

## 缺口處理語義

1. **序號檢測**: 每個 symbol 維護 `last_sequence`，發現跳號觸發重連
2. **快照恢復**: 缺口時請求完整快照，重置序號基線
3. **時間窗**: 超過 `stale_threshold_us` 的事件直接丟棄

## MarketStream 契約

```rust
trait MarketStream {
    // 訂閱指定品種，返回統一事件流
    fn subscribe(&self, symbols: Vec<Symbol>) -> BoxStream<MarketEvent>;
    
    // 健康檢查
    fn health(&self) -> ConnectionHealth;
}

enum MarketEvent {
    Snapshot(MarketSnapshot),
    Update(BookUpdate), 
    Trade(Trade),
    Disconnect { reason: String },
}
```

## ExecutionClient 契約

```rust
trait ExecutionClient {
    // 下單 (統一介面，live/mock 實現不同)
    async fn place_order(&mut self, intent: OrderIntent) -> Result<OrderId>;
    
    // 撤單
    async fn cancel_order(&mut self, order_id: OrderId) -> Result<()>;
    
    // 執行回報流
    fn execution_stream(&self) -> BoxStream<ExecutionEvent>;
}
```

---

**重要**: 修改任何上述結構前，必須確保向後相容或提供遷移路徑。