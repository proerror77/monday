#!/usr/bin/env python3
"""
Agno 工作區自動確認部署工具
==========================

解決 Agno workspace 需要手動確認的問題
通過自動化輸入 'Y' 來確認部署
"""

import subprocess
import sys
import time
import signal
import os
from pathlib import Path

def auto_deploy_workspace(workspace_name, timeout=600):
    """
    自動部署工作區，自動確認所有提示
    
    Args:
        workspace_name: 工作區名稱
        timeout: 超時時間（秒）
    """
    print(f"🚀 自動部署 {workspace_name}...")
    
    # 創建日誌目錄
    log_dir = Path("../logs")
    log_dir.mkdir(exist_ok=True)
    log_file = log_dir / f"{workspace_name}_auto_deploy.log"
    
    try:
        # 使用 pexpect 風格的自動輸入
        process = subprocess.Popen(
            ["ag", "ws", "up", "--env", "dev"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
            universal_newlines=True
        )
        
        output_lines = []
        start_time = time.time()
        
        while True:
            # 檢查超時
            if time.time() - start_time > timeout:
                print(f"❌ {workspace_name} 部署超時")
                process.terminate()
                return False
            
            # 檢查進程是否結束
            if process.poll() is not None:
                break
            
            # 讀取輸出
            try:
                line = process.stdout.readline()
                if line:
                    output_lines.append(line)
                    print(f"📝 {line.strip()}")
                    
                    # 檢查是否需要確認
                    if "Confirm deploy [Y/n]:" in line or "Confirm" in line:
                        print("✅ 發送自動確認...")
                        process.stdin.write("Y\n")
                        process.stdin.flush()
                        time.sleep(1)
                        
                    elif "Confirm delete [Y/n]:" in line:
                        print("✅ 發送刪除確認...")
                        process.stdin.write("Y\n")
                        process.stdin.flush()
                        time.sleep(1)
                        
                else:
                    time.sleep(0.1)
                    
            except Exception as e:
                print(f"⚠️ 讀取輸出時出錯: {e}")
                break
        
        # 等待進程完成
        return_code = process.wait()
        
        # 寫入日誌
        with open(log_file, 'w') as f:
            f.write('\n'.join(output_lines))
        
        if return_code == 0:
            print(f"✅ {workspace_name} 部署成功")
            return True
        else:
            print(f"❌ {workspace_name} 部署失敗 (返回碼: {return_code})")
            return False
            
    except Exception as e:
        print(f"❌ 部署 {workspace_name} 時發生錯誤: {e}")
        return False

def main():
    """主函數"""
    if len(sys.argv) < 2:
        print("用法: python agno_auto_confirm.py <workspace_name>")
        print("例如: python agno_auto_confirm.py master_workspace")
        sys.exit(1)
    
    workspace_name = sys.argv[1]
    
    # 檢查工作區目錄是否存在
    if not os.path.isdir(workspace_name):
        print(f"❌ 工作區目錄 {workspace_name} 不存在")
        sys.exit(1)
    
    # 切換到工作區目錄
    original_dir = os.getcwd()
    os.chdir(workspace_name)
    
    try:
        success = auto_deploy_workspace(workspace_name)
        sys.exit(0 if success else 1)
    finally:
        os.chdir(original_dir)

if __name__ == "__main__":
    main()