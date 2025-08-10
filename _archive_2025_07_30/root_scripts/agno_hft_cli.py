#!/usr/bin/env python3
"""
Agno HFT 系統標準化 CLI
=====================

基於 Agno 官方標準的 HFT 系統管理接口
"""

import asyncio
import cmd
import json
import logging
import subprocess
import sys
import time
from pathlib import Path
from typing import Dict, List, Optional

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class AgnoWorkspace:
    """標準 Agno Workspace 管理器"""
    
    def __init__(self, name: str, path: Path, description: str):
        self.name = name
        self.path = path
        self.description = description
        self.is_running = False
        self.port_mapping = {
            "ops": {"streamlit": 8501, "api": 8000},
            "ml": {"streamlit": 8502, "api": 8001}, 
            "master": {"streamlit": 8503, "api": 8002}
        }
    
    async def up(self) -> bool:
        """啟動 workspace"""
        try:
            logger.info(f"🚀 啟動 {self.name} workspace...")
            
            # 檢查 pyproject.toml 和 agno.toml
            pyproject = self.path / "pyproject.toml"
            agno_config = self.path / "agno.toml"
            
            if not pyproject.exists():
                logger.error(f"❌ {self.name} pyproject.toml 不存在")
                return False
                
            if not agno_config.exists():
                logger.error(f"❌ {self.name} agno.toml 不存在")
                return False
            
            # 使用官方命令啟動
            result = subprocess.run(
                ["ag", "ws", "up"],
                cwd=self.path,
                capture_output=True,
                text=True,
                timeout=60
            )
            
            if result.returncode == 0:
                self.is_running = True
                logger.info(f"✅ {self.name} workspace 啟動成功")
                
                # 顯示可用端點
                if self.name in self.port_mapping:
                    ports = self.port_mapping[self.name]
                    logger.info(f"📡 {self.name} 可用服務:")
                    logger.info(f"   • Streamlit UI: http://localhost:{ports['streamlit']}")
                    logger.info(f"   • FastAPI: http://localhost:{ports['api']}/docs")
                
                return True
            else:
                logger.error(f"❌ {self.name} 啟動失敗: {result.stderr}")
                return False
                
        except subprocess.TimeoutExpired:
            logger.error(f"⏰ {self.name} 啟動超時")
            return False
        except Exception as e:
            logger.error(f"❌ {self.name} 啟動異常: {e}")
            return False
    
    async def down(self) -> bool:
        """停止 workspace"""
        try:
            logger.info(f"🛑 停止 {self.name} workspace...")
            
            result = subprocess.run(
                ["ag", "ws", "down"],
                cwd=self.path,
                capture_output=True,
                text=True,
                timeout=30
            )
            
            if result.returncode == 0:
                self.is_running = False
                logger.info(f"✅ {self.name} workspace 已停止")
                return True
            else:
                logger.warning(f"⚠️ {self.name} 停止時有警告: {result.stderr}")
                self.is_running = False
                return True
                
        except Exception as e:
            logger.error(f"❌ {self.name} 停止異常: {e}")
            return False
    
    def chat(self, message: str) -> str:
        """與 workspace 中的 agent 對話"""
        try:
            logger.info(f"💬 與 {self.name} 對話: {message}")
            
            # 檢查是否有可用的 agent
            agents_dir = self.path / "agents"
            if not agents_dir.exists():
                return f"❌ {self.name} 沒有配置 agents"
            
            # 嘗試通過 API 與 agent 對話
            import requests
            if self.name in self.port_mapping:
                api_port = self.port_mapping[self.name]["api"]
                try:
                    response = requests.post(
                        f"http://localhost:{api_port}/chat",
                        json={"message": message},
                        timeout=30
                    )
                    if response.status_code == 200:
                        return response.json().get("response", "無響應")
                    else:
                        return f"❌ API 調用失敗: {response.status_code}"
                except requests.exceptions.RequestException:
                    pass
            
            # 回退到本地模擬對話
            return f"🤖 {self.name} 收到消息: '{message}' (模擬響應)"
            
        except Exception as e:
            return f"❌ 對話異常: {e}"
    
    def status(self) -> Dict:
        """獲取 workspace 狀態"""
        return {
            "name": self.name,
            "description": self.description,
            "running": self.is_running,
            "path": str(self.path),
            "endpoints": self.port_mapping.get(self.name, {}) if self.is_running else {}
        }

class RustCoreManager:
    """Rust 核心管理器"""
    
    def __init__(self, rust_path: Path):
        self.rust_path = rust_path
        self.process: Optional[subprocess.Popen] = None
        self.is_running = False
    
    async def start(self) -> bool:
        """啟動 Rust 核心"""
        try:
            logger.info("⚡ 啟動 Rust HFT 核心...")
            
            # 編譯
            logger.info("🔨 編譯 Rust 代碼...")
            compile_result = subprocess.run(
                ["cargo", "build", "--release"],
                cwd=self.rust_path,
                capture_output=True,
                text=True
            )
            
            if compile_result.returncode != 0:
                logger.error(f"❌ 編譯失敗: {compile_result.stderr}")
                return False
            
            # 啟動
            self.process = subprocess.Popen(
                ["cargo", "run", "--release", "--bin", "market_data_collector"],
                cwd=self.rust_path,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE
            )
            
            # 等待啟動
            await asyncio.sleep(5)
            
            if self.process.poll() is None:
                self.is_running = True
                logger.info(f"✅ Rust 核心啟動成功 (PID: {self.process.pid})")
                return True
            else:
                logger.error("❌ Rust 核心啟動失敗")
                return False
                
        except Exception as e:
            logger.error(f"❌ Rust 核心啟動異常: {e}")
            return False
    
    async def stop(self) -> bool:
        """停止 Rust 核心"""
        try:
            if self.process and self.process.poll() is None:
                logger.info("🛑 停止 Rust 核心...")
                self.process.terminate()
                self.process.wait(timeout=10)
                self.is_running = False
                logger.info("✅ Rust 核心已停止")
            return True
        except Exception as e:
            logger.error(f"❌ 停止 Rust 核心異常: {e}")
            return False

class AgnoHFTCLI(cmd.Cmd):
    """Agno HFT 系統標準化 CLI"""
    
    intro = """
🤖 Agno HFT 系統標準化管理接口 v2.0
=====================================

基於 Agno 官方標準 (docs.agno.com) 構建

可用命令:
  ws up <name>          - 啟動指定 workspace
  ws down <name>        - 停止指定 workspace  
  ws status            - 顯示所有 workspace 狀態
  ws list              - 列出所有 workspace
  chat <name> <msg>    - 與指定 workspace 的 agent 對話
  rust start           - 啟動 Rust HFT 核心
  rust stop            - 停止 Rust HFT 核心
  system start         - 啟動完整系統
  system stop          - 停止完整系統
  system status        - 顯示系統整體狀態
  help                 - 顯示幫助
  quit                 - 退出

示例:
  ws up ops            - 啟動 Ops workspace
  chat ops "系統狀態如何?" - 與 Ops agent 對話
  system start         - 啟動完整 HFT 系統
    """
    
    prompt = "agno-hft> "
    
    def __init__(self):
        super().__init__()
        self.project_root = Path("/Users/proerror/Documents/monday")
        
        # 初始化 workspaces
        self.workspaces = {
            "master": AgnoWorkspace(
                "master",
                self.project_root / "master_workspace",
                "HFT 系統主控制器"
            ),
            "ops": AgnoWorkspace(
                "ops", 
                self.project_root / "ops_workspace",
                "運維監控和告警系統"
            ),
            "ml": AgnoWorkspace(
                "ml",
                self.project_root / "ml_workspace", 
                "機器學習模型訓練"
            )
        }
        
        # Rust 核心管理器
        self.rust_core = RustCoreManager(self.project_root / "rust_hft")
    
    def do_ws(self, args):
        """Workspace 管理: ws <command> [workspace_name]"""
        parts = args.split()
        if not parts:
            print("❌ 用法: ws <up|down|status|list> [workspace_name]")
            return
        
        command = parts[0]
        
        if command == "up":
            if len(parts) < 2:
                print("❌ 用法: ws up <workspace_name>")
                return
            workspace_name = parts[1]
            if workspace_name in self.workspaces:
                asyncio.run(self.workspaces[workspace_name].up())
            else:
                print(f"❌ 未知 workspace: {workspace_name}")
        
        elif command == "down":
            if len(parts) < 2:
                print("❌ 用法: ws down <workspace_name>")
                return
            workspace_name = parts[1]
            if workspace_name in self.workspaces:
                asyncio.run(self.workspaces[workspace_name].down())
            else:
                print(f"❌ 未知 workspace: {workspace_name}")
        
        elif command == "status":
            self._show_workspace_status()
        
        elif command == "list":
            self._list_workspaces()
        
        else:
            print(f"❌ 未知 workspace 命令: {command}")
    
    def do_chat(self, args):
        """與 workspace agent 對話: chat <workspace> <message>"""
        parts = args.split(" ", 1)
        if len(parts) < 2:
            print("❌ 用法: chat <workspace> <message>")
            return
        
        workspace_name, message = parts
        
        if workspace_name not in self.workspaces:
            print(f"❌ 未知 workspace: {workspace_name}")
            return
        
        workspace = self.workspaces[workspace_name]
        if not workspace.is_running:
            print(f"❌ {workspace_name} workspace 未運行，請先使用 'ws up {workspace_name}' 啟動")
            return
        
        response = workspace.chat(message)
        print(f"\n🤖 {workspace_name.title()} Agent:")
        print(response)
    
    def do_rust(self, args):
        """Rust 核心管理: rust <start|stop|status>"""
        if not args:
            print("❌ 用法: rust <start|stop|status>")
            return
        
        command = args.strip()
        
        if command == "start":
            asyncio.run(self.rust_core.start())
        elif command == "stop":
            asyncio.run(self.rust_core.stop())
        elif command == "status":
            status = "🟢 運行中" if self.rust_core.is_running else "🔴 已停止"
            print(f"⚡ Rust HFT 核心: {status}")
            if self.rust_core.is_running and self.rust_core.process:
                print(f"   PID: {self.rust_core.process.pid}")
        else:
            print(f"❌ 未知 rust 命令: {command}")
    
    def do_system(self, args):
        """系統整體管理: system <start|stop|status>"""
        if not args:
            print("❌ 用法: system <start|stop|status>")
            return
        
        command = args.strip()
        
        if command == "start":
            asyncio.run(self._start_system())
        elif command == "stop":
            asyncio.run(self._stop_system())
        elif command == "status":
            self._show_system_status()
        else:
            print(f"❌ 未知 system 命令: {command}")
    
    def do_quit(self, args):
        """退出 CLI"""
        print("🛑 正在關閉所有服務...")
        asyncio.run(self._stop_system())
        print("👋 再見！")
        return True
    
    def do_EOF(self, args):
        """處理 Ctrl+D"""
        return self.do_quit(args)
    
    async def _start_system(self):
        """啟動完整系統"""
        print("🚀 啟動 HFT 完整系統...")
        print("=" * 40)
        
        # 1. 啟動 Rust 核心
        print("1️⃣ 啟動 Rust HFT 核心...")
        rust_success = await self.rust_core.start()
        if not rust_success:
            print("❌ Rust 核心啟動失敗，停止啟動流程")
            return
        
        await asyncio.sleep(2)
        
        # 2. 並行啟動所有 workspaces
        print("2️⃣ 並行啟動所有 Agno Workspaces...")
        tasks = []
        for name, workspace in self.workspaces.items():
            tasks.append(workspace.up())
        
        results = await asyncio.gather(*tasks, return_exceptions=True)
        
        # 檢查結果
        success_count = 0
        for i, (name, result) in enumerate(zip(self.workspaces.keys(), results)):
            if isinstance(result, Exception):
                print(f"❌ {name} workspace 啟動異常: {result}")
            elif result:
                success_count += 1
                print(f"✅ {name} workspace 啟動成功")
            else:
                print(f"❌ {name} workspace 啟動失敗")
        
        print("\n🎉 HFT 系統啟動完成！")
        print(f"📊 成功啟動: Rust核心 + {success_count}/{len(self.workspaces)} Workspaces")
        
        # 顯示可用服務
        print("\n🌐 可用服務:")
        for name, workspace in self.workspaces.items():
            if workspace.is_running and name in workspace.port_mapping:
                ports = workspace.port_mapping[name]
                print(f"• {name.title()}: http://localhost:{ports['streamlit']} (UI) | http://localhost:{ports['api']}/docs (API)")
    
    async def _stop_system(self):
        """停止完整系統"""
        print("🛑 停止 HFT 完整系統...")
        
        # 並行停止所有 workspaces
        tasks = []
        for workspace in self.workspaces.values():
            if workspace.is_running:
                tasks.append(workspace.down())
        
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        
        # 停止 Rust 核心
        await self.rust_core.stop()
        
        print("✅ HFT 系統已完全停止")
    
    def _show_workspace_status(self):
        """顯示所有 workspace 狀態"""
        print("\n📊 Agno Workspaces 狀態")
        print("=" * 40)
        
        for name, workspace in self.workspaces.items():
            status = workspace.status()
            state_icon = "🟢" if status["running"] else "🔴"
            print(f"{state_icon} {status['name'].title()}: {status['description']}")
            
            if status["running"] and status["endpoints"]:
                for service, port in status["endpoints"].items():
                    print(f"   • {service}: http://localhost:{port}")
    
    def _list_workspaces(self):
        """列出所有可用 workspaces"""
        print("\n📁 可用 Agno Workspaces:")
        for name, workspace in self.workspaces.items():
            print(f"• {name} - {workspace.description}")
    
    def _show_system_status(self):
        """顯示系統整體狀態"""
        print("\n📊 HFT 系統整體狀態")
        print("=" * 40)
        
        # Rust 狀態
        rust_status = "🟢 運行中" if self.rust_core.is_running else "🔴 已停止"
        print(f"⚡ Rust HFT 核心: {rust_status}")
        
        # Workspace 狀態
        running_count = sum(1 for ws in self.workspaces.values() if ws.is_running)
        total_count = len(self.workspaces)
        
        print(f"🤖 Agno Workspaces: {running_count}/{total_count} 運行中")
        
        for name, workspace in self.workspaces.items():
            state_icon = "🟢" if workspace.is_running else "🔴"
            print(f"   {state_icon} {name}")
        
        # 整體評估
        if self.rust_core.is_running and running_count == total_count:
            print("\n🎉 系統狀態: 完全運行中")
        elif self.rust_core.is_running and running_count > 0:
            print("\n⚠️ 系統狀態: 部分運行中")
        else:
            print("\n🔴 系統狀態: 已停止")

def main():
    """主函數"""
    print("🔧 檢查 Agno 安裝...")
    
    # 檢查 ag 命令是否可用
    try:
        result = subprocess.run(["ag", "--help"], capture_output=True, text=True)
        if result.returncode == 0 and "Agno" in result.stdout:
            print(f"✅ Agno 已安裝和可用")
        else:
            print("❌ Agno 未正確安裝，請運行: pip install -U 'agno[aws]'")
            return
    except FileNotFoundError:
        print("❌ 找不到 ag 命令，請安裝 Agno: pip install -U 'agno[aws]'")
        return
    
    try:
        cli = AgnoHFTCLI()
        cli.cmdloop()
    except KeyboardInterrupt:
        print("\n👋 收到中斷信號，正在退出...")
    except Exception as e:
        print(f"❌ CLI 異常: {e}")

if __name__ == "__main__":
    main()