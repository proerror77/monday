# 多交易對高頻交易機器人

基於Bitget現貨交易的多交易對市場製造策略機器人，支持異步處理多個交易對，實現高效的做市交易。

## 功能特點

### 🚀 核心功能
- **多交易對支持**: 同時處理多個交易對，如 BTCUSDT、ETHUSDT 等
- **市場製造策略**: 在買賣價差中放置限價單獲利
- **異步高性能**: 基於 asyncio 的高並發架構
- **智能風險控制**: 多層風險管理機制
- **實時監控**: WebSocket 實時數據流處理

### 📊 交易策略
- **做市策略**: 在最佳買賣價附近放置訂單
- **深度檢查**: 確保足夠的市場流動性
- **價差控制**: 最小價差要求避免無效交易
- **訂單管理**: 自動取消超時訂單，維護訂單健康

### 🛡️ 風險管理
- **交易量限制**: 總交易量達到500,000 USDT時停止
- **持倉限制**: 每個交易對的最大持倉控制
- **頻率限制**: API請求頻率控制
- **異常處理**: 完善的錯誤處理和恢復機制

## 快速開始

### 1. 安裝依賴

```bash
pip install -r requirements.txt
```

### 2. 配置API

編輯 `config.yaml` 文件，填入您的Bitget API信息：

```yaml
api:
  api_key: "your_api_key_here"
  api_secret: "your_api_secret_here"
  passphrase: "your_passphrase_here"
```

### 3. 配置交易對

在 `config.yaml` 中配置要交易的交易對：

```yaml
trading_pairs:
  BTCUSDT:
    enabled: true
    order_size: 0.001      # 每單交易量
    spread: 0.001          # 價差 (0.1%)
    min_depth: 1000.0      # 最小深度
    max_orders: 4          # 最大同時訂單數
```

### 4. 運行機器人

```bash
python multi_symbol_hft_bot.py
```

## 配置說明

### 交易對配置
- `enabled`: 是否啟用該交易對
- `order_size`: 每筆訂單的交易量
- `spread`: 價差百分比
- `min_depth`: 最小訂單簿深度要求
- `max_orders`: 最大同時活躍訂單數

### 風險管理配置
- `total_volume_limit`: 總交易量限制 (USDT)
- `fee_offset`: 手續費抵扣額度 (USDT)
- `max_daily_trades`: 每日最大交易次數
- `position_limits`: 各交易對持倉限制

### 性能配置
- `order_interval`: 最小下單間隔
- `cleanup_interval`: 清理週期
- `order_timeout`: 訂單超時時間

## 架構設計

### 核心組件

1. **MultiSymbolHFTBot**: 主控制器，協調所有組件
2. **SymbolManager**: 單個交易對管理器
3. **WebSocketManager**: WebSocket連接池管理
4. **OrderManager**: 訂單生命週期管理
5. **RiskManager**: 風險控制引擎
6. **AsyncAPIClient**: 異步API客戶端

### 數據流程

```
WebSocket數據 → SymbolManager → 策略判斷 → 風險檢查 → OrderManager → API請求
```

### 併發模型

- 每個交易對獨立處理，互不影響
- WebSocket消息異步處理
- API請求使用連接池和頻率限制
- 訂單管理支持批量操作

## 監控和日誌

### 日誌級別
- `INFO`: 正常運行信息
- `WARNING`: 警告信息
- `ERROR`: 錯誤信息
- `DEBUG`: 調試信息

### 關鍵指標監控
- 活躍訂單數量
- 累計交易量
- 成交率
- 延遲統計
- 錯誤率

## 風險提示

⚠️ **重要提示**:
- 這是一個高頻交易機器人，請確保充分理解風險
- 建議先在測試環境中運行
- 請合理設置風險參數
- 監控市場條件，避免異常市況下的損失
- 確保API權限設置正確，避免過度交易

## 故障排除

### 常見問題

1. **WebSocket連接失敗**
   - 檢查網絡連接
   - 確認API權限
   - 查看防火牆設置

2. **下單失敗**
   - 檢查餘額是否充足
   - 確認交易對是否支持
   - 查看API頻率限制

3. **性能問題**
   - 調整並發參數
   - 檢查系統資源使用
   - 優化網絡延遲

### 調試模式

設置日誌級別為 `DEBUG` 獲取詳細調試信息：

```yaml
logging:
  level: "DEBUG"
```

## 版本歷史

- **v2.0.0**: 多交易對支持，架構重構
- **v1.0.0**: 單交易對基礎版本

## 聯繫支持

如有問題請查看日誌文件或提交Issue。

---

**免責聲明**: 本軟件僅供學習和研究使用，使用者需自行承擔交易風險。