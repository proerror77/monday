#!/usr/bin/env python3
"""
Workflow Definitions - 工作流核心定義
====================================

基於深度架構分析設計的完整工作流定義模塊
整合 ML Training Workflow 和 Workflow Manager 的共享組件

設計模式：
- 分層架構：管理層 + 執行層
- 策略模式：可插拔的工作流實現
- 狀態機：步驟驅動的執行流程
- 命令模式：封裝的任務對象
- 異步任務隊列：解耦創建和執行
"""

import asyncio
import time
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Dict, List, Any, Optional, Tuple, Union
import uuid

# =============================================================================
# 核心枚舉定義
# =============================================================================

class WorkflowStatus(Enum):
    """工作流狀態枚舉"""
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"
    RETRYING = "retrying"
    PAUSED = "paused"

class WorkflowPriority(Enum):
    """工作流優先級枚舉"""
    LOW = "low"
    MEDIUM = "medium"
    HIGH = "high"
    CRITICAL = "critical"
    EMERGENCY = "emergency"

class TrainingStepResult(Enum):
    """訓練步驟結果枚舉"""
    SUCCESS = "success"
    FAILURE = "failure"
    RETRY = "retry"
    SKIP = "skip"

# =============================================================================
# 步驟級別數據結構（用於工作流內部執行）
# =============================================================================

@dataclass
class StepInput:
    """
    Agno 步驟輸入接口
    
    為每個工作流步驟提供標準化的輸入結構
    """
    data: Dict[str, Any] = field(default_factory=dict)
    context: Dict[str, Any] = field(default_factory=dict)
    previous_results: List[Dict[str, Any]] = field(default_factory=list)
    step_name: Optional[str] = None
    execution_id: Optional[str] = None

@dataclass
class StepOutput:
    """
    Agno 步驟輸出接口
    
    封裝步驟執行結果並控制工作流流向
    """
    success: bool
    data: Dict[str, Any] = field(default_factory=dict)
    metadata: Dict[str, Any] = field(default_factory=dict)
    next_step: Optional[str] = None
    error_message: Optional[str] = None
    retry_count: int = 0
    execution_time_seconds: Optional[float] = None
    step_name: Optional[str] = None

# =============================================================================
# 任務級別數據結構（用於工作流管理）
# =============================================================================

@dataclass
class WorkflowTask:
    """
    工作流任務定義
    
    封裝一個可由 Agent 或 Evaluator 執行的高級任務
    """
    task_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    task_name: str = ""
    description: str = ""
    agent_type: str = ""
    action: str = ""
    parameters: Dict[str, Any] = field(default_factory=dict)
    dependencies: List[str] = field(default_factory=list)
    priority: WorkflowPriority = WorkflowPriority.MEDIUM
    status: WorkflowStatus = WorkflowStatus.PENDING
    result: Optional[Dict[str, Any]] = None
    error_message: Optional[str] = None
    created_at: datetime = field(default_factory=datetime.now)
    started_at: Optional[datetime] = None
    completed_at: Optional[datetime] = None
    retry_count: int = 0
    max_retries: int = 3
    timeout_seconds: Optional[int] = None

@dataclass
class WorkflowSession:
    """
    工作流會話定義
    
    封裝一個完整的工作流執行實例
    """
    session_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    workflow_name: str = ""
    description: str = ""
    priority: WorkflowPriority = WorkflowPriority.MEDIUM
    status: WorkflowStatus = WorkflowStatus.PENDING
    tasks: List[WorkflowTask] = field(default_factory=list)
    context: Dict[str, Any] = field(default_factory=dict)
    metadata: Dict[str, Any] = field(default_factory=dict)
    created_at: datetime = field(default_factory=datetime.now)
    started_at: Optional[datetime] = None
    completed_at: Optional[datetime] = None
    created_by: Optional[str] = None
    tags: List[str] = field(default_factory=list)
    
    def add_task(self, task: WorkflowTask) -> None:
        """添加任務到會話"""
        self.tasks.append(task)
    
    def get_task_by_id(self, task_id: str) -> Optional[WorkflowTask]:
        """根據 ID 獲取任務"""
        for task in self.tasks:
            if task.task_id == task_id:
                return task
        return None
    
    def get_completed_tasks(self) -> List[WorkflowTask]:
        """獲取已完成的任務"""
        return [task for task in self.tasks if task.status == WorkflowStatus.COMPLETED]
    
    def get_failed_tasks(self) -> List[WorkflowTask]:
        """獲取失敗的任務"""
        return [task for task in self.tasks if task.status == WorkflowStatus.FAILED]

# =============================================================================
# 抽象基類定義
# =============================================================================

class Workflow(ABC):
    """
    工作流抽象基類
    
    所有具體工作流實現都應繼承此基類
    提供標準化的工作流接口和生命週期管理
    """
    
    def __init__(self, name: str, description: str):
        self.name = name
        self.description = description
        self.workflow_id = str(uuid.uuid4())
        self.status = WorkflowStatus.PENDING
        self.created_at = datetime.now()
        self.started_at: Optional[datetime] = None
        self.completed_at: Optional[datetime] = None
        self.context: Dict[str, Any] = {}
        self.metadata: Dict[str, Any] = {}
        
        # 工作流執行狀態
        self.current_step: Optional[str] = None
        self.step_history: List[Dict[str, Any]] = []
        self.error_message: Optional[str] = None
    
    @abstractmethod
    async def execute(self) -> Dict[str, Any]:
        """
        執行工作流
        
        Returns:
            Dict[str, Any]: 工作流執行結果
        """
        pass
    
    def get_status(self) -> WorkflowStatus:
        """獲取工作流狀態"""
        return self.status
    
    def set_status(self, status: WorkflowStatus) -> None:
        """設置工作流狀態"""
        self.status = status
        if status == WorkflowStatus.RUNNING and not self.started_at:
            self.started_at = datetime.now()
        elif status in [WorkflowStatus.COMPLETED, WorkflowStatus.FAILED, WorkflowStatus.CANCELLED]:
            self.completed_at = datetime.now()
    
    def add_step_to_history(self, step_name: str, step_result: StepOutput) -> None:
        """添加步驟執行記錄到歷史"""
        self.step_history.append({
            "step_name": step_name,
            "timestamp": datetime.now().isoformat(),
            "success": step_result.success,
            "data": step_result.data,
            "metadata": step_result.metadata,
            "error_message": step_result.error_message,
            "execution_time_seconds": step_result.execution_time_seconds
        })
    
    def get_execution_summary(self) -> Dict[str, Any]:
        """獲取執行摘要"""
        return {
            "workflow_id": self.workflow_id,
            "name": self.name,
            "description": self.description,
            "status": self.status.value,
            "created_at": self.created_at.isoformat(),
            "started_at": self.started_at.isoformat() if self.started_at else None,
            "completed_at": self.completed_at.isoformat() if self.completed_at else None,
            "execution_time_seconds": (
                (self.completed_at - self.started_at).total_seconds() 
                if self.started_at and self.completed_at else None
            ),
            "current_step": self.current_step,
            "steps_completed": len([h for h in self.step_history if h["success"]]),
            "steps_failed": len([h for h in self.step_history if not h["success"]]),
            "total_steps": len(self.step_history),
            "error_message": self.error_message,
            "context_keys": list(self.context.keys()),
            "metadata_keys": list(self.metadata.keys())
        }

# =============================================================================
# 工具函數
# =============================================================================

def create_step_input(
    data: Dict[str, Any] = None,
    context: Dict[str, Any] = None,
    previous_results: List[Dict[str, Any]] = None,
    step_name: str = None,
    execution_id: str = None
) -> StepInput:
    """創建標準化的步驟輸入"""
    return StepInput(
        data=data or {},
        context=context or {},
        previous_results=previous_results or [],
        step_name=step_name,
        execution_id=execution_id
    )

def create_step_output(
    success: bool,
    data: Dict[str, Any] = None,
    metadata: Dict[str, Any] = None,
    next_step: str = None,
    error_message: str = None,
    step_name: str = None
) -> StepOutput:
    """創建標準化的步驟輸出"""
    return StepOutput(
        success=success,
        data=data or {},
        metadata=metadata or {},
        next_step=next_step,
        error_message=error_message,
        step_name=step_name
    )

def create_workflow_task(
    task_name: str,
    agent_type: str,
    action: str,
    parameters: Dict[str, Any] = None,
    dependencies: List[str] = None,
    priority: WorkflowPriority = WorkflowPriority.MEDIUM,
    description: str = ""
) -> WorkflowTask:
    """創建標準化的工作流任務"""
    return WorkflowTask(
        task_name=task_name,
        description=description,
        agent_type=agent_type,
        action=action,
        parameters=parameters or {},
        dependencies=dependencies or [],
        priority=priority
    )

def create_workflow_session(
    workflow_name: str,
    description: str = "",
    priority: WorkflowPriority = WorkflowPriority.MEDIUM,
    created_by: str = None,
    tags: List[str] = None
) -> WorkflowSession:
    """創建標準化的工作流會話"""
    return WorkflowSession(
        workflow_name=workflow_name,
        description=description,
        priority=priority,
        created_by=created_by,
        tags=tags or []
    )

# =============================================================================
# 常量定義
# =============================================================================

# 預定義的工作流步驟名稱常量
class WorkflowSteps:
    """常用工作流步驟名稱常量"""
    START = "start"
    END = "end"
    DATA_VALIDATION = "data_validation"
    FEATURE_ENGINEERING = "feature_engineering"
    MODEL_TRAINING = "model_training"
    MODEL_EVALUATION = "model_evaluation"
    QUALITY_GATE_CHECK = "quality_gate_check"
    DEPLOYMENT = "deployment"
    MONITORING = "monitoring"
    CLEANUP = "cleanup"
    ERROR_HANDLING = "error_handling"
    RETRY = "retry"

# 預定義的 Agent 類型常量
class AgentTypes:
    """常用 Agent 類型常量"""
    DATA_ANALYST = "data_analyst"
    MODEL_TRAINER = "model_trainer"
    RISK_MANAGER = "risk_manager"
    SYSTEM_MONITOR = "system_monitor"
    TRADING_STRATEGIST = "trading_strategist"

# 預定義的動作常量
class Actions:
    """常用動作常量"""
    ANALYZE = "analyze"
    TRAIN = "train"
    EVALUATE = "evaluate"
    DEPLOY = "deploy"
    MONITOR = "monitor"
    OPTIMIZE = "optimize"
    VALIDATE = "validate"
    CLEAN = "clean"

# =============================================================================
# 版本信息
# =============================================================================

__version__ = "3.0.0"
__author__ = "UltraThink Agent Architecture"
__description__ = "Complete workflow definitions for Agent-driven ML training system"

# 導出所有公共接口
__all__ = [
    # 枚舉
    "WorkflowStatus",
    "WorkflowPriority", 
    "TrainingStepResult",
    
    # 數據類
    "StepInput",
    "StepOutput",
    "WorkflowTask",
    "WorkflowSession",
    
    # 基類
    "Workflow",
    
    # 工具函數
    "create_step_input",
    "create_step_output",
    "create_workflow_task",
    "create_workflow_session",
    
    # 常量類
    "WorkflowSteps",
    "AgentTypes",
    "Actions"
]