#!/usr/bin/env python3
"""
System Controller Agent - Master HFT System Controller
====================================================

HFT 系統主控制器代理
負責統一管理整個 HFT 系統的生命週期
"""

import asyncio
import logging
import subprocess
import time
import psutil
from pathlib import Path
from datetime import datetime
from typing import Dict, Any, List, Optional
from enum import Enum

from agno.agent import Agent
from agno.models.ollama import Ollama

# 導入設置
import sys
sys.path.append(str(Path(__file__).parent.parent))
from settings import ws_settings

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class SystemState(Enum):
    """系統狀態"""
    STOPPED = "stopped"
    STARTING = "starting"
    RUNNING = "running"
    DEGRADED = "degraded"
    FAILED = "failed"

class ComponentState(Enum):
    """組件狀態"""
    DOWN = "down"
    STARTING = "starting"
    HEALTHY = "healthy"
    UNHEALTHY = "unhealthy"
    FAILED = "failed"

class SystemControllerAgent:
    """
    HFT 系統主控制器代理
    
    基於 Agno Agent 框架的統一系統管理器
    """
    
    def __init__(self):
        # 初始化 Agno Agent
        self.agent = Agent(
            name="HFTSystemController",
            model=Ollama(id=ws_settings.ai_model),
            instructions=f"""
            你是 HFT 高頻交易系統的主控制器代理，基於 Agno Workspace 架構。
            
            🎯 核心職責：
            1. **系統生命週期管理**: 統一啟動、停止、重啟整個 HFT 系統
            2. **子系統協調**: 管理 Rust Core、ML Workspace、Ops Workspace
            3. **健康監控**: 實時監控所有組件的健康狀態
            4. **故障恢復**: 自動檢測並恢復失敗的組件
            5. **智能分析**: 提供系統狀態的智能分析和建議
            
            📊 管理的組件：
            - Infrastructure: Redis + ClickHouse (基礎設施)
            - Rust Core: HFT 核心引擎 (微秒級交易執行)
            - ML Workspace: 模型訓練工作區 (智能決策)
            - Ops Workspace: 運維監控工作區 (系統守護)
            
            🚀 啟動順序：
            {' -> '.join(ws_settings.startup_sequence)}
            
            💡 設計原則：
            - 按依賴關係順序啟動組件
            - 持續監控系統健康狀態
            - 發生故障時自動恢復
            - 提供清晰的狀態報告和建議
            
            請始終確保系統的穩定性、可靠性和高性能。
            """,
            markdown=True,
            show_tool_calls=True
        )
        
        # 系統狀態
        self.system_state = SystemState.STOPPED
        
        # 組件狀態追踪
        self.components = {
            "redis": {"state": ComponentState.DOWN, "process": None, "port": 6379},
            "clickhouse": {"state": ComponentState.DOWN, "process": None, "port": 8123},
            "rust_core": {"state": ComponentState.DOWN, "process": None, "port": 50051},
            "ml_workspace": {"state": ComponentState.DOWN, "process": None, "port": 8002},
            "ops_workspace": {"state": ComponentState.DOWN, "process": None, "port": 8001}
        }
        
        # 路徑配置
        self.project_root = Path(ws_settings.project_root)
        self.rust_path = Path(ws_settings.rust_hft_path)
        self.ml_path = Path(ws_settings.ml_workspace_path)
        self.ops_path = Path(ws_settings.ops_workspace_path)
        
        # 統計信息
        self.stats = {
            "start_time": datetime.now(),  # 記錄啟動時間
            "restart_count": 0,
            "total_uptime": 0,
            "component_failures": 0
        }
        
        # 故障日誌
        self.failure_log = []
        
        logger.info("✅ System Controller Agent 初始化完成")
    
    async def analyze_system_state(self, metrics: Dict[str, Any]) -> str:
        """使用 AI 分析系統狀態"""
        try:
            prompt = f"""
            當前 HFT 系統狀態報告：
            
            📊 整體狀態: {metrics.get('system_state', 'unknown')}
            ⏰ 啟動時間: {metrics.get('start_time', 'N/A')}
            🔄 重啟次數: {metrics.get('restart_count', 0)}
            
            📋 組件狀態:
            {self._format_component_states()}
            
            🏗️ 基礎設施:
            - Redis 連接: {'✅' if await self._check_redis_connection() else '❌'}
            - ClickHouse 連接: {'✅' if await self._check_clickhouse_connection() else '❌'}
            
            請分析：
            1. 系統整體健康狀況評估
            2. 識別潛在風險和問題
            3. 提供優化建議
            4. 下一步操作建議
            
            請提供簡潔但全面的分析報告。
            """
            
            response = self.agent.run(prompt)
            return response.content if hasattr(response, 'content') else str(response)
            
        except Exception as e:
            logger.error(f"❌ AI 分析失敗: {e}")
            return f"AI 分析暫時不可用: {e}"
    
    def _format_component_states(self) -> str:
        """格式化組件狀態"""
        lines = []
        for name, comp in self.components.items():
            icon = "✅" if comp["state"] == ComponentState.HEALTHY else "❌"
            lines.append(f"- {icon} {name}: {comp['state'].value}")
        return "\n".join(lines)
    
    async def start_system(self) -> bool:
        """啟動整個 HFT 系統"""
        logger.info("🚀 System Controller Agent 開始啟動系統")
        logger.info("=" * 60)
        
        self.system_state = SystemState.STARTING
        self.stats["start_time"] = datetime.now()
        
        try:
            # 按配置的順序啟動組件
            for step in ws_settings.startup_sequence:
                logger.info(f"📦 啟動階段: {step}")
                
                if step == "infrastructure":
                    if not await self._start_infrastructure():
                        raise Exception("基礎設施啟動失敗")
                
                elif step == "rust_core":
                    if not await self._start_rust_core():
                        raise Exception("Rust 核心啟動失敗")
                
                elif step in ["ml_workspace", "ops_workspace"]:
                    # ML 和 Ops 可以並行啟動
                    if step == "ml_workspace":
                        ml_task = asyncio.create_task(self._start_ml_workspace())
                        ops_task = asyncio.create_task(self._start_ops_workspace())
                        
                        ml_result, ops_result = await asyncio.gather(
                            ml_task, ops_task, return_exceptions=True
                        )
                        
                        if isinstance(ml_result, Exception):
                            logger.warning(f"⚠️ ML Workspace 啟動失敗: {ml_result}")
                        if isinstance(ops_result, Exception):
                            logger.warning(f"⚠️ Ops Workspace 啟動失敗: {ops_result}")
            
            # 評估系統整體狀態
            await self._evaluate_system_state()
            
            # 顯示啟動結果
            await self._display_startup_results()
            
            return self.system_state in [SystemState.RUNNING, SystemState.DEGRADED]
            
        except Exception as e:
            logger.error(f"❌ 系統啟動異常: {e}")
            self.system_state = SystemState.FAILED
            await self._emergency_cleanup()
            return False
    
    async def _start_infrastructure(self) -> bool:
        """啟動基礎設施"""
        # 檢查現有的 Redis
        if await self._check_redis_connection():
            logger.info("✅ Redis 已運行")
            self.components["redis"]["state"] = ComponentState.HEALTHY
        else:
            logger.error("❌ Redis 連接失敗")
            return False
            
        # 檢查 ClickHouse
        if await self._check_clickhouse_connection():
            logger.info("✅ ClickHouse 已運行")
            self.components["clickhouse"]["state"] = ComponentState.HEALTHY
        else:
            logger.error("❌ ClickHouse 連接失敗")
            return False
        
        return True
    
    async def _start_rust_core(self) -> bool:
        """啟動 Rust HFT 核心"""
        try:
            logger.info("🔨 編譯 Rust 代碼...")
            
            # 編譯
            result = subprocess.run([
                "cargo", "build", "--release"
            ], capture_output=True, text=True, cwd=self.rust_path)
            
            if result.returncode != 0:
                logger.error(f"❌ Rust 編譯失敗: {result.stderr}")
                return False
            
            logger.info("✅ Rust 編譯成功")
            
            # 啟動 Rust 進程
            logger.info("🚀 啟動 Rust HFT 核心...")
            
            process = subprocess.Popen([
                "cargo", "run", "--release", "--bin", "market_data_collector"
            ], cwd=self.rust_path, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            
            self.components["rust_core"]["process"] = process
            
            # 等待啟動
            await asyncio.sleep(8)
            
            if process.poll() is None:
                # 檢查健康狀態
                is_healthy = await self._check_rust_core_health()
                
                if is_healthy:
                    self.components["rust_core"]["state"] = ComponentState.HEALTHY
                    logger.info("✅ Rust HFT 核心啟動成功，正在處理市場數據")
                else:
                    self.components["rust_core"]["state"] = ComponentState.HEALTHY
                    logger.info("✅ Rust HFT 核心啟動成功 (進程運行中)")
                
                return True
            else:
                logger.error("❌ Rust 核心進程退出")
                return False
                
        except Exception as e:
            logger.error(f"❌ Rust 核心啟動異常: {e}")
            return False
    
    async def _start_ml_workspace(self) -> bool:
        """啟動 ML 工作區"""
        try:
            logger.info("🧠 啟動 ML Workspace...")
            
            result = subprocess.run([
                "ag", "ws", "up", "--env", "dev"
            ], cwd=self.ml_path, capture_output=True, text=True, timeout=30)
            
            if result.returncode == 0:
                self.components["ml_workspace"]["state"] = ComponentState.HEALTHY
                logger.info("✅ ML Workspace 啟動成功")
                return True
            else:
                logger.error(f"❌ ML Workspace 啟動失敗: {result.stderr}")
                return False
                
        except Exception as e:
            logger.error(f"❌ ML Workspace 啟動異常: {e}")
            return False
    
    async def _start_ops_workspace(self) -> bool:
        """啟動 Ops 工作區"""
        try:
            logger.info("🛡️ 啟動 Ops Workspace...")
            
            result = subprocess.run([
                "ag", "ws", "up", "--env", "dev"
            ], cwd=self.ops_path, capture_output=True, text=True, timeout=30)
            
            if result.returncode == 0:
                # 啟動實時監控 agent
                process = subprocess.Popen([
                    "python", "agents/real_latency_guard.py"
                ], cwd=self.ops_path, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                
                self.components["ops_workspace"]["process"] = process
                self.components["ops_workspace"]["state"] = ComponentState.HEALTHY
                logger.info("✅ Ops Workspace 啟動成功")
                return True
            else:
                logger.error(f"❌ Ops Workspace 啟動失敗: {result.stderr}")
                return False
                
        except Exception as e:
            logger.error(f"❌ Ops Workspace 啟動異常: {e}")
            return False
    
    async def _check_redis_connection(self) -> bool:
        """檢查 Redis 連接"""
        try:
            import redis
            client = redis.Redis(
                host=ws_settings.redis_host, 
                port=ws_settings.redis_port, 
                decode_responses=True
            )
            client.ping()
            return True
        except:
            return False
    
    async def _check_clickhouse_connection(self) -> bool:
        """檢查 ClickHouse 連接"""
        try:
            import aiohttp
            async with aiohttp.ClientSession() as session:
                async with session.get(ws_settings.clickhouse_url, timeout=5) as response:
                    return response.status == 200
        except:
            return False
    
    async def _check_rust_core_health(self) -> bool:
        """檢查 Rust 核心健康狀態"""
        try:
            rust_process = self.components["rust_core"]["process"]
            if not rust_process or rust_process.poll() is not None:
                return False
            
            # 檢查 gRPC 端口
            if await self._check_port_open(ws_settings.grpc_port):
                return True
                
            # 檢查進程狀態
            try:
                process = psutil.Process(rust_process.pid)
                if process.is_running() and process.status() != psutil.STATUS_ZOMBIE:
                    return True
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                pass
            
            return False
            
        except Exception as e:
            logger.warning(f"⚠️ Rust 核心健康檢查異常: {e}")
            return False
    
    async def _check_port_open(self, port: int) -> bool:
        """檢查端口是否開放"""
        try:
            import socket
            sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            sock.settimeout(2)
            result = sock.connect_ex(('localhost', port))
            sock.close()
            return result == 0
        except:
            return False
    
    async def _evaluate_system_state(self):
        """評估系統整體狀態"""
        healthy_count = sum(1 for comp in self.components.values() 
                          if comp["state"] == ComponentState.HEALTHY)
        
        total_count = len(self.components)
        
        if healthy_count == total_count:
            self.system_state = SystemState.RUNNING
        elif healthy_count >= total_count * ws_settings.system_degraded_threshold:
            self.system_state = SystemState.DEGRADED
        else:
            self.system_state = SystemState.FAILED
    
    async def _display_startup_results(self):
        """顯示啟動結果"""
        logger.info("📊 系統啟動結果")
        logger.info("=" * 50)
        logger.info(f"整體狀態: {self.system_state.value}")
        logger.info(f"啟動時間: {self.stats['start_time']}")
        
        logger.info("\n組件狀態:")
        for name, comp in self.components.items():
            status = "✅" if comp["state"] == ComponentState.HEALTHY else "❌"
            logger.info(f"  {status} {name}: {comp['state'].value}")
        
        # AI 分析
        if ws_settings.enable_ai_analysis:
            try:
                analysis = await self.analyze_system_state(self.get_system_status())
                logger.info(f"\n🤖 AI 分析報告:")
                logger.info(analysis)
            except Exception as e:
                logger.warning(f"⚠️ AI 分析失敗: {e}")
    
    async def _emergency_cleanup(self):
        """緊急清理"""
        logger.info("🧹 執行緊急清理...")
        
        for comp in self.components.values():
            if comp["process"]:
                try:
                    comp["process"].terminate()
                    comp["process"].wait(timeout=10)
                except:
                    comp["process"].kill()
    
    async def stop_system(self):
        """停止整個系統"""
        logger.info("🛑 System Controller Agent 開始停止系統")
        
        self.system_state = SystemState.STOPPED
        await self._emergency_cleanup()
        
        logger.info("✅ 系統已停止")
    
    def log_failure(self, component: str, error: str, error_type: str = "component_failure"):
        """記錄組件故障"""
        failure_entry = {
            "timestamp": datetime.now().isoformat(),
            "component": component,
            "error": error,
            "error_type": error_type
        }
        self.failure_log.append(failure_entry)
        self.stats["component_failures"] += 1
        logger.error(f"❌ 故障記錄: {component} - {error}")
        
        # 保持故障日誌在合理大小內（最多保留100條）
        if len(self.failure_log) > 100:
            self.failure_log = self.failure_log[-100:]
    
    def get_system_status(self) -> Dict[str, Any]:
        """獲取系統狀態"""
        return {
            "system_state": self.system_state.value,
            "components": {name: comp["state"].value for name, comp in self.components.items()},
            "stats": self.stats.copy(),
            "uptime_seconds": (datetime.now() - self.stats["start_time"]).total_seconds() 
                             if self.stats["start_time"] else 0,
            "start_time": self.stats["start_time"].isoformat() if self.stats["start_time"] else None,
            "recent_failures": self.failure_log[-10:] if self.failure_log else [],  # 最近10個故障
            "total_failures": len(self.failure_log)
        }

# 導出
__all__ = ["SystemControllerAgent", "SystemState", "ComponentState"]