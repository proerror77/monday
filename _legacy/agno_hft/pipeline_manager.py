#!/usr/bin/env python3
"""
多Pipeline管理器
支持同時運行多個商品的交易Pipeline，統一監控和管理
"""

import asyncio
import logging
from typing import Dict, List, Optional, Any
from dataclasses import dataclass
from enum import Enum
import uuid
from datetime import datetime
from hft_agents import HFTTeam

logger = logging.getLogger(__name__)

class PipelineStatus(Enum):
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"

class PipelineType(Enum):
    TRAINING = "training"
    EVALUATION = "evaluation"
    DRYRUN = "dryrun"
    LIVE_TRADING = "live_trading"
    FULL_WORKFLOW = "full_workflow"

@dataclass
class PipelineTask:
    id: str
    pipeline_type: PipelineType
    symbol: str
    parameters: Dict[str, Any]
    status: PipelineStatus = PipelineStatus.PENDING
    created_at: datetime = None
    started_at: Optional[datetime] = None
    completed_at: Optional[datetime] = None
    result: Optional[Dict[str, Any]] = None
    error_message: Optional[str] = None

    def __post_init__(self):
        if self.created_at is None:
            self.created_at = datetime.now()

class PipelineManager:
    """多Pipeline管理器"""
    
    def __init__(self):
        self.hft_team = HFTTeam()
        self.active_tasks: Dict[str, PipelineTask] = {}
        self.task_futures: Dict[str, asyncio.Task] = {}
        self.max_concurrent_tasks = 5
        
    async def submit_task(self, pipeline_type: PipelineType, symbol: str, **parameters) -> str:
        """提交新的Pipeline任務"""
        task_id = str(uuid.uuid4())[:8]
        
        task = PipelineTask(
            id=task_id,
            pipeline_type=pipeline_type,
            symbol=symbol,
            parameters=parameters
        )
        
        self.active_tasks[task_id] = task
        
        # 如果當前運行任務數未達到限制，立即執行
        if len([t for t in self.active_tasks.values() if t.status == PipelineStatus.RUNNING]) < self.max_concurrent_tasks:
            await self._start_task(task_id)
        
        logger.info(f"📋 提交任務 {task_id}: {pipeline_type.value} - {symbol}")
        return task_id
    
    async def _start_task(self, task_id: str):
        """啟動Pipeline任務"""
        task = self.active_tasks[task_id]
        task.status = PipelineStatus.RUNNING
        task.started_at = datetime.now()
        
        # 創建異步任務
        future = asyncio.create_task(self._execute_task(task))
        self.task_futures[task_id] = future
        
        logger.info(f"🚀 啟動任務 {task_id}: {task.pipeline_type.value} - {task.symbol}")
    
    async def _execute_task(self, task: PipelineTask):
        """執行具體的Pipeline任務"""
        try:
            logger.info(f"⚙️ 執行任務 {task.id}: {task.pipeline_type.value}")
            
            if task.pipeline_type == PipelineType.TRAINING:
                result = await self.hft_team.training_agent.train_model(
                    task.symbol, 
                    **task.parameters
                )
            
            elif task.pipeline_type == PipelineType.EVALUATION:
                result = await self.hft_team.evaluation_agent.evaluate_model(
                    task.parameters.get('model_path'),
                    task.symbol,
                    **{k: v for k, v in task.parameters.items() if k != 'model_path'}
                )
            
            elif task.pipeline_type == PipelineType.DRYRUN:
                result = await self.hft_team.rust_tools.start_dryrun(
                    task.symbol,
                    **task.parameters
                )
            
            elif task.pipeline_type == PipelineType.LIVE_TRADING:
                result = await self.hft_team.rust_tools.start_live_trading(
                    task.symbol,
                    **task.parameters
                )
            
            elif task.pipeline_type == PipelineType.FULL_WORKFLOW:
                result = await self.hft_team.execute_full_workflow(
                    task.symbol,
                    **task.parameters
                )
            
            else:
                raise ValueError(f"未知的Pipeline類型: {task.pipeline_type}")
            
            # 更新任務狀態
            task.result = result
            task.status = PipelineStatus.COMPLETED if result.get('success', False) else PipelineStatus.FAILED
            task.completed_at = datetime.now()
            
            if not result.get('success', False):
                task.error_message = result.get('error_message', '未知錯誤')
            
            logger.info(f"✅ 任務完成 {task.id}: {task.status.value}")
            
        except Exception as e:
            task.status = PipelineStatus.FAILED
            task.error_message = str(e)
            task.completed_at = datetime.now()
            logger.error(f"❌ 任務失敗 {task.id}: {e}")
        
        finally:
            # 清理futures
            if task.id in self.task_futures:
                del self.task_futures[task.id]
            
            # 嘗試啟動下一個等待的任務
            await self._start_next_pending_task()
    
    async def _start_next_pending_task(self):
        """啟動下一個等待的任務"""
        pending_tasks = [t for t in self.active_tasks.values() if t.status == PipelineStatus.PENDING]
        running_tasks = [t for t in self.active_tasks.values() if t.status == PipelineStatus.RUNNING]
        
        if pending_tasks and len(running_tasks) < self.max_concurrent_tasks:
            # 按創建時間排序，先進先出
            next_task = min(pending_tasks, key=lambda t: t.created_at)
            await self._start_task(next_task.id)
    
    async def cancel_task(self, task_id: str) -> bool:
        """取消任務"""
        if task_id not in self.active_tasks:
            return False
        
        task = self.active_tasks[task_id]
        
        if task.status == PipelineStatus.RUNNING:
            # 取消異步任務
            if task_id in self.task_futures:
                self.task_futures[task_id].cancel()
                del self.task_futures[task_id]
        
        task.status = PipelineStatus.CANCELLED
        task.completed_at = datetime.now()
        
        logger.info(f"🚫 取消任務 {task_id}")
        return True
    
    def get_task_status(self, task_id: str) -> Optional[PipelineTask]:
        """獲取任務狀態"""
        return self.active_tasks.get(task_id)
    
    def get_all_tasks(self) -> List[PipelineTask]:
        """獲取所有任務"""
        return list(self.active_tasks.values())
    
    def get_tasks_by_symbol(self, symbol: str) -> List[PipelineTask]:
        """獲取特定商品的所有任務"""
        return [t for t in self.active_tasks.values() if t.symbol == symbol]
    
    def get_running_tasks(self) -> List[PipelineTask]:
        """獲取正在運行的任務"""
        return [t for t in self.active_tasks.values() if t.status == PipelineStatus.RUNNING]
    
    async def stop_all_tasks(self):
        """停止所有任務"""
        for task_id in list(self.task_futures.keys()):
            await self.cancel_task(task_id)
        
        logger.info("🛑 已停止所有Pipeline任務")
    
    def get_system_overview(self) -> Dict[str, Any]:
        """獲取系統總覽"""
        tasks = list(self.active_tasks.values())
        
        status_counts = {}
        for status in PipelineStatus:
            status_counts[status.value] = len([t for t in tasks if t.status == status])
        
        symbol_counts = {}
        for task in tasks:
            symbol_counts[task.symbol] = symbol_counts.get(task.symbol, 0) + 1
        
        return {
            "total_tasks": len(tasks),
            "status_breakdown": status_counts,
            "symbol_breakdown": symbol_counts,
            "running_tasks": len(self.get_running_tasks()),
            "max_concurrent": self.max_concurrent_tasks
        }

# 全局Pipeline管理器實例
pipeline_manager = PipelineManager()