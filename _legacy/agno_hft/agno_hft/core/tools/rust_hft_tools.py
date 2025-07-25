#!/usr/bin/env python3
"""
RustHFTTools - Agno框架的自定義工具
提供與Rust HFT系統的集成接口
"""

import asyncio
import json
import logging
from typing import Dict, List, Optional, Any
import subprocess
import os
import sys

# 设置日志
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class RustHFTTools:
    """Rust HFT系統的Python工具包装器"""
    
    def __init__(self, rust_project_path: str = "/Users/shihsonic/Documents/hft_bitget/rust_hft"):
        """
        初始化RustHFTTools
        
        Args:
            rust_project_path: Rust项目的路径
        """
        self.rust_project_path = rust_project_path
        self.hft_engine = None
        self._initialize_rust_engine()
    
    def _initialize_rust_engine(self):
        """初始化Rust HFT引擎"""
        try:
            # 尝试导入编译好的Rust模块
            sys.path.insert(0, self.rust_project_path)
            import rust_hft_py
            self.hft_engine = rust_hft_py.HftEngine()
            logger.info("✅ Rust HFT引擎初始化成功")
        except ImportError as e:
            logger.warning(f"⚠️ 无法导入Rust模块: {e}")
            logger.info("将使用命令行接口调用Rust功能")
            self.hft_engine = None
    
    async def train_model(
        self, 
        symbol: str, 
        hours: int = 24, 
        model_path: Optional[str] = None,
        **kwargs
    ) -> Dict[str, Any]:
        """
        训练LOB Transformer模型
        
        Args:
            symbol: 交易对符号 (如 'SOLUSDT')
            hours: 训练数据时长（小时）
            model_path: 模型保存路径
            **kwargs: 其他训练参数
            
        Returns:
            训练结果字典
        """
        logger.info(f"🔄 开始训练 {symbol} 模型，时长: {hours} 小时")
        
        if self.hft_engine:
            # 使用PyO3直接调用
            try:
                result = self.hft_engine.train_model(
                    symbol=symbol,
                    hours=hours,
                    model_path=model_path
                )
                return {
                    "success": result.success,
                    "model_path": result.model_path,
                    "final_loss": result.final_loss,
                    "training_duration": result.training_duration_seconds,
                    "error_message": result.error_message
                }
            except Exception as e:
                logger.error(f"❌ Rust引擎调用失败: {e}")
                return {"success": False, "error_message": str(e)}
        else:
            # 使用命令行接口
            return await self._run_rust_command("train_lob_transformer", {
                "symbol": symbol,
                "training-hours": hours,
                "model-path": model_path or f"models/{symbol.lower()}_lob_transformer.safetensors"
            })
    
    async def evaluate_model(
        self,
        model_path: str,
        symbol: str,
        eval_hours: int = 6,
        **kwargs
    ) -> Dict[str, Any]:
        """
        评估模型性能
        
        Args:
            model_path: 模型文件路径
            symbol: 交易对符号
            eval_hours: 评估数据时长（小时）
            **kwargs: 其他评估参数
            
        Returns:
            评估结果字典
        """
        logger.info(f"🔄 开始评估 {symbol} 模型: {model_path}")
        
        if self.hft_engine:
            try:
                result = self.hft_engine.evaluate_model(
                    model_path=model_path,
                    symbol=symbol,
                    eval_hours=eval_hours
                )
                return {
                    "success": result.success,
                    "accuracy": result.accuracy,
                    "sharpe_ratio": result.sharpe_ratio,
                    "max_drawdown": result.max_drawdown,
                    "performance_metrics": result.performance_metrics,
                    "recommendation": result.recommendation,
                    "error_message": result.error_message
                }
            except Exception as e:
                logger.error(f"❌ Rust引擎调用失败: {e}")
                return {"success": False, "error_message": str(e)}
        else:
            return await self._run_rust_command("evaluate_lob_transformer", {
                "model-path": model_path,
                "symbol": symbol,
                "eval-hours": eval_hours
            })
    
    async def start_dryrun(
        self,
        symbol: str,
        model_path: str,
        capital: float,
        duration_minutes: int = 60,
        **kwargs
    ) -> Dict[str, Any]:
        """
        启动干跑测试
        
        Args:
            symbol: 交易对符号
            model_path: 模型文件路径
            capital: 初始资金（USDT）
            duration_minutes: 测试时长（分钟）
            **kwargs: 其他配置参数
            
        Returns:
            测试结果字典
        """
        logger.info(f"🔄 开始 {symbol} 干跑测试，资金: {capital} USDT")
        
        if self.hft_engine:
            try:
                result = self.hft_engine.start_dryrun(
                    symbol=symbol,
                    model_path=model_path,
                    capital=capital,
                    duration_minutes=duration_minutes
                )
                return {
                    "success": result.success,
                    "task_id": result.task_id,
                    "current_capital": result.current_capital,
                    "total_trades": result.total_trades,
                    "running": result.running,
                    "error_message": result.error_message
                }
            except Exception as e:
                logger.error(f"❌ Rust引擎调用失败: {e}")
                return {"success": False, "error_message": str(e)}
        else:
            return await self._run_rust_command("lob_transformer_hft_system", {
                "mode": "dry-run",
                "symbol": symbol,
                "model-path": model_path,
                "initial-capital": capital,
                "duration-minutes": duration_minutes
            })
    
    async def start_live_trading(
        self,
        symbol: str,
        model_path: str,
        capital: float,
        **kwargs
    ) -> Dict[str, Any]:
        """
        启动实盘交易
        
        Args:
            symbol: 交易对符号
            model_path: 模型文件路径
            capital: 初始资金（USDT）
            **kwargs: 其他配置参数
            
        Returns:
            交易结果字典
        """
        logger.info(f"⚠️ 准备开始 {symbol} 实盘交易，资金: {capital} USDT")
        
        if self.hft_engine:
            try:
                result = self.hft_engine.start_live_trading(
                    symbol=symbol,
                    model_path=model_path,
                    capital=capital
                )
                return {
                    "success": result.success,
                    "task_id": result.task_id,
                    "current_capital": result.current_capital,
                    "total_trades": result.total_trades,
                    "running": result.running,
                    "error_message": result.error_message
                }
            except Exception as e:
                logger.error(f"❌ Rust引擎调用失败: {e}")
                return {"success": False, "error_message": str(e)}
        else:
            return await self._run_rust_command("lob_transformer_hft_system", {
                "mode": "live",
                "symbol": symbol,
                "model-path": model_path,
                "initial-capital": capital
            })
    
    async def get_system_status(self) -> Dict[str, Any]:
        """
        获取系统状态
        
        Returns:
            系统状态字典
        """
        if self.hft_engine:
            try:
                status = self.hft_engine.get_system_status()
                return {
                    "overall_status": status.overall_status,
                    "active_tasks": status.active_tasks,
                    "uptime_seconds": status.uptime_seconds,
                    "memory_usage_mb": status.memory_usage_mb,
                    "cpu_usage_percent": status.cpu_usage_percent
                }
            except Exception as e:
                logger.error(f"❌ 获取状态失败: {e}")
                return {"error": str(e)}
        else:
            return {"overall_status": "unknown", "message": "Rust引擎未初始化"}
    
    async def stop_all_tasks(self) -> Dict[str, Any]:
        """
        停止所有任务
        
        Returns:
            停止结果
        """
        logger.info("🛑 停止所有任务...")
        
        if self.hft_engine:
            try:
                success = self.hft_engine.stop_all_tasks()
                return {"success": success, "message": "所有任务已停止"}
            except Exception as e:
                logger.error(f"❌ 停止任务失败: {e}")
                return {"success": False, "error_message": str(e)}
        else:
            return {"success": False, "message": "Rust引擎未初始化"}
    
    async def emergency_stop(self) -> Dict[str, Any]:
        """
        紧急停止
        
        Returns:
            紧急停止结果
        """
        logger.info("🚨 执行紧急停止...")
        
        if self.hft_engine:
            try:
                success = self.hft_engine.emergency_stop()
                return {"success": success, "message": "紧急停止完成"}
            except Exception as e:
                logger.error(f"❌ 紧急停止失败: {e}")
                return {"success": False, "error_message": str(e)}
        else:
            return {"success": False, "message": "Rust引擎未初始化"}
    
    async def _run_rust_command(self, example_name: str, args: Dict[str, Any]) -> Dict[str, Any]:
        """
        运行Rust命令行程序（备用方案）
        
        Args:
            example_name: 示例程序名称
            args: 命令行参数
            
        Returns:
            执行结果
        """
        try:
            # 正確的cargo命令格式：cargo run --example name -- --arg1 value1 --arg2 value2  
            cmd = ["cargo", "run", "--release", "--example", example_name, "--"]
            
            # 添加参数（在--之後）
            for key, value in args.items():
                cmd.extend([f"--{key}", str(value)])
            
            logger.info(f"执行命令: {' '.join(cmd)}")
            
            # 在Rust项目目录中执行命令
            process = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=self.rust_project_path,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            stdout, stderr = await process.communicate()
            
            if process.returncode == 0:
                logger.info(f"✅ {example_name} 执行成功")
                return {
                    "success": True,
                    "output": stdout.decode('utf-8'),
                    "command": ' '.join(cmd)
                }
            else:
                logger.error(f"❌ {example_name} 执行失败: {stderr.decode('utf-8')}")
                return {
                    "success": False,
                    "error_message": stderr.decode('utf-8'),
                    "command": ' '.join(cmd)
                }
                
        except Exception as e:
            logger.error(f"❌ 命令执行异常: {e}")
            return {
                "success": False,
                "error_message": str(e)
            }

    def validate_symbol(self, symbol: str) -> bool:
        """
        验证交易对符号
        
        Args:
            symbol: 交易对符号
            
        Returns:
            是否有效
        """
        valid_symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "ADAUSDT", "DOTUSDT"]
        return symbol.upper() in valid_symbols
    
    def get_supported_symbols(self) -> List[str]:
        """
        获取支持的交易对列表
        
        Returns:
            支持的交易对列表
        """
        return ["BTCUSDT", "ETHUSDT", "SOLUSDT", "ADAUSDT", "DOTUSDT"]

# 为Agno框架创建工具实例
def create_rust_hft_tools() -> RustHFTTools:
    """创建RustHFTTools实例"""
    return RustHFTTools()