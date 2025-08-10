#!/usr/bin/env python3
"""
HFT 系統監控面板
==============

實時顯示系統狀態、數據統計和健康信息
"""

import asyncio
import json
import time
from datetime import datetime
from pathlib import Path
import sys

# 添加路徑
sys.path.append(str(Path(__file__).parent / "master_workspace"))

from master_workspace_app import MasterWorkspaceApp

async def get_system_metrics():
    """獲取系統指標"""
    app = MasterWorkspaceApp()
    await app.setup()
    
    # 獲取系統狀態
    status = await app.get_system_status()
    
    return status

def format_uptime(seconds):
    """格式化運行時間"""
    hours = int(seconds // 3600)
    minutes = int((seconds % 3600) // 60)
    secs = int(seconds % 60)
    return f"{hours:02d}:{minutes:02d}:{secs:02d}"

def display_dashboard(status):
    """顯示監控面板"""
    print("\033[2J\033[H")  # 清屏
    print("🚀 HFT 系統監控面板")
    print("=" * 60)
    
    # 系統狀態
    state_icon = "🟢" if status["system_state"] == "running" else "🔴"
    print(f"📊 系統狀態: {state_icon} {status['system_state'].upper()}")
    
    if status.get("start_time"):
        print(f"⏰ 啟動時間: {status['start_time']}")
        print(f"⏱️  運行時長: {format_uptime(status.get('uptime_seconds', 0))}")
    
    # 組件狀態
    print("\n🔧 組件狀態:")
    for name, state in status["components"].items():
        icon = "🟢" if state == "healthy" else "🔴"
        print(f"  {icon} {name}: {state}")
    
    # 統計信息
    stats = status.get("stats", {})
    print(f"\n📈 統計信息:")
    print(f"  🔄 重啟次數: {stats.get('restart_count', 0)}")
    print(f"  ❌ 故障次數: {stats.get('component_failures', 0)}")
    print(f"  📝 故障記錄: {status.get('total_failures', 0)} 條")
    
    # 最近故障
    recent_failures = status.get("recent_failures", [])
    if recent_failures:
        print(f"\n⚠️  最近故障:")
        for failure in recent_failures[-3:]:  # 只顯示最近3個
            print(f"  📍 {failure['timestamp'][:19]} - {failure['component']}: {failure['error'][:50]}...")
    
    print(f"\n🕐 更新時間: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    print("按 Ctrl+C 退出監控")

async def monitor_loop():
    """監控循環"""
    while True:
        try:
            status = await get_system_metrics()
            display_dashboard(status)
            await asyncio.sleep(5)  # 每5秒更新一次
        except KeyboardInterrupt:
            print("\n👋 監控已停止")
            break
        except Exception as e:
            print(f"❌ 監控錯誤: {e}")
            await asyncio.sleep(10)

if __name__ == "__main__":
    asyncio.run(monitor_loop())