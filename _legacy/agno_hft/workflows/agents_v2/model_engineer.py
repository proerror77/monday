"""
模型工程師 Agent
===============

專責 ML 模型的全生命週期管理：
- TLOB 模型訓練和調優
- 模型評估和驗證
- 生產部署和版本管理
- A/B 測試設計
"""

from agno import Agent
from agno.models import Claude
from typing import Dict, Any, Optional, List
import asyncio
import json

from ..tools import ModelManagementTools, DataProcessingTools, RustHFTTools


class ModelEngineer(Agent):
    """
    模型工程師
    
    專精領域：
    - TLOB 模型開發
    - 超參數優化
    - 模型部署
    - A/B 測試
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="ModelEngineer", 
            model=Claude(id="claude-sonnet-4-20250514"),
            tools=[
                ModelManagementTools(),
                DataProcessingTools(),
                RustHFTTools()
            ],
            instructions=self._get_instructions(),
            markdown=True,
            session_id=session_id,
            show_tool_calls=True
        )
        
    def _get_instructions(self) -> str:
        return """
        你是一位資深的 ML 工程師，專精於金融深度學習模型的生產化部署。

        核心職責：
        1. **模型開發**：
           - TLOB Transformer 模型架構設計
           - 超參數調優和模型優化
           - 模型集成和融合技術
           - 在線學習算法實現

        2. **模型評估**：
           - 多維度性能評估
           - 金融指標計算（Sharpe, 回撤等）
           - 模型穩定性測試
           - 泛化能力驗證

        3. **生產部署**：
           - TorchScript 模型導出
           - 推理引擎優化
           - 藍綠部署實施
           - 熱更新機制

        4. **MLOps 實施**：
           - 模型版本管理
           - 監控和告警設置
           - A/B 測試設計
           - 性能降級檢測

        工作標準：
        - 重視模型推理延遲（目標 <100μs）
        - 確保模型部署穩定性
        - 實施嚴格的驗證流程
        - 維持高質量的模型性能

        輸出要求：
        - 提供詳細的技術文檔
        - 包含性能基準測試
        - 給出部署風險評估
        - 記錄完整的實驗過程
        """
        
    async def train_production_model(
        self,
        symbol: str,
        model_config: Dict[str, Any],
        training_data_spec: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        訓練生產級模型
        
        Args:
            symbol: 交易對
            model_config: 模型配置
            training_data_spec: 訓練數據規格
            
        Returns:
            訓練結果
        """
        config_str = json.dumps(model_config, indent=2)
        data_spec_str = json.dumps(training_data_spec, indent=2)
        
        query = f"""
        請訓練 {symbol} 的生產級 TLOB 模型：
        
        模型配置：
        ```json
        {config_str}
        ```
        
        數據規格：
        ```json
        {data_spec_str}
        ```
        
        生產級訓練流程：
        1. **數據驗證**：
           - 訓練數據質量檢查
           - 特徵分佈驗證
           - 時序數據完整性
           - 標籤平衡性分析

        2. **模型訓練**：
           - 分階段訓練策略
           - 早停和檢查點機制
           - 學習率調度優化
           - 正則化技術應用

        3. **性能驗證**：
           - 交叉驗證評估
           - Walk-forward 分析
           - 極端市場測試
           - 延遲性能測試

        4. **部署準備**：
           - TorchScript 轉換和優化
           - 推理延遲基準測試
           - 模型大小優化
           - 生產環境兼容性測試

        請執行完整的訓練流程並提供詳細的模型評估報告。
        """
        
        response = await self.arun(query)
        return {"training_result": response.content, "symbol": symbol}
        
    async def design_ab_test(
        self,
        baseline_model: str,
        candidate_model: str,
        test_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        設計 A/B 測試
        
        Args:
            baseline_model: 基準模型
            candidate_model: 候選模型
            test_config: 測試配置
            
        Returns:
            A/B 測試設計
        """
        config_str = json.dumps(test_config, indent=2)
        
        query = f"""
        請設計模型 A/B 測試方案：
        
        基準模型：{baseline_model}
        候選模型：{candidate_model}
        
        測試配置：
        ```json
        {config_str}
        ```
        
        A/B 測試設計：
        1. **測試架構**：
           - 流量分割策略（50/50 或其他比例）
           - 樣本分配機制
           - 隨機化方法
           - 交叉污染防範

        2. **評估指標**：
           - 主要指標：Sharpe 比率、總收益
           - 次要指標：勝率、最大回撤、波動率
           - 風險指標：VaR、尾部風險
           - 執行指標：延遲、成交率

        3. **統計設計**：
           - 樣本大小計算
           - 檢驗功效分析
           - 顯著性水平設定
           - 多重檢驗校正

        4. **實施計劃**：
           - 測試時長規劃
           - 監控檢查點設置
           - 早停標準定義
           - 結果判定標準

        5. **風險控制**：
           - 最大損失限制
           - 異常檢測機制
           - 緊急停止條件
           - 回滾預案

        請提供完整的 A/B 測試實施方案。
        """
        
        response = await self.arun(query)
        return {"ab_test_design": response.content}
        
    async def deploy_model_blue_green(
        self,
        symbol: str,
        model_path: str,
        deployment_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        藍綠部署模型
        
        Args:
            symbol: 交易對
            model_path: 模型路徑
            deployment_config: 部署配置
            
        Returns:
            部署結果
        """
        config_str = json.dumps(deployment_config, indent=2)
        
        query = f"""
        請執行 {symbol} 模型的藍綠部署：
        
        模型路徑：{model_path}
        
        部署配置：
        ```json
        {config_str}
        ```
        
        藍綠部署流程：
        1. **綠環境準備**：
           - 新模型加載到綠環境
           - 推理服務啟動和預熱
           - 健康檢查和煙霧測試
           - 延遲和吞吐量驗證

        2. **影子模式測試**：
           - 綠環境並行運行（不執行交易）
           - 預測結果對比分析
           - 性能指標監控
           - 異常行為檢測

        3. **漸進式切換**：
           - 小比例流量切換（5%）
           - 監控關鍵指標
           - 逐步增加切換比例
           - 性能對比驗證

        4. **全量切換**：
           - 完全切換到綠環境
           - 藍環境保留作備份
           - 監控和告警設置
           - 回滾機制準備

        5. **部署驗證**：
           - 生產環境功能測試
           - 性能基準對比
           - 業務指標驗證
           - 用戶體驗檢查

        請使用部署工具執行實際的藍綠部署流程。
        """
        
        response = await self.arun(query)
        return {"deployment_result": response.content, "symbol": symbol}
        
    async def monitor_model_performance(
        self,
        symbol: str,
        monitoring_duration: str = "24h"
    ) -> Dict[str, Any]:
        """
        監控模型性能
        
        Args:
            symbol: 交易對
            monitoring_duration: 監控時長
            
        Returns:
            監控結果
        """
        query = f"""
        請監控 {symbol} 模型在過去 {monitoring_duration} 的性能表現：
        
        監控維度：
        1. **預測性能**：
           - 預測準確率趨勢
           - 各類別精確率和召回率
           - 預測置信度分佈
           - 模型校準度分析

        2. **金融性能**：
           - 實時 Sharpe 比率
           - 滾動回撤分析
           - 收益率分佈特徵
           - 風險調整收益

        3. **技術性能**：
           - 推理延遲統計
           - 模型調用頻率
           - 資源使用情況
           - 異常和錯誤率

        4. **市場適應性**：
           - 不同市場條件下的表現
           - 波動率敏感性
           - 成交量影響分析
           - 時段性能差異

        5. **異常檢測**：
           - 預測漂移檢測
           - 數據分佈變化
           - 模型行為異常
           - 性能降級警告

        請提供完整的模型性能監控報告和改進建議。
        """
        
        response = await self.arun(query)
        return {"monitoring_result": response.content, "symbol": symbol}
        
    async def optimize_model_inference(
        self,
        model_path: str,
        optimization_targets: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        優化模型推理性能
        
        Args:
            model_path: 模型路徑
            optimization_targets: 優化目標
            
        Returns:
            優化結果
        """
        targets_str = json.dumps(optimization_targets, indent=2)
        
        query = f"""
        請優化模型推理性能：
        
        模型路徑：{model_path}
        
        優化目標：
        ```json
        {targets_str}
        ```
        
        推理優化策略：
        1. **模型壓縮**：
           - 量化技術（INT8/FP16）
           - 剪枝和蒸餾
           - 模型融合優化
           - 內存佔用優化

        2. **推理引擎優化**：
           - TensorRT/ONNX Runtime
           - 批處理大小調優
           - 並行推理優化
           - 內存池管理

        3. **硬體優化**：
           - GPU 利用率提升
           - CPU 親和性設置
           - NUMA 感知優化
           - 緩存友好訪問

        4. **系統優化**：
           - 預處理流水線優化
           - 輸入數據格式優化
           - 輸出處理優化
           - 錯誤處理簡化

        5. **性能測試**：
           - 延遲基準測試
           - 吞吐量測試
           - 壓力測試
           - 穩定性測試

        請提供詳細的優化實施方案和性能提升預期。
        """
        
        response = await self.arun(query)
        return {"optimization_result": response.content}