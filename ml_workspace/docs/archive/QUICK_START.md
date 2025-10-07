# 快速開始指南

## CLI 使用狀況總結

### ✅ 可用的CLI工具

#### 1. `cli_tcn_gru.py` - TCN-GRU 無監督訓練
```bash
# 查看35特徵系統
python cli_tcn_gru.py features --verbose

# 使用Taker策略訓練（需要AWS ClickHouse連接）
python cli_tcn_gru.py train --symbol WLFIUSDT --use_taker --epochs 5

# 使用Maker策略訓練
python cli_tcn_gru.py train --symbol WLFIUSDT --use_maker --epochs 5
```

#### 2. `cli.py` - 原始複雜CLI（需要ClickHouse）
```bash
# 查看可用命令
python cli.py --help

# 查看特徵工作流
python cli.py features --help

# 無監督訓練（需要ClickHouse）
python cli.py unsup train --start "2025-09-01T00:00:00Z" --end "2025-09-08T00:00:00Z"
```

### ✅ AWS ClickHouse Cloud 連接

- 已配置 AWS ClickHouse Cloud 連接
- 主機：`ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443`
- 使用真實的35個預計算特徵進行無監督訓練

## 推薦工作流程

### 步驟1: 測試連接
```bash
python test_aws_clickhouse.py
```

### 步驟2: 查看特徵系統
```bash
python cli_tcn_gru.py features --verbose
```

### 步驟3: 無監督TCN-GRU訓練
```bash
# Taker策略（0.06%費率）
python cli_tcn_gru.py train --symbol WLFIUSDT --use_taker --epochs 10

# Maker策略（0.02%費率）
python cli_tcn_gru.py train --symbol WLFIUSDT --use_maker --epochs 10
```

### 步驟4: 檢查訓練結果
```bash
ls -la models/tcn_gru/
```

## 特徵系統總覽

系統包含35個預計算特徵，分為4個主要類別：

1. **基礎價格特徵** (3個): spread_pct, size_imbalance, tick_direction
2. **L2深度特徵** (12個): 不平衡、加權不平衡、深度比率、流動性壓力
3. **交易流特徵** (18個): 30s/60s/180s窗口的買賣量、不平衡等
4. **價格動量特徵** (2個): 短期動量和波動率

## 模型架構

- **TCN編碼器**: 提取時序特徵
- **GRU解碼器**: 生成預測
- **多任務學習**: 重構 + 下一步預測 + 多時間尺度預測
- **成本感知**: 嵌入Taker(0.06%)和Maker(0.02%)手續費

## 文件結構

```
ml_workspace/
├── cli_tcn_gru.py         # ✅ TCN-GRU無監督訓練
├── cli.py                 # ⚠️  原始複雜CLI
├── test_aws_clickhouse.py # ✅ AWS連接測試
├── algorithms/tcn_gru.py  # TCN-GRU模型
├── utils/features_37.py   # 35特徵定義
└── models/tcn_gru/        # 模型輸出目錄
```

## 故障排除

### 如果遇到 AWS ClickHouse 連接錯誤
1. 檢查網路連通性
2. 確認憑證配置正確
3. 運行 `python test_aws_clickhouse.py` 診斷問題

### 真實數據訓練
1. 確保 AWS ClickHouse 連接正常
2. 確認35特徵表存在並有數據
3. 使用 `cli_tcn_gru.py` 進行無監督訓練

### 模型部署
- 使用生成的 `.torchscript.pt` 文件
- 可以直接在Rust環境中加載
