
# TLOB (Transformer-based LOB) 模型實現計畫

## 1. 專案目標

本計畫旨在實現一個基於 Transformer 架構的端到端（End-to-End）深度學習模型（TLOB），用於預測高頻交易中的訂單簿（LOB）數據，並直接輸出短期價格走勢（上漲/下跌/不變）。

最終目標是生成一個可以被 Rust 高性能交易引擎直接加載和執行的、經過驗證的 TorchScript 模型 (`model.pt`)。

---

## 2. 核心階段與任務分解

本計畫分為四個主要階段，每個階段都有明確的任務和交付成果。

### **階段一：數據準備與表示 (Data Preparation & Representation)**

**目標**: 建立一個標準化、可重複的數據處理管道，將原始的 LOB 數據轉換為模型可以理解的輸入張量和標籤。

*   **任務 1.1: 數據收集與定義**
    *   [ ] **動作**: 確認 Rust 數據收集器能夠穩定記錄以下數據，並以 `.parquet` 或類似的高效格式存儲：
        *   **LOB 快照**: 至少 15 檔的買賣盤（價格、數量）。
        *   **Ticker 數據**: 最新成交價、成交量。
        *   **時間戳**: 統一使用納秒級 Unix 時間戳。
    *   [ ] **交付**: 穩定、持續的原始數據源。

*   **任務 1.2: 數據標籤生成 (Label Generation)**
    *   [ ] **動作**: 編寫一個 Python 腳本 (`ml/data/labeling.py`)，實現以下邏輯：
        1.  定義預測窗口 `k` (例如: 10 ticks) 和波動閾值 `α` (例如: 0.01%)。
        2.  對於 `t` 時刻的 LOB，計算 `t` 到 `t+k` 時間窗口內的未來中間價 `m_future`。
        3.  與 `t` 時刻的中間價 `m_t` 比較：
            *   若 `(m_future - m_t) / m_t > α`，標籤為 `2 (上漲)`。
            *   若 `(m_t - m_future) / m_t > α`，標籤為 `0 (下跌)`。
            *   否則，標籤為 `1 (不變)`。
    *   [ ] **交付**: 一個可以為任何原始 LOB 數據集生成對應標籤的腳本。

*   **任務 1.3: 輸入張量構建 (Input Tensor Creation)**
    *   [ ] **動作**: 編寫一個 Python 腳本 (`ml/data/preprocessing.py`)，實現以下數據轉換流程：
        1.  **扁平化 (Flatten)**: 將 15 檔 LOB 快照（買價, 買量, 賣價, 賣量）轉換為一個 60 維的一維向量。
        2.  **拼接 (Concatenate)**: 將 Ticker 數據（例如，最新成交價、成交量）拼接到上述向量後。
        3.  **標準化 (Normalize)**:
            *   在**訓練集**上計算每個特徵維度的均值（mean）和標準差（std）。
            *   **非常重要**: 將計算出的 `mean` 和 `std` 向量保存到一個文件中（例如 `normalization_stats.json`），以便在驗證、測試和**最終的 Rust 推理**中使用完全相同的標準化參數。
            *   應用 `(x - mean) / std` 公式進行標準化。
        4.  **序列化 (Sequencing)**:
            *   定義序列長度 `N` (例如: 100)。
            *   將連續 `N` 個處理過的向量堆疊起來，形成一個形如 `(N, D)` 的輸入張量，其中 `D` 是特徵總維度。
    *   [ ] **交付**:
        1.  一個完整的數據預處理腳本。
        2.  `normalization_stats.json` 檔案。
        3.  預處理完成、可供模型直接讀取的訓練集、驗證集和測試集檔案（例如 `.npz` 或 `.parquet` 格式）。

---

### **階段二：模型實現與訓練 (Model Implementation & Training)**

**目標**: 在 PyTorch 中實現 TLOB 模型架構，並完成訓練、調優和導出。

*   **任務 2.1: 項目結構規劃**
    *   [ ] **動作**: 在 `ml/` 目錄下建立清晰的子目錄和文件：
        ```
        ml/
        ├── data/
        │   ├── __init__.py
        │   ├── labeling.py       # 任務 1.2
        │   └── preprocessing.py    # 任務 1.3
        ├── models/
        │   ├── __init__.py
        │   └── tlob.py           # 任務 2.2
        ├── utils/
        │   └── ...
        ├── train.py              # 任務 2.3
        └── export.py             # 任務 2.4
        ```

*   **任務 2.2: TLOB 模型架構實現**
    *   [ ] **動作**: 在 `ml/models/tlob.py` 中，創建一個 `TLOB(nn.Module)` 類，包含：
        *   輸入層: `nn.Linear` (將特徵維度 `D` 映射到 `d_model`)
        *   位置編碼層: `PositionalEncoding`
        *   核心編碼器: `nn.TransformerEncoder`
        *   分類頭: `nn.Linear` (將輸出映射到 3 個類別)
    *   [ ] **交付**: 一個完整的、可實例化的 PyTorch 模型類。

*   **任務 2.3: 訓練腳本編寫**
    *   [ ] **動作**: 在 `ml/train.py` 中，編寫主訓練邏輯：
        *   使用 `argparse` 接收訓練參數（學習率、批量大小等）。
        *   實現一個自定義的 `torch.utils.data.Dataset` 來加載階段一處理好的數據。
        *   設置優化器 (`AdamW`)、損失函數 (`CrossEntropyLoss`) 和學習率調度器。
        *   編寫標準的訓練和驗證循環。
        *   (推薦) 集成 `wandb` 或 `TensorBoard` 來記錄和可視化訓練過程。
    *   [ ] **交付**: 一個可以從命令行啟動並完整執行模型訓練的腳本。

*   **任務 2.4: 模型導出**
    *   [ ] **動作**: 在 `ml/export.py` 中，編寫一個腳本，該腳本：
        1.  加載訓練好的最佳模型權重。
        2.  將模型設置為評估模式 (`model.eval()`)。
        3.  使用 `torch.jit.trace` 將模型轉換為 TorchScript 格式。
    *   [ ] **交付**: 一個 `model.pt` 檔案。

---

### **階段三：評估與迭代 (Evaluation & Iteration)**

**目標**: 客觀、全面地評估模型性能，並指導後續的優化方向。

*   **任務 3.1: 實現離線評估腳本**
    *   [ ] **動作**: 編寫 `evaluate.py`，該腳本在測試集上計算以下指標：
        *   **分類指標**: F1-Score (Macro/Micro), Precision, Recall, Confusion Matrix。
        *   **金融指標**: 透過模擬交易（例如：預測上漲則買入）計算 Sharpe Ratio, Max Drawdown, Win Rate, PnL Curve。
    *   [ ] **交付**: 一個可以生成詳細評估報告（JSON 格式）的評估腳本。

*   **任務 3.2: 超參數調優**
    *   [ ] **動作**: 系統性地實驗不同的超參數組合，找到最優配置。關鍵參數包括：
        *   模型結構: `d_model`, `nhead`, `num_layers`
        *   訓練參數: `learning_rate`, `batch_size`, `optimizer`
        *   數據參數: 序列長度 `N`
    *   [ ] **交付**: 一份關於超參數影響的分析報告和一組最佳配置。

---

### **階段四：部署到 Rust (Deployment to Rust)**

**目標**: 在 Rust 交易引擎中成功加載並運行模型，實現與 Python 中一致的推理結果。

*   **任務 4.1: Rust 數據預處理實現**
    *   [ ] **核心挑戰**: 在 Rust 中編寫一個 `Preprocessor` 模組。
    *   [ ] **動作**:
        1.  該模組需要能夠加載 Python 階段一生成的 `normalization_stats.json`。
        2.  它必須實現與 `ml/data/preprocessing.py` **完全相同的**數據轉換邏輯：扁平化 -> 拼接 -> 標準化 -> 序列化。
    *   [ ] **交付**: 一個 Rust `Preprocessor` 模組。

*   **任務 4.2: 跨語言一致性驗證**
    *   [ ] **動作**: 建立一個自動化的 CI 測試流程：
        1.  準備一份標準的原始 LOB 測試數據 (`test_data.jsonl`)。
        2.  CI 流程首先調用 Python `Preprocessor` 處理該數據，生成 `py_features.bin`。
        3.  CI 流程接著調用 Rust `Preprocessor` 處理同一份數據，生成 `rs_features.bin`。
        4.  CI 流程最後**逐字節比對** `py_features.bin` 和 `rs_features.bin`。若有任何不一致，CI 失敗。
    *   [ ] **交付**: 一個自動化的、可靠的 CI 驗證工作流程。

*   **任務 4.3: 集成到 Rust 交易引擎**
    *   [ ] **動作**:
        1.  在 Rust 引擎中，使用 `tch-rs` 加載 `model.pt`。
        2.  在實時的「熱循環」中，將收到的 LOB 數據傳遞給 Rust `Preprocessor`。
        3.  將 `Preprocessor` 產出的特徵張量送入模型進行推理。
        4.  根據模型輸出執行交易邏輯。
    *   [ ] **交付**: 一個功能完整的、由 TLOB 模型驅動的 Rust 交易引擎。
