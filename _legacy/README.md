# Legacy Files Archive

本目錄包含重構前的歷史代碼和實驗性實現，已被新的三層架構取代。

## 📋 歸檔說明

### 重構時間
- **歸檔日期**: 2025-07-25
- **重構原因**: 基於PRD v2.0規範，實施L1+L2+L3分層架構
- **新架構**: `ml_workspace` + `ops_workspace` + `rust_hft`

### 歸檔內容

#### 🗂️ agno_hft/ (原主要目錄)
- **舊架構**: 混合的Agent實現，未分離L2/L3職責
- **問題**: ML Agent和Ops Agent職責混合，未充分利用ag ws
- **取代方案**: 分別由`ml_workspace`和`ops_workspace`取代

#### 🐍 Python腳本文件
- `agno_*.py`: 各種實驗性Agent實現
- `hft_*.py`: 早期CLI和Agent嘗試
- `test_*.py`: 舊的測試腳本
- `*_workflow.py`: 重構前的工作流程實現

#### ⚙️ 配置文件
- `config.yaml`: 舊的統一配置文件
- `requirements_old.txt`: 舊的依賴文件
- 取代方案: 每個workspace有獨立的配置

#### 🧪 測試文件
- `tests/`: 舊的測試目錄結構
- `tests_old/`: 更早期的測試文件
- 取代方案: 每個workspace有自己的tests/目錄

## 🏗️ 新架構對比

| 舊架構問題 | 新架構解決方案 |
|------------|----------------|
| 職責混合 | L2 Ops (7x24常駐) + L3 ML (批次GPU) |
| 資源浪費 | 精確資源分配 |
| 部署複雜 | Docker獨立部署 |
| ag ws未利用 | 完全基於ag ws管理 |

## 🚫 不建議使用

⚠️ **注意**: 本目錄中的代碼僅供參考，不建議在新系統中使用：

1. **架構過時**: 不符合PRD v2.0規範
2. **職責混合**: L2/L3 Agent未分離
3. **依賴衝突**: 可能與新workspace依賴衝突
4. **性能問題**: 未優化的實現

## 📚 學習價值

這些代碼對理解系統演進過程有價值：
- Agent設計思路的發展
- 工作流程架構的改進
- 性能優化的歷程
- 測試策略的演變

## 🗑️ 清理計劃

建議6個月後考慮刪除此目錄，屆時新架構應已完全穩定。

---

**重構負責人**: HFT Team  
**新架構文檔**: [../ARCHITECTURE_RESTRUCTURE_SUMMARY.md](../ARCHITECTURE_RESTRUCTURE_SUMMARY.md)  
**生產部署**: [../deployment/](../deployment/)  