"""
Rust HFT 工具集
==============

與 Rust 高性能引擎交互的工具集：
- 模型訓練控制
- 實時交易執行
- 系統監控
- 配置管理
"""

from agno.tools import Tool
from typing import Dict, Any, Optional, List
import asyncio
import subprocess
import json
import logging
import os
from pathlib import Path

logger = logging.getLogger(__name__)


class RustHFTTools(Tool):
    """
    Rust HFT 引擎工具集
    
    提供與 Rust 端的完整交互能力
    """
    
    def __init__(self, rust_project_path: Optional[str] = None):
        super().__init__()
        self.rust_project_path = rust_project_path or "../rust_hft"
        self.rust_path = Path(self.rust_project_path).resolve()
        
        # 檢查 Rust 項目路徑
        if not self.rust_path.exists():
            logger.warning(f"Rust project path not found: {self.rust_path}")
        
    async def train_model(
        self,
        symbol: str,
        config: Dict[str, Any],
        hours: int = 24
    ) -> Dict[str, Any]:
        """
        訓練 TLOB 模型
        
        Args:
            symbol: 交易對
            config: 訓練配置
            hours: 訓練數據時長（小時）
            
        Returns:
            訓練結果
        """
        try:
            # 準備配置文件
            config_path = self.rust_path / f"config/train_{symbol.lower()}.yaml"
            config_path.parent.mkdir(parents=True, exist_ok=True)
            
            # 寫入配置
            import yaml
            with open(config_path, 'w') as f:
                yaml.dump(config, f)
            
            # 執行 Rust 訓練
            cmd = [
                "cargo", "run", "--release", "--bin", "train",
                "--", 
                "--symbol", symbol,
                "--config", str(config_path),
                "--hours", str(hours)
            ]
            
            result = await self._run_rust_command(cmd)
            
            return {
                "success": True,
                "symbol": symbol,
                "training_output": result["output"],
                "model_path": result.get("model_path"),
                "metrics": result.get("metrics", {})
            }
            
        except Exception as e:
            logger.error(f"Training failed: {e}")
            return {
                "success": False,
                "error": str(e),
                "symbol": symbol
            }
    
    async def evaluate_model(
        self,
        model_path: str,
        test_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        評估模型性能
        
        Args:
            model_path: 模型路徑
            test_config: 測試配置
            
        Returns:
            評估結果
        """
        try:
            # 準備評估配置
            config_path = self.rust_path / "config/evaluate.yaml"
            config_path.parent.mkdir(parents=True, exist_ok=True)
            
            eval_config = {
                "model_path": model_path,
                **test_config
            }
            
            import yaml
            with open(config_path, 'w') as f:
                yaml.dump(eval_config, f)
            
            # 執行評估
            cmd = [
                "cargo", "run", "--release", "--bin", "evaluate",
                "--",
                "--config", str(config_path)
            ]
            
            result = await self._run_rust_command(cmd)
            
            return {
                "success": True,
                "model_path": model_path,
                "evaluation_output": result["output"],
                "metrics": result.get("metrics", {}),
                "performance_report": result.get("report")
            }
            
        except Exception as e:
            logger.error(f"Evaluation failed: {e}")
            return {
                "success": False,
                "error": str(e),
                "model_path": model_path
            }
    
    async def start_live_trading(
        self,
        symbol: str,
        model_path: str,
        capital: float,
        config: Optional[Dict[str, Any]] = None
    ) -> Dict[str, Any]:
        """
        開始實時交易
        
        Args:
            symbol: 交易對
            model_path: 模型路徑
            capital: 初始資金
            config: 交易配置
            
        Returns:
            交易啟動結果
        """
        try:
            trading_config = {
                "symbol": symbol,
                "model_path": model_path,
                "initial_capital": capital,
                "dry_run": config.get("dry_run", True),
                **(config or {})
            }
            
            # 準備交易配置
            config_path = self.rust_path / f"config/trading_{symbol.lower()}.yaml"
            
            import yaml
            with open(config_path, 'w') as f:
                yaml.dump(trading_config, f)
            
            # 啟動交易
            cmd = [
                "cargo", "run", "--release", "--bin", "trade",
                "--",
                "--symbol", symbol,
                "--config", str(config_path)
            ]
            
            # 非阻塞啟動
            process = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=self.rust_path,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            # 等待一小段時間確保啟動
            await asyncio.sleep(2)
            
            if process.returncode is None:
                return {
                    "success": True,
                    "symbol": symbol,
                    "process_id": process.pid,
                    "status": "trading_started",
                    "config": trading_config
                }
            else:
                stdout, stderr = await process.communicate()
                return {
                    "success": False,
                    "error": f"Failed to start trading: {stderr.decode()}",
                    "symbol": symbol
                }
                
        except Exception as e:
            logger.error(f"Failed to start live trading: {e}")
            return {
                "success": False,
                "error": str(e),
                "symbol": symbol
            }
    
    async def stop_live_trading(
        self,
        symbol: str
    ) -> Dict[str, Any]:
        """
        停止實時交易
        
        Args:
            symbol: 交易對
            
        Returns:
            停止結果
        """
        try:
            # 發送停止信號
            cmd = [
                "cargo", "run", "--release", "--bin", "control",
                "--",
                "--action", "stop",
                "--symbol", symbol
            ]
            
            result = await self._run_rust_command(cmd)
            
            return {
                "success": True,
                "symbol": symbol,
                "status": "trading_stopped",
                "output": result["output"]
            }
            
        except Exception as e:
            logger.error(f"Failed to stop trading: {e}")
            return {
                "success": False,
                "error": str(e),
                "symbol": symbol
            }
    
    async def get_system_status(self) -> Dict[str, Any]:
        """
        獲取系統狀態
        
        Returns:
            系統狀態信息
        """
        try:
            cmd = [
                "cargo", "run", "--release", "--bin", "status"
            ]
            
            result = await self._run_rust_command(cmd)
            
            # 解析狀態信息
            try:
                status_data = json.loads(result["output"])
            except json.JSONDecodeError:
                status_data = {"raw_output": result["output"]}
            
            return {
                "success": True,
                "status": status_data,
                "timestamp": result.get("timestamp")
            }
            
        except Exception as e:
            logger.error(f"Failed to get system status: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    async def get_trading_metrics(
        self,
        symbol: Optional[str] = None
    ) -> Dict[str, Any]:
        """
        獲取交易指標
        
        Args:
            symbol: 交易對（可選）
            
        Returns:
            交易指標
        """
        try:
            cmd = [
                "cargo", "run", "--release", "--bin", "metrics"
            ]
            
            if symbol:
                cmd.extend(["--", "--symbol", symbol])
            
            result = await self._run_rust_command(cmd)
            
            # 解析指標數據
            try:
                metrics_data = json.loads(result["output"])
            except json.JSONDecodeError:
                metrics_data = {"raw_output": result["output"]}
            
            return {
                "success": True,
                "metrics": metrics_data,
                "symbol": symbol
            }
            
        except Exception as e:
            logger.error(f"Failed to get trading metrics: {e}")
            return {
                "success": False,
                "error": str(e),
                "symbol": symbol
            }
    
    async def update_model(
        self,
        symbol: str,
        new_model_path: str,
        deployment_strategy: str = "blue_green"
    ) -> Dict[str, Any]:
        """
        更新模型
        
        Args:
            symbol: 交易對
            new_model_path: 新模型路徑
            deployment_strategy: 部署策略
            
        Returns:
            更新結果
        """
        try:
            cmd = [
                "cargo", "run", "--release", "--bin", "deploy",
                "--",
                "--symbol", symbol,
                "--model-path", new_model_path,
                "--strategy", deployment_strategy
            ]
            
            result = await self._run_rust_command(cmd)
            
            return {
                "success": True,
                "symbol": symbol,
                "new_model_path": new_model_path,
                "deployment_strategy": deployment_strategy,
                "output": result["output"]
            }
            
        except Exception as e:
            logger.error(f"Failed to update model: {e}")
            return {
                "success": False,
                "error": str(e),
                "symbol": symbol
            }
    
    async def emergency_stop(
        self,
        reason: str = "Manual emergency stop"
    ) -> Dict[str, Any]:
        """
        緊急停止所有交易
        
        Args:
            reason: 停止原因
            
        Returns:
            停止結果
        """
        try:
            cmd = [
                "cargo", "run", "--release", "--bin", "emergency",
                "--",
                "--reason", reason
            ]
            
            result = await self._run_rust_command(cmd)
            
            return {
                "success": True,
                "reason": reason,
                "status": "all_trading_stopped",
                "output": result["output"]
            }
            
        except Exception as e:
            logger.error(f"Emergency stop failed: {e}")
            return {
                "success": False,
                "error": str(e),
                "reason": reason
            }
    
    async def _run_rust_command(
        self,
        cmd: List[str],
        timeout: int = 300
    ) -> Dict[str, Any]:
        """
        執行 Rust 命令
        
        Args:
            cmd: 命令列表
            timeout: 超時時間（秒）
            
        Returns:
            執行結果
        """
        try:
            process = await asyncio.create_subprocess_exec(
                *cmd,
                cwd=self.rust_path,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            stdout, stderr = await asyncio.wait_for(
                process.communicate(),
                timeout=timeout
            )
            
            output = stdout.decode('utf-8')
            error = stderr.decode('utf-8')
            
            if process.returncode != 0:
                raise subprocess.CalledProcessError(
                    process.returncode, cmd, output, error
                )
            
            # 嘗試解析 JSON 輸出
            try:
                parsed_output = json.loads(output)
                return parsed_output
            except json.JSONDecodeError:
                return {
                    "output": output,
                    "error": error,
                    "return_code": process.returncode
                }
                
        except asyncio.TimeoutError:
            raise Exception(f"Command timed out after {timeout} seconds")
        except subprocess.CalledProcessError as e:
            raise Exception(f"Command failed: {e.stderr}")
        except Exception as e:
            raise Exception(f"Execution failed: {str(e)}")
    
    def get_info(self) -> Dict[str, str]:
        """獲取工具信息"""
        return {
            "name": "RustHFTTools",
            "description": "與 Rust HFT 引擎交互的工具集",
            "rust_path": str(self.rust_path),
            "available_commands": [
                "train_model", "evaluate_model", "start_live_trading",
                "stop_live_trading", "get_system_status", "get_trading_metrics",
                "update_model", "emergency_stop"
            ]
        }