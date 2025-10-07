# TCN-GRU 無監督學習系統

## 系統架構

本系統使用TCN-GRU（Temporal Convolutional Network + Gated Recurrent Unit）進行無監督學習，整合了35個預計算的訂單簿動力學特徵。

## 特徵系統 (35個特徵)

### 1. 基礎價格特徵 (3個)
- `spread_pct`: 買賣價差百分比
- `size_imbalance`: 買賣量不平衡  
- `tick_direction`: 價格tick方向

### 2. L2深度特徵 (12個)
- **深度不平衡** (3個): `imb_5`, `imb_10`, `imb_20`
- **加權不平衡** (3個): `weighted_imb_5`, `weighted_imb_10`, `weighted_imb_20`
- **深度比率** (3個): `depth_ratio_5`, `depth_ratio_10`, `depth_ratio_20`
- **流動性壓力** (3個): `pressure_5`, `pressure_10`, `pressure_20`

### 3. 交易流特徵 (18個)
三個時間窗口 (30s, 60s, 180s) × 6個指標:
- 買量/賣量
- 成交量不平衡
- 交易強度
- 價格影響
- VWAP

### 4. 價格動量特徵 (2個)
- `momentum_short`: 短期動量
- `volatility_short`: 短期波動率

## 使用方法

### 查看特徵資訊
```bash
python cli_tcn_gru.py features
python cli_tcn_gru.py features --verbose  # 詳細描述
```

### 訓練模型

#### Taker策略 (市價單, 0.06%費率)
```bash
python cli_tcn_gru.py train \
    --symbol WLFIUSDT \
    --start_date "2025-09-01T00:00:00Z" \
    --end_date "2025-09-08T00:00:00Z" \
    --use_taker \
    --epochs 20 \
    --batch_size 256
```

#### Maker策略 (限價單, 0.02%費率)
```bash
python cli_tcn_gru.py train \
    --symbol WLFIUSDT \
    --start_date "2025-09-01T00:00:00Z" \
    --end_date "2025-09-08T00:00:00Z" \
    --use_maker \
    --epochs 20 \
    --batch_size 256
```

### 使用訓練腳本
```bash
bash train_tcn_gru.sh
```

## 檔案結構

```
ml_workspace/
├── cli_tcn_gru.py           # 統一命令行介面
├── train_tcn_gru.sh         # 訓練腳本
├── algorithms/
│   └── tcn_gru.py          # TCN-GRU模型架構
├── workflows/
│   └── tcn_gru_train.py    # 訓練工作流
├── utils/
│   ├── features_37.py      # 35個特徵定義
│   ├── ch_queries.py       # ClickHouse查詢
│   └── clickhouse_client.py # ClickHouse客戶端
└── models/tcn_gru/         # 模型保存目錄
```

## 核心特點

1. **無監督學習**: 包含重構任務和下一步預測任務
2. **成本感知**: 嵌入Taker/Maker手續費到損失函數
3. **多時間尺度**: 預測1, 5, 10, 30, 60秒的收益
4. **訂單簿動力學**: 整合MLOFI和深度不平衡特徵
5. **TorchScript導出**: 支持部署到Rust生產環境

## 模型輸出

訓練完成後會生成：
- `tcn_gru_SYMBOL_TIMESTAMP.pt`: PyTorch模型
- `tcn_gru_SYMBOL_TIMESTAMP.torchscript.pt`: TorchScript模型（用於Rust部署）
- `history_SYMBOL_TIMESTAMP.json`: 訓練歷史

## 數據流程

1. **ClickHouse** → 預計算35個特徵
2. **特徵提取** → 使用`utils.ch_queries.build_feature_sql()`
3. **序列構建** → 60秒滾動窗口
4. **TCN編碼** → 提取時序特徵
5. **GRU解碼** → 生成多時間尺度預測
6. **成本調整** → 考慮手續費和滑點

## 性能指標

- Information Coefficient (IC): 預測與實際收益的相關性
- 成本調整後收益: 扣除手續費後的淨收益
- 各時間尺度IC: 1s, 5s, 10s, 30s, 60s的預測準確度
