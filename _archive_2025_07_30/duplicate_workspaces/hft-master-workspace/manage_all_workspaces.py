#!/usr/bin/env python3
"""
HFT 工作區統一管理腳本

此腳本負責協調三個 HFT 工作區的啟動、停止和狀態監控：
- hft-master-workspace (端口: 8504, 8002, 5432)
- hft-ml-workspace (端口: 8502, 8001, 5433)  
- hft-ops-workspace (端口: 8503, 8003, 5434)
"""

import subprocess
import time
import os
import sys
from pathlib import Path
import concurrent.futures
import requests
from typing import Dict, List, Tuple

# 工作區配置
WORKSPACES = {
    "master": {
        "path": "/Users/proerror/Documents/monday/hft-master-workspace",
        "ui_port": 8504,
        "api_port": 8002,
        "db_port": 5432,
        "name": "HFT Master Control"
    },
    "ml": {
        "path": "/Users/proerror/Documents/monday/hft-ml-workspace", 
        "ui_port": 8502,
        "api_port": 8001,
        "db_port": 5433,
        "name": "HFT ML Workspace"
    },
    "ops": {
        "path": "/Users/proerror/Documents/monday/hft-ops-workspace",
        "ui_port": 8503, 
        "api_port": 8003,
        "db_port": 5434,
        "name": "HFT Ops Workspace"
    }
}

def run_command(command: str, cwd: str = None) -> Tuple[int, str, str]:
    """執行 shell 命令並返回結果"""
    try:
        result = subprocess.run(
            command, 
            shell=True, 
            cwd=cwd,
            capture_output=True, 
            text=True,
            timeout=30
        )
        return result.returncode, result.stdout, result.stderr
    except subprocess.TimeoutExpired:
        return -1, "", "Command timed out"
    except Exception as e:
        return -1, "", str(e)

def check_port_status(port: int) -> bool:
    """檢查端口是否可用"""
    try:
        response = requests.get(f"http://localhost:{port}", timeout=5)
        return response.status_code == 200
    except:
        return False

def start_workspace(workspace_id: str) -> bool:
    """啟動單個工作區"""
    config = WORKSPACES[workspace_id]
    print(f"🚀 啟動 {config['name']} ({workspace_id})...")
    
    # 切換到工作區目錄
    os.chdir(config["path"])
    
    # 執行 ag ws up
    returncode, stdout, stderr = run_command("ag ws up")
    
    if returncode == 0:
        print(f"✅ {config['name']} 啟動成功")
        return True
    else:
        print(f"❌ {config['name']} 啟動失敗:")
        print(f"   stdout: {stdout}")
        print(f"   stderr: {stderr}")
        return False

def stop_workspace(workspace_id: str) -> bool:
    """停止單個工作區"""
    config = WORKSPACES[workspace_id]
    print(f"🛑 停止 {config['name']} ({workspace_id})...")
    
    # 切換到工作區目錄
    os.chdir(config["path"])
    
    # 執行 ag ws down
    returncode, stdout, stderr = run_command("ag ws down")
    
    if returncode == 0:
        print(f"✅ {config['name']} 停止成功")
        return True
    else:
        print(f"❌ {config['name']} 停止失敗:")
        print(f"   stdout: {stdout}")
        print(f"   stderr: {stderr}")
        return False

def check_workspace_status(workspace_id: str) -> Dict:
    """檢查工作區狀態"""
    config = WORKSPACES[workspace_id]
    
    ui_status = check_port_status(config["ui_port"])
    api_status = check_port_status(config["api_port"])
    
    return {
        "name": config["name"],
        "ui_port": config["ui_port"],
        "api_port": config["api_port"], 
        "db_port": config["db_port"],
        "ui_status": ui_status,
        "api_status": api_status,
        "overall_status": ui_status and api_status
    }

def start_all_workspaces():
    """並行啟動所有工作區"""
    print("🚀 開始啟動所有 HFT 工作區...")
    
    # 使用並行執行提高啟動速度
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as executor:
        futures = {
            executor.submit(start_workspace, ws_id): ws_id 
            for ws_id in WORKSPACES.keys()
        }
        
        results = {}
        for future in concurrent.futures.as_completed(futures):
            ws_id = futures[future]
            try:
                results[ws_id] = future.result()
            except Exception as e:
                print(f"❌ 工作區 {ws_id} 啟動異常: {e}")
                results[ws_id] = False
    
    # 等待服務啟動
    print("⏳ 等待服務啟動完成...")
    time.sleep(10)
    
    # 檢查最終狀態
    print("\n📊 工作區狀態檢查:")
    all_healthy = True
    for ws_id in WORKSPACES.keys():
        status = check_workspace_status(ws_id)
        status_icon = "🟢" if status["overall_status"] else "🔴"
        print(f"   {status_icon} {status['name']}")
        print(f"      UI:  http://localhost:{status['ui_port']} {'✅' if status['ui_status'] else '❌'}")
        print(f"      API: http://localhost:{status['api_port']} {'✅' if status['api_status'] else '❌'}")
        print(f"      DB:  localhost:{status['db_port']}")
        
        if not status["overall_status"]:
            all_healthy = False
    
    if all_healthy:
        print("\n🎉 所有工作區啟動成功！")
        print("\n🌐 訪問地址:")
        print("   🎛️  HFT Master Control: http://localhost:8504")
        print("   🤖 HFT ML Workspace:   http://localhost:8502") 
        print("   ⚡ HFT Ops Workspace:  http://localhost:8503")
    else:
        print("\n⚠️  部分工作區啟動失敗，請檢查日誌")

def stop_all_workspaces():
    """停止所有工作區"""
    print("🛑 開始停止所有 HFT 工作區...")
    
    # 並行停止所有工作區
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as executor:
        futures = {
            executor.submit(stop_workspace, ws_id): ws_id 
            for ws_id in WORKSPACES.keys()
        }
        
        for future in concurrent.futures.as_completed(futures):
            ws_id = futures[future]
            try:
                future.result()
            except Exception as e:
                print(f"❌ 工作區 {ws_id} 停止異常: {e}")
    
    print("✅ 所有工作區停止完成")

def show_status():
    """顯示所有工作區狀態"""
    print("📊 HFT 工作區狀態總覽:")
    print("=" * 60)
    
    for ws_id in WORKSPACES.keys():
        status = check_workspace_status(ws_id)
        status_icon = "🟢" if status["overall_status"] else "🔴"
        print(f"\n{status_icon} {status['name']}")
        print(f"   UI:  http://localhost:{status['ui_port']} {'✅' if status['ui_status'] else '❌'}")
        print(f"   API: http://localhost:{status['api_port']} {'✅' if status['api_status'] else '❌'}")
        print(f"   DB:  localhost:{status['db_port']}")

def main():
    """主函數"""
    if len(sys.argv) < 2:
        print("使用方法:")
        print("  python manage_all_workspaces.py start   # 啟動所有工作區")
        print("  python manage_all_workspaces.py stop    # 停止所有工作區") 
        print("  python manage_all_workspaces.py status  # 檢查狀態")
        sys.exit(1)
    
    command = sys.argv[1].lower()
    
    if command == "start":
        start_all_workspaces()
    elif command == "stop":
        stop_all_workspaces()
    elif command == "status":
        show_status()
    else:
        print(f"未知命令: {command}")
        sys.exit(1)

if __name__ == "__main__":
    main()