"""
Rust HFT Tools v2 - 使用 PyO3 绑定的高性能接口
=============================================

这个版本直接使用编译好的 Rust Python 绑定 (rust_hft_py)，
提供真正的零拷贝、高性能的 Rust-Python 集成。
"""

import asyncio
import json
import time
import traceback
from typing import Dict, Any, List, Optional, Union
from dataclasses import dataclass
from pathlib import Path
from loguru import logger

try:
    # 尝试导入编译好的 Rust 绑定
    import rust_hft_py
    RUST_AVAILABLE = True
    logger.info("🚀 Rust PyO3 绑定成功导入")
    logger.info(f"📦 rust_hft_py 版本: {rust_hft_py.__version__}")
except ImportError as e:
    RUST_AVAILABLE = False
    logger.warning(f"⚠️ 无法导入 Rust PyO3 绑定: {e}")
    logger.info("🔄 将回退到命令行接口")


@dataclass
class RustEngineStatus:
    """Rust 引擎状态"""
    overall_status: str
    active_tasks: Dict[str, str]
    uptime_seconds: int
    memory_usage_mb: float
    cpu_usage_percent: float


@dataclass 
class TrainingResult:
    """训练结果"""
    success: bool
    model_path: Optional[str] = None
    final_loss: Optional[float] = None
    training_duration_seconds: Optional[float] = None
    error_message: Optional[str] = None


@dataclass
class EvaluationResult:
    """评估结果"""
    success: bool
    accuracy: Optional[float] = None
    sharpe_ratio: Optional[float] = None
    max_drawdown: Optional[float] = None
    performance_metrics: Optional[Dict[str, float]] = None
    recommendation: Optional[str] = None
    error_message: Optional[str] = None


@dataclass
class TradingResult:
    """交易结果"""
    success: bool
    task_id: Optional[str] = None
    current_capital: Optional[float] = None
    total_trades: Optional[int] = None
    running: bool = False
    error_message: Optional[str] = None


class RustHFTEngineV2:
    """
    Rust HFT Engine v2 - PyO3 绑定版本
    
    提供直接的 Rust-Python 接口，实现：
    - 零拷贝数据传输
    - 微秒级响应延迟
    - 真正的并行处理
    - 内存安全保证
    """
    
    def __init__(self, config_path: Optional[str] = None):
        """
        初始化 Rust HFT 引擎
        
        Args:
            config_path: 配置文件路径 (可选)
        """
        self.config_path = config_path
        self._engine = None
        self._initialized = False
        
        if RUST_AVAILABLE:
            try:
                logger.info("🔧 初始化 Rust PyO3 引擎...")
                self._engine = rust_hft_py.HftEngine(config_path)
                self._initialized = True
                logger.info("✅ Rust PyO3 引擎初始化成功")
            except Exception as e:
                logger.error(f"❌ Rust PyO3 引擎初始化失败: {e}")
                logger.info("🔄 将使用模拟模式")
                self._initialized = False
        else:
            logger.warning("🚫 Rust 绑定不可用，使用模拟模式")
    
    def is_available(self) -> bool:
        """检查 Rust 引擎是否可用"""
        return RUST_AVAILABLE and self._initialized
    
    async def train_model(
        self,
        symbol: str,
        hours: int = 24,
        model_path: Optional[str] = None,
        **kwargs
    ) -> TrainingResult:
        """
        训练模型
        
        Args:
            symbol: 交易对符号 (如 "BTCUSDT")
            hours: 训练数据时长 (小时)
            model_path: 模型保存路径 (可选)
            **kwargs: 其他训练参数
        
        Returns:
            TrainingResult: 训练结果
        """
        if self.is_available():
            try:
                logger.info(f"🤖 开始训练 {symbol} 模型 (使用 Rust PyO3)")
                
                # 直接调用 Rust 引擎
                result = self._engine.train_model(
                    symbol=symbol,
                    hours=hours,
                    model_path=model_path
                )
                
                return TrainingResult(
                    success=result.success,
                    model_path=result.model_path,
                    final_loss=result.final_loss,
                    training_duration_seconds=result.training_duration_seconds,
                    error_message=result.error_message
                )
                
            except Exception as e:
                logger.error(f"❌ Rust 训练失败: {e}")
                return TrainingResult(
                    success=False,
                    error_message=str(e)
                )
        else:
            # 模拟模式
            logger.info(f"🔄 模拟训练 {symbol} 模型")
            await asyncio.sleep(0.5)  # 模拟训练时间
            
            return TrainingResult(
                success=True,
                model_path=model_path or f"models/{symbol.lower()}_model.safetensors",
                final_loss=0.0234,
                training_duration_seconds=0.5,
                error_message=None
            )
    
    async def evaluate_model(
        self,
        model_path: str,
        symbol: str,
        eval_hours: int = 6,
        **kwargs
    ) -> EvaluationResult:
        """
        评估模型
        
        Args:
            model_path: 模型文件路径
            symbol: 交易对符号
            eval_hours: 评估数据时长 (小时)
            **kwargs: 其他评估参数
        
        Returns:
            EvaluationResult: 评估结果
        """
        if self.is_available():
            try:
                logger.info(f"📊 开始评估 {symbol} 模型 (使用 Rust PyO3)")
                
                result = self._engine.evaluate_model(
                    model_path=model_path,
                    symbol=symbol,
                    eval_hours=eval_hours
                )
                
                return EvaluationResult(
                    success=result.success,
                    accuracy=result.accuracy,
                    sharpe_ratio=result.sharpe_ratio,
                    max_drawdown=result.max_drawdown,
                    performance_metrics=result.performance_metrics,
                    recommendation=result.recommendation,
                    error_message=result.error_message
                )
                
            except Exception as e:
                logger.error(f"❌ Rust 评估失败: {e}")
                return EvaluationResult(
                    success=False,
                    error_message=str(e)
                )
        else:
            # 模拟模式
            logger.info(f"🔄 模拟评估 {symbol} 模型")
            await asyncio.sleep(0.3)
            
            return EvaluationResult(
                success=True,
                accuracy=0.7234,
                sharpe_ratio=1.67,
                max_drawdown=0.0456,
                performance_metrics={
                    "win_rate": 0.62,
                    "profit_factor": 1.34,
                    "avg_trade_duration_minutes": 8.5
                },
                recommendation="模型表现良好，建议进行实盘测试",
                error_message=None
            )
    
    async def start_dryrun(
        self,
        symbol: str,
        model_path: str,
        capital: float,
        duration_minutes: int = 60,
        **kwargs
    ) -> TradingResult:
        """
        开始干跑测试
        
        Args:
            symbol: 交易对符号
            model_path: 模型文件路径
            capital: 测试资金
            duration_minutes: 测试时长 (分钟)
            **kwargs: 其他交易参数
        
        Returns:
            TradingResult: 交易结果
        """
        if self.is_available():
            try:
                logger.info(f"🧪 开始 {symbol} 干跑测试 (使用 Rust PyO3)")
                
                result = self._engine.start_dryrun(
                    symbol=symbol,
                    model_path=model_path,
                    capital=capital,
                    duration_minutes=duration_minutes
                )
                
                return TradingResult(
                    success=result.success,
                    task_id=result.task_id,
                    current_capital=result.current_capital,
                    total_trades=result.total_trades,
                    running=result.running,
                    error_message=result.error_message
                )
                
            except Exception as e:
                logger.error(f"❌ Rust 干跑启动失败: {e}")
                return TradingResult(
                    success=False,
                    error_message=str(e)
                )
        else:
            # 模拟模式
            logger.info(f"🔄 模拟 {symbol} 干跑测试")
            await asyncio.sleep(0.2)
            
            return TradingResult(
                success=True,
                task_id=f"dryrun_{symbol}_{int(time.time())}",
                current_capital=capital,
                total_trades=0,
                running=True,
                error_message=None
            )
    
    async def start_live_trading(
        self,
        symbol: str,
        model_path: str,
        capital: float,
        **kwargs
    ) -> TradingResult:
        """
        开始实盘交易
        
        Args:
            symbol: 交易对符号
            model_path: 模型文件路径
            capital: 交易资金
            **kwargs: 其他交易参数
        
        Returns:
            TradingResult: 交易结果
        """
        if self.is_available():
            try:
                logger.warning(f"⚡ 开始 {symbol} 实盘交易 (使用 Rust PyO3)")
                
                result = self._engine.start_live_trading(
                    symbol=symbol,
                    model_path=model_path,
                    capital=capital
                )
                
                return TradingResult(
                    success=result.success,
                    task_id=result.task_id,
                    current_capital=result.current_capital,
                    total_trades=result.total_trades,
                    running=result.running,
                    error_message=result.error_message
                )
                
            except Exception as e:
                logger.error(f"❌ Rust 实盘交易启动失败: {e}")
                return TradingResult(
                    success=False,
                    error_message=str(e)
                )
        else:
            # 模拟模式 - 实盘交易不应该在模拟模式下运行
            logger.error("🚫 实盘交易需要 Rust 引擎支持")
            return TradingResult(
                success=False,
                error_message="实盘交易需要 Rust 引擎支持，当前为模拟模式"
            )
    
    async def get_system_status(self) -> RustEngineStatus:
        """获取系统状态"""
        if self.is_available():
            try:
                status = self._engine.get_system_status()
                return RustEngineStatus(
                    overall_status=status.overall_status,
                    active_tasks=status.active_tasks,
                    uptime_seconds=status.uptime_seconds,
                    memory_usage_mb=status.memory_usage_mb,
                    cpu_usage_percent=status.cpu_usage_percent
                )
            except Exception as e:
                logger.error(f"❌ 获取 Rust 状态失败: {e}")
                return RustEngineStatus(
                    overall_status="error",
                    active_tasks={},
                    uptime_seconds=0,
                    memory_usage_mb=0,
                    cpu_usage_percent=0
                )
        else:
            # 模拟状态
            return RustEngineStatus(
                overall_status="simulation",
                active_tasks={},
                uptime_seconds=3600,
                memory_usage_mb=128.5,
                cpu_usage_percent=15.2
            )
    
    async def stop_all_tasks(self) -> bool:
        """停止所有任务"""
        if self.is_available():
            try:
                return self._engine.stop_all_tasks()
            except Exception as e:
                logger.error(f"❌ 停止 Rust 任务失败: {e}")
                return False
        else:
            logger.info("🔄 模拟停止所有任务")
            return True
    
    async def emergency_stop(self) -> bool:
        """紧急停止"""
        if self.is_available():
            try:
                logger.warning("🚨 执行 Rust 引擎紧急停止")
                return self._engine.emergency_stop()
            except Exception as e:
                logger.error(f"❌ Rust 紧急停止失败: {e}")
                return False
        else:
            logger.warning("🔄 模拟紧急停止")
            return True
    
    def get_engine_info(self) -> Dict[str, Any]:
        """获取引擎信息"""
        return {
            "engine_type": "RustHFTEngineV2",
            "rust_available": RUST_AVAILABLE,
            "initialized": self._initialized,
            "mode": "PyO3" if self.is_available() else "Simulation",
            "config_path": self.config_path,
            "version": rust_hft_py.__version__ if RUST_AVAILABLE else "N/A"
        }


# 向后兼容的工具类
class RustHFTToolsV2:
    """
    Rust HFT Tools v2 - 高级 PyO3 工具集合
    
    提供完整的 Rust-Python 集成功能，包括：
    - 高性能模型训练和评估
    - 实时交易执行
    - 零拷贝数据处理
    - 系统监控和管理
    """
    
    def __init__(self, config_path: Optional[str] = None):
        self.engine = RustHFTEngineV2(config_path)
        logger.info(f"🚀 RustHFTToolsV2 初始化完成: {self.engine.get_engine_info()}")
    
    # 模型训练相关
    async def train_model(self, symbol: str, hours: int = 24, **kwargs) -> Dict[str, Any]:
        """训练模型"""
        result = await self.engine.train_model(symbol, hours, **kwargs)
        return {
            "success": result.success,
            "model_path": result.model_path,
            "final_loss": result.final_loss,
            "duration": result.training_duration_seconds,
            "error": result.error_message
        }
    
    # 模型评估相关
    async def evaluate_model(self, model_path: str, symbol: str, **kwargs) -> Dict[str, Any]:
        """评估模型"""
        result = await self.engine.evaluate_model(model_path, symbol, **kwargs)
        return {
            "success": result.success,
            "accuracy": result.accuracy,
            "sharpe_ratio": result.sharpe_ratio,
            "max_drawdown": result.max_drawdown,
            "metrics": result.performance_metrics or {},
            "recommendation": result.recommendation,
            "error": result.error_message
        }
    
    # 交易相关
    async def start_dryrun(self, symbol: str, model_path: str, capital: float, **kwargs) -> Dict[str, Any]:
        """开始干跑测试"""
        result = await self.engine.start_dryrun(symbol, model_path, capital, **kwargs)
        return {
            "success": result.success,
            "task_id": result.task_id,
            "capital": result.current_capital,
            "trades": result.total_trades,
            "running": result.running,
            "error": result.error_message
        }
    
    async def start_live_trading(self, symbol: str, model_path: str, capital: float, **kwargs) -> Dict[str, Any]:
        """开始实盘交易"""
        result = await self.engine.start_live_trading(symbol, model_path, capital, **kwargs)
        return {
            "success": result.success,
            "task_id": result.task_id,
            "capital": result.current_capital,
            "trades": result.total_trades,
            "running": result.running,
            "error": result.error_message
        }
    
    # 系统管理相关
    async def get_system_status(self) -> Dict[str, Any]:
        """获取系统状态"""
        status = await self.engine.get_system_status()
        return {
            "status": status.overall_status,
            "tasks": status.active_tasks,
            "uptime": status.uptime_seconds,
            "memory_mb": status.memory_usage_mb,
            "cpu_percent": status.cpu_usage_percent
        }
    
    async def stop_all(self) -> bool:
        """停止所有任务"""
        return await self.engine.stop_all_tasks()
    
    async def emergency_stop(self) -> bool:
        """紧急停止"""
        return await self.engine.emergency_stop()
    
    def get_capabilities(self) -> Dict[str, Any]:
        """获取系统能力"""
        info = self.engine.get_engine_info()
        return {
            "rust_enabled": info["rust_available"],
            "mode": info["mode"],
            "features": [
                "model_training",
                "model_evaluation", 
                "dryrun_trading",
                "live_trading" if info["rust_available"] else "live_trading_disabled",
                "system_monitoring",
                "emergency_controls"
            ],
            "performance": {
                "decision_latency_target_us": 1,
                "throughput_ops_per_second": 10000,
                "memory_efficiency": "zero_copy" if info["rust_available"] else "standard"
            },
            "version": info["version"]
        }


# 创建全局实例供向后兼容使用
rust_hft_tools_v2 = RustHFTToolsV2()


# 异步测试函数
async def test_rust_hft_v2():
    """测试 Rust HFT v2 功能"""
    logger.info("🧪 开始测试 Rust HFT v2 功能")
    
    # 1. 系统状态检查
    logger.info("1. 检查系统状态...")
    status = await rust_hft_tools_v2.get_system_status()
    logger.info(f"系统状态: {status}")
    
    # 2. 获取系统能力
    logger.info("2. 获取系统能力...")
    capabilities = rust_hft_tools_v2.get_capabilities()
    logger.info(f"系统能力: {capabilities}")
    
    # 3. 模型训练测试
    logger.info("3. 测试模型训练...")
    train_result = await rust_hft_tools_v2.train_model("BTCUSDT", hours=1)
    logger.info(f"训练结果: {train_result}")
    
    # 4. 模型评估测试
    if train_result["success"] and train_result["model_path"]:
        logger.info("4. 测试模型评估...")
        eval_result = await rust_hft_tools_v2.evaluate_model(
            train_result["model_path"], "BTCUSDT"
        )
        logger.info(f"评估结果: {eval_result}")
        
        # 5. 干跑测试
        if eval_result["success"]:
            logger.info("5. 测试干跑交易...")
            dryrun_result = await rust_hft_tools_v2.start_dryrun(
                "BTCUSDT", train_result["model_path"], 1000.0
            )
            logger.info(f"干跑结果: {dryrun_result}")
    
    logger.info("✅ Rust HFT v2 测试完成")


if __name__ == "__main__":
    # 运行测试
    asyncio.run(test_rust_hft_v2())