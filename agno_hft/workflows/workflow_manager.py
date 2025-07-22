"""
工作流管理器
============

統一管理所有 Agno Workflows v2 工作流：
- 工作流註冊和發現
- 工作流執行調度
- 結果追蹤和監控
- 錯誤處理和重試
"""

from typing import Dict, Any, Optional, List, Type
import asyncio
import logging
from datetime import datetime
import json
import uuid

from .definitions import TrainingWorkflow, TradingWorkflow
from .agents import DataAnalyst, ModelTrainer, TradingStrategist, RiskManager, SystemMonitor

logger = logging.getLogger(__name__)


class WorkflowManager:
    """
    Agno Workflows v2 工作流管理器
    
    統一管理和調度所有業務工作流
    """
    
    def __init__(self):
        self.workflows = {}
        self.workflow_sessions = {}
        self.execution_history = []
        
        # 註冊工作流
        self._register_workflows()
    
    def _register_workflows(self):
        """註冊所有工作流"""
        self.workflows = {
            "training": TrainingWorkflow,
            "trading": TradingWorkflow
        }
        
        logger.info(f"Registered {len(self.workflows)} workflow types")
    
    async def execute_training_workflow(
        self,
        symbol: str,
        training_config: Dict[str, Any],
        data_config: Optional[Dict[str, Any]] = None
    ) -> Dict[str, Any]:
        """
        執行模型訓練工作流
        
        Args:
            symbol: 交易對
            training_config: 訓練配置
            data_config: 數據配置
            
        Returns:
            工作流執行結果
        """
        try:
            session_id = str(uuid.uuid4())
            workflow = TrainingWorkflow(session_id=session_id)
            
            # 默認數據配置
            data_config = data_config or {
                "data_source": "clickhouse",
                "time_range": "30d",
                "validation_split": 0.2
            }
            
            logger.info(f"Starting training workflow for {symbol} with session {session_id}")
            
            # 執行工作流
            result = await workflow.execute(symbol, training_config, data_config)
            
            # 記錄執行歷史
            execution_record = {
                "workflow_type": "training",
                "session_id": session_id,
                "symbol": symbol,
                "start_time": datetime.now().isoformat(),
                "status": result.get("status"),
                "config": {
                    "training": training_config,
                    "data": data_config
                },
                "result": result
            }
            
            self.execution_history.append(execution_record)
            self.workflow_sessions[session_id] = execution_record
            
            logger.info(f"Training workflow completed with status: {result.get('status')}")
            
            return {
                "success": result.get("status") == "completed",
                "session_id": session_id,
                "workflow_result": result
            }
            
        except Exception as e:
            logger.error(f"Training workflow execution failed: {e}")
            return {
                "success": False,
                "error": str(e),
                "symbol": symbol
            }
    
    async def execute_trading_workflow(
        self,
        symbol: str,
        model_predictions: Dict[str, Any],
        market_context: Optional[Dict[str, Any]] = None
    ) -> Dict[str, Any]:
        """
        執行交易工作流
        
        Args:
            symbol: 交易對
            model_predictions: ML 模型預測
            market_context: 市場環境
            
        Returns:
            工作流執行結果
        """
        try:
            session_id = str(uuid.uuid4())
            workflow = TradingWorkflow(session_id=session_id)
            
            # 默認市場環境
            market_context = market_context or {
                "volatility": 0.025,
                "liquidity": 0.8,
                "spread": 0.001,
                "market_trend": "neutral"
            }
            
            logger.info(f"Starting trading workflow for {symbol} with session {session_id}")
            
            # 執行工作流
            result = await workflow.execute_trading_cycle(symbol, model_predictions, market_context)
            
            # 記錄執行歷史
            execution_record = {
                "workflow_type": "trading",
                "session_id": session_id,
                "symbol": symbol,
                "start_time": datetime.now().isoformat(),
                "status": result.get("status"),
                "predictions": model_predictions,
                "market_context": market_context,
                "result": result
            }
            
            self.execution_history.append(execution_record)
            self.workflow_sessions[session_id] = execution_record
            
            logger.info(f"Trading workflow completed with status: {result.get('status')}")
            
            return {
                "success": result.get("status") in ["ready_to_execute", "completed"],
                "session_id": session_id,
                "workflow_result": result
            }
            
        except Exception as e:
            logger.error(f"Trading workflow execution failed: {e}")
            return {
                "success": False,
                "error": str(e),
                "symbol": symbol
            }
    
    async def monitor_system_health(self) -> Dict[str, Any]:
        """
        監控系統整體健康狀況
        
        Returns:
            系統健康監控結果
        """
        try:
            session_id = str(uuid.uuid4())
            system_monitor = SystemMonitor(session_id=session_id)
            
            logger.info(f"Starting system health monitoring with session {session_id}")
            
            # 執行系統健康檢查
            health_status = await system_monitor.monitor_system_health()
            
            return {
                "success": True,
                "session_id": session_id,
                "health_status": health_status
            }
            
        except Exception as e:
            logger.error(f"System health monitoring failed: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    async def get_workflow_status(self, session_id: str) -> Dict[str, Any]:
        """
        獲取工作流執行狀態
        
        Args:
            session_id: 工作流會話 ID
            
        Returns:
            工作流狀態
        """
        if session_id in self.workflow_sessions:
            session_info = self.workflow_sessions[session_id]
            return {
                "found": True,
                "session_info": session_info
            }
        else:
            return {
                "found": False,
                "reason": f"Session {session_id} not found"
            }
    
    def get_execution_history(
        self,
        workflow_type: Optional[str] = None,
        limit: int = 10
    ) -> List[Dict[str, Any]]:
        """
        獲取工作流執行歷史
        
        Args:
            workflow_type: 工作流類型過濾
            limit: 返回數量限制
            
        Returns:
            執行歷史列表
        """
        history = self.execution_history
        
        # 按工作流類型過濾
        if workflow_type:
            history = [h for h in history if h.get("workflow_type") == workflow_type]
        
        # 按時間排序並限制數量
        history.sort(key=lambda x: x.get("start_time", ""), reverse=True)
        return history[:limit]
    
    def get_workflow_statistics(self) -> Dict[str, Any]:
        """
        獲取工作流統計信息
        
        Returns:
            統計信息
        """
        total_executions = len(self.execution_history)
        
        # 按類型統計
        type_stats = {}
        status_stats = {}
        
        for record in self.execution_history:
            workflow_type = record.get("workflow_type", "unknown")
            status = record.get("status", "unknown")
            
            type_stats[workflow_type] = type_stats.get(workflow_type, 0) + 1
            status_stats[status] = status_stats.get(status, 0) + 1
        
        # 計算成功率
        completed_count = status_stats.get("completed", 0) + status_stats.get("ready_to_execute", 0)
        success_rate = completed_count / total_executions if total_executions > 0 else 0
        
        return {
            "total_executions": total_executions,
            "workflow_types": type_stats,
            "status_distribution": status_stats,
            "success_rate": round(success_rate, 3),
            "active_sessions": len(self.workflow_sessions)
        }
    
    async def create_agent(self, agent_type: str, session_id: Optional[str] = None) -> Any:
        """
        創建 Agent 實例
        
        Args:
            agent_type: Agent 類型
            session_id: 會話 ID
            
        Returns:
            Agent 實例
        """
        agent_classes = {
            "data_analyst": DataAnalyst,
            "model_trainer": ModelTrainer,
            "trading_strategist": TradingStrategist,
            "risk_manager": RiskManager,
            "system_monitor": SystemMonitor
        }
        
        if agent_type not in agent_classes:
            raise ValueError(f"Unknown agent type: {agent_type}")
        
        agent_class = agent_classes[agent_type]
        return agent_class(session_id=session_id)
    
    def get_available_workflows(self) -> List[str]:
        """獲取可用的工作流列表"""
        return list(self.workflows.keys())
    
    def get_available_agents(self) -> List[str]:
        """獲取可用的 Agent 列表"""
        return ["data_analyst", "model_trainer", "trading_strategist", "risk_manager", "system_monitor"]


# 全局工作流管理器實例
workflow_manager = WorkflowManager()


async def demo_workflow_execution():
    """
    演示工作流執行
    """
    print("🚀 Agno Workflows v2 Demo")
    print("=" * 50)
    
    # 1. 執行訓練工作流
    print("\n📚 1. 執行 TLOB 模型訓練工作流")
    training_config = {
        "batch_size": 128,
        "epochs": 100,
        "learning_rate": 0.001,
        "hidden_size": 256,
        "num_layers": 3,
        "dropout": 0.1
    }
    
    training_result = await workflow_manager.execute_training_workflow(
        "BTCUSDT", training_config
    )
    
    print(f"✅ 訓練工作流完成: {training_result['success']}")
    if training_result['success']:
        print(f"   會話ID: {training_result['session_id']}")
    
    # 2. 執行交易工作流
    print("\n💰 2. 執行智能交易工作流")
    model_predictions = {
        "prediction_class": 2,  # 預測上漲
        "confidence": 0.75,
        "probabilities": [0.15, 0.10, 0.75]
    }
    
    trading_result = await workflow_manager.execute_trading_workflow(
        "BTCUSDT", model_predictions
    )
    
    print(f"✅ 交易工作流完成: {trading_result['success']}")
    if trading_result['success']:
        print(f"   會話ID: {trading_result['session_id']}")
        status = trading_result['workflow_result'].get('status')
        print(f"   執行狀態: {status}")
    
    # 3. 系統健康監控
    print("\n🔍 3. 系統健康監控")
    health_result = await workflow_manager.monitor_system_health()
    
    print(f"✅ 健康監控完成: {health_result['success']}")
    
    # 4. 統計信息
    print("\n📊 4. 工作流統計信息")
    stats = workflow_manager.get_workflow_statistics()
    print(f"   總執行次數: {stats['total_executions']}")
    print(f"   成功率: {stats['success_rate']:.1%}")
    print(f"   活躍會話: {stats['active_sessions']}")
    
    print("\n🎉 Demo 完成!")


if __name__ == "__main__":
    asyncio.run(demo_workflow_execution())