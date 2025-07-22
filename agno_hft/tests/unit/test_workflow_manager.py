"""
工作流管理器單元測試
==================

測試 WorkflowManagerV2 的核心功能
"""

import pytest
import asyncio
from unittest.mock import AsyncMock, MagicMock, patch
from datetime import datetime

from workflows.workflow_manager_v2 import WorkflowManagerV2, WorkflowStatus, WorkflowPriority


@pytest.mark.unit
class TestWorkflowManagerV2:
    """工作流管理器 v2 測試類"""
    
    def test_initialization(self):
        """測試工作流管理器初始化"""
        manager = WorkflowManagerV2()
        
        # 驗證基本屬性
        assert len(manager.sessions) == 0
        assert len(manager.agent_pool) == 6  # 6個 Agent
        assert len(manager.evaluator_pool) == 8  # 8個 Evaluator
        assert manager.parameter_optimizer is not None
        
        # 驗證 Agent 池
        expected_agents = [
            "research_analyst", "model_engineer", "strategy_manager",
            "risk_supervisor", "system_operator", "compliance_officer"
        ]
        assert all(agent in manager.agent_pool for agent in expected_agents)
        
        # 驗證 Evaluator 池
        expected_evaluators = [
            "system_health", "performance", "research_quality",
            "strategy_viability", "model_quality", "deployment_readiness",
            "compliance", "risk_threshold"
        ]
        assert all(evaluator in manager.evaluator_pool for evaluator in expected_evaluators)
    
    @pytest.mark.asyncio
    async def test_create_comprehensive_trading_workflow(self, mock_workflow_manager):
        """測試創建綜合交易工作流"""
        strategy_config = {
            "model_config": {"hidden_size": 256},
            "risk_profile": "moderate"
        }
        
        # 創建工作流
        session_id = await mock_workflow_manager.create_comprehensive_trading_workflow(
            "BTCUSDT", strategy_config, WorkflowPriority.HIGH
        )
        
        # 驗證結果
        assert session_id is not None
        assert session_id in mock_workflow_manager.sessions
        
        session = mock_workflow_manager.sessions[session_id]
        assert session.workflow_type == "comprehensive_trading"
        assert session.symbol == "BTCUSDT"
        assert session.priority == WorkflowPriority.HIGH
        assert session.status == WorkflowStatus.PENDING
        assert len(session.tasks) > 0
    
    @pytest.mark.asyncio
    async def test_create_model_optimization_workflow(self, mock_workflow_manager):
        """測試創建模型優化工作流"""
        current_performance = {
            "sharpe_ratio": 0.8,
            "max_drawdown": -0.12,
            "win_rate": 0.45
        }
        
        # 創建優化工作流
        session_id = await mock_workflow_manager.create_model_optimization_workflow(
            "ETHUSDT", current_performance, WorkflowPriority.MEDIUM
        )
        
        # 驗證結果
        assert session_id is not None
        assert session_id in mock_workflow_manager.sessions
        
        session = mock_workflow_manager.sessions[session_id]
        assert session.workflow_type == "model_optimization"
        assert session.symbol == "ETHUSDT"
        assert session.priority == WorkflowPriority.MEDIUM
        assert len(session.tasks) == 5  # 優化工作流有5個任務
    
    def test_get_session_status_found(self, mock_workflow_manager):
        """測試獲取存在的會話狀態"""
        # 創建測試會話
        from workflows.workflow_manager_v2 import WorkflowSession
        
        session = WorkflowSession(
            session_id="test_session",
            workflow_type="test",
            symbol="BTCUSDT",
            status=WorkflowStatus.RUNNING,
            priority=WorkflowPriority.HIGH,
            created_time=datetime.now(),
            tasks=[]
        )
        mock_workflow_manager.sessions["test_session"] = session
        
        # 獲取狀態
        status = mock_workflow_manager.get_session_status("test_session")
        
        # 驗證結果
        assert status["found"] is True
        assert "session" in status
        assert "progress" in status
        assert status["progress"]["total_tasks"] == 0
    
    def test_get_session_status_not_found(self, mock_workflow_manager):
        """測試獲取不存在的會話狀態"""
        status = mock_workflow_manager.get_session_status("nonexistent_session")
        
        assert status["found"] is False
        assert "reason" in status
    
    def test_get_comprehensive_statistics_empty(self, mock_workflow_manager):
        """測試空系統的統計信息"""
        stats = mock_workflow_manager.get_comprehensive_statistics()
        
        # 驗證基本統計
        assert stats["total_workflows"] == 0
        assert stats["successful_workflows"] == 0
        assert stats["failed_workflows"] == 0
        assert stats["active_sessions"] == 0
        assert stats["pending_sessions"] == 0
        assert stats["total_sessions"] == 0
        assert stats["success_rate"] == 0
        assert stats["agent_pool_size"] == 6
        assert stats["evaluator_pool_size"] == 8
    
    def test_get_comprehensive_statistics_with_data(self, mock_workflow_manager):
        """測試有數據的統計信息"""
        # 添加統計數據
        mock_workflow_manager.execution_statistics = {
            "total_workflows": 10,
            "successful_workflows": 8,
            "failed_workflows": 2,
            "average_execution_time": 45.5,
            "performance_trends": []
        }
        
        # 添加測試會話
        from workflows.workflow_manager_v2 import WorkflowSession
        
        session1 = WorkflowSession(
            session_id="session1",
            workflow_type="test",
            symbol="BTCUSDT",
            status=WorkflowStatus.RUNNING,
            priority=WorkflowPriority.HIGH,
            created_time=datetime.now(),
            tasks=[]
        )
        session2 = WorkflowSession(
            session_id="session2", 
            workflow_type="test",
            symbol="ETHUSDT",
            status=WorkflowStatus.PENDING,
            priority=WorkflowPriority.MEDIUM,
            created_time=datetime.now(),
            tasks=[]
        )
        
        mock_workflow_manager.sessions = {
            "session1": session1,
            "session2": session2
        }
        
        # 獲取統計信息
        stats = mock_workflow_manager.get_comprehensive_statistics()
        
        # 驗證結果
        assert stats["total_workflows"] == 10
        assert stats["successful_workflows"] == 8
        assert stats["failed_workflows"] == 2
        assert stats["success_rate"] == 80.0
        assert stats["active_sessions"] == 1
        assert stats["pending_sessions"] == 1
        assert stats["total_sessions"] == 2
    
    @pytest.mark.asyncio
    async def test_emergency_stop_workflow_exists(self, mock_workflow_manager):
        """測試緊急停止存在的工作流"""
        # 創建測試會話
        from workflows.workflow_manager_v2 import WorkflowSession
        
        session = WorkflowSession(
            session_id="test_session",
            workflow_type="test",
            symbol="BTCUSDT", 
            status=WorkflowStatus.RUNNING,
            priority=WorkflowPriority.HIGH,
            created_time=datetime.now(),
            tasks=[]
        )
        mock_workflow_manager.sessions["test_session"] = session
        
        # 執行緊急停止
        result = await mock_workflow_manager.emergency_stop_workflow("test_session")
        
        # 驗證結果
        assert result["success"] is True
        assert "Workflow test_session stopped" in result["message"]
        assert session.status == WorkflowStatus.CANCELLED
    
    @pytest.mark.asyncio 
    async def test_emergency_stop_workflow_not_exists(self, mock_workflow_manager):
        """測試緊急停止不存在的工作流"""
        result = await mock_workflow_manager.emergency_stop_workflow("nonexistent")
        
        assert result["success"] is False
        assert "not found" in result["error"]


@pytest.mark.unit
@pytest.mark.performance
class TestWorkflowManagerPerformance:
    """工作流管理器性能測試"""
    
    @pytest.mark.asyncio
    async def test_workflow_creation_performance(self, mock_workflow_manager):
        """測試工作流創建性能"""
        import time
        
        strategy_config = {"model_config": {"hidden_size": 128}}
        
        # 測試創建時間
        start_time = time.time()
        session_id = await mock_workflow_manager.create_comprehensive_trading_workflow(
            "BTCUSDT", strategy_config, WorkflowPriority.HIGH
        )
        end_time = time.time()
        
        # 創建應該在 1 秒內完成
        creation_time = end_time - start_time
        assert creation_time < 1.0
        assert session_id is not None
    
    def test_statistics_calculation_performance(self, mock_workflow_manager):
        """測試統計計算性能"""
        import time
        
        # 添加大量會話數據
        from workflows.workflow_manager_v2 import WorkflowSession
        
        for i in range(100):
            session = WorkflowSession(
                session_id=f"session_{i}",
                workflow_type="test",
                symbol="BTCUSDT",
                status=WorkflowStatus.COMPLETED if i % 2 == 0 else WorkflowStatus.FAILED,
                priority=WorkflowPriority.MEDIUM,
                created_time=datetime.now(),
                tasks=[]
            )
            mock_workflow_manager.sessions[f"session_{i}"] = session
        
        # 測試統計計算時間
        start_time = time.time()
        stats = mock_workflow_manager.get_comprehensive_statistics()
        end_time = time.time()
        
        # 統計計算應該在 0.1 秒內完成
        calculation_time = end_time - start_time
        assert calculation_time < 0.1
        assert stats["total_sessions"] == 100


@pytest.mark.unit
class TestWorkflowTask:
    """工作流任務測試"""
    
    def test_task_creation(self):
        """測試任務創建"""
        from workflows.workflow_manager_v2 import WorkflowTask
        
        task = WorkflowTask(
            task_id="test_task",
            workflow_id="test_workflow",
            agent_type="strategy_manager",
            action="monitor_performance",
            parameters={"symbol": "BTCUSDT"},
            priority=WorkflowPriority.HIGH
        )
        
        # 驗證任務屬性
        assert task.task_id == "test_task"
        assert task.workflow_id == "test_workflow"
        assert task.agent_type == "strategy_manager" 
        assert task.action == "monitor_performance"
        assert task.parameters["symbol"] == "BTCUSDT"
        assert task.priority == WorkflowPriority.HIGH
        assert task.status == WorkflowStatus.PENDING
        assert task.retry_count == 0
        assert task.max_retries == 3
    
    def test_task_with_dependencies(self):
        """測試帶依賴的任務"""
        from workflows.workflow_manager_v2 import WorkflowTask
        
        task = WorkflowTask(
            task_id="dependent_task",
            workflow_id="test_workflow",
            agent_type="model_engineer",
            action="deploy_model",
            parameters={},
            dependencies=["task1", "task2"],
            priority=WorkflowPriority.MEDIUM
        )
        
        assert task.dependencies == ["task1", "task2"]
        assert task.status == WorkflowStatus.PENDING


@pytest.mark.unit
class TestWorkflowSession:
    """工作流會話測試"""
    
    def test_session_creation(self):
        """測試會話創建"""
        from workflows.workflow_manager_v2 import WorkflowSession
        
        session = WorkflowSession(
            session_id="test_session",
            workflow_type="comprehensive_trading",
            symbol="BTCUSDT",
            status=WorkflowStatus.PENDING,
            priority=WorkflowPriority.HIGH,
            created_time=datetime.now()
        )
        
        # 驗證會話屬性
        assert session.session_id == "test_session"
        assert session.workflow_type == "comprehensive_trading"
        assert session.symbol == "BTCUSDT"
        assert session.status == WorkflowStatus.PENDING
        assert session.priority == WorkflowPriority.HIGH
        assert session.error_count == 0
        assert session.recovery_attempts == 0
    
    def test_session_with_tasks(self):
        """測試帶任務的會話"""
        from workflows.workflow_manager_v2 import WorkflowSession, WorkflowTask
        
        task1 = WorkflowTask(
            task_id="task1",
            workflow_id="session1",
            agent_type="research_analyst", 
            action="conduct_research",
            parameters={}
        )
        
        task2 = WorkflowTask(
            task_id="task2",
            workflow_id="session1",
            agent_type="model_engineer",
            action="train_model",
            parameters={},
            dependencies=["task1"]
        )
        
        session = WorkflowSession(
            session_id="session1",
            workflow_type="comprehensive_trading",
            symbol="BTCUSDT",
            status=WorkflowStatus.PENDING,
            priority=WorkflowPriority.HIGH,
            created_time=datetime.now(),
            tasks=[task1, task2]
        )
        
        assert len(session.tasks) == 2
        assert session.tasks[0].task_id == "task1"
        assert session.tasks[1].dependencies == ["task1"]