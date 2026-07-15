# NBA Swing Strategy - 前端可視化界面

## 概述

我已經為 NBA Swing Strategy 創建了一個完整的可視化前端界面，展示實時交易狀態、市場數據、倉位管理和信號歷史。

## 功能特性

### 1. 實時狀態監控

**狀態指示器**：
- WATCH（觀察）- 灰色
- ARMED（準備）- 黃色
- ENTERING（進場中）- 藍色
- MANAGING（管理中）- 綠色
- EXITING（出場中）- 橙色
- EXITED（已出場）- 紫色
- HALT（緊急停止）- 紅色

### 2. 比賽實時數據

顯示：
- 球隊名稱和比分
- 當前節數和剩餘時間
- 球權狀態
- 比分差距

### 3. 關鍵指標卡片

**4 個核心指標**：

1. **Model Win Prob**（模型勝率）
   - 顯示模型預測的勝率
   - 顯示信心水平

2. **Market Price**（市場價格）
   - 當前市場價格
   - 價差（basis points）

3. **Edge**（優勢）
   - p_model - p_market
   - 顏色編碼（綠色 = 正，紅色 = 負）
   - 強度標籤（Strong/Moderate/Weak）

4. **Unrealized PnL**（未實現盈虧）
   - 當前未實現盈虧金額
   - 百分比回報

### 4. 倉位詳情

顯示：
- 入場價格
- 當前價格
- 峰值價格
- 倉位大小
- 進度條（從入場到峰值的位置）

### 5. Market Filters（市場濾網）

顯示：
- 濾網通過/失敗狀態
- 失敗原因列表
- 警告信息

### 6. Market Data（市場數據）

顯示：
- Best Bid/Ask
- Bid/Ask Depth
- 數據延遲

### 7. Signal History（信號歷史）

顯示：
- 所有進場/出場/拒絕信號
- 時間戳
- 原因說明
- Edge、Net EV、Confidence（如果有）

### 8. 控制按鈕

- **Pause Strategy**：暫停策略
- **Emergency Halt**：緊急停止

## 訪問方式

### 開發環境

1. 啟動前端：
```bash
cd ploy-frontend
npm install
npm run dev
```

2. 訪問：
```
http://localhost:5173/nba-swing
```

### 導航

在左側邊欄中點擊 **"NBA Swing"** 即可進入。

## 數據流

### 當前實現（Mock 數據）

前端當前使用 mock 數據進行展示。在 `NBASwingMonitor.tsx` 的 `useEffect` 中：

```typescript
useEffect(() => {
  const mockData: NBASwingState = {
    state: 'MANAGING',
    currentGame: { /* ... */ },
    prediction: { /* ... */ },
    marketData: { /* ... */ },
    position: { /* ... */ },
    signals: [ /* ... */ ],
    filters: { /* ... */ },
  };
  setState(mockData);
}, []);
```

### 生產環境（WebSocket 集成）

需要連接到後端 WebSocket：

```typescript
useEffect(() => {
  // Connect to NBA Swing WebSocket
  const ws = new WebSocket('ws://localhost:8080/nba-swing');

  ws.onmessage = (event) => {
    const data = JSON.parse(event.data);
    setState(data);
  };

  return () => ws.close();
}, []);
```

## 後端 API 需求

### WebSocket Events

前端期望接收以下格式的數據：

```typescript
interface NBASwingState {
  state: 'WATCH' | 'ARMED' | 'ENTERING' | 'MANAGING' | 'EXITING' | 'EXITED' | 'HALT';
  currentGame: {
    gameId: string;
    homeTeam: string;
    awayTeam: string;
    homeScore: number;
    awayScore: number;
    quarter: number;
    timeRemaining: number;
    possession: string;
  } | null;
  prediction: {
    winProb: number;
    confidence: number;
    uncertainty: number;
    features: {
      pointDiff: number;
      timeRemaining: number;
      quarter: number;
    };
  } | null;
  marketData: {
    marketId: string;
    teamName: string;
    price: number;
    bestBid: number;
    bestAsk: number;
    spreadBps: number;
    bidDepth: number;
    askDepth: number;
    dataLatencyMs: number;
  } | null;
  position: {
    entryPrice: number;
    currentPrice: number;
    size: number;
    unrealizedPnl: number;
    unrealizedPnlPct: number;
    peakPrice: number;
    entryTime: string;
  } | null;
  signals: Array<{
    timestamp: string;
    type: 'ENTRY' | 'EXIT' | 'REJECTED';
    reason: string;
    edge?: number;
    netEv?: number;
    confidence?: number;
  }>;
  filters: {
    passed: boolean;
    reasons: string[];
    warnings: string[];
  } | null;
}
```

### REST API Endpoints（可選）

```
GET  /api/nba-swing/status        # 獲取當前狀態
POST /api/nba-swing/pause         # 暫停策略
POST /api/nba-swing/resume        # 恢復策略
POST /api/nba-swing/halt          # 緊急停止
GET  /api/nba-swing/signals       # 獲取信號歷史
```

## 自定義配置

### 顏色主題

在 `NBASwingMonitor.tsx` 中修改 `getStateColor` 函數：

```typescript
const getStateColor = (state: string) => {
  switch (state) {
    case 'WATCH': return 'bg-gray-500';
    case 'ARMED': return 'bg-yellow-500';
    // ... 自定義顏色
  }
};
```

### 刷新頻率

修改 WebSocket 連接或輪詢間隔：

```typescript
// 輪詢方式
useEffect(() => {
  const interval = setInterval(() => {
    fetch('/api/nba-swing/status')
      .then(res => res.json())
      .then(data => setState(data));
  }, 1000); // 每秒刷新

  return () => clearInterval(interval);
}, []);
```

## 響應式設計

界面已針對不同屏幕尺寸優化：

- **桌面**（≥ 768px）：4 列網格佈局
- **平板**（< 768px）：2 列網格佈局
- **手機**（< 640px）：單列佈局

## 未來增強

### 1. 圖表可視化

添加實時圖表：
- 勝率變化曲線
- 價格走勢圖
- PnL 曲線
- Edge 歷史

可以使用 `recharts` 或 `victory` 庫：

```bash
npm install recharts
```

### 2. 通知系統

添加桌面通知：
```typescript
if (signal.type === 'ENTRY') {
  new Notification('NBA Swing', {
    body: `Entry signal: ${signal.reason}`,
    icon: '/icon.png'
  });
}
```

### 3. 音效提示

添加音效：
```typescript
const playSound = (type: 'entry' | 'exit' | 'alert') => {
  const audio = new Audio(`/sounds/${type}.mp3`);
  audio.play();
};
```

### 4. 歷史回放

添加歷史數據回放功能，用於分析過去的交易。

### 5. 多市場監控

支持同時監控多個 NBA 比賽。

## 文件結構

```
ploy-frontend/
├── src/
│   ├── pages/
│   │   └── NBASwingMonitor.tsx    # NBA Swing 主頁面
│   ├── components/
│   │   ├── Layout.tsx              # 更新：添加導航
│   │   └── ui/                     # UI 組件
│   └── App.tsx                     # 更新：添加路由
```

## 截圖說明

界面包含以下區域（從上到下）：

1. **Header**：標題 + 狀態徽章
2. **Live Game**：比賽實時數據
3. **Key Metrics**：4 個關鍵指標卡片
4. **Position Details**：倉位詳情（如果有倉位）
5. **Market Filters**：濾網狀態
6. **Market Data**：市場數據
7. **Signal History**：信號歷史
8. **Control Buttons**：控制按鈕

## 故障排除

### 問題：頁面空白

**解決**：檢查控制台錯誤，確保所有依賴已安裝：
```bash
npm install lucide-react
```

### 問題：數據不更新

**解決**：檢查 WebSocket 連接狀態，確保後端正在運行。

### 問題：樣式錯誤

**解決**：確保 Tailwind CSS 配置正確，運行：
```bash
npm run build
```

## 總結

你現在有了一個完整的 NBA Swing Strategy 可視化前端！

**特點**：
- ✅ 實時狀態監控
- ✅ 完整的市場數據展示
- ✅ 倉位管理界面
- ✅ 信號歷史追蹤
- ✅ 響應式設計
- ✅ 易於擴展

**下一步**：
1. 連接後端 WebSocket
2. 實現控制按鈕功能
3. 添加圖表可視化
4. 添加通知系統

---

**版本**：v0.1.0
**日期**：2026-01-13
**狀態**：✅ 前端完成，待後端集成
