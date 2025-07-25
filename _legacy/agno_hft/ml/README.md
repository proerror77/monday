# Python ML 訓練層

完整的深度學習和強化學習訓練基礎設施，符合 TLOB 計劃和三層架構設計。

## 🏗️ 架構概覽

### 三層架構中的定位
- **🟦 Rust 底層**：高性能推理執行 (`<1μs`)
- **🟨 Python 訓練層**：ML 模型訓練和評估 ← **本模塊**
- **🟩 Agno 控制層**：智能化工作流管理

### 核心功能
- **數據處理**：ClickHouse 加載、標籤生成、預處理
- **模型架構**：TLOB Transformer、PPO Agent、通用 Transformer
- **訓練組件**：統一訓練器、數據集、評估器
- **模型導出**：TorchScript 導出，支持 Rust 集成

## 📁 目錄結構

```
ml/
├── data/                       # 數據處理管道
│   ├── data_loader.py         # ClickHouse 數據加載
│   ├── labeling.py            # 標籤生成（TLOB 任務 1.2）
│   └── preprocessing.py       # 數據預處理（TLOB 任務 1.3）
├── models/                     # 深度學習模型
│   ├── tlob.py               # TLOB 模型（TLOB 任務 2.2）
│   ├── lob_transformer.py    # 通用 LOB Transformer
│   └── ppo_agent.py          # PPO 強化學習代理
├── utils/                      # 工具函數
│   └── features.py           # 特徵工程
├── train.py                   # 主訓練腳本（TLOB 任務 2.3）
├── evaluate.py                # 評估腳本（TLOB 任務 3.1）
├── trainer.py                 # 可重用訓練組件
├── dataset.py                 # 數據集實現
├── export.py                  # 模型導出（TLOB 任務 2.4）
├── example_usage.py           # 使用示例
└── README.md                  # 本文件
```

## 🚀 快速開始

### 1. 環境設置

```bash
# 安裝依賴
pip install -r requirements.txt

# 或者使用 conda
conda env create -f environment.yml
```

### 2. 數據準備

```python
from ml.data import DataLoader, LabelGenerator, LOBPreprocessor

# 加載數據
data_loader = DataLoader()
raw_data = data_loader.load_orderbook_data(
    symbol='BTCUSDT',
    start_time='2024-01-01',
    end_time='2024-01-31'
)

# 生成標籤
label_generator = LabelGenerator()
labeled_data = label_generator.generate_labels(raw_data)

# 預處理
preprocessor = LOBPreprocessor()
preprocessor.fit(train_data)
sequences = preprocessor.transform(test_data)
```

### 3. 模型訓練

```python
from ml.models import TLOB, TLOBConfig
from ml.trainer import TLOBTrainerImplementation

# 創建模型
config = TLOBConfig(
    input_dim=62,
    d_model=256,
    nhead=8,
    num_layers=4
)
model = TLOB(config)

# 訓練
trainer = TLOBTrainerImplementation(
    model=model,
    train_loader=train_loader,
    val_loader=val_loader
)
results = trainer.train(epochs=100)
```

### 4. 模型評估

```python
from ml.evaluate import TLOBEvaluator

# 評估
evaluator = TLOBEvaluator('./evaluation_results')
report = evaluator.run_complete_evaluation(
    model_path='./best_model.pt',
    data_config=data_config,
    trading_config=trading_config
)
```

### 5. 模型導出

```python
from ml.export import ModelExporter

# 導出為 TorchScript
exporter = ModelExporter()
export_info = exporter.export_for_rust(
    model,
    'tlob_btcusdt',
    example_input
)
```

## 🎯 TLOB 計劃實現狀態

### ✅ 已完成任務

| 任務 | 描述 | 狀態 | 文件 |
|------|------|------|------|
| 1.2 | 標籤生成 | ✅ | `data/labeling.py` |
| 1.3 | 數據預處理 | ✅ | `data/preprocessing.py` |
| 2.2 | TLOB 模型實現 | ✅ | `models/tlob.py` |
| 2.3 | 訓練腳本 | ✅ | `train.py` |
| 2.4 | 模型導出 | ✅ | `export.py` |
| 3.1 | 評估腳本 | ✅ | `evaluate.py` |

### 🔄 關鍵特性

- **標準化一致性**：保存 `normalization_stats.json`，Rust 可直接加載
- **TorchScript 導出**：專門優化的模型導出，支持 CPU 推理
- **測試數據導出**：支持 Python-Rust 一致性驗證（任務 4.2）
- **完整評估**：分類指標 + 金融指標 + 可視化

## 💡 使用示例

查看 `example_usage.py` 了解完整的使用示例：

```bash
python ml/example_usage.py
```

## 🔧 命令行工具

### 訓練模型

```bash
python -m ml.train \
    --symbol BTCUSDT \
    --data-start 2024-01-01 \
    --data-end 2024-01-31 \
    --epochs 100 \
    --batch-size 64 \
    --output-dir ./outputs
```

### 評估模型

```bash
python -m ml.evaluate \
    --model-path ./outputs/best_model.pt \
    --symbol BTCUSDT \
    --test-start 2024-02-01 \
    --test-end 2024-02-28 \
    --normalization-stats ./outputs/normalization_stats.json
```

## 🧪 測試

```bash
# 運行所有測試
python -m pytest tests/

# 運行特定測試
python -m pytest tests/test_tlob_model.py
```

## 📊 模型性能指標

### 分類指標
- Accuracy：模型準確率
- F1-Score：精確率和召回率的調和平均
- Confusion Matrix：混淆矩陣

### 金融指標
- Sharpe Ratio：風險調整收益
- Maximum Drawdown：最大回撤
- Win Rate：勝率
- Calmar Ratio：年化收益/最大回撤

### 系統指標
- 訓練時間：模型訓練耗時
- 推理延遲：單次預測時間
- 模型大小：TorchScript 文件大小

## 🔗 與其他模塊集成

### 與 Rust 推理引擎集成

```python
# 導出模型供 Rust 使用
exporter = ModelExporter()
export_info = exporter.export_for_rust(
    model,
    'tlob_btcusdt',
    example_input,
    rust_config={'optimize_for_inference': True}
)

# 驗證 Python-Rust 一致性
preprocessor.export_test_data(
    test_data, 
    './test_data_for_rust.npz'
)
```

### 與 Agno 控制層集成

Python ML 層通過標準化接口與 Agno 工作流集成：

- **模型訓練**：`train_model(symbol, config)`
- **模型評估**：`evaluate_model(model_path, test_config)`
- **模型導出**：`export_model(model, export_config)`

## ⚡ 性能優化

### 訓練優化
- **混合精度**：使用 `torch.cuda.amp` 加速訓練
- **梯度累積**：處理大批次訓練
- **多GPU**：分佈式訓練支持

### 推理優化
- **TorchScript JIT**：編譯優化
- **量化**：模型量化減少內存
- **批處理**：批量推理提升吞吐

### 數據優化
- **內存映射**：大數據集高效加載
- **數據預取**：異步數據加載
- **數據增強**：在線數據增強

## 🛠️ 開發和調試

### 日誌配置

```python
import logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
```

### TensorBoard 監控

```bash
tensorboard --logdir ./outputs/tensorboard
```

### Wandb 集成

```python
config['use_wandb'] = True
config['wandb_project'] = 'hft-tlob'
```

## 📚 相關文檔

- [TLOB 實施計劃](../TLOB_Implementation_Plan.md)
- [Rust 集成指南](../rust_hft/README.md)
- [Agno 工作流文檔](https://docs.agno.com/)

## 🤝 貢獻指南

1. Fork 項目
2. 創建特性分支 (`git checkout -b feature/new-feature`)
3. 提交更改 (`git commit -am 'Add new feature'`)
4. 推送分支 (`git push origin feature/new-feature`)
5. 創建 Pull Request

## 📄 許可證

本項目遵循 MIT 許可證。詳見 [LICENSE](../LICENSE) 文件。