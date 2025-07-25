#!/usr/bin/env python3
"""
TLOB模型訓練工作流程 - ML Workspace核心工作流
==================================================

基於PRD第3.1節設計的L3 ML-Agent訓練流水線：
1. 數據收集與預處理
2. 特徵工程與構建
3. 模型訓練與評估  
4. 模型驗證與部署準備

專責：
- 夜間批次訓練任務
- GPU資源密集型操作
- 模型生命週期管理
"""

import asyncio
import logging
from typing import Dict, Any, List, Optional
from datetime import datetime, timedelta
from pathlib import Path
import torch
import pandas as pd
import numpy as np

from agno import Agent, Workflow, Step
from agno.models import Claude
from components.feature_engineering import FeatureEngineer
from components.data_loaders import ClickHouseDataLoader
from components.model_trainer import TLOBModelTrainer
from components.model_evaluator import ModelEvaluator

logger = logging.getLogger(__name__)

class TLOBTrainingWorkflow(Workflow):
    """
    TLOB Transformer模型訓練工作流程
    
    PRD對應：第3.1節 L3 ML-Agent職責
    - 批次訓練：每日凌晨執行
    - GPU需求：8GB+ GPU記憶體
    - 時長：1-3小時典型訓練時間
    """
    
    def __init__(self, config: Dict[str, Any]):
        super().__init__(
            name="TLOB_Training_Pipeline",
            description="Complete TLOB model training and validation workflow"
        )
        
        self.config = config
        self.symbol = config.get("symbol", "BTCUSDT")
        self.training_hours = config.get("training_hours", 24)
        self.model_type = config.get("model_type", "TLOB_transformer")
        
        # 初始化組件
        self.data_loader = ClickHouseDataLoader()
        self.feature_engineer = FeatureEngineer()
        self.model_trainer = TLOBModelTrainer()
        self.model_evaluator = ModelEvaluator()
        
        # 配置工作流程步驟
        self._setup_workflow_steps()
        
        logger.info(f"✅ TLOB訓練工作流程初始化: {self.symbol}")
    
    def _setup_workflow_steps(self):
        """配置工作流程步驟"""
        
        # 步驟1: 數據收集
        self.add_step(
            Step(
                name="data_collection",
                description="從ClickHouse收集歷史市場數據",
                agent=self._create_data_agent(),
                timeout=300,  # 5分鐘
                retries=2
            )
        )
        
        # 步驟2: 特徵工程
        self.add_step(
            Step(
                name="feature_engineering", 
                description="LOB數據特徵提取與工程化",
                agent=self._create_feature_agent(),
                depends_on=["data_collection"],
                timeout=600,  # 10分鐘
                retries=1
            )
        )
        
        # 步驟3: 模型訓練
        self.add_step(
            Step(
                name="model_training",
                description="TLOB Transformer模型訓練",
                agent=self._create_training_agent(),
                depends_on=["feature_engineering"],
                timeout=7200,  # 2小時
                retries=1
            )
        )
        
        # 步驟4: 模型評估
        self.add_step(
            Step(
                name="model_evaluation",
                description="模型性能評估與驗證",
                agent=self._create_evaluation_agent(),
                depends_on=["model_training"],
                timeout=1800,  # 30分鐘
                retries=0
            )
        )
        
        # 步驟5: 部署準備
        self.add_step(
            Step(
                name="deployment_preparation",
                description="模型部署準備與通知",
                agent=self._create_deployment_agent(),
                depends_on=["model_evaluation"],
                timeout=300,  # 5分鐘
                retries=1
            )
        )
    
    def _create_data_agent(self) -> Agent:
        """創建數據收集Agent"""
        return Agent(
            name="DataCollectionAgent",
            model=Claude(id="claude-sonnet-4-20250514"),
            instructions=f"""
            你是數據收集專家，負責從ClickHouse收集{self.symbol}的歷史市場數據。
            
            任務要求：
            1. 收集過去{self.training_hours}小時的LOB數據
            2. 驗證數據完整性和質量
            3. 處理缺失數據和異常值
            4. 確保數據格式符合TLOB模型要求
            
            數據源：ClickHouse hft.orderbook_snapshots表
            質量標準：>95%數據完整性，<1%異常值
            """,
            tools=[self.data_loader]
        )
    
    def _create_feature_agent(self) -> Agent:
        """創建特徵工程Agent"""
        return Agent(
            name="FeatureEngineeringAgent", 
            model=Claude(id="claude-sonnet-4-20250514"),
            instructions=f"""
            你是特徵工程專家，負責將原始LOB數據轉換為TLOB模型特徵。
            
            特徵工程任務：
            1. 訂單簿不平衡特徵提取
            2. 價格動量和趨勢特徵
            3. 成交量加權價格特徵
            4. 時間序列技術指標
            5. 微觀結構特徵
            
            輸出格式：PyTorch Tensor，形狀 (batch_size, seq_len, features)
            目標：為{self.symbol}優化的特徵集合
            """,
            tools=[self.feature_engineer]
        )
    
    def _create_training_agent(self) -> Agent:
        """創建模型訓練Agent"""
        return Agent(
            name="ModelTrainingAgent",
            model=Claude(id="claude-sonnet-4-20250514"), 
            instructions=f"""
            你是TLOB Transformer模型訓練專家。
            
            訓練任務：
            1. 使用Transformer架構處理LOB序列
            2. 實現注意力機制捕捉價格動態
            3. 優化超參數以提升預測性能
            4. 監控訓練過程防止過擬合
            5. 實現早停和學習率調度
            
            性能目標：
            - 信息係數 (IC) > 0.03
            - 信息比率 (IR) > 1.2  
            - 最大回撤 < 5%
            - 預測精度 > 55%
            
            硬體要求：{torch.cuda.device_count()}個GPU可用
            """,
            tools=[self.model_trainer]
        )
    
    def _create_evaluation_agent(self) -> Agent:
        """創建模型評估Agent"""
        return Agent(
            name="ModelEvaluationAgent",
            model=Claude(id="claude-sonnet-4-20250514"),
            instructions=f"""
            你是模型性能評估專家。
            
            評估維度：
            1. 預測精度指標 (Accuracy, Precision, Recall)
            2. 財務收益指標 (Sharpe, IR, Max Drawdown)
            3. 模型穩定性 (不同時期一致性)
            4. 推理效率 (延遲 < 50μs)
            
            部署決策標準：
            ✅ 通過：IC>0.03 AND IR>1.2 AND MaxDD<5%
            ❌ 拒絕：任一指標未達標
            
            輸出：詳細評估報告 + 部署建議
            """,
            tools=[self.model_evaluator]
        )
    
    def _create_deployment_agent(self) -> Agent:
        """創建部署準備Agent"""
        return Agent(
            name="DeploymentAgent",
            model=Claude(id="claude-sonnet-4-20250514"),
            instructions=f"""
            你是模型部署準備專家。
            
            部署準備任務：
            1. 模型序列化為TorchScript格式
            2. 生成模型元數據和配置
            3. 創建部署配置文件
            4. 通知L2 Ops-Agent新模型就緒
            5. 更新Redis ml.deploy channel
            
            通知格式：符合PRD第4節Redis Channel契約
            目標：準備{self.symbol}模型的生產部署
            """,
            tools=["redis_notifier", "model_registry"]
        )
    
    async def execute_training_pipeline(self) -> Dict[str, Any]:
        """
        執行完整的訓練流水線
        
        Returns:
            訓練結果字典
        """
        logger.info(f"🚀 開始執行{self.symbol} TLOB訓練流水線")
        
        start_time = datetime.now()
        
        try:
            # 執行工作流程
            result = await self.execute()
            
            end_time = datetime.now()
            duration = end_time - start_time
            
            if result["success"]:
                logger.info(f"✅ {self.symbol} 訓練流水線完成 - 耗時: {duration}")
                
                # 提取關鍵結果
                training_results = self._extract_training_results(result)
                
                # 通知部署就緒
                await self._notify_deployment_ready(training_results)
                
                return {
                    "success": True,
                    "symbol": self.symbol,
                    "duration_seconds": duration.total_seconds(),
                    "model_path": training_results.get("model_path"),
                    "performance_metrics": training_results.get("metrics"),
                    "deployment_ready": training_results.get("deployment_ready", False)
                }
            else:
                logger.error(f"❌ {self.symbol} 訓練流水線失敗: {result.get('error')}")
                return {
                    "success": False,
                    "symbol": self.symbol,
                    "error": result.get("error"),
                    "duration_seconds": duration.total_seconds()
                }
                
        except Exception as e:
            logger.error(f"❌ 訓練流水線異常: {e}")
            return {
                "success": False,
                "symbol": self.symbol,
                "error": str(e),
                "duration_seconds": (datetime.now() - start_time).total_seconds()
            }
    
    def _extract_training_results(self, workflow_result: Dict[str, Any]) -> Dict[str, Any]:
        """提取訓練結果"""
        results = workflow_result.get("results", [])
        
        if len(results) >= 4:  # 至少完成到評估步驟
            evaluation_result = results[3]  # 第4步是評估
            return evaluation_result.get("evaluation_metrics", {})
        
        return {}
    
    async def _notify_deployment_ready(self, training_results: Dict[str, Any]):
        """通知部署就緒"""
        if training_results.get("deployment_ready"):
            # 發送Redis通知到ml.deploy channel
            notification = {
                "event": "model_ready",
                "symbol": self.symbol,
                "model_path": training_results.get("model_path"),
                "metrics": training_results.get("metrics", {}),
                "timestamp": datetime.now().isoformat()
            }
            
            # TODO: 實現Redis發布邏輯
            logger.info(f"📡 發送模型就緒通知: {notification}")

class MultiAssetTrainingWorkflow(Workflow):
    """
    多資產並行訓練工作流程
    
    同時訓練多個交易對的TLOB模型
    GPU資源調度和優化
    """
    
    def __init__(self, symbols: List[str], config: Dict[str, Any]):
        super().__init__(
            name="Multi_Asset_Training_Pipeline",
            description="Parallel training workflow for multiple trading symbols"
        )
        
        self.symbols = symbols
        self.config = config
        self.training_workflows = {}
        
        # 為每個交易對創建獨立的訓練工作流程
        for symbol in symbols:
            symbol_config = config.copy()
            symbol_config["symbol"] = symbol
            self.training_workflows[symbol] = TLOBTrainingWorkflow(symbol_config)
        
        logger.info(f"✅ 多資產訓練工作流程初始化: {symbols}")
    
    async def execute_parallel_training(self) -> Dict[str, Any]:
        """
        執行並行訓練
        
        Returns:
            所有資產的訓練結果
        """
        logger.info(f"🔄 開始並行訓練 {len(self.symbols)} 個資產")
        
        # 創建並行任務
        training_tasks = [
            workflow.execute_training_pipeline()
            for workflow in self.training_workflows.values()
        ]
        
        # 等待所有訓練完成
        results = await asyncio.gather(*training_tasks, return_exceptions=True)
        
        # 整理結果
        final_results = {}
        successful_count = 0
        
        for symbol, result in zip(self.symbols, results):
            if isinstance(result, Exception):
                final_results[symbol] = {
                    "success": False,
                    "error": str(result)
                }
            else:
                final_results[symbol] = result
                if result.get("success"):
                    successful_count += 1
        
        logger.info(f"✅ 並行訓練完成: {successful_count}/{len(self.symbols)} 成功")
        
        return {
            "multi_training_completed": True,
            "total_symbols": len(self.symbols),
            "successful_symbols": successful_count,
            "results": final_results,
            "completion_time": datetime.now().isoformat()
        }

# ==========================================
# 工作流程工廠函數
# ==========================================

async def create_training_workflow(symbol: str, config: Dict[str, Any]) -> TLOBTrainingWorkflow:
    """創建單資產訓練工作流程"""
    config["symbol"] = symbol
    return TLOBTrainingWorkflow(config)

async def create_multi_asset_workflow(symbols: List[str], config: Dict[str, Any]) -> MultiAssetTrainingWorkflow:
    """創建多資產訓練工作流程"""
    return MultiAssetTrainingWorkflow(symbols, config)

# ==========================================
# CLI入口點
# ==========================================

async def main():
    """ML Workspace訓練工作流程入口點"""
    import argparse
    
    parser = argparse.ArgumentParser(description="TLOB Model Training Workflow")
    parser.add_argument("--symbol", required=True, help="Trading symbol (e.g., BTCUSDT)")
    parser.add_argument("--hours", type=int, default=24, help="Training data hours")
    parser.add_argument("--config", help="Training configuration file")
    
    args = parser.parse_args()
    
    config = {
        "symbol": args.symbol,
        "training_hours": args.hours,
        "model_type": "TLOB_transformer"
    }
    
    # 創建並執行訓練工作流程
    workflow = await create_training_workflow(args.symbol, config)
    result = await workflow.execute_training_pipeline()
    
    print(f"Training Result: {result}")
    return 0 if result["success"] else 1

if __name__ == "__main__":
    exit_code = asyncio.run(main())
    exit(exit_code)