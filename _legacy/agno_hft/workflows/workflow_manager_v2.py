"""
工作流管理器 v2
==============

基於重新設計的架構，提供更強大的工作流管理能力：
- 統一管理 Agents v2 和 Evaluators v2
- 智能工作流編排和自動恢復
- 實時監控和性能分析
- 參數優化建議和自動調整
- 完整的審計追蹤和合規檢查
"""

from typing import Dict, Any, Optional, List, Type, Callable
import asyncio
import logging
from datetime import datetime, timedelta
import json
import uuid
from dataclasses import dataclass, asdict
from enum import Enum

# 導入 Agents v2
from .agents_v2 import (
    ResearchAnalyst, ModelEngineer, StrategyManager,
    RiskSupervisor, SystemOperator, ComplianceOfficer
)

# 導入 Evaluators v2
from .evaluators_v2 import (
    SystemHealthEvaluator, PerformanceEvaluator, ResearchQualityEvaluator,
    StrategyViabilityEvaluator, ModelQualityEvaluator, DeploymentReadinessEvaluator,
    ComplianceEvaluator, RiskThresholdEvaluator
)

# 導入核心定義 - 恢復原有結構
from .definitions import (
    WorkflowStatus, WorkflowPriority, WorkflowTask, WorkflowSession,
    create_workflow_task, create_workflow_session
)

# 導入工具（使用容錯導入）
try:
    from .tools_v2.parameter_optimization_tools import ParameterOptimizationTools
except ImportError:
    class ParameterOptimizationTools:
        def __init__(self): pass

logger = logging.getLogger(__name__)


class WorkflowPriority(Enum):
    """工作流優先級"""
    LOW = 1
    MEDIUM = 2
    HIGH = 3
    CRITICAL = 4


@dataclass
class WorkflowTask:
    """工作流任務定義"""
    task_id: str
    workflow_id: str
    agent_type: str
    action: str
    parameters: Dict[str, Any]
    dependencies: List[str] = None
    priority: WorkflowPriority = WorkflowPriority.MEDIUM
    timeout_seconds: int = 300
    retry_count: int = 0
    max_retries: int = 3
    status: WorkflowStatus = WorkflowStatus.PENDING
    start_time: Optional[datetime] = None
    end_time: Optional[datetime] = None
    result: Optional[Dict[str, Any]] = None
    error_message: Optional[str] = None


@dataclass 
class WorkflowSession:
    """工作流會話"""
    session_id: str
    workflow_type: str
    symbol: str
    status: WorkflowStatus
    priority: WorkflowPriority
    created_time: datetime
    start_time: Optional[datetime] = None
    end_time: Optional[datetime] = None
    tasks: List[WorkflowTask] = None
    context: Dict[str, Any] = None
    evaluation_results: Dict[str, Any] = None
    performance_metrics: Dict[str, Any] = None
    error_count: int = 0
    recovery_attempts: int = 0


class WorkflowManagerV2:
    """
    Agno Workflows v2 增強工作流管理器
    
    提供企業級工作流管理和監控能力
    """
    
    def __init__(self):
        self.sessions: Dict[str, WorkflowSession] = {}
        self.execution_queue = asyncio.Queue()
        self.running_tasks: Dict[str, asyncio.Task] = {}
        
        # Agent 實例池
        self.agent_pool = {
            "research_analyst": ResearchAnalyst,
            "model_engineer": ModelEngineer, 
            "strategy_manager": StrategyManager,
            "risk_supervisor": RiskSupervisor,
            "system_operator": SystemOperator,
            "compliance_officer": ComplianceOfficer
        }
        
        # Evaluator 實例池
        self.evaluator_pool = {
            "system_health": SystemHealthEvaluator(),
            "performance": PerformanceEvaluator(),
            "research_quality": ResearchQualityEvaluator(),
            "strategy_viability": StrategyViabilityEvaluator(),
            "model_quality": ModelQualityEvaluator(),
            "deployment_readiness": DeploymentReadinessEvaluator(),
            "compliance": ComplianceEvaluator(),
            "risk_threshold": RiskThresholdEvaluator()
        }
        
        # 工具實例
        self.parameter_optimizer = ParameterOptimizationTools()
        
        # 統計和監控
        self.execution_statistics = {
            "total_workflows": 0,
            "successful_workflows": 0,
            "failed_workflows": 0,
            "average_execution_time": 0,
            "performance_trends": []
        }
        
        # 啟動後台任務
        self._start_background_tasks()
        
        logger.info("WorkflowManager v2 initialized with enhanced capabilities")
    
    def _start_background_tasks(self):
        """啟動後台任務"""
        # 工作流執行引擎
        asyncio.create_task(self._workflow_execution_engine())
        # 健康監控
        asyncio.create_task(self._health_monitoring_loop())
        # 性能分析
        asyncio.create_task(self._performance_analysis_loop())
    
    async def create_comprehensive_trading_workflow(
        self,
        symbol: str,
        strategy_config: Dict[str, Any],
        priority: WorkflowPriority = WorkflowPriority.HIGH
    ) -> str:
        """
        創建綜合交易工作流
        
        包含完整的研究、開發、部署、監控流程
        """
        session_id = str(uuid.uuid4())
        
        # 創建工作流會話
        session = WorkflowSession(
            session_id=session_id,
            workflow_type="comprehensive_trading",
            symbol=symbol,
            status=WorkflowStatus.PENDING,
            priority=priority,
            created_time=datetime.now(),
            tasks=[],
            context={
                "symbol": symbol,
                "strategy_config": strategy_config,
                "workflow_start_time": datetime.now().isoformat()
            }
        )
        
        # 定義任務序列
        tasks = [
            # Phase 1: 研究分析
            WorkflowTask(
                task_id=f"{session_id}_research",
                workflow_id=session_id,
                agent_type="research_analyst",
                action="conduct_market_research",
                parameters={"symbol": symbol, "analysis_depth": "comprehensive"},
                priority=priority,
                timeout_seconds=1800  # 30 分鐘
            ),
            
            # Phase 2: 策略可行性評估
            WorkflowTask(
                task_id=f"{session_id}_viability",
                workflow_id=session_id,
                agent_type="evaluator",
                action="evaluate_strategy_viability",
                parameters={"evaluator_type": "strategy_viability"},
                dependencies=[f"{session_id}_research"],
                priority=priority
            ),
            
            # Phase 3: 模型開發
            WorkflowTask(
                task_id=f"{session_id}_model_dev",
                workflow_id=session_id,
                agent_type="model_engineer",
                action="develop_tlob_model",
                parameters={
                    "symbol": symbol,
                    "model_config": strategy_config.get("model_config", {}),
                    "training_data_days": 90
                },
                dependencies=[f"{session_id}_viability"],
                priority=priority,
                timeout_seconds=3600  # 1 小時
            ),
            
            # Phase 4: 模型質量評估
            WorkflowTask(
                task_id=f"{session_id}_model_eval",
                workflow_id=session_id,
                agent_type="evaluator",
                action="evaluate_model_quality",
                parameters={"evaluator_type": "model_quality"},
                dependencies=[f"{session_id}_model_dev"],
                priority=priority
            ),
            
            # Phase 5: 風險參數設置
            WorkflowTask(
                task_id=f"{session_id}_risk_setup",
                workflow_id=session_id,
                agent_type="risk_supervisor",
                action="configure_risk_parameters",
                parameters={
                    "symbol": symbol,
                    "risk_profile": strategy_config.get("risk_profile", "moderate")
                },
                dependencies=[f"{session_id}_model_eval"],
                priority=priority
            ),
            
            # Phase 6: 風險閾值評估
            WorkflowTask(
                task_id=f"{session_id}_risk_eval",
                workflow_id=session_id,
                agent_type="evaluator",
                action="evaluate_risk_thresholds",
                parameters={"evaluator_type": "risk_threshold"},
                dependencies=[f"{session_id}_risk_setup"],
                priority=priority
            ),
            
            # Phase 7: 部署準備
            WorkflowTask(
                task_id=f"{session_id}_deployment_prep",
                workflow_id=session_id,
                agent_type="system_operator",
                action="prepare_deployment",
                parameters={"symbol": symbol, "deployment_type": "production"},
                dependencies=[f"{session_id}_risk_eval"],
                priority=priority
            ),
            
            # Phase 8: 部署就緒度評估
            WorkflowTask(
                task_id=f"{session_id}_deployment_eval",
                workflow_id=session_id,
                agent_type="evaluator",
                action="evaluate_deployment_readiness",
                parameters={"evaluator_type": "deployment_readiness"},
                dependencies=[f"{session_id}_deployment_prep"],
                priority=priority
            ),
            
            # Phase 9: 合規檢查
            WorkflowTask(
                task_id=f"{session_id}_compliance",
                workflow_id=session_id,
                agent_type="compliance_officer",
                action="conduct_compliance_check",
                parameters={"symbol": symbol, "check_type": "pre_deployment"},
                dependencies=[f"{session_id}_deployment_eval"],
                priority=priority
            ),
            
            # Phase 10: 策略上線
            WorkflowTask(
                task_id=f"{session_id}_launch",
                workflow_id=session_id,
                agent_type="strategy_manager",
                action="launch_strategy",
                parameters={
                    "symbol": symbol,
                    "launch_mode": "gradual",
                    "initial_position_size": 0.1
                },
                dependencies=[f"{session_id}_compliance"],
                priority=priority
            )
        ]
        
        session.tasks = tasks
        self.sessions[session_id] = session
        
        # 將會話加入執行隊列
        await self.execution_queue.put(session_id)
        
        logger.info(f"Created comprehensive trading workflow {session_id} for {symbol}")
        return session_id
    
    async def create_model_optimization_workflow(
        self,
        symbol: str,
        current_performance: Dict[str, Any],
        priority: WorkflowPriority = WorkflowPriority.MEDIUM
    ) -> str:
        """
        創建模型優化工作流
        
        專注於現有模型的性能優化
        """
        session_id = str(uuid.uuid4())
        
        session = WorkflowSession(
            session_id=session_id,
            workflow_type="model_optimization",
            symbol=symbol,
            status=WorkflowStatus.PENDING,
            priority=priority,
            created_time=datetime.now(),
            tasks=[],
            context={
                "symbol": symbol,
                "current_performance": current_performance,
                "optimization_target": "sharpe_ratio"
            }
        )
        
        # 定義優化任務序列
        tasks = [
            # Phase 1: 性能分析
            WorkflowTask(
                task_id=f"{session_id}_performance_analysis",
                workflow_id=session_id,
                agent_type="evaluator",
                action="evaluate_performance",
                parameters={
                    "evaluator_type": "performance",
                    "performance_data": current_performance
                },
                priority=priority
            ),
            
            # Phase 2: 參數敏感性分析
            WorkflowTask(
                task_id=f"{session_id}_sensitivity_analysis",
                workflow_id=session_id,
                agent_type="parameter_optimizer",
                action="analyze_parameter_sensitivity",
                parameters={
                    "symbol": symbol,
                    "performance_history": [current_performance],
                    "parameter_name": "signal_threshold"
                },
                dependencies=[f"{session_id}_performance_analysis"],
                priority=priority
            ),
            
            # Phase 3: 貝葉斯優化
            WorkflowTask(
                task_id=f"{session_id}_bayesian_opt",
                workflow_id=session_id,
                agent_type="parameter_optimizer", 
                action="optimize_parameters_bayesian",
                parameters={
                    "symbol": symbol,
                    "current_performance": current_performance,
                    "parameter_category": "trading_parameters",
                    "optimization_target": "sharpe_ratio"
                },
                dependencies=[f"{session_id}_sensitivity_analysis"],
                priority=priority
            ),
            
            # Phase 4: 優化實施
            WorkflowTask(
                task_id=f"{session_id}_implement_optimization",
                workflow_id=session_id,
                agent_type="parameter_optimizer",
                action="implement_parameter_change",
                parameters={
                    "symbol": symbol,
                    "change_strategy": "gradual"
                },
                dependencies=[f"{session_id}_bayesian_opt"],
                priority=priority
            ),
            
            # Phase 5: 結果驗證
            WorkflowTask(
                task_id=f"{session_id}_validation",
                workflow_id=session_id,
                agent_type="strategy_manager",
                action="validate_optimization_results",
                parameters={
                    "symbol": symbol,
                    "validation_period": "1h"
                },
                dependencies=[f"{session_id}_implement_optimization"],
                priority=priority
            )
        ]
        
        session.tasks = tasks
        self.sessions[session_id] = session
        
        await self.execution_queue.put(session_id)
        
        logger.info(f"Created model optimization workflow {session_id} for {symbol}")
        return session_id
    
    async def _workflow_execution_engine(self):
        """工作流執行引擎"""
        while True:
            try:
                # 從隊列中獲取待執行的工作流
                session_id = await self.execution_queue.get()
                
                if session_id in self.sessions:
                    session = self.sessions[session_id]
                    
                    # 創建執行任務
                    execution_task = asyncio.create_task(
                        self._execute_workflow_session(session)
                    )
                    
                    self.running_tasks[session_id] = execution_task
                
                # 短暫休眠避免占用過多資源
                await asyncio.sleep(0.1)
                
            except Exception as e:
                logger.error(f"Workflow execution engine error: {e}")
                await asyncio.sleep(1)
    
    async def _execute_workflow_session(self, session: WorkflowSession):
        """執行單個工作流會話"""
        try:
            session.status = WorkflowStatus.RUNNING
            session.start_time = datetime.now()
            
            logger.info(f"Starting workflow execution for session {session.session_id}")
            
            # 按依賴關係執行任務
            completed_tasks = set()
            
            while len(completed_tasks) < len(session.tasks):
                # 找到可以執行的任務（依賴已完成）
                ready_tasks = []
                for task in session.tasks:
                    if (task.status == WorkflowStatus.PENDING and 
                        (not task.dependencies or 
                         all(dep in completed_tasks for dep in task.dependencies))):
                        ready_tasks.append(task)
                
                if not ready_tasks:
                    # 檢查是否有任務失敗導致死鎖
                    failed_tasks = [t for t in session.tasks if t.status == WorkflowStatus.FAILED]
                    if failed_tasks:
                        logger.error(f"Workflow {session.session_id} blocked by failed tasks")
                        session.status = WorkflowStatus.FAILED
                        break
                    
                    # 等待運行中的任務完成
                    await asyncio.sleep(1)
                    continue
                
                # 並行執行準備好的任務
                execution_tasks = []
                for task in ready_tasks:
                    execution_tasks.append(
                        asyncio.create_task(self._execute_task(task, session))
                    )
                
                # 等待任務完成
                await asyncio.gather(*execution_tasks, return_exceptions=True)
                
                # 更新完成任務集合
                for task in ready_tasks:
                    if task.status == WorkflowStatus.COMPLETED:
                        completed_tasks.add(task.task_id)
            
            # 更新會話狀態
            session.end_time = datetime.now()
            if all(task.status == WorkflowStatus.COMPLETED for task in session.tasks):
                session.status = WorkflowStatus.COMPLETED
                logger.info(f"Workflow {session.session_id} completed successfully")
            else:
                session.status = WorkflowStatus.FAILED
                logger.error(f"Workflow {session.session_id} failed")
            
            # 執行最終評估
            await self._conduct_final_evaluation(session)
            
            # 更新統計信息
            self._update_execution_statistics(session)
            
        except Exception as e:
            logger.error(f"Workflow session execution failed: {e}")
            session.status = WorkflowStatus.FAILED
            session.error_count += 1
        
        finally:
            # 清理運行任務
            if session.session_id in self.running_tasks:
                del self.running_tasks[session.session_id]
    
    async def _execute_task(self, task: WorkflowTask, session: WorkflowSession):
        """執行單個任務"""
        try:
            task.status = WorkflowStatus.RUNNING
            task.start_time = datetime.now()
            
            logger.info(f"Executing task {task.task_id} for workflow {session.session_id}")
            
            # 根據任務類型執行
            if task.agent_type == "evaluator":
                result = await self._execute_evaluator_task(task, session)
            elif task.agent_type == "parameter_optimizer":
                result = await self._execute_optimizer_task(task, session)
            else:
                result = await self._execute_agent_task(task, session)
            
            # 更新任務狀態
            task.end_time = datetime.now()
            task.result = result
            
            if result and result.get("success", True):
                task.status = WorkflowStatus.COMPLETED
                logger.info(f"Task {task.task_id} completed successfully")
            else:
                task.status = WorkflowStatus.FAILED
                task.error_message = result.get("error", "Unknown error") if result else "No result"
                logger.error(f"Task {task.task_id} failed: {task.error_message}")
                
        except Exception as e:
            task.status = WorkflowStatus.FAILED
            task.error_message = str(e)
            task.end_time = datetime.now()
            logger.error(f"Task {task.task_id} execution error: {e}")
    
    async def _execute_evaluator_task(self, task: WorkflowTask, session: WorkflowSession):
        """執行評估器任務"""
        evaluator_type = task.parameters.get("evaluator_type")
        if evaluator_type in self.evaluator_pool:
            evaluator = self.evaluator_pool[evaluator_type]
            
            # 準備評估上下文
            context = {**session.context, **task.parameters}
            
            # 執行評估
            result = await evaluator.evaluate(context)
            
            # 保存評估結果到會話
            if not session.evaluation_results:
                session.evaluation_results = {}
            session.evaluation_results[evaluator_type] = result
            
            return result
        else:
            return {"success": False, "error": f"Unknown evaluator type: {evaluator_type}"}
    
    async def _execute_optimizer_task(self, task: WorkflowTask, session: WorkflowSession):
        """執行參數優化任務"""
        action = task.action
        
        if hasattr(self.parameter_optimizer, action):
            method = getattr(self.parameter_optimizer, action)
            return await method(**task.parameters)
        else:
            return {"success": False, "error": f"Unknown optimizer action: {action}"}
    
    async def _execute_agent_task(self, task: WorkflowTask, session: WorkflowSession):
        """執行 Agent 任務"""
        agent_type = task.agent_type
        
        if agent_type in self.agent_pool:
            agent_class = self.agent_pool[agent_type]
            agent = agent_class(session_id=session.session_id)
            
            # 執行 Agent 動作
            action = task.action
            if hasattr(agent, action):
                method = getattr(agent, action)
                return await method(**task.parameters)
            else:
                return {"success": False, "error": f"Unknown agent action: {action}"}
        else:
            return {"success": False, "error": f"Unknown agent type: {agent_type}"}
    
    async def _conduct_final_evaluation(self, session: WorkflowSession):
        """進行最終評估"""
        try:
            # 系統健康評估
            health_result = await self.evaluator_pool["system_health"].evaluate({
                "health_status": {"system_health": {}},
                "workflow_session": session
            })
            
            # 合規性評估
            compliance_result = await self.evaluator_pool["compliance"].evaluate({
                "compliance_data": {
                    "trading_records": {},
                    "system_status": {}
                },
                "workflow_session": session
            })
            
            if not session.evaluation_results:
                session.evaluation_results = {}
                
            session.evaluation_results["final_health"] = health_result
            session.evaluation_results["final_compliance"] = compliance_result
            
            logger.info(f"Final evaluation completed for session {session.session_id}")
            
        except Exception as e:
            logger.error(f"Final evaluation failed for session {session.session_id}: {e}")
    
    def _update_execution_statistics(self, session: WorkflowSession):
        """更新執行統計"""
        self.execution_statistics["total_workflows"] += 1
        
        if session.status == WorkflowStatus.COMPLETED:
            self.execution_statistics["successful_workflows"] += 1
        elif session.status == WorkflowStatus.FAILED:
            self.execution_statistics["failed_workflows"] += 1
        
        # 計算執行時間
        if session.start_time and session.end_time:
            execution_time = (session.end_time - session.start_time).total_seconds()
            
            # 更新平均執行時間
            total = self.execution_statistics["total_workflows"]
            current_avg = self.execution_statistics["average_execution_time"]
            new_avg = ((current_avg * (total - 1)) + execution_time) / total
            self.execution_statistics["average_execution_time"] = new_avg
            
            # 記錄性能趨勢
            self.execution_statistics["performance_trends"].append({
                "timestamp": session.end_time.isoformat(),
                "execution_time": execution_time,
                "status": session.status.value,
                "workflow_type": session.workflow_type
            })
            
            # 保持最近100條記錄
            if len(self.execution_statistics["performance_trends"]) > 100:
                self.execution_statistics["performance_trends"] = \
                    self.execution_statistics["performance_trends"][-100:]
    
    async def _health_monitoring_loop(self):
        """健康監控循環"""
        while True:
            try:
                # 每5分鐘進行一次健康檢查
                await asyncio.sleep(300)
                
                # 檢查運行中的工作流
                current_time = datetime.now()
                
                for session_id, session in self.sessions.items():
                    if session.status == WorkflowStatus.RUNNING:
                        # 檢查是否超時
                        if (session.start_time and 
                            (current_time - session.start_time).total_seconds() > 7200):  # 2小時超時
                            
                            logger.warning(f"Workflow {session_id} timeout detected")
                            await self._handle_workflow_timeout(session)
                
                logger.debug("Health monitoring cycle completed")
                
            except Exception as e:
                logger.error(f"Health monitoring error: {e}")
                await asyncio.sleep(60)
    
    async def _performance_analysis_loop(self):
        """性能分析循環"""
        while True:
            try:
                # 每15分鐘進行一次性能分析
                await asyncio.sleep(900)
                
                # 分析性能趨勢
                trends = self.execution_statistics["performance_trends"]
                if len(trends) >= 10:
                    recent_trends = trends[-10:]
                    avg_time = sum(t["execution_time"] for t in recent_trends) / len(recent_trends)
                    
                    # 如果平均執行時間增加超過50%，發出警告
                    overall_avg = self.execution_statistics["average_execution_time"]
                    if avg_time > overall_avg * 1.5:
                        logger.warning(f"Performance degradation detected: {avg_time:.2f}s vs {overall_avg:.2f}s")
                
                logger.debug("Performance analysis cycle completed")
                
            except Exception as e:
                logger.error(f"Performance analysis error: {e}")
                await asyncio.sleep(300)
    
    async def _handle_workflow_timeout(self, session: WorkflowSession):
        """處理工作流超時"""
        try:
            logger.warning(f"Handling timeout for workflow {session.session_id}")
            
            # 嘗試優雅停止
            session.status = WorkflowStatus.FAILED
            session.error_count += 1
            session.recovery_attempts += 1
            
            # 如果恢復次數較少，可以嘗試重啟
            if session.recovery_attempts < 2:
                logger.info(f"Attempting recovery for workflow {session.session_id}")
                # 重置任務狀態
                for task in session.tasks:
                    if task.status == WorkflowStatus.RUNNING:
                        task.status = WorkflowStatus.PENDING
                        task.retry_count += 1
                
                # 重新加入執行隊列
                await self.execution_queue.put(session.session_id)
        
        except Exception as e:
            logger.error(f"Error handling workflow timeout: {e}")
    
    def get_session_status(self, session_id: str) -> Dict[str, Any]:
        """獲取會話狀態"""
        if session_id in self.sessions:
            session = self.sessions[session_id]
            
            # 計算進度
            total_tasks = len(session.tasks)
            completed_tasks = sum(1 for task in session.tasks if task.status == WorkflowStatus.COMPLETED)
            progress = completed_tasks / total_tasks if total_tasks > 0 else 0
            
            return {
                "found": True,
                "session": asdict(session),
                "progress": {
                    "completed_tasks": completed_tasks,
                    "total_tasks": total_tasks,
                    "percentage": round(progress * 100, 2)
                },
                "current_tasks": [
                    asdict(task) for task in session.tasks 
                    if task.status == WorkflowStatus.RUNNING
                ]
            }
        else:
            return {"found": False, "reason": f"Session {session_id} not found"}
    
    def get_comprehensive_statistics(self) -> Dict[str, Any]:
        """獲取全面統計信息"""
        stats = self.execution_statistics.copy()
        
        # 添加實時狀態
        active_sessions = sum(1 for s in self.sessions.values() if s.status == WorkflowStatus.RUNNING)
        pending_sessions = sum(1 for s in self.sessions.values() if s.status == WorkflowStatus.PENDING)
        
        stats.update({
            "active_sessions": active_sessions,
            "pending_sessions": pending_sessions,
            "total_sessions": len(self.sessions),
            "success_rate": (stats["successful_workflows"] / max(stats["total_workflows"], 1)) * 100,
            "agent_pool_size": len(self.agent_pool),
            "evaluator_pool_size": len(self.evaluator_pool)
        })
        
        return stats
    
    async def emergency_stop_workflow(self, session_id: str) -> Dict[str, Any]:
        """緊急停止工作流"""
        if session_id in self.sessions:
            session = self.sessions[session_id]
            session.status = WorkflowStatus.CANCELLED
            
            # 停止運行中的任務
            if session_id in self.running_tasks:
                self.running_tasks[session_id].cancel()
                del self.running_tasks[session_id]
            
            logger.warning(f"Emergency stop executed for workflow {session_id}")
            return {"success": True, "message": f"Workflow {session_id} stopped"}
        else:
            return {"success": False, "error": f"Session {session_id} not found"}


# 全局工作流管理器實例
workflow_manager_v2 = WorkflowManagerV2()


async def demo_enhanced_workflows():
    """
    演示增強工作流功能
    """
    print("🚀 Agno Workflows v2 Enhanced Demo")
    print("=" * 50)
    
    # 1. 創建綜合交易工作流
    print("\n📈 1. 創建綜合交易工作流")
    strategy_config = {
        "model_config": {
            "hidden_size": 256,
            "num_layers": 3,
            "dropout": 0.1
        },
        "risk_profile": "aggressive"
    }
    
    session_id = await workflow_manager_v2.create_comprehensive_trading_workflow(
        "ETHUSDT", strategy_config, WorkflowPriority.HIGH
    )
    
    print(f"✅ 綜合交易工作流創建: {session_id}")
    
    # 2. 等待一段時間讓工作流開始執行
    await asyncio.sleep(2)
    
    # 3. 檢查會話狀態
    print("\n📊 2. 檢查工作流狀態")
    status = workflow_manager_v2.get_session_status(session_id)
    if status["found"]:
        progress = status["progress"]
        print(f"   進度: {progress['completed_tasks']}/{progress['total_tasks']} ({progress['percentage']}%)")
        print(f"   狀態: {status['session']['status']}")
        
        if status["current_tasks"]:
            print("   運行中任務:")
            for task in status["current_tasks"]:
                print(f"     - {task['action']} ({task['agent_type']})")
    
    # 4. 創建模型優化工作流
    print("\n🔧 3. 創建模型優化工作流")
    current_performance = {
        "sharpe_ratio": 0.85,
        "max_drawdown": -0.12,
        "win_rate": 0.52
    }
    
    opt_session_id = await workflow_manager_v2.create_model_optimization_workflow(
        "ETHUSDT", current_performance, WorkflowPriority.MEDIUM
    )
    
    print(f"✅ 模型優化工作流創建: {opt_session_id}")
    
    # 5. 獲取綜合統計
    print("\n📈 4. 系統統計信息")
    stats = workflow_manager_v2.get_comprehensive_statistics()
    print(f"   總工作流: {stats['total_workflows']}")
    print(f"   活躍會話: {stats['active_sessions']}")
    print(f"   待處理會話: {stats['pending_sessions']}")
    print(f"   成功率: {stats['success_rate']:.1f}%")
    print(f"   Agent 池大小: {stats['agent_pool_size']}")
    print(f"   Evaluator 池大小: {stats['evaluator_pool_size']}")
    
    print("\n🎉 Enhanced Demo 完成!")


if __name__ == "__main__":
    asyncio.run(demo_enhanced_workflows())