"""
模型訓練專家 Agent
================

專責機器學習模型訓練和優化的 Agent：
- TLOB 模型訓練管理
- 超參數調優
- 模型版本管理
- 性能評估和比較
"""

from agno import Agent
from agno.models import Claude
from typing import Dict, Any, Optional, List
import asyncio
import json

from ..tools import ModelManagementTools, RustHFTTools
from ..tools.monitoring_tools import MonitoringTools


class ModelTrainer(Agent):
    """
    模型訓練專家
    
    專精領域：
    - TLOB 模型訓練
    - 超參數優化
    - 模型評估
    - 訓練管道管理
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="ModelTrainer",
            model=Claude(id="claude-sonnet-4-20250514"),
            tools=[
                ModelManagementTools(),
                RustHFTTools(),
                MonitoringTools()
            ],
            instructions=self._get_instructions(),
            markdown=True,
            session_id=session_id,
            show_tool_calls=True
        )
        
    def _get_instructions(self) -> str:
        return """
        你是一位頂尖的機器學習工程師和量化研究員，專精於金融深度學習模型。

        核心職責：
        1. **模型訓練管理**：
           - 管理 TLOB Transformer 模型訓練流程
           - 監控訓練進度和性能指標
           - 處理訓練異常和錯誤恢復
           - 實施早停和檢查點策略

        2. **超參數優化**：
           - 設計超參數搜索空間
           - 執行貝葉斯優化和網格搜索
           - 分析超參數敏感性
           - 推薦最優配置

        3. **模型評估**：
           - 評估模型分類性能
           - 計算金融指標（Sharpe、回撤等）
           - 進行模型比較和 A/B 測試
           - 生成性能報告

        4. **模型部署**：
           - 導出 TorchScript 模型
           - 驗證模型兼容性
           - 管理模型版本
           - 實施藍綠部署

        專業要求：
        - 深度理解 Transformer 架構和 LOB 數據
        - 熟悉 PyTorch 和 TLOB 訓練管道
        - 重視實驗記錄和可重現性
        - 關注模型性能和風險

        輸出格式：
        - 提供詳細的訓練日誌
        - 包含性能指標和圖表
        - 給出明確的部署建議
        - 記錄實驗配置
        """
        
    async def train_tlob_model(
        self,
        symbol: str,
        training_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        訓練 TLOB 模型
        
        Args:
            symbol: 交易對
            training_config: 訓練配置
            
        Returns:
            訓練結果
        """
        config_str = json.dumps(training_config, indent=2)
        
        query = f"""
        請執行 {symbol} 的 TLOB 模型訓練任務：
        
        訓練配置：
        ```json
        {config_str}
        ```
        
        執行流程：
        1. 準備訓練數據和預處理
        2. 初始化 TLOB 模型
        3. 設置優化器和調度器
        4. 開始訓練循環
        5. 監控訓練指標
        6. 保存最佳模型
        7. 生成訓練報告
        
        請使用工具執行實際訓練並提供詳細反饋。
        """
        
        response = await self.arun(query)
        return {"training_result": response.content, "symbol": symbol}
        
    async def optimize_hyperparameters(
        self,
        symbol: str,
        search_space: Dict[str, Any],
        optimization_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        超參數優化
        
        Args:
            symbol: 交易對
            search_space: 搜索空間
            optimization_config: 優化配置
            
        Returns:
            最優超參數
        """
        search_str = json.dumps(search_space, indent=2)
        config_str = json.dumps(optimization_config, indent=2)
        
        query = f"""
        請對 {symbol} 執行超參數優化：
        
        搜索空間：
        ```json
        {search_str}
        ```
        
        優化配置：
        ```json
        {config_str}
        ```
        
        優化流程：
        1. 設置超參數搜索空間
        2. 選擇優化算法（貝葉斯、網格等）
        3. 定義目標函數（驗證準確率、Sharpe 比等）
        4. 執行優化搜索
        5. 分析超參數敏感性
        6. 推薦最優配置
        
        請提供詳細的優化結果和分析。
        """
        
        response = await self.arun(query)
        return {"optimization_result": response.content, "symbol": symbol}
        
    async def evaluate_model_performance(
        self,
        model_path: str,
        test_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        評估模型性能
        
        Args:
            model_path: 模型路徑
            test_config: 測試配置
            
        Returns:
            性能評估結果
        """
        config_str = json.dumps(test_config, indent=2)
        
        query = f"""
        請評估模型性能：
        
        模型路徑：{model_path}
        
        測試配置：
        ```json
        {config_str}
        ```
        
        評估內容：
        1. **分類性能**：
           - 準確率、F1 分數、混淆矩陣
           - 各類別的精確率和召回率
           - ROC 曲線和 AUC 值
        
        2. **金融性能**：
           - 模擬交易回報
           - Sharpe 比率和最大回撤
           - 勝率和風險調整收益
        
        3. **模型分析**：
           - 預測置信度分布
           - 錯誤分析和失敗案例
           - 特徵重要性分析
        
        請提供詳細的評估報告和改進建議。
        """
        
        response = await self.arun(query)
        return {"evaluation_result": response.content, "model_path": model_path}
        
    async def compare_models(
        self,
        model_configs: List[Dict[str, Any]]
    ) -> Dict[str, Any]:
        """
        比較多個模型
        
        Args:
            model_configs: 模型配置列表
            
        Returns:
            模型比較結果
        """
        configs_str = json.dumps(model_configs, indent=2)
        
        query = f"""
        請比較以下模型的性能：
        
        ```json
        {configs_str}
        ```
        
        比較維度：
        1. **分類性能**：準確率、F1、AUC
        2. **金融性能**：收益率、Sharpe、最大回撤
        3. **效率指標**：訓練時間、推理速度、模型大小
        4. **穩定性**：不同數據集上的性能一致性
        5. **實用性**：部署複雜度、維護成本
        
        請提供模型排名和選擇建議。
        """
        
        response = await self.arun(query)
        return {"comparison_result": response.content}
        
    async def manage_model_versions(
        self,
        symbol: str,
        action: str,
        version_info: Optional[Dict] = None
    ) -> Dict[str, Any]:
        """
        管理模型版本
        
        Args:
            symbol: 交易對
            action: 操作類型（list, create, deploy, rollback）
            version_info: 版本信息
            
        Returns:
            操作結果
        """
        version_str = json.dumps(version_info, indent=2) if version_info else "N/A"
        
        query = f"""
        請執行 {symbol} 的模型版本管理操作：
        
        操作：{action}
        版本信息：
        ```json
        {version_str}
        ```
        
        支持操作：
        - **list**：列出所有模型版本
        - **create**：創建新版本
        - **deploy**：部署指定版本
        - **rollback**：回滾到previous版本
        
        請提供操作結果和版本狀態更新。
        """
        
        response = await self.arun(query)
        return {"version_result": response.content, "symbol": symbol, "action": action}
        
    async def setup_training_pipeline(
        self,
        symbols: List[str],
        schedule_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        設置訓練管道
        
        Args:
            symbols: 交易對列表
            schedule_config: 調度配置
            
        Returns:
            管道設置結果
        """
        symbols_str = ", ".join(symbols)
        config_str = json.dumps(schedule_config, indent=2)
        
        query = f"""
        請為以下交易對設置自動化訓練管道：{symbols_str}
        
        調度配置：
        ```json
        {config_str}
        ```
        
        管道設置：
        1. **數據管道**：定期數據更新和驗證
        2. **訓練調度**：週期性模型重訓練
        3. **評估流程**：自動性能評估
        4. **部署管道**：模型自動部署和回滾
        5. **監控告警**：訓練狀態和性能監控
        
        請設置完整的 MLOps 管道並提供管理界面。
        """
        
        response = await self.arun(query)
        return {"pipeline_result": response.content, "symbols": symbols}
        
    async def diagnose_training_issues(
        self,
        training_log: str,
        error_context: Optional[Dict] = None
    ) -> Dict[str, Any]:
        """
        診斷訓練問題
        
        Args:
            training_log: 訓練日誌
            error_context: 錯誤上下文
            
        Returns:
            診斷結果和解決方案
        """
        context_str = json.dumps(error_context, indent=2) if error_context else "N/A"
        
        query = f"""
        請分析訓練問題並提供解決方案：
        
        訓練日誌：
        ```
        {training_log}
        ```
        
        錯誤上下文：
        ```json
        {context_str}
        ```
        
        診斷流程：
        1. **問題識別**：分析日誌中的錯誤和異常
        2. **根因分析**：確定問題的根本原因
        3. **影響評估**：評估問題的嚴重程度
        4. **解決方案**：提供具體的修復建議
        5. **預防措施**：防止問題再次發生
        
        請提供詳細的診斷報告和行動計劃。
        """
        
        response = await self.arun(query)
        return {"diagnosis_result": response.content}