#!/usr/bin/env python3
"""測試統一啟動功能"""

import subprocess
import time
import os
from pathlib import Path

WORKSPACES = [
    {
        "name": "HFT Master Control",
        "path": "/Users/proerror/Documents/monday/hft-master-workspace",
        "ui_port": 8504
    },
    {
        "name": "HFT ML Workspace", 
        "path": "/Users/proerror/Documents/monday/hft-ml-workspace",
        "ui_port": 8502
    },
    {
        "name": "HFT Ops Workspace",
        "path": "/Users/proerror/Documents/monday/hft-ops-workspace", 
        "ui_port": 8503
    }
]

def start_workspace_with_auto_confirm(workspace):
    """啟動工作區並自動確認"""
    print(f"🚀 啟動 {workspace['name']}...")
    
    # 切換到工作區目錄
    os.chdir(workspace["path"])
    
    # 執行 ag ws up 並自動確認
    try:
        process = subprocess.Popen(
            ["ag", "ws", "up"], 
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True
        )
        
        # 自動發送 "Y" 來確認
        stdout, _ = process.communicate(input="Y\n", timeout=60)
        
        if process.returncode == 0:
            print(f"✅ {workspace['name']} 啟動成功")
            print(f"   UI: http://localhost:{workspace['ui_port']}")
            return True
        else:
            print(f"❌ {workspace['name']} 啟動失敗:")
            print(f"   輸出: {stdout}")
            return False
            
    except subprocess.TimeoutExpired:
        process.kill()
        print(f"❌ {workspace['name']} 啟動超時")
        return False
    except Exception as e:
        print(f"❌ {workspace['name']} 啟動異常: {e}")
        return False

def main():
    print("🎯 測試 HFT 三工作區統一啟動")
    print("=" * 50)
    
    success_count = 0
    
    for workspace in WORKSPACES:
        if start_workspace_with_auto_confirm(workspace):
            success_count += 1
        print()  # 空行分隔
        time.sleep(5)  # 等待一下再啟動下一個
    
    print(f"📊 啟動結果: {success_count}/{len(WORKSPACES)} 工作區成功啟動")
    
    if success_count == len(WORKSPACES):
        print("\n🎉 所有工作區啟動成功！")
        print("\n🌐 HFT 系統訪問入口：")
        for ws in WORKSPACES:
            print(f"   {ws['name']}: http://localhost:{ws['ui_port']}")
    else:
        print("\n⚠️  部分工作區啟動失敗，請檢查配置")

if __name__ == "__main__":
    main()