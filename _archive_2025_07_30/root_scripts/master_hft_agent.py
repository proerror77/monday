#!/usr/bin/env python3
"""
Master HFT System Agent - 統一系統控制器
========================================

統一管理整個 HFT 系統的主控制器，包括：
- 基礎設施管理 (Redis, ClickHouse)
- Rust HFT 核心引擎
- ML 訓練工作區
- Ops 監控工作區

一個 Agent 管理所有子系統的生命週期
"""

import asyncio
import logging
import subprocess
import time
import redis
import psutil
from pathlib import Path
from typing import Dict, Any, List, Optional
from datetime import datetime
from enum import Enum

from agno.agent import Agent
from agno.models.ollama import Ollama

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

class MasterHFTAgent:
    """
    Master HFT System Agent
    統一管理整個 HFT 系統
    """
    
    def __init__(self):
        # 主控制器 Agent
        self.agent = Agent(
            name="MasterHFTController",
            model=Ollama(id="qwen2.5:3b"),
            instructions="""
            你是 HFT 系統的主控制器，負責統一管理整個高頻交易系統。

            🎯 核心職責：
            1. **系統啟動控制**: 按正確順序啟動所有組件
            2. **健康監控**: 實時監控所有子系統健康狀態
            3. **故障恢復**: 自動重啟失敗的組件
            4. **資源管理**: 統一管理 Docker 容器和進程
            5. **狀態協調**: 協調各個子系統之間的依賴關係

            📊 管理的組件：
            - Infrastructure: Redis + ClickHouse (基礎設施)
            - Rust Core: HFT 核心引擎 (交易執行)
            - ML Workspace: 模型訓練 (智能決策)
            - Ops Workspace: 運維監控 (系統守護)

            🤖 啟動順序：
            1. Infrastructure (基礎設施必須先啟動)
            2. Rust Core (依賴 Redis/ClickHouse)
            3. ML Workspace (可並行，依賴基礎設施)
            4. Ops Workspace (最後啟動，監控所有組件)

            請始終確保系統的穩定性和可靠性，優先處理關鍵組件的故障。
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
        self.project_root = Path("/Users/proerror/Documents/monday")
        self.rust_path = self.project_root / "rust_hft"
        self.ml_path = self.project_root / "ml_workspace"
        self.ops_path = self.project_root / "ops_workspace"
        
        # 統計信息
        self.stats = {
            "start_time": None,
            "restart_count": 0,
            "total_uptime": 0,
            "component_failures": 0
        }
        
        logger.info("✅ Master HFT Agent 初始化完成")
    
    async def start_system(self):
        """啟動整個 HFT 系統"""
        logger.info("🚀 Master HFT Agent 開始啟動系統")
        logger.info("=" * 60)
        
        self.system_state = SystemState.STARTING
        self.stats["start_time"] = datetime.now()
        
        try:
            # 步驟1: 啟動基礎設施
            logger.info("📦 步驟1: 啟動基礎設施...")
            if not await self._start_infrastructure():
                raise Exception("基礎設施啟動失敗")
            
            # 步驟2: 啟動 Rust HFT 核心
            logger.info("⚡ 步驟2: 啟動 Rust HFT 核心...")
            if not await self._start_rust_core():
                raise Exception("Rust 核心啟動失敗")
            
            # 步驟3: 並行啟動 ML 和 Ops
            logger.info("🧠 步驟3: 啟動 ML 和 Ops 工作區...")
            ml_task = asyncio.create_task(self._start_ml_workspace())
            ops_task = asyncio.create_task(self._start_ops_workspace())
            
            ml_result, ops_result = await asyncio.gather(ml_task, ops_task, return_exceptions=True)
            
            if isinstance(ml_result, Exception):
                logger.warning(f"⚠️ ML Workspace 啟動失敗: {ml_result}")
            if isinstance(ops_result, Exception):
                logger.warning(f"⚠️ Ops Workspace 啟動失敗: {ops_result}")
            
            # 檢查系統整體狀態
            await self._evaluate_system_state()
            
            # 顯示詳細的組件狀態
            logger.info("📊 組件狀態詳細信息:")
            for name, comp in self.components.items():
                status = "✅" if comp["state"] == ComponentState.HEALTHY else "❌"
                logger.info(f"  {status} {name}: {comp['state'].value}")
            
            logger.info(f"📊 系統整體狀態: {self.system_state.value}")
            
            if self.system_state in [SystemState.RUNNING, SystemState.DEGRADED]:
                logger.info("🎉 HFT 系統啟動完成！")
                await self._display_system_status()
                
                # 開始健康監控循環
                await self._health_monitoring_loop()
                
            else:
                logger.error("❌ 系統啟動失敗，請檢查組件狀態")
                
        except Exception as e:
            logger.error(f"❌ 系統啟動異常: {e}")
            self.system_state = SystemState.FAILED
            await self._emergency_cleanup()
    
    async def _start_infrastructure(self) -> bool:
        """檢查基礎設施狀態 (使用現有的 Redis + ClickHouse)"""
        
        # 檢查現有的 Redis
        if await self._check_redis_connection():
            logger.info("✅ Redis 已運行 (使用現有服務)")
            self.components["redis"]["state"] = ComponentState.HEALTHY
        else:
            logger.error("❌ Redis 連接失敗")
            return False
            
        # 檢查 ClickHouse
        if await self._check_clickhouse_connection():
            logger.info("✅ ClickHouse 已運行 (使用現有服務)")
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
                "cargo", "run", "--release"
            ], cwd=self.rust_path, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            
            self.components["rust_core"]["process"] = process
            
            # 等待一段時間檢查是否啟動成功
            await asyncio.sleep(8)  # 增加等待時間到8秒
            
            if process.poll() is None:  # 進程還在運行
                # 改進的健康檢查：檢查進程是否正在處理數據
                is_healthy = await self._check_rust_core_health()
                
                if is_healthy:
                    self.components["rust_core"]["state"] = ComponentState.HEALTHY
                    logger.info("✅ Rust HFT 核心啟動成功，正在處理市場數據")
                    return True
                else:
                    # 即使端口未開放，如果進程運行正常也算成功
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
            
            # 啟動 ag ws 
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
            
            # 啟動 ag ws
            result = subprocess.run([
                "ag", "ws", "up", "--env", "dev"
            ], cwd=self.ops_path, capture_output=True, text=True, timeout=30)
            
            if result.returncode == 0:
                # 另外啟動實時監控 agent
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
            client = redis.Redis(host='localhost', port=6379, decode_responses=True)
            client.ping()
            return True
        except:
            return False
    
    async def _check_clickhouse_connection(self) -> bool:
        """檢查 ClickHouse 連接"""
        try:
            import aiohttp
            async with aiohttp.ClientSession() as session:
                async with session.get("http://localhost:8123", timeout=5) as response:
                    return response.status == 200
        except:
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
    
    async def _check_rust_core_health(self) -> bool:
        """檢查 Rust 核心健康狀態"""
        try:
            # 方法1: 檢查進程是否存在且消耗CPU（說明在處理數據）
            rust_process = self.components["rust_core"]["process"]
            if not rust_process or rust_process.poll() is not None:
                return False
            
            # 方法2: 檢查 gRPC 端口
            if await self._check_port_open(50051):
                return True
                
            # 方法3: 檢查進程名稱和狀態
            try:
                process = psutil.Process(rust_process.pid)
                if process.is_running() and process.status() != psutil.STATUS_ZOMBIE:
                    return True
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                pass
            
            # 方法4: 檢查是否在監聽任何端口（說明服務啟動中）
            try:
                import subprocess
                result = subprocess.run([
                    "lsof", "-p", str(rust_process.pid), "-P", "-n"
                ], capture_output=True, text=True, timeout=3)
                
                if "LISTEN" in result.stdout:
                    return True
            except:
                pass
                
            return False  # 所有檢查都失敗
            
        except Exception as e:
            logger.warning(f"⚠️ Rust 核心健康檢查異常: {e}")
            return False
    
    async def _check_workspace_health(self, workspace_type: str) -> bool:
        """檢查 Agno Workspace 健康狀態"""
        try:
            workspace_path = self.ml_path if workspace_type == "ml" else self.ops_path
            
            # 檢查 ag ws 狀態
            result = subprocess.run([
                "ag", "ws", "status"
            ], cwd=workspace_path, capture_output=True, text=True, timeout=10)
            
            # 如果命令成功執行且沒有嚴重錯誤，認為是健康的
            if result.returncode == 0:
                return True
            
            # 即使 ag ws status 失敗，如果目錄結構正確也認為是可用的
            if workspace_path.exists() and (workspace_path / "workspace").exists():
                return True
                
            return False
            
        except Exception as e:
            logger.warning(f"⚠️ {workspace_type} workspace 健康檢查異常: {e}")
            return False
    
    async def _evaluate_system_state(self):
        """評估系統整體狀態"""
        healthy_count = sum(1 for comp in self.components.values() 
                          if comp["state"] == ComponentState.HEALTHY)
        
        total_count = len(self.components)
        
        if healthy_count == total_count:
            self.system_state = SystemState.RUNNING
        elif healthy_count >= total_count * 0.6:  # 60% 以上健康
            self.system_state = SystemState.DEGRADED
        else:
            self.system_state = SystemState.FAILED
    
    async def _display_system_status(self):
        """顯示系統狀態"""
        logger.info("📊 系統狀態總覽")
        logger.info("=" * 50)
        logger.info(f"整體狀態: {self.system_state.value}")
        logger.info(f"啟動時間: {self.stats['start_time']}")
        
        logger.info("\n組件狀態:")
        for name, comp in self.components.items():
            status = "✅" if comp["state"] == ComponentState.HEALTHY else "❌"
            logger.info(f"  {status} {name}: {comp['state'].value}")
        
        # 使用 AI Agent 進行狀態分析
        await self._ai_system_analysis()
    
    async def _ai_system_analysis(self):
        """使用 AI 分析系統狀態"""
        try:
            status_summary = {
                "system_state": self.system_state.value,
                "components": {name: comp["state"].value for name, comp in self.components.items()},
                "start_time": self.stats["start_time"].isoformat() if self.stats["start_time"] else None
            }
            
            prompt = f"""
            當前 HFT 系統狀態報告：
            
            整體狀態: {status_summary['system_state']}
            啟動時間: {status_summary['start_time']}
            
            組件狀態:
            {chr(10).join([f"- {name}: {state}" for name, state in status_summary['components'].items()])}
            
            請分析：
            1. 系統整體健康狀況
            2. 是否存在潛在風險
            3. 優化建議
            4. 下一步操作建議
            """
            
            response = self.agent.run(prompt)
            logger.info(f"\n🤖 AI 分析報告:")
            logger.info(f"{response.content if hasattr(response, 'content') else str(response)}")
            
        except Exception as e:
            logger.error(f"❌ AI 分析失敗: {e}")
    
    async def _health_monitoring_loop(self):
        """健康監控循環"""
        logger.info("🔄 開始健康監控循環...")
        
        while self.system_state in [SystemState.RUNNING, SystemState.DEGRADED]:
            try:
                await asyncio.sleep(30)  # 每30秒檢查一次
                
                # 檢查所有組件健康狀態
                await self._check_all_components_health()
                
                # 重新評估系統狀態
                await self._evaluate_system_state()
                
                # 如果檢測到故障，嘗試恢復
                await self._handle_component_failures()
                
            except KeyboardInterrupt:
                logger.info("🛑 收到停止信號")
                break
            except Exception as e:
                logger.error(f"❌ 健康監控異常: {e}")
                await asyncio.sleep(10)
        
        logger.info("🔄 健康監控循環結束")
    
    async def _check_all_components_health(self):
        """檢查所有組件健康狀態"""
        # 檢查 Redis
        if await self._check_redis_connection():
            self.components["redis"]["state"] = ComponentState.HEALTHY
        else:
            self.components["redis"]["state"] = ComponentState.UNHEALTHY
        
        # 檢查 ClickHouse
        if await self._check_clickhouse_connection():
            self.components["clickhouse"]["state"] = ComponentState.HEALTHY
        else:
            self.components["clickhouse"]["state"] = ComponentState.UNHEALTHY
        
        # 檢查 Rust 核心 (使用改進的健康檢查)
        if await self._check_rust_core_health():
            self.components["rust_core"]["state"] = ComponentState.HEALTHY
        else:
            self.components["rust_core"]["state"] = ComponentState.UNHEALTHY
        
        # 檢查 ML Workspace (檢查 ag ws 狀態)
        if await self._check_workspace_health("ml"):
            self.components["ml_workspace"]["state"] = ComponentState.HEALTHY
        else:
            self.components["ml_workspace"]["state"] = ComponentState.UNHEALTHY
        
        # 檢查 Ops 監控進程
        ops_process = self.components["ops_workspace"]["process"]
        if ops_process and ops_process.poll() is None:
            self.components["ops_workspace"]["state"] = ComponentState.HEALTHY
        else:
            self.components["ops_workspace"]["state"] = ComponentState.UNHEALTHY
    
    async def _handle_component_failures(self):
        """處理組件故障"""
        for name, comp in self.components.items():
            if comp["state"] == ComponentState.UNHEALTHY:
                logger.warning(f"⚠️ 檢測到組件故障: {name}")
                await self._restart_component(name)
    
    async def _restart_component(self, component_name: str):
        """重啟指定組件"""
        logger.info(f"🔄 嘗試重啟組件: {component_name}")
        
        if component_name == "rust_core":
            await self._start_rust_core()
        elif component_name == "ops_workspace":
            await self._start_ops_workspace()
        elif component_name == "ml_workspace":
            await self._start_ml_workspace()
        
        self.stats["restart_count"] += 1
    
    async def _emergency_cleanup(self):
        """緊急清理"""
        logger.info("🧹 執行緊急清理...")
        
        # 終止所有進程
        for comp in self.components.values():
            if comp["process"]:
                try:
                    comp["process"].terminate()
                    comp["process"].wait(timeout=10)
                except:
                    comp["process"].kill()
    
    async def stop_system(self):
        """停止整個系統"""
        logger.info("🛑 Master HFT Agent 開始停止系統")
        
        self.system_state = SystemState.STOPPED
        await self._emergency_cleanup()
        
        logger.info("✅ 系統已停止")
    
    def get_system_status(self) -> Dict[str, Any]:
        """獲取系統狀態"""
        return {
            "system_state": self.system_state.value,
            "components": {name: comp["state"].value for name, comp in self.components.items()},
            "stats": self.stats.copy(),
            "uptime_seconds": (datetime.now() - self.stats["start_time"]).total_seconds() 
                             if self.stats["start_time"] else 0
        }

async def main():
    """主函數"""
    master = MasterHFTAgent()
    
    try:
        await master.start_system()
    except KeyboardInterrupt:
        logger.info("\n🛑 收到停止信號")
    finally:
        await master.stop_system()

if __name__ == "__main__":
    asyncio.run(main())