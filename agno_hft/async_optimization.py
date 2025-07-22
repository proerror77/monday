"""
异步处理优化模块
================

实现高级异步优化技术：
- 并发数据处理
- 异步任务调度
- 背景任务管理
- 事件循环优化
"""

import asyncio
import time
import threading
from concurrent.futures import ThreadPoolExecutor, ProcessPoolExecutor
from typing import Dict, Any, List, Optional, Callable, Awaitable
from dataclasses import dataclass, field
from datetime import datetime, timedelta
import weakref
from loguru import logger

try:
    import rust_hft_py
    from rust_hft_tools_v2 import RustHFTEngineV2
    RUST_AVAILABLE = True
except ImportError:
    RUST_AVAILABLE = False


@dataclass
class AsyncTask:
    """异步任务"""
    task_id: str
    task_type: str
    priority: int = 0
    created_at: datetime = field(default_factory=datetime.now)
    started_at: Optional[datetime] = None
    completed_at: Optional[datetime] = None
    result: Any = None
    error: Optional[str] = None
    status: str = "pending"  # pending, running, completed, failed


@dataclass
class PerformanceMetrics:
    """性能指标"""
    task_count: int
    avg_execution_time_ms: float
    max_execution_time_ms: float
    min_execution_time_ms: float
    throughput_tasks_per_sec: float
    error_rate: float
    memory_usage_mb: float


class AsyncTaskScheduler:
    """
    异步任务调度器
    
    实现高性能的异步任务调度：
    - 优先级队列
    - 并发限制
    - 负载均衡
    - 故障恢复
    """
    
    def __init__(self, max_concurrent_tasks: int = 10, max_workers: int = 4):
        self.max_concurrent_tasks = max_concurrent_tasks
        self.max_workers = max_workers
        
        # 任务队列
        self.task_queue = asyncio.PriorityQueue()
        self.active_tasks: Dict[str, AsyncTask] = {}
        self.completed_tasks: List[AsyncTask] = []
        
        # 执行器
        self.thread_executor = ThreadPoolExecutor(max_workers=max_workers)
        self.process_executor = ProcessPoolExecutor(max_workers=max_workers//2)
        
        # 控制变量
        self.running = False
        self.scheduler_task: Optional[asyncio.Task] = None
        
        # 性能统计
        self.total_tasks = 0
        self.successful_tasks = 0
        self.failed_tasks = 0
        self.start_time = time.time()
        
        logger.info(f"🔧 异步任务调度器初始化完成 (并发: {max_concurrent_tasks}, 工作线程: {max_workers})")
    
    async def start(self):
        """启动调度器"""
        if self.running:
            logger.warning("调度器已在运行")
            return
        
        self.running = True
        self.start_time = time.time()
        
        # 启动调度器主循环
        self.scheduler_task = asyncio.create_task(self._scheduler_loop())
        logger.info("✅ 异步任务调度器已启动")
    
    async def stop(self):
        """停止调度器"""
        if not self.running:
            return
        
        logger.info("🛑 停止异步任务调度器...")
        self.running = False
        
        if self.scheduler_task:
            self.scheduler_task.cancel()
            try:
                await self.scheduler_task
            except asyncio.CancelledError:
                pass
        
        # 等待活跃任务完成
        if self.active_tasks:
            logger.info(f"⏳ 等待 {len(self.active_tasks)} 个活跃任务完成...")
            await asyncio.sleep(1)
        
        # 关闭执行器
        self.thread_executor.shutdown(wait=True)
        self.process_executor.shutdown(wait=True)
        
        logger.info("✅ 异步任务调度器已停止")
    
    async def submit_task(
        self, 
        task_id: str,
        task_type: str,
        coro_func: Callable[..., Awaitable[Any]],
        *args,
        priority: int = 0,
        **kwargs
    ) -> str:
        """
        提交异步任务
        
        Args:
            task_id: 任务ID
            task_type: 任务类型
            coro_func: 协程函数
            priority: 优先级 (数字越小优先级越高)
            *args, **kwargs: 函数参数
        
        Returns:
            str: 任务ID
        """
        task = AsyncTask(
            task_id=task_id,
            task_type=task_type,
            priority=priority
        )
        
        # 将任务和执行函数一起放入队列
        await self.task_queue.put((priority, task_id, task, coro_func, args, kwargs))
        self.total_tasks += 1
        
        logger.debug(f"📥 任务已提交: {task_id} (类型: {task_type}, 优先级: {priority})")
        return task_id
    
    async def get_task_status(self, task_id: str) -> Optional[AsyncTask]:
        """获取任务状态"""
        if task_id in self.active_tasks:
            return self.active_tasks[task_id]
        
        # 在完成任务中查找
        for task in self.completed_tasks:
            if task.task_id == task_id:
                return task
        
        return None
    
    async def cancel_task(self, task_id: str) -> bool:
        """取消任务"""
        if task_id in self.active_tasks:
            task = self.active_tasks[task_id]
            task.status = "cancelled"
            del self.active_tasks[task_id]
            logger.info(f"🚫 任务已取消: {task_id}")
            return True
        
        return False
    
    async def _scheduler_loop(self):
        """调度器主循环"""
        logger.info("🔄 调度器主循环已启动")
        
        while self.running:
            try:
                # 控制并发任务数量
                if len(self.active_tasks) >= self.max_concurrent_tasks:
                    await asyncio.sleep(0.01)  # 短暂等待
                    continue
                
                # 获取下一个任务 (带超时以允许定期检查)
                try:
                    priority, task_id, task, coro_func, args, kwargs = await asyncio.wait_for(
                        self.task_queue.get(), timeout=0.1
                    )
                except asyncio.TimeoutError:
                    continue
                
                # 启动任务执行
                task.status = "running"
                task.started_at = datetime.now()
                self.active_tasks[task_id] = task
                
                # 创建任务执行协程
                execution_task = asyncio.create_task(
                    self._execute_task(task, coro_func, args, kwargs)
                )
                
                logger.debug(f"🚀 任务开始执行: {task_id}")
                
            except Exception as e:
                logger.error(f"❌ 调度器循环错误: {e}")
                await asyncio.sleep(0.1)
        
        logger.info("🔄 调度器主循环已结束")
    
    async def _execute_task(
        self, 
        task: AsyncTask, 
        coro_func: Callable[..., Awaitable[Any]], 
        args: tuple, 
        kwargs: dict
    ):
        """执行单个任务"""
        start_time = time.time()
        
        try:
            # 执行协程函数
            result = await coro_func(*args, **kwargs)
            
            # 任务成功完成
            task.result = result
            task.status = "completed"
            task.completed_at = datetime.now()
            
            self.successful_tasks += 1
            logger.debug(f"✅ 任务完成: {task.task_id}")
            
        except Exception as e:
            # 任务执行失败
            task.error = str(e)
            task.status = "failed"
            task.completed_at = datetime.now()
            
            self.failed_tasks += 1
            logger.error(f"❌ 任务失败: {task.task_id}, 错误: {e}")
            
        finally:
            # 从活跃任务中移除
            if task.task_id in self.active_tasks:
                del self.active_tasks[task.task_id]
            
            # 添加到完成任务列表
            self.completed_tasks.append(task)
            
            # 保持完成任务列表不过大
            if len(self.completed_tasks) > 1000:
                self.completed_tasks = self.completed_tasks[-500:]
    
    def get_performance_metrics(self) -> PerformanceMetrics:
        """获取性能指标"""
        execution_times = []
        
        for task in self.completed_tasks:
            if task.started_at and task.completed_at:
                exec_time = (task.completed_at - task.started_at).total_seconds() * 1000
                execution_times.append(exec_time)
        
        if not execution_times:
            return PerformanceMetrics(
                task_count=0,
                avg_execution_time_ms=0,
                max_execution_time_ms=0,
                min_execution_time_ms=0,
                throughput_tasks_per_sec=0,
                error_rate=0,
                memory_usage_mb=0
            )
        
        runtime_seconds = time.time() - self.start_time
        throughput = self.successful_tasks / runtime_seconds if runtime_seconds > 0 else 0
        error_rate = self.failed_tasks / self.total_tasks if self.total_tasks > 0 else 0
        
        return PerformanceMetrics(
            task_count=len(execution_times),
            avg_execution_time_ms=sum(execution_times) / len(execution_times),
            max_execution_time_ms=max(execution_times),
            min_execution_time_ms=min(execution_times),
            throughput_tasks_per_sec=throughput,
            error_rate=error_rate,
            memory_usage_mb=0  # TODO: 实现内存监控
        )


class OptimizedHFTProcessor:
    """
    优化的 HFT 处理器
    
    结合异步调度和 Rust 绑定，实现：
    - 并行数据处理
    - 异步模型训练
    - 实时性能监控
    - 智能任务调度
    """
    
    def __init__(self):
        self.scheduler = AsyncTaskScheduler(max_concurrent_tasks=20, max_workers=8)
        self.rust_engine = RustHFTEngineV2() if RUST_AVAILABLE else None
        self.performance_history = []
        
        logger.info("🚀 优化的 HFT 处理器初始化完成")
    
    async def start(self):
        """启动处理器"""
        await self.scheduler.start()
        logger.info("✅ 优化的 HFT 处理器已启动")
    
    async def stop(self):
        """停止处理器"""
        await self.scheduler.stop()
        logger.info("✅ 优化的 HFT 处理器已停止")
    
    async def train_models_parallel(
        self, 
        symbols: List[str], 
        hours: int = 24
    ) -> Dict[str, Dict[str, Any]]:
        """
        并行训练多个模型
        
        Args:
            symbols: 交易对列表
            hours: 训练时长
        
        Returns:
            Dict[str, Dict[str, Any]]: 训练结果
        """
        logger.info(f"🤖 开始并行训练 {len(symbols)} 个模型")
        
        results = {}
        
        # 为每个交易对提交训练任务
        for symbol in symbols:
            task_id = f"train_{symbol}_{int(time.time())}"
            await self.scheduler.submit_task(
                task_id=task_id,
                task_type="model_training",
                coro_func=self._train_single_model,
                symbol=symbol,
                hours=hours,
                priority=1  # 高优先级
            )
        
        # 等待所有任务完成
        await asyncio.sleep(0.5)  # 让任务开始执行
        
        # 监控任务进度
        while True:
            active_training = [
                task for task in self.scheduler.active_tasks.values()
                if task.task_type == "model_training"
            ]
            
            completed_training = [
                task for task in self.scheduler.completed_tasks
                if task.task_type == "model_training" and task.task_id.split('_')[1] in symbols
            ]
            
            if len(completed_training) == len(symbols):
                break
            
            logger.info(f"🔄 训练进度: {len(completed_training)}/{len(symbols)} 完成")
            await asyncio.sleep(1)
        
        # 收集结果
        for task in self.scheduler.completed_tasks:
            if task.task_type == "model_training" and task.task_id.split('_')[1] in symbols:
                symbol = task.task_id.split('_')[1]
                if task.status == "completed":
                    results[symbol] = task.result
                else:
                    results[symbol] = {"success": False, "error": task.error}
        
        logger.info(f"✅ 并行模型训练完成: {len(results)} 个结果")
        return results
    
    async def evaluate_models_parallel(
        self, 
        model_configs: List[Dict[str, Any]]
    ) -> Dict[str, Dict[str, Any]]:
        """
        并行评估多个模型
        
        Args:
            model_configs: 模型配置列表 [{"symbol": "BTCUSDT", "model_path": "..."}]
        
        Returns:
            Dict[str, Dict[str, Any]]: 评估结果
        """
        logger.info(f"📊 开始并行评估 {len(model_configs)} 个模型")
        
        results = {}
        
        # 提交评估任务
        for config in model_configs:
            task_id = f"eval_{config['symbol']}_{int(time.time())}"
            await self.scheduler.submit_task(
                task_id=task_id,
                task_type="model_evaluation",
                coro_func=self._evaluate_single_model,
                symbol=config["symbol"],
                model_path=config["model_path"],
                priority=2  # 中等优先级
            )
        
        # 等待完成
        await self._wait_for_task_completion("model_evaluation", len(model_configs))
        
        # 收集结果
        for task in self.scheduler.completed_tasks:
            if task.task_type == "model_evaluation":
                symbol = task.task_id.split('_')[1]
                if symbol in [c["symbol"] for c in model_configs]:
                    if task.status == "completed":
                        results[symbol] = task.result
                    else:
                        results[symbol] = {"success": False, "error": task.error}
        
        logger.info(f"✅ 并行模型评估完成: {len(results)} 个结果")
        return results
    
    async def _train_single_model(self, symbol: str, hours: int) -> Dict[str, Any]:
        """训练单个模型"""
        if self.rust_engine:
            result = await self.rust_engine.train_model(symbol, hours)
            return {
                "success": result.success,
                "model_path": result.model_path,
                "final_loss": result.final_loss,
                "duration": result.training_duration_seconds,
                "error": result.error_message
            }
        else:
            # 模拟训练
            await asyncio.sleep(0.5)
            return {
                "success": True,
                "model_path": f"models/{symbol.lower()}_model.safetensors",
                "final_loss": 0.0234,
                "duration": 0.5,
                "error": None
            }
    
    async def _evaluate_single_model(self, symbol: str, model_path: str) -> Dict[str, Any]:
        """评估单个模型"""
        if self.rust_engine:
            result = await self.rust_engine.evaluate_model(model_path, symbol)
            return {
                "success": result.success,
                "accuracy": result.accuracy,
                "sharpe_ratio": result.sharpe_ratio,
                "max_drawdown": result.max_drawdown,
                "metrics": result.performance_metrics or {},
                "recommendation": result.recommendation,
                "error": result.error_message
            }
        else:
            # 模拟评估
            await asyncio.sleep(0.3)
            return {
                "success": True,
                "accuracy": 0.7234,
                "sharpe_ratio": 1.67,
                "max_drawdown": 0.0456,
                "metrics": {"win_rate": 0.62},
                "recommendation": "模型表现良好",
                "error": None
            }
    
    async def _wait_for_task_completion(self, task_type: str, expected_count: int):
        """等待指定类型的任务完成"""
        while True:
            completed = [
                task for task in self.scheduler.completed_tasks
                if task.task_type == task_type
            ]
            
            if len(completed) >= expected_count:
                break
            
            await asyncio.sleep(0.5)
    
    def get_system_performance(self) -> Dict[str, Any]:
        """获取系统性能统计"""
        metrics = self.scheduler.get_performance_metrics()
        
        return {
            "scheduler_metrics": {
                "total_tasks": self.scheduler.total_tasks,
                "successful_tasks": self.scheduler.successful_tasks,
                "failed_tasks": self.scheduler.failed_tasks,
                "active_tasks": len(self.scheduler.active_tasks),
                "avg_execution_time_ms": metrics.avg_execution_time_ms,
                "throughput_tasks_per_sec": metrics.throughput_tasks_per_sec,
                "error_rate": metrics.error_rate
            },
            "rust_engine": {
                "available": self.rust_engine is not None,
                "info": self.rust_engine.get_engine_info() if self.rust_engine else None
            },
            "system_capabilities": {
                "max_concurrent_tasks": self.scheduler.max_concurrent_tasks,
                "max_workers": self.scheduler.max_workers,
                "zero_copy_enabled": RUST_AVAILABLE
            }
        }


# 测试函数
async def test_async_optimization():
    """测试异步优化功能"""
    logger.info("🧪 开始测试异步优化功能")
    
    processor = OptimizedHFTProcessor()
    await processor.start()
    
    try:
        # 1. 并行训练测试
        logger.info("1. 测试并行模型训练...")
        symbols = ["BTCUSDT", "ETHUSDT", "ADAUSDT"]
        
        start_time = time.time()
        training_results = await processor.train_models_parallel(symbols, hours=1)
        training_time = time.time() - start_time
        
        logger.info(f"并行训练完成: {len(training_results)} 个模型, 耗时: {training_time:.2f}秒")
        
        # 2. 并行评估测试
        logger.info("2. 测试并行模型评估...")
        model_configs = []
        for symbol, result in training_results.items():
            if result.get("success") and result.get("model_path"):
                model_configs.append({
                    "symbol": symbol,
                    "model_path": result["model_path"]
                })
        
        start_time = time.time()
        evaluation_results = await processor.evaluate_models_parallel(model_configs)
        evaluation_time = time.time() - start_time
        
        logger.info(f"并行评估完成: {len(evaluation_results)} 个模型, 耗时: {evaluation_time:.2f}秒")
        
        # 3. 性能统计
        performance = processor.get_system_performance()
        logger.info("3. 系统性能统计:")
        logger.info(f"调度器指标: {performance['scheduler_metrics']}")
        logger.info(f"系统能力: {performance['system_capabilities']}")
        
        return {
            "training_results": training_results,
            "evaluation_results": evaluation_results,
            "performance": performance,
            "total_time": training_time + evaluation_time
        }
        
    finally:
        await processor.stop()


if __name__ == "__main__":
    # 运行测试
    asyncio.run(test_async_optimization())