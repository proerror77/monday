#!/usr/bin/env python3
"""
HFT Docker 容器統一管理工具

此腳本提供對所有 HFT 系統 Docker 容器的統一管理，
支持按工作區分組操作和狀態監控。
"""

import subprocess
import json
import sys
from typing import Dict, List, Optional
from datetime import datetime
import argparse

class HFTDockerManager:
    def __init__(self):
        self.workspaces = {
            "master": {
                "name": "HFT Master Control",
                "containers": ["hft-master-db", "hft-master-ui", "hft-master-api"],
                "ports": {"ui": "8504", "api": "8002", "db": "5432"},
                "color": "🎯"
            },
            "ops": {
                "name": "HFT Operations", 
                "containers": ["hft-ops-db", "hft-ops-ui", "hft-ops-api"],
                "ports": {"ui": "8503", "api": "8003", "db": "5434"},
                "color": "⚡"
            },
            "ml": {
                "name": "HFT ML Workspace",
                "containers": ["hft-ml-db", "hft-ml-ui", "hft-ml-api"],
                "ports": {"ui": "8502", "api": "8001", "db": "5433"},
                "color": "🧠"
            },
            "infra": {
                "name": "HFT Infrastructure",
                "containers": ["hft-redis", "hft-clickhouse"],
                "ports": {"redis": "6379", "clickhouse": "8123,9000"},
                "color": "🏗️"
            }
        }
    
    def run_command(self, command: str) -> tuple:
        """執行 Docker 命令並返回結果"""
        try:
            result = subprocess.run(
                command.split(),
                capture_output=True,
                text=True,
                timeout=30
            )
            return result.returncode, result.stdout, result.stderr
        except Exception as e:
            return -1, "", str(e)
    
    def get_container_status(self, container_name: str) -> Optional[Dict]:
        """獲取容器狀態信息"""
        code, stdout, stderr = self.run_command(f"docker inspect {container_name}")
        if code == 0:
            try:
                data = json.loads(stdout)[0]
                state = data['State']
                network = data['NetworkSettings']
                
                # 提取端口映射
                ports = []
                if network.get('Ports'):
                    for container_port, host_bindings in network['Ports'].items():
                        if host_bindings:
                            for binding in host_bindings:
                                ports.append(f"{binding['HostPort']}:{container_port}")
                
                return {
                    "name": container_name,
                    "status": "running" if state['Running'] else "stopped",
                    "health": state.get('Health', {}).get('Status', 'no-check') if 'Health' in state else 'no-check',
                    "started_at": state.get('StartedAt', 'N/A'),
                    "ports": ports
                }
            except Exception as e:
                return {"name": container_name, "status": "error", "error": str(e)}
        return None
    
    def show_status(self, workspace: Optional[str] = None):
        """顯示容器狀態"""
        workspaces = {workspace: self.workspaces[workspace]} if workspace and workspace in self.workspaces else self.workspaces
        
        print("🐳 HFT Docker 容器狀態監控")
        print("=" * 80)
        print(f"📅 時間: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
        print()
        
        total_containers = 0
        running_containers = 0
        
        for ws_id, ws_config in workspaces.items():
            print(f"{ws_config['color']} {ws_config['name']}")
            print("-" * 60)
            
            for container in ws_config['containers']:
                info = self.get_container_status(container)
                total_containers += 1
                
                if info and info['status'] == 'running':
                    running_containers += 1
                    status_icon = "🟢"
                    health_icon = {"healthy": "✅", "unhealthy": "❌", "starting": "⏳", "no-check": "➖"}.get(info['health'], "❓")
                elif info and info['status'] == 'stopped':
                    status_icon = "🔴"
                    health_icon = "⏹️"
                else:
                    status_icon = "❌"
                    health_icon = "❓"
                
                print(f"  {status_icon} {container}")
                if info:
                    print(f"     Status: {info['status']} {health_icon}")
                    if info['ports']:
                        print(f"     Ports: {', '.join(info['ports'])}")
                    if info['status'] == 'running' and info['started_at'] != 'N/A':
                        print(f"     Started: {info['started_at'][:19]}")
                else:
                    print(f"     Status: Not found")
                print()
        
        print(f"📊 總覽: {running_containers}/{total_containers} 容器運行中")
        if running_containers == total_containers:
            print("🎉 所有容器運行正常！")
        else:
            print("⚠️  部分容器未運行")
    
    def restart_workspace(self, workspace: str):
        """重啟工作區容器"""
        if workspace not in self.workspaces:
            print(f"❌ 未知工作區: {workspace}")
            print(f"可用工作區: {', '.join(self.workspaces.keys())}")
            return
        
        ws_config = self.workspaces[workspace]
        print(f"🔄 重啟 {ws_config['color']} {ws_config['name']}...")
        
        for container in ws_config['containers']:
            print(f"   重啟 {container}...")
            code, stdout, stderr = self.run_command(f"docker restart {container}")
            if code == 0:
                print(f"   ✅ {container} 重啟成功")
            else:
                print(f"   ❌ {container} 重啟失敗: {stderr}")
        
        print("🎉 工作區重啟完成！")
    
    def stop_workspace(self, workspace: str):
        """停止工作區容器"""
        if workspace not in self.workspaces:
            print(f"❌ 未知工作區: {workspace}")
            return
        
        ws_config = self.workspaces[workspace]
        print(f"⏹️ 停止 {ws_config['color']} {ws_config['name']}...")
        
        for container in ws_config['containers']:
            print(f"   停止 {container}...")
            code, stdout, stderr = self.run_command(f"docker stop {container}")
            if code == 0:
                print(f"   ✅ {container} 已停止")
            else:
                print(f"   ❌ {container} 停止失敗: {stderr}")
    
    def start_workspace(self, workspace: str):
        """啟動工作區容器"""
        if workspace not in self.workspaces:
            print(f"❌ 未知工作區: {workspace}")
            return
        
        ws_config = self.workspaces[workspace]
        print(f"🚀 啟動 {ws_config['color']} {ws_config['name']}...")
        
        for container in ws_config['containers']:
            print(f"   啟動 {container}...")
            code, stdout, stderr = self.run_command(f"docker start {container}")
            if code == 0:
                print(f"   ✅ {container} 已啟動")
            else:
                print(f"   ❌ {container} 啟動失敗: {stderr}")
    
    def show_logs(self, workspace: str, lines: int = 50):
        """顯示工作區日誌"""
        if workspace not in self.workspaces:
            print(f"❌ 未知工作區: {workspace}")
            return
        
        ws_config = self.workspaces[workspace]
        print(f"📋 {ws_config['color']} {ws_config['name']} 日誌 (最後 {lines} 行)")
        print("=" * 80)
        
        for container in ws_config['containers']:
            print(f"\n🔍 {container} 日誌:")
            print("-" * 40)
            code, stdout, stderr = self.run_command(f"docker logs --tail {lines} {container}")
            if code == 0:
                print(stdout)
            else:
                print(f"❌ 無法獲取日誌: {stderr}")

def main():
    parser = argparse.ArgumentParser(description="HFT Docker 容器管理工具")
    parser.add_argument("command", choices=["status", "restart", "stop", "start", "logs"], 
                       help="操作命令")
    parser.add_argument("workspace", nargs="?", 
                       choices=["master", "ops", "ml", "infra"],
                       help="工作區名稱")
    parser.add_argument("--lines", type=int, default=50,
                       help="日誌行數 (僅用於 logs 命令)")
    
    args = parser.parse_args()
    manager = HFTDockerManager()
    
    if args.command == "status":
        manager.show_status(args.workspace)
    elif args.command == "restart":
        if not args.workspace:
            print("❌ restart 命令需要指定工作區")
            sys.exit(1)
        manager.restart_workspace(args.workspace)
    elif args.command == "stop":
        if not args.workspace:
            print("❌ stop 命令需要指定工作區")
            sys.exit(1)
        manager.stop_workspace(args.workspace)
    elif args.command == "start":
        if not args.workspace:
            print("❌ start 命令需要指定工作區")
            sys.exit(1)
        manager.start_workspace(args.workspace)
    elif args.command == "logs":
        if not args.workspace:
            print("❌ logs 命令需要指定工作區")
            sys.exit(1)
        manager.show_logs(args.workspace, args.lines)

if __name__ == "__main__":
    main()