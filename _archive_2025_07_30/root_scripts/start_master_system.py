#!/usr/bin/env python3
"""
Master HFT System Launcher - Agno 標準啟動
==========================================

使用標準 Agno Workspace 命令啟動整個 HFT 系統
"""

import subprocess
import sys
import os
from pathlib import Path

def main():
    """主函數 - 使用 Agno 標準方式啟動 Master Workspace"""
    
    print("""
🚀 HFT Master System Launcher
============================
使用 Agno Workspace 標準命令啟動

系統架構:
  📦 Master Workspace (統一控制器)
    ├── 🦀 Rust HFT Core
    ├── 🧠 ML Workspace  
    └── 🛡️ Ops Workspace

開始啟動...
    """)
    
    # 切換到 master_workspace 目錄
    master_workspace_path = Path(__file__).parent / "master_workspace"
    
    if not master_workspace_path.exists():
        print("❌ Master workspace 目錄不存在!")
        return False
    
    print(f"📁 切換到目錄: {master_workspace_path}")
    os.chdir(master_workspace_path)
    
    try:
        # 使用標準 Agno 命令啟動
        print("🔧 執行: ag ws up --env dev")
        result = subprocess.run([
            "ag", "ws", "up", "--env", "dev"
        ], check=True)
        
        if result.returncode == 0:
            print("✅ Master Workspace 啟動成功！")
            print("""
🎉 系統已啟動！

監控系統狀態:
  ag ws config    # 查看配置
  ag ws down      # 停止系統
  
或運行自定義啟動器:
  python master_workspace_app.py
            """)
            return True
        else:
            print("❌ Master Workspace 啟動失敗")
            return False
            
    except subprocess.CalledProcessError as e:
        print(f"❌ 命令執行失敗: {e}")
        print("""
🔧 故障排除:

1. 確保已設置 workspace:
   cd master_workspace
   ag ws setup

2. 檢查配置:
   ag ws config

3. 使用備用啟動方式:
   python master_workspace_app.py
        """)
        return False
    
    except Exception as e:
        print(f"❌ 啟動異常: {e}")
        return False

if __name__ == "__main__":
    success = main()
    sys.exit(0 if success else 1)