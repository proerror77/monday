#!/usr/bin/env python3
"""
Agno CLI 對話接口
================

與各個 Agno Workspace 進行交互對話的統一接口
"""

import asyncio
import cmd
import json
import logging
import sys
from pathlib import Path
from typing import Dict, Any, Optional
import subprocess
import time

# 設置日誌
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class AgnoWorkspaceClient:
    """Agno Workspace 客戶端"""
    
    def __init__(self, workspace_name: str, workspace_path: Path):
        self.name = workspace_name
        self.path = workspace_path
        self.process: Optional[subprocess.Popen] = None
        self.is_running = False
    
    async def start(self) -> bool:
        """啟動 workspace"""
        try:
            logger.info(f"🚀 啟動 {self.name} workspace...")
            
            # 檢查 agno.toml 存在
            agno_config = self.path / "agno.toml"
            if not agno_config.exists():
                logger.error(f"❌ {self.name} agno.toml 不存在")
                return False
            
            # 使用 ag ws up 啟動
            cmd = ["ag", "ws", "up", "--env", "dev"]
            self.process = subprocess.Popen(
                cmd,
                cwd=self.path,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True
            )
            
            # 等待啟動
            await asyncio.sleep(3)
            
            if self.process.poll() is None:
                self.is_running = True
                logger.info(f"✅ {self.name} 啟動成功")
                return True
            else:
                stdout, stderr = self.process.communicate()
                logger.error(f"❌ {self.name} 啟動失敗: {stderr}")
                return False
                
        except Exception as e:
            logger.error(f"❌ {self.name} 啟動異常: {e}")
            return False
    
    async def chat(self, message: str) -> str:
        """與 workspace 對話"""
        try:
            # 使用 ag chat 命令
            cmd = ["ag", "chat", message]
            result = subprocess.run(
                cmd,
                cwd=self.path,
                capture_output=True,
                text=True,
                timeout=30
            )
            
            if result.returncode == 0:
                return result.stdout.strip()
            else:
                return f"❌ 對話失敗: {result.stderr.strip()}"
                
        except subprocess.TimeoutExpired:
            return "⏰ 對話超時"
        except Exception as e:
            return f"❌ 對話異常: {e}"
    
    async def run_workflow(self, workflow_name: str, params: Dict[str, Any] = None) -> str:
        """運行指定工作流"""
        try:
            cmd = ["ag", "workflow", "run", workflow_name]
            if params:
                cmd.extend(["--params", json.dumps(params)])
            
            result = subprocess.run(
                cmd,
                cwd=self.path,
                capture_output=True,
                text=True,
                timeout=120
            )
            
            if result.returncode == 0:
                return result.stdout.strip()
            else:
                return f"❌ 工作流失敗: {result.stderr.strip()}"
                
        except Exception as e:
            return f"❌ 工作流異常: {e}"
    
    def stop(self):
        """停止 workspace"""
        if self.process and self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=10)
            self.is_running = False
            logger.info(f"🛑 {self.name} 已停止")

class AgnoCLI(cmd.Cmd):
    """Agno 對話命令行接口"""
    
    intro = """
🤖 Agno HFT 系統對話接口 v1.0
=============================

可用命令:
  start <workspace>     - 啟動指定工作區
  stop <workspace>      - 停止指定工作區
  status               - 顯示所有工作區狀態
  chat <workspace> <message> - 與指定工作區對話
  workflow <workspace> <name> - 運行指定工作流
  list                 - 列出所有可用工作區
  help                 - 顯示幫助
  quit                 - 退出

示例:
  start ops            - 啟動 Ops workspace
  chat ml "訓練一個新模型" - 與 ML workspace 對話
  workflow ops alert_pipeline - 運行告警工作流
    """
    
    prompt = "agno> "
    
    def __init__(self):
        super().__init__()
        self.project_root = Path("/Users/proerror/Documents/monday")
        
        # 初始化各個 workspace 客戶端
        self.workspaces = {
            "master": AgnoWorkspaceClient("Master", self.project_root / "master_workspace"),
            "ops": AgnoWorkspaceClient("Ops", self.project_root / "ops_workspace"),
            "ml": AgnoWorkspaceClient("ML", self.project_root / "ml_workspace"),
        }
        
        # Rust 核心的特殊處理
        self.rust_process = None
    
    def do_start(self, args):
        """啟動指定工作區: start <workspace_name>"""
        if not args:
            print("❌ 請指定工作區名稱: master, ops, ml, 或 rust")
            return
        
        workspace_name = args.strip().lower()
        
        if workspace_name == "rust":
            self._start_rust_core()
        elif workspace_name in self.workspaces:
            asyncio.run(self._start_workspace(workspace_name))
        elif workspace_name == "all":
            self._start_all()
        else:
            print(f"❌ 未知工作區: {workspace_name}")
    
    def do_stop(self, args):
        """停止指定工作區: stop <workspace_name>"""
        if not args:
            print("❌ 請指定工作區名稱")
            return
        
        workspace_name = args.strip().lower()
        
        if workspace_name == "rust":
            self._stop_rust_core()
        elif workspace_name in self.workspaces:
            self.workspaces[workspace_name].stop()
        elif workspace_name == "all":
            self._stop_all()
        else:
            print(f"❌ 未知工作區: {workspace_name}")
    
    def do_status(self, args):
        """顯示所有工作區狀態"""
        print("\n📊 HFT 系統狀態")
        print("=" * 40)
        
        # Rust 核心狀態
        rust_status = "🟢 運行中" if self._is_rust_running() else "🔴 已停止"
        print(f"⚡ Rust Core: {rust_status}")
        
        # Workspace 狀態
        for name, client in self.workspaces.items():
            status = "🟢 運行中" if client.is_running else "🔴 已停止"
            print(f"🤖 {name.title()} Workspace: {status}")
    
    def do_chat(self, args):
        """與指定工作區對話: chat <workspace> <message>"""
        parts = args.split(" ", 1)
        if len(parts) < 2:
            print("❌ 用法: chat <workspace> <message>")
            return
        
        workspace_name, message = parts
        workspace_name = workspace_name.lower()
        
        if workspace_name not in self.workspaces:
            print(f"❌ 未知工作區: {workspace_name}")
            return
        
        if not self.workspaces[workspace_name].is_running:
            print(f"❌ {workspace_name} workspace 未運行，請先啟動")
            return
        
        print(f"💬 與 {workspace_name} 對話中...")
        response = asyncio.run(self.workspaces[workspace_name].chat(message))
        print(f"\n🤖 {workspace_name.title()} 回應:")
        print(response)
    
    def do_workflow(self, args):
        """運行指定工作流: workflow <workspace> <workflow_name>"""
        parts = args.split(" ", 1)
        if len(parts) < 2:
            print("❌ 用法: workflow <workspace> <workflow_name>")
            return
        
        workspace_name, workflow_name = parts
        workspace_name = workspace_name.lower()
        
        if workspace_name not in self.workspaces:
            print(f"❌ 未知工作區: {workspace_name}")
            return
        
        if not self.workspaces[workspace_name].is_running:
            print(f"❌ {workspace_name} workspace 未運行，請先啟動")
            return
        
        print(f"⚙️ 運行 {workspace_name} 的 {workflow_name} 工作流...")
        response = asyncio.run(self.workspaces[workspace_name].run_workflow(workflow_name))
        print(f"\n📋 工作流結果:")
        print(response)
    
    def do_list(self, args):
        """列出所有可用工作區"""
        print("\n📁 可用工作區:")
        print("• rust    - Rust HFT 核心引擎")
        print("• master  - Master 控制器")
        print("• ops     - 運維監控工作區")
        print("• ml      - 機器學習工作區")
    
    def do_quit(self, args):
        """退出 CLI"""
        print("👋 正在關閉所有工作區...")
        self._stop_all()
        print("✅ 再見！")
        return True
    
    def do_EOF(self, args):
        """處理 Ctrl+D"""
        return self.do_quit(args)
    
    async def _start_workspace(self, name: str):
        """啟動指定 workspace"""
        client = self.workspaces[name]
        success = await client.start()
        if success:
            print(f"✅ {name.title()} workspace 啟動成功")
        else:
            print(f"❌ {name.title()} workspace 啟動失敗")
    
    def _start_rust_core(self):
        """啟動 Rust 核心"""
        try:
            print("🔨 編譯並啟動 Rust HFT 核心...")
            rust_path = self.project_root / "rust_hft"
            
            # 編譯
            compile_result = subprocess.run(
                ["cargo", "build", "--release"],
                cwd=rust_path,
                capture_output=True,
                text=True
            )
            
            if compile_result.returncode != 0:
                print(f"❌ Rust 編譯失敗: {compile_result.stderr}")
                return
            
            # 啟動
            self.rust_process = subprocess.Popen(
                ["cargo", "run", "--release", "--bin", "market_data_collector"],
                cwd=rust_path,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE
            )
            
            print("✅ Rust 核心啟動成功")
            
        except Exception as e:
            print(f"❌ Rust 核心啟動異常: {e}")
    
    def _stop_rust_core(self):
        """停止 Rust 核心"""
        if self.rust_process and self.rust_process.poll() is None:
            self.rust_process.terminate()
            self.rust_process.wait(timeout=10)
            print("🛑 Rust 核心已停止")
    
    def _is_rust_running(self) -> bool:
        """檢查 Rust 核心是否運行"""
        return self.rust_process and self.rust_process.poll() is None
    
    def _start_all(self):
        """啟動所有組件"""
        print("🚀 啟動完整 HFT 系統...")
        
        # 按順序啟動
        self._start_rust_core()
        time.sleep(2)
        
        for name in ["master", "ops", "ml"]:
            asyncio.run(self._start_workspace(name))
            time.sleep(1)
        
        print("🎉 HFT 系統啟動完成！")
    
    def _stop_all(self):
        """停止所有組件"""
        self._stop_rust_core()
        for client in self.workspaces.values():
            client.stop()

def main():
    """主函數"""
    try:
        cli = AgnoCLI()
        cli.cmdloop()
    except KeyboardInterrupt:
        print("\n👋 收到中斷信號，正在退出...")
    except Exception as e:
        print(f"❌ CLI 異常: {e}")

if __name__ == "__main__":
    main()