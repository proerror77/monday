#!/usr/bin/env python3
"""
HFT 系統進程管理器
=================

管理四個獨立持久化進程：Master, Ops, ML, Rust
確保系統的持久化運行和故障恢復
"""

import asyncio
import json
import logging
import psutil
import signal
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional
from dataclasses import dataclass
from enum import Enum

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class ProcessState(Enum):
    """進程狀態"""
    STOPPED = "stopped"
    STARTING = "starting"
    RUNNING = "running"
    FAILED = "failed"
    RESTARTING = "restarting"

@dataclass
class ProcessConfig:
    """進程配置"""
    name: str
    display_name: str
    cmd: List[str]
    cwd: Path
    env: Dict[str, str] = None
    restart_policy: str = "always"  # always, on-failure, never
    max_restarts: int = 5
    restart_delay: float = 10.0
    health_check_cmd: List[str] = None
    dependencies: List[str] = None  # 依賴的其他進程

class ManagedProcess:
    """被管理的進程"""
    
    def __init__(self, config: ProcessConfig):
        self.config = config
        self.process: Optional[subprocess.Popen] = None
        self.state = ProcessState.STOPPED
        self.start_time: Optional[datetime] = None
        self.restart_count = 0
        self.last_restart_time: Optional[datetime] = None
        self.failure_log: List[Dict] = []
    
    async def start(self) -> bool:
        """啟動進程"""
        if self.state == ProcessState.RUNNING:
            logger.warning(f"⚠️ {self.config.name} 已在運行中")
            return True
        
        try:
            logger.info(f"🚀 啟動 {self.config.display_name}...")
            self.state = ProcessState.STARTING
            
            # 準備環境變量
            env = self.config.env.copy() if self.config.env else {}
            
            # 啟動進程
            self.process = subprocess.Popen(
                self.config.cmd,
                cwd=self.config.cwd,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True
            )
            
            # 等待啟動
            await asyncio.sleep(3)
            
            if self.process.poll() is None:
                self.state = ProcessState.RUNNING
                self.start_time = datetime.now()
                logger.info(f"✅ {self.config.display_name} 啟動成功 (PID: {self.process.pid})")
                return True
            else:
                stdout, stderr = self.process.communicate()
                error_msg = f"啟動失敗: {stderr or stdout}"
                self.log_failure(error_msg)
                self.state = ProcessState.FAILED
                logger.error(f"❌ {self.config.display_name} {error_msg}")
                return False
                
        except Exception as e:
            error_msg = f"啟動異常: {e}"
            self.log_failure(error_msg)
            self.state = ProcessState.FAILED
            logger.error(f"❌ {self.config.display_name} {error_msg}")
            return False
    
    async def stop(self, timeout: float = 10.0) -> bool:
        """停止進程"""
        if self.state == ProcessState.STOPPED:
            return True
        
        try:
            logger.info(f"🛑 停止 {self.config.display_name}...")
            
            if self.process and self.process.poll() is None:
                # 溫和終止
                self.process.terminate()
                
                # 等待進程結束
                try:
                    self.process.wait(timeout=timeout)
                except subprocess.TimeoutExpired:
                    # 強制殺死
                    logger.warning(f"⚠️ {self.config.display_name} 超時，強制殺死")
                    self.process.kill()
                    self.process.wait()
            
            self.state = ProcessState.STOPPED
            self.start_time = None
            logger.info(f"✅ {self.config.display_name} 已停止")
            return True
            
        except Exception as e:
            logger.error(f"❌ 停止 {self.config.display_name} 異常: {e}")
            return False
    
    async def restart(self) -> bool:
        """重啟進程"""
        logger.info(f"🔄 重啟 {self.config.display_name}...")
        self.state = ProcessState.RESTARTING
        
        success = await self.stop()
        if success:
            await asyncio.sleep(self.config.restart_delay)
            success = await self.start()
            
            if success:
                self.restart_count += 1
                self.last_restart_time = datetime.now()
                logger.info(f"✅ {self.config.display_name} 重啟成功 (第 {self.restart_count} 次)")
            else:
                logger.error(f"❌ {self.config.display_name} 重啟失敗")
        
        return success
    
    def is_running(self) -> bool:
        """檢查進程是否運行"""
        if not self.process:
            return False
        
        # 檢查進程狀態
        if self.process.poll() is not None:
            self.state = ProcessState.FAILED
            return False
        
        # 使用 psutil 進行更精確的檢查
        try:
            proc = psutil.Process(self.process.pid)
            if proc.is_running() and proc.status() != psutil.STATUS_ZOMBIE:
                return True
            else:
                self.state = ProcessState.FAILED
                return False
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            self.state = ProcessState.FAILED
            return False
    
    async def health_check(self) -> bool:
        """健康檢查"""
        if not self.is_running():
            return False
        
        # 如果有自定義健康檢查命令
        if self.config.health_check_cmd:
            try:
                result = subprocess.run(
                    self.config.health_check_cmd,
                    cwd=self.config.cwd,
                    capture_output=True,
                    timeout=10
                )
                return result.returncode == 0
            except Exception:
                return False
        
        return True
    
    def log_failure(self, error: str):
        """記錄故障"""
        failure_entry = {
            "timestamp": datetime.now().isoformat(),
            "error": error,
            "restart_count": self.restart_count
        }
        self.failure_log.append(failure_entry)
        
        # 保持日誌在合理大小
        if len(self.failure_log) > 50:
            self.failure_log = self.failure_log[-50:]
    
    def get_status(self) -> Dict:
        """獲取進程狀態"""
        uptime = (datetime.now() - self.start_time).total_seconds() if self.start_time else 0
        
        return {
            "name": self.config.name,
            "display_name": self.config.display_name,
            "state": self.state.value,
            "pid": self.process.pid if self.process else None,
            "uptime_seconds": uptime,
            "restart_count": self.restart_count,
            "last_restart": self.last_restart_time.isoformat() if self.last_restart_time else None,
            "failure_count": len(self.failure_log),
            "recent_failures": self.failure_log[-3:] if self.failure_log else []
        }

class HFTProcessManager:
    """HFT 系統進程管理器"""
    
    def __init__(self):
        self.project_root = Path("/Users/proerror/Documents/monday")
        self.processes: Dict[str, ManagedProcess] = {}
        self.running = False
        self.monitoring_task: Optional[asyncio.Task] = None
        
        self._setup_processes()
        self._setup_signal_handlers()
    
    def _setup_processes(self):
        """設置進程配置"""
        configs = [
            ProcessConfig(
                name="rust_core",
                display_name="Rust HFT 核心",
                cmd=["cargo", "run", "--release", "--bin", "market_data_collector"],
                cwd=self.project_root / "rust_hft",
                restart_policy="always",
                max_restarts=10,
                restart_delay=5.0,
                dependencies=[]
            ),
            ProcessConfig(
                name="master_workspace",
                display_name="Master 控制器",
                cmd=["python", "master_workspace_app.py"],
                cwd=self.project_root / "master_workspace",
                restart_policy="always",
                max_restarts=5,
                restart_delay=10.0,
                dependencies=["rust_core"]
            ),
            ProcessConfig(
                name="ops_workspace",
                display_name="Ops 運維工作區",
                cmd=["ag", "ws", "up", "--env", "dev"],
                cwd=self.project_root / "ops_workspace",
                restart_policy="always",
                max_restarts=5,
                restart_delay=10.0,
                dependencies=["rust_core"]
            ),
            ProcessConfig(
                name="ml_workspace",
                display_name="ML 機器學習工作區",
                cmd=["ag", "ws", "up", "--env", "dev"],
                cwd=self.project_root / "ml_workspace",
                restart_policy="on-failure",
                max_restarts=3,
                restart_delay=30.0,
                dependencies=["rust_core"]
            )
        ]
        
        for config in configs:
            self.processes[config.name] = ManagedProcess(config)
    
    def _setup_signal_handlers(self):
        """設置信號處理器"""
        signal.signal(signal.SIGINT, self._signal_handler)
        signal.signal(signal.SIGTERM, self._signal_handler)
    
    def _signal_handler(self, signum, frame):
        """信號處理"""
        logger.info(f"📡 收到信號 {signum}，正在優雅關閉...")
        asyncio.create_task(self.stop_all())
    
    async def start_process(self, name: str) -> bool:
        """啟動指定進程"""
        if name not in self.processes:
            logger.error(f"❌ 未知進程: {name}")
            return False
        
        # 檢查依賴
        process = self.processes[name]
        if process.config.dependencies:
            for dep in process.config.dependencies:
                if dep in self.processes and not self.processes[dep].is_running():
                    logger.warning(f"⚠️ 依賴進程 {dep} 未運行，先啟動依賴")
                    if not await self.start_process(dep):
                        logger.error(f"❌ 無法啟動依賴進程 {dep}")
                        return False
        
        return await process.start()
    
    async def stop_process(self, name: str) -> bool:
        """停止指定進程"""
        if name not in self.processes:
            logger.error(f"❌ 未知進程: {name}")
            return False
        
        return await self.processes[name].stop()
    
    async def restart_process(self, name: str) -> bool:
        """重啟指定進程"""
        if name not in self.processes:
            logger.error(f"❌ 未知進程: {name}")
            return False
        
        return await self.processes[name].restart()
    
    async def start_all(self) -> bool:
        """啟動所有進程"""
        logger.info("🚀 啟動完整 HFT 系統...")
        
        # 按依賴順序啟動
        start_order = ["rust_core", "master_workspace", "ops_workspace", "ml_workspace"]
        
        for name in start_order:
            success = await self.start_process(name)
            if not success:
                logger.error(f"❌ 啟動 {name} 失敗，停止啟動流程")
                return False
            
            # 啟動間隔
            await asyncio.sleep(2)
        
        logger.info("🎉 HFT 系統啟動完成！")
        return True
    
    async def stop_all(self) -> bool:
        """停止所有進程"""
        logger.info("🛑 停止完整 HFT 系統...")
        
        # 按相反順序停止
        stop_order = ["ml_workspace", "ops_workspace", "master_workspace", "rust_core"]
        
        tasks = []
        for name in stop_order:
            if name in self.processes:
                tasks.append(self.stop_process(name))
        
        results = await asyncio.gather(*tasks, return_exceptions=True)
        
        success = all(not isinstance(r, Exception) and r for r in results)
        logger.info("✅ HFT 系統已停止" if success else "⚠️ 部分進程停止異常")
        
        self.running = False
        return success
    
    async def monitor_loop(self):
        """監控循環"""
        logger.info("🔄 開始進程監控循環...")
        
        while self.running:
            try:
                # 檢查所有進程健康狀態
                for name, process in self.processes.items():
                    if process.state == ProcessState.RUNNING and not process.is_running():
                        logger.warning(f"⚠️ 檢測到 {process.config.display_name} 異常退出")
                        
                        # 檢查是否需要重啟
                        if (process.config.restart_policy == "always" or 
                            (process.config.restart_policy == "on-failure" and 
                             process.restart_count < process.config.max_restarts)):
                            
                            logger.info(f"🔄 自動重啟 {process.config.display_name}...")
                            await process.restart()
                        else:
                            logger.error(f"❌ {process.config.display_name} 達到最大重啟次數或重啟策略為 never")
                
                # 每30秒檢查一次
                await asyncio.sleep(30)
                
            except Exception as e:
                logger.error(f"❌ 監控循環異常: {e}")
                await asyncio.sleep(10)
        
        logger.info("🛑 進程監控循環已停止")
    
    async def run(self):
        """運行進程管理器"""
        try:
            self.running = True
            
            # 啟動所有進程
            await self.start_all()
            
            # 開始監控
            self.monitoring_task = asyncio.create_task(self.monitor_loop())
            
            # 等待監控任務完成或被中斷
            await self.monitoring_task
            
        except KeyboardInterrupt:
            logger.info("📡 收到中斷信號")
        except Exception as e:
            logger.error(f"❌ 進程管理器異常: {e}")
        finally:
            await self.stop_all()
    
    def get_system_status(self) -> Dict:
        """獲取系統狀態"""
        statuses = {}
        for name, process in self.processes.items():
            statuses[name] = process.get_status()
        
        return {
            "timestamp": datetime.now().isoformat(),
            "system_running": self.running,
            "total_processes": len(self.processes),
            "running_processes": sum(1 for p in self.processes.values() if p.is_running()),
            "processes": statuses
        }
    
    def print_status(self):
        """打印系統狀態"""
        status = self.get_system_status()
        
        print("\n📊 HFT 系統進程狀態")
        print("=" * 50)
        print(f"🕐 時間: {status['timestamp']}")
        print(f"📈 運行狀態: {'🟢 正常' if status['system_running'] else '🔴 停止'}")
        print(f"📊 進程統計: {status['running_processes']}/{status['total_processes']} 正在運行")
        
        print("\n🔧 進程詳情:")
        for name, proc_status in status['processes'].items():
            state_icon = {
                "running": "🟢",
                "stopped": "🔴", 
                "starting": "🟡",
                "failed": "❌",
                "restarting": "🔄"
            }.get(proc_status['state'], "❓")
            
            uptime = proc_status['uptime_seconds']
            uptime_str = f"{int(uptime//3600):02d}:{int((uptime%3600)//60):02d}:{int(uptime%60):02d}"
            
            print(f"  {state_icon} {proc_status['display_name']}")
            print(f"    狀態: {proc_status['state']}")
            print(f"    PID: {proc_status['pid'] or 'N/A'}")
            print(f"    運行時間: {uptime_str}")
            print(f"    重啟次數: {proc_status['restart_count']}")
            
            if proc_status['recent_failures']:
                print(f"    最近故障: {len(proc_status['recent_failures'])} 條")

async def main():
    """主函數"""
    manager = HFTProcessManager()
    
    try:
        await manager.run()
    except Exception as e:
        logger.error(f"❌ 主程序異常: {e}")
    finally:
        logger.info("👋 進程管理器已退出")

if __name__ == "__main__":
    print("""
🚀 HFT 系統進程管理器 v1.0
===========================

管理四個獨立持久化進程：
  • Rust HFT 核心 (數據收集引擎)
  • Master 控制器 (系統協調)
  • Ops 運維工作區 (監控告警)
  • ML 機器學習工作區 (模型訓練)

特性：
  ✅ 自動重啟
  ✅ 健康監控  
  ✅ 依賴管理
  ✅ 故障恢復
  ✅ 優雅關閉

按 Ctrl+C 優雅退出
    """)
    
    asyncio.run(main())