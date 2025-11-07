# Hyperliquid 完整集成演示

這是一個完整的端到端演示應用，展示如何同時使用 Hyperliquid 的數據適配器和執行適配器來構建一個完整的交易系統。

## 功能特性

✅ **雙適配器集成**
- 數據適配器：接收實時市場數據（L2 訂單簿、交易流）
- 執行適配器：下單管理（限價、市價、撤單、改單）

✅ **Paper/Live 模式支持**
- Paper 模式：使用真實市場數據進行模擬交易
- Live 模式：執行真實交易（需要私鑰）

✅ **完整的配置管理**
- YAML 配置文件支持
- 環境變量配置
- 命令行參數配置

✅ **實時策略演示**
- 簡單的日誌記錄策略
- 價格變動追蹤
- 市場數據統計

## 使用方法

### 1. Paper 模式測試（推薦）

Paper 模式使用真實市場數據但不執行真實交易，非常適合測試和開發：

```bash
# 基礎運行（默認 30 秒，BTC-PERP + ETH-PERP）
cargo run -p hyperliquid-demo --bin complete_demo

# 自定義參數
cargo run -p hyperliquid-demo --bin complete_demo -- \
  --mode paper \
  --duration 60 \
  --symbols BTC-PERP,SOL-PERP \
  --test-orders

# 查看幫助
cargo run -p hyperliquid-demo --bin complete_demo -- --help
```

### 2. Live 模式（真實交易）

⚠️ **警告：Live 模式會執行真實交易，請謹慎使用！**

首先獲取您的 Hyperliquid 私鑰：

1. 登錄 [Hyperliquid](https://app.hyperliquid.xyz/)
2. 從 MetaMask 導出私鑰（64 位十六進制字符串，不含 0x 前缀）
3. 確保賬戶有足夠的 USDC 保證金

```bash
# 設置私鑰環境變量
export HYPERLIQUID_PRIVATE_KEY=your_64_character_hex_private_key_here

# 運行 Live 模式
cargo run -p hyperliquid-demo --bin complete_demo -- \
  --mode live \
  --duration 30
```

### 3. 命令行參數

| 參數 | 短選項 | 默認值 | 說明 |
|------|--------|--------|------|
| `--mode` | `-m` | `paper` | 交易模式：`paper` 或 `live` |
| `--duration` | `-d` | `30` | 演示持續時間（秒） |
| `--symbols` | `-s` | `BTC-PERP,ETH-PERP` | 交易對列表 |
| `--test-orders` | | `false` | 是否測試下單功能（僅 Paper 模式） |

## 輸出示例

### Paper 模式輸出

```
🚀 Hyperliquid 完整集成演示開始
模式: Paper, 持續時間: 30秒
交易對: ["BTC-PERP", "ETH-PERP"]
📡 數據適配器已創建
📝 使用 Paper 模式 - 安全的模擬交易
💼 執行適配器已連接
📈 啟動市場數據流
⏱️  開始接收市場數據 (30 秒)...

🔥 BTC-PERP Trade: HL_1721810005345 @ 67188.40 (Size: 0.02, Side: Buy)
📈 ETH-PERP Trade: HL_1721810005456 @ 3456.78 (Size: 0.1, Side: Sell)
📊 BTC-PERP OrderBook: Bid 67188.10 @ 0.5 | Ask 67188.50 @ 0.4

📊 演示結束統計:
  - 總事件數: 245
  - 交易事件: 89
  - 快照事件: 156
  - 最後價格記錄: 2 個交易對
    BTC-PERP 最後價格: 67188.40
    ETH-PERP 最後價格: 3456.78

🔌 連接已斷開
✨ 演示完成！
```

### 測試訂單功能

啟用 `--test-orders` 時，系統會在 Paper 模式下執行以下測試：

1. 提交一個限價買單（遠離市價）
2. 等待 2 秒
3. 取消訂單
4. 顯示結果

```
🧪 測試下單功能
✅ 測試訂單已提交: OrderId("HL_PAPER_1721810005678")
❌ 測試訂單已取消
```

## 代碼結構

```
apps/hyperliquid-demo/
├── Cargo.toml              # 依賴配置
├── README.md               # 本文檔
└── src/
    └── complete_demo.rs     # 主要演示代碼
```

### 主要組件

1. **SimpleLoggingStrategy**: 簡單的策略實現
   - 跟蹤交易價格變動
   - 記錄訂單簿更新
   - 提供統計信息

2. **配置管理**: 
   - 支持命令行參數解析
   - 環境變量讀取
   - 適配器配置

3. **事件處理循環**:
   - 異步市場數據流處理
   - 定期健康檢查
   - 優雅關閉處理

## 依賴項目

- `hft-core`: 核心類型和工具
- `hft-ports`: 統一接口定義
- `hft-data-adapter-hyperliquid`: Hyperliquid 數據適配器
- `hft-execution-adapter-hyperliquid`: Hyperliquid 執行適配器

## 安全注意事項

1. **私鑰保護**
   - 永遠不要在代碼中硬編碼私鑰
   - 使用環境變量或安全的密鑰管理
   - 定期輪換私鑰

2. **測試優先**
   - 先在 Paper 模式下充分測試
   - 確認策略邏輯正確
   - 驗證風險控制機制

3. **資金管理**
   - 只使用您能承受損失的資金
   - 設置合理的止損限額
   - 監控交易活動

## 故障排除

### 常見問題

1. **連接錯誤**
   ```
   錯誤: WebSocket 連接失敗
   解決: 檢查網絡連接和防火牆設置
   ```

2. **認證錯誤**
   ```
   錯誤: Live 模式需要私鑰
   解決: 設置 HYPERLIQUID_PRIVATE_KEY 環境變量
   ```

3. **編譯錯誤**
   ```bash
   # 清理並重新編譯
   cargo clean
   cargo build -p hyperliquid-demo
   ```

### 調試模式

啟用詳細日誌：

```bash
RUST_LOG=debug cargo run -p hyperliquid-demo --bin complete_demo
```

## 擴展建議

1. **添加更多策略**
   - 實現 RSI、MACD 等技術指標
   - 添加套利策略
   - 實現風險管理規則

2. **改進監控**
   - 添加 Prometheus 指標
   - 實現告警機制
   - 添加性能監控

3. **數據持久化**
   - 保存歷史交易數據
   - 實現策略回測
   - 添加數據分析功能

## 許可證

本項目遵循與主項目相同的許可證。