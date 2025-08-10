#!/usr/bin/env python3
"""
按照 Agno 官方規範測試工作區啟動

根據 https://docs.agno.com/workspaces/agent-app/local 的指導
"""

import subprocess
import time
import os
import requests
from typing import Dict, List

# HFT 工作區配置 (符合 Agno 官方規範)
WORKSPACES = {
    "hft-master": {
        "name": "HFT Master Control",
        "path": "/Users/proerror/Documents/monday/hft-master-workspace",
        "ui_url": "http://localhost:8504",
        "api_url": "http://localhost:8002",
        "ws_name": "hft-master"
    },
    "hft-ml": {
        "name": "HFT ML Workspace", 
        "path": "/Users/proerror/Documents/monday/hft-ml-workspace",
        "ui_url": "http://localhost:8502",
        "api_url": "http://localhost:8001", 
        "ws_name": "hft-ml"
    },
    "hft-ops": {
        "name": "HFT Ops Workspace",
        "path": "/Users/proerror/Documents/monday/hft-ops-workspace",
        "ui_url": "http://localhost:8503",
        "api_url": "http://localhost:8003",
        "ws_name": "hft-ops"
    }
}

def run_agno_command(command: str, workspace_path: str) -> tuple:
    """執行 Agno 工作區命令"""
    print(f"執行: {command} (在 {workspace_path})")
    
    try:
        result = subprocess.run(
            command.split(),
            cwd=workspace_path,
            capture_output=True,
            text=True,
            timeout=300  # 5分鐘超時 (Docker 構建需要時間)
        )
        return result.returncode, result.stdout, result.stderr
    except subprocess.TimeoutExpired:
        return -1, "", "Command timed out after 5 minutes"
    except Exception as e:
        return -1, "", str(e)

def check_service_health(url: str) -> bool:
    """檢查服務健康狀態"""
    try:
        response = requests.get(url, timeout=10)
        return response.status_code == 200
    except:
        return False

def check_workspace_config(workspace_id: str) -> bool:
    """檢查工作區配置"""
    config = WORKSPACES[workspace_id]
    workspace_path = config["path"]
    
    print(f"📋 檢查 {config['name']} 配置...")
    
    # 檢查必要文件是否存在
    required_files = [
        "workspace/settings.py",
        "workspace/dev_resources.py", 
        "agents/__init__.py",
        "api/main.py",
        "ui/Home.py"
    ]
    
    for file_path in required_files:
        full_path = os.path.join(workspace_path, file_path)
        if not os.path.exists(full_path):
            print(f"❌ 缺少必要文件: {file_path}")
            return False
    
    print(f"✅ {config['name']} 配置檢查通過")
    return True

def start_workspace(workspace_id: str) -> bool:
    """啟動單個工作區 (使用 Agno 官方方式)"""
    config = WORKSPACES[workspace_id]
    
    print(f"\n🚀 啟動 {config['name']}...")
    print(f"   路徑: {config['path']}")
    print(f"   工作區名稱: {config['ws_name']}")
    
    # 首先檢查配置
    if not check_workspace_config(workspace_id):
        return False
    
    # 執行 ag ws up -y (使用 -y 參數自動確認)
    returncode, stdout, stderr = run_agno_command("ag ws up -y", config["path"])
    
    if returncode == 0:
        print(f"✅ {config['name']} 啟動命令執行成功")
        
        # 等待服務啟動
        print("⏳ 等待服務啟動...")
        time.sleep(15)
        
        # 檢查服務健康狀態
        ui_healthy = check_service_health(config["ui_url"])
        api_healthy = check_service_health(f"{config['api_url']}/docs")
        
        if ui_healthy and api_healthy:
            print(f"🎉 {config['name']} 啟動成功並運行正常")
            print(f"   UI:  {config['ui_url']}")
            print(f"   API: {config['api_url']}/docs")
            return True
        else:
            print(f"⚠️  {config['name']} 啟動但服務不健康:")
            print(f"   UI:  {config['ui_url']} {'✅' if ui_healthy else '❌'}")
            print(f"   API: {config['api_url']}/docs {'✅' if api_healthy else '❌'}")
            return False
    else:
        print(f"❌ {config['name']} 啟動失敗:")
        print(f"   返回碼: {returncode}")
        print(f"   標準輸出: {stdout}")
        print(f"   錯誤輸出: {stderr}")
        return False

def check_workspace_status(workspace_id: str) -> Dict:
    """檢查工作區當前狀態"""
    config = WORKSPACES[workspace_id]
    
    ui_healthy = check_service_health(config["ui_url"])
    api_healthy = check_service_health(f"{config['api_url']}/docs")
    
    return {
        "name": config["name"],
        "ui_url": config["ui_url"],
        "api_url": config["api_url"],
        "ui_healthy": ui_healthy,
        "api_healthy": api_healthy,
        "overall_healthy": ui_healthy and api_healthy
    }

def show_all_status():
    """顯示所有工作區狀態"""
    print("📊 HFT 工作區狀態總覽 (Agno 官方規範)")
    print("=" * 60)
    
    all_healthy = True
    
    for workspace_id in WORKSPACES.keys():
        status = check_workspace_status(workspace_id)
        status_icon = "🟢" if status["overall_healthy"] else "🔴"
        
        print(f"\n{status_icon} {status['name']}")
        print(f"   UI:  {status['ui_url']} {'✅' if status['ui_healthy'] else '❌'}")
        print(f"   API: {status['api_url']}/docs {'✅' if status['api_healthy'] else '❌'}")
        
        if not status["overall_healthy"]:
            all_healthy = False
    
    if all_healthy:
        print(f"\n🎉 所有工作區運行正常！")
    else:
        print(f"\n⚠️  部分工作區有問題，請檢查配置")

def test_sequential_startup():
    """順序測試啟動所有工作區"""
    print("🎯 HFT 工作區順序啟動測試 (Agno 官方規範)")
    print("=" * 60)
    
    success_count = 0
    
    for workspace_id in WORKSPACES.keys():
        if start_workspace(workspace_id):
            success_count += 1
        else:
            print(f"⚠️  {workspace_id} 啟動失敗，跳過剩餘工作區")
            break
        
        # 啟動間隔，避免資源競爭
        time.sleep(5)
    
    print(f"\n📊 啟動結果: {success_count}/{len(WORKSPACES)} 工作區成功啟動")
    
    if success_count == len(WORKSPACES):
        print("\n🎉 所有 HFT 工作區啟動成功！")
        print("\n🌐 系統訪問地址:")
        for workspace_id, config in WORKSPACES.items():
            print(f"   {config['name']}: {config['ui_url']}")
    else:
        print("\n❌ 部分工作區啟動失敗")
    
    return success_count == len(WORKSPACES)

def main():
    """主函數"""
    import sys
    
    if len(sys.argv) < 2:
        print("使用方法:")
        print("  python test_agno_workspaces.py status  # 檢查當前狀態")
        print("  python test_agno_workspaces.py start   # 順序啟動所有工作區")
        sys.exit(1)
    
    command = sys.argv[1].lower()
    
    if command == "status":
        show_all_status()
    elif command == "start":
        test_sequential_startup()
    else:
        print(f"未知命令: {command}")
        sys.exit(1)

if __name__ == "__main__":
    main()