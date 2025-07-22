"""
模型訓練工作流
==============

定義完整的 TLOB 模型訓練工作流：
- 數據準備和驗證
- 模型訓練和評估
- 模型部署和監控
"""

from agno import Workflow
from typing import Dict, Any, Optional
from ..agents import DataAnalyst, ModelTrainer
from ..evaluators import ModelPerformanceEvaluator, DataQualityEvaluator


class TrainingWorkflow(Workflow):
    """
    TLOB 模型訓練工作流
    
    自動化端到端的模型訓練流程
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="TLOBTrainingWorkflow",
            description="端到端 TLOB 模型訓練工作流",
            agents=[
                DataAnalyst(session_id=session_id),
                ModelTrainer(session_id=session_id)
            ],
            evaluators=[
                DataQualityEvaluator(),
                ModelPerformanceEvaluator()
            ],
            session_id=session_id
        )
    
    async def execute(
        self,
        symbol: str,
        training_config: Dict[str, Any],
        data_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        執行訓練工作流
        
        Args:
            symbol: 交易對
            training_config: 訓練配置
            data_config: 數據配置
            
        Returns:
            工作流執行結果
        """
        workflow_results = {
            "workflow_id": f"training_{symbol}_{self.session_id}",
            "symbol": symbol,
            "status": "running",
            "stages": {}
        }
        
        try:
            # 階段 1: 數據質量評估
            print(f"🔍 階段 1: 評估 {symbol} 數據質量...")
            data_analyst = self.get_agent("DataAnalyst")
            
            data_quality_result = await data_analyst.assess_data_quality(
                data_config.get("data_source", "clickhouse"),
                data_config.get("time_range", "30d")
            )
            
            workflow_results["stages"]["data_assessment"] = data_quality_result
            
            # 數據質量評估
            quality_evaluator = self.get_evaluator("DataQualityEvaluator")
            quality_check = await quality_evaluator.evaluate({
                "data_quality": data_quality_result
            })
            
            if not quality_check["passed"]:
                workflow_results["status"] = "failed"
                workflow_results["error"] = "Data quality check failed"
                return workflow_results
            
            # 階段 2: 特徵重要性分析
            print(f"📊 階段 2: 分析 {symbol} 特徵重要性...")
            feature_analysis = await data_analyst.calculate_feature_importance(
                symbol, "price_direction"
            )
            
            workflow_results["stages"]["feature_analysis"] = feature_analysis
            
            # 階段 3: 模型訓練
            print(f"🤖 階段 3: 訓練 {symbol} TLOB 模型...")
            model_trainer = self.get_agent("ModelTrainer")
            
            training_result = await model_trainer.train_tlob_model(
                symbol, training_config
            )
            
            workflow_results["stages"]["training"] = training_result
            
            # 階段 4: 模型評估
            print(f"📈 階段 4: 評估模型性能...")
            model_path = training_result.get("training_result", {}).get("model_path")
            if model_path:
                evaluation_result = await model_trainer.evaluate_model_performance(
                    model_path, {"test_size": 0.2, "validation_split": 0.2}
                )
                
                workflow_results["stages"]["evaluation"] = evaluation_result
                
                # 模型性能評估
                performance_evaluator = self.get_evaluator("ModelPerformanceEvaluator")
                performance_check = await performance_evaluator.evaluate({
                    "model_performance": evaluation_result
                })
                
                if performance_check["passed"]:
                    # 階段 5: 模型版本管理
                    print(f"📦 階段 5: 創建模型版本...")
                    version_result = await model_trainer.manage_model_versions(
                        symbol, "create", {
                            "model_path": model_path,
                            "metrics": evaluation_result.get("evaluation_result", {}),
                            "config": training_config
                        }
                    )
                    
                    workflow_results["stages"]["version_management"] = version_result
                    workflow_results["status"] = "completed"
                    workflow_results["model_ready"] = True
                else:
                    workflow_results["status"] = "failed"
                    workflow_results["error"] = "Model performance evaluation failed"
            else:
                workflow_results["status"] = "failed"
                workflow_results["error"] = "Model training failed - no model path"
            
            return workflow_results
            
        except Exception as e:
            workflow_results["status"] = "failed"
            workflow_results["error"] = str(e)
            return workflow_results
    
    async def schedule_retraining(
        self,
        symbol: str,
        schedule_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        調度自動重訓練
        
        Args:
            symbol: 交易對
            schedule_config: 調度配置
            
        Returns:
            調度結果
        """
        try:
            model_trainer = self.get_agent("ModelTrainer")
            
            pipeline_result = await model_trainer.setup_training_pipeline(
                [symbol], schedule_config
            )
            
            return {
                "success": True,
                "schedule_id": f"schedule_{symbol}_{self.session_id}",
                "pipeline_result": pipeline_result
            }
            
        except Exception as e:
            return {
                "success": False,
                "error": str(e),
                "symbol": symbol
            }