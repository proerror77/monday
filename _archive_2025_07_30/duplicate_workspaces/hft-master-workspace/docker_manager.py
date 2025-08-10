#!/usr/bin/env python3
"""
HFT Docker 容器分組管理工具

此腳本提供對 HFT 系統所有 Docker 容器的統一管理，
按工作區分組顯示和操作容器。
"""

import subprocess
import json
from typing import Dict, List, Optional
from datetime import datetime
import sys

class DockerManager:
    def __init__(self):
        self.workspaces = {
            "master": {
                "name": "HFT Master Control",
                "containers": ["hft-master-db", "hft-master-ui", "hft-master-api"],
                "network": "hft-master",
                "color": "🎛️"
            },
            "ml": {
                "name": "HFT ML Workspace", 
                "containers": ["hft-ml-db", "hft-ml-ui", "hft-ml-api"],
                "network": "hft-ml",
                "color": "🤖"
            },
            "ops": {
                "name": "HFT Ops Workspace",
                "containers": ["hft-ops-db", "hft-ops-ui", "hft-ops-api"], 
                "network": "hft-ops",
                "color": "⚡"
            },
            "infrastructure": {
                "name": "HFT Infrastructure",
                "containers": ["hft-redis", "hft-clickhouse"],
                "network": "shared",
                "color": "🏗️"
            }
        }
    
    def run_command(self, command: str) -> tuple:
        """執行 Docker 命令"""
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
    
    def get_container_info(self, container_name: str) -> Optional[Dict]:
        """獲取容器詳細信息"""
        code, stdout, stderr = self.run_command(f"docker inspect {container_name}")
        if code == 0:
            try:
                container_data = json.loads(stdout)[0]
                state = container_data['State']
                config = container_data['Config']
                network_settings = container_data['NetworkSettings']
                
                # 提取端口映射
                ports = []
                if network_settings.get('Ports'):
                    for container_port, host_bindings in network_settings['Ports'].items():
                        if host_bindings:
                            for binding in host_bindings:
                                ports.append(f"{binding['HostPort']}:{container_port}")
                
                return {
                    "name": container_name,
                    "status": "running" if state['Running'] else "stopped",
                    "image": config['Image'],
                    "started_at": state.get('StartedAt', 'N/A'),
                    "ports": ports,
                    "health": state.get('Health', {}).get('Status', 'unknown') if 'Health' in state else 'no-healthcheck'
                }
            except:
                return None
        return None
    
    def show_workspace_status(self, workspace_id: str = None):
        """顯示工作區容器狀態"""
        if workspace_id and workspace_id in self.workspaces:
            workspaces_to_show = {workspace_id: self.workspaces[workspace_id]}
        else:
            workspaces_to_show = self.workspaces
        
        print("🐳 HFT Docker 容器狀態 (按工作區分組)")
        print("=" * 80)
        
        total_containers = 0
        running_containers = 0
        
        for ws_id, ws_config in workspaces_to_show.items():
            print(f"\n{ws_config['color']} {ws_config['name']}")
            print(f"   網絡: {ws_config['network']}")
            print("-" * 60)
            
            for container_name in ws_config['containers']:
                info = self.get_container_info(container_name)
                total_containers += 1
                
                if info:
                    status_icon = "🟢" if info['status'] == 'running' else "🔴"
                    health_icon = {
                        'healthy': '✅',
                        'unhealthy': '❌', 
                        'starting': '⏳',
                        'no-healthcheck': '➖',
                        'unknown': '❓'
                    }.get(info['health'], '❓')
                    
                    if info['status'] == 'running':
                        running_containers += 1
                    
                    ports_str = ", ".join(info['ports']) if info['ports'] else "No ports"
                    
                    print(f"   {status_icon} {container_name}")
                    print(f"      Image: {info['image']}")
                    print(f"      Status: {info['status']} {health_icon}")
                    print(f"      Ports: {ports_str}")
                    
                    if info['status'] == 'running' and info['started_at'] != 'N/A':
                        try:
                            started = datetime.fromisoformat(info['started_at'].replace('Z', '+00:00'))
                            uptime = datetime.now(started.tzinfo) - started
                            days = uptime.days
                            hours, remainder = divmod(uptime.seconds, 3600)
                            minutes, _ = divmod(remainder, 60)
                            
                            if days > 0:
                                uptime_str = f"{days}d {hours}h {minutes}m"
                            elif hours > 0:
                                uptime_str = f"{hours}h {minutes}m"
                            else:
                                uptime_str = f"{minutes}m"
                            
                            print(f"      Uptime: {uptime_str}")
                        except:
                            print(f"      Uptime: Unknown")
                    print()
                else:
                    print(f"   🔴 {container_name}")
                    print(f"      Status: not found or error")
                    print()
        
        print(f"📊 總覽: {running_containers}/{total_containers} 容器運行中")
        
        if running_containers == total_containers:
            print("🎉 所有容器運行正常！")
        else:
            print("⚠️  部分容器未運行，請檢查狀態")
    
    def show_images(self):
        """顯示 HFT 相關的 Docker 鏡像"""
        print("🐳 HFT Docker 鏡像列表")
        print("=" * 80)
        
        code, stdout, stderr = self.run_command("docker images --format json")
        if code == 0:
            images = []
            for line in stdout.strip().split('\n'):
                if line:
                    try:
                        image_data = json.loads(line)
                        repo = image_data.get('Repository', '')
                        if any(keyword in repo.lower() for keyword in ['hft', 'local', 'agno', 'redis', 'clickhouse', 'pgvector']):
                            images.append(image_data)
                    except:
                        continue
            
            # 按工作區分組顯示
            for ws_id, ws_config in self.workspaces.items():
                ws_images = []
                for image in images:
                    repo = image.get('Repository', '')
                    if ws_id == 'infrastructure':
                        if any(keyword in repo for keyword in ['redis', 'clickhouse', 'pgvector']):
                            ws_images.append(image)
                    else:
                        if f"hft-{ws_id}" in repo or f"local/hft-{ws_id}" in repo:
                            ws_images.append(image)
                
                if ws_images:
                    print(f"\n{ws_config['color']} {ws_config['name']}")
                    print("-" * 60)
                    for image in ws_images:
                        size = image.get('Size', 'N/A')
                        created = image.get('CreatedSince', 'N/A')
                        print(f"   📦 {image['Repository']}:{image['Tag']}")
                        print(f"      Size: {size}, Created: {created}")
                        print()
    
    def cleanup_unused(self):
        """清理未使用的 Docker 資源"""
        print("🧹 清理未使用的 Docker 資源...")
        
        # 清理未使用的容器
        print("\n1. 清理停止的容器...")
        code, stdout, stderr = self.run_command("docker container prune -f")
        if code == 0:
            print("✅ 停止的容器已清理")
        else:
            print(f"❌ 清理容器失敗: {stderr}")
        
        # 清理未使用的鏡像
        print("\n2. 清理懸空鏡像...")
        code, stdout, stderr = self.run_command("docker image prune -f")
        if code == 0:
            print("✅ 懸空鏡像已清理")
        else:
            print(f"❌ 清理鏡像失敗: {stderr}")
        
        # 清理未使用的網絡
        print("\n3. 清理未使用的網絡...")
        code, stdout, stderr = self.run_command("docker network prune -f")
        if code == 0:
            print("✅ 未使用的網絡已清理")
        else:
            print(f"❌ 清理網絡失敗: {stderr}")
        
        print("\n🎉 Docker 資源清理完成！")
    
    def show_networks(self):
        """顯示 Docker 網絡信息"""
        print("🌐 HFT Docker 網絡狀態")
        print("=" * 80)
        
        code, stdout, stderr = self.run_command("docker network ls --format json")
        if code == 0:
            networks = []
            for line in stdout.strip().split('\n'):
                if line:
                    try:
                        network_data = json.loads(line)
                        name = network_data.get('Name', '')
                        if 'hft' in name.lower() or name in ['bridge', 'host', 'none']:
                            networks.append(network_data)
                    except:
                        continue
            
            for network in networks:
                name = network['Name']
                driver = network['Driver']
                scope = network['Scope']
                
                # 檢查哪個工作區使用此網絡
                workspace_info = ""
                for ws_id, ws_config in self.workspaces.items():
                    if ws_config['network'] == name or name.startswith(f"hft-{ws_id}"):
                        workspace_info = f" ({ws_config['color']} {ws_config['name']})"
                        break
                
                print(f"🌐 {name}{workspace_info}")
                print(f"   Driver: {driver}, Scope: {scope}")
                print()
    
    def restart_workspace(self, workspace_id: str):
        """重啟指定工作區的所有容器"""
        if workspace_id not in self.workspaces:
            print(f"❌ 未知的工作區: {workspace_id}")
            return
        
        ws_config = self.workspaces[workspace_id]
        print(f"🔄 重啟 {ws_config['color']} {ws_config['name']}...")
        
        # 重啟容器
        for container_name in ws_config['containers']:
            print(f"   重啟 {container_name}...")
            code, stdout, stderr = self.run_command(f"docker restart {container_name}")
            if code == 0:
                print(f"   ✅ {container_name} 重啟成功")
            else:
                print(f"   ❌ {container_name} 重啟失敗: {stderr}")
        
        print("🎉 工作區重啟完成！")

def main():
    manager = DockerManager()
    
    if len(sys.argv) < 2:
        print("HFT Docker 容器管理工具")
        print("\n使用方法:")
        print("  python docker_manager.py status [workspace]  # 顯示容器狀態")
        print("  python docker_manager.py images              # 顯示鏡像列表") 
        print("  python docker_manager.py networks            # 顯示網絡狀態")
        print("  python docker_manager.py cleanup             # 清理未使用資源")
        print("  python docker_manager.py restart <workspace> # 重啟工作區")
        print("\n可用的工作區: master, ml, ops, infrastructure")
        sys.exit(1)
    
    command = sys.argv[1].lower()
    
    if command == "status":
        workspace = sys.argv[2] if len(sys.argv) > 2 else None
        manager.show_workspace_status(workspace)
    elif command == "images":
        manager.show_images()
    elif command == "networks":
        manager.show_networks()
    elif command == "cleanup":
        manager.cleanup_unused()
    elif command == "restart":
        if len(sys.argv) < 3:
            print("❌ 請指定要重啟的工作區")
            sys.exit(1)
        workspace = sys.argv[2]
        manager.restart_workspace(workspace)
    else:
        print(f"❌ 未知命令: {command}")
        sys.exit(1)

if __name__ == "__main__":
    main()