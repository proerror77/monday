#!/usr/bin/env python3
"""
Master HFT Agent 測試腳本
======================

測試改進後的組件檢測邏輯
"""

import asyncio
import logging
from master_hft_agent import MasterHFTAgent

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

async def test_component_detection():
    """測試組件檢測功能"""
    
    print("""
🧪 Master HFT Agent 組件檢測測試
=====================================
測試改進後的健康檢查邏輯
    """)
    
    master = MasterHFTAgent()
    
    # 測試基礎設施檢測
    print("\n📦 測試基礎設施檢測...")
    redis_ok = await master._check_redis_connection()
    clickhouse_ok = await master._check_clickhouse_connection()
    
    print(f"Redis 連接: {'✅' if redis_ok else '❌'}")
    print(f"ClickHouse 連接: {'✅' if clickhouse_ok else '❌'}")
    
    # 測試端口檢測
    print("\n🔌 測試端口檢測...")
    ports_to_check = [6379, 8123, 50051]
    for port in ports_to_check:
        is_open = await master._check_port_open(port)
        print(f"端口 {port}: {'✅ 開放' if is_open else '❌ 關閉'}")
    
    # 測試工作區健康檢查
    print("\n🏢 測試工作區健康檢查...")
    ml_health = await master._check_workspace_health("ml")
    ops_health = await master._check_workspace_health("ops")
    
    print(f"ML Workspace: {'✅' if ml_health else '❌'}")
    print(f"Ops Workspace: {'✅' if ops_health else '❌'}")
    
    # 獲取當前系統狀態
    print("\n📊 當前系統狀態:")
    status = master.get_system_status()
    
    print(f"系統狀態: {status['system_state']}")
    print("組件狀態:")
    for name, state in status['components'].items():
        icon = "✅" if state == "healthy" else "❌"
        print(f"  {icon} {name}: {state}")
    
    print(f"\n統計信息:")
    print(f"  重啟次數: {status['stats']['restart_count']}")
    print(f"  運行時間: {status['uptime_seconds']:.1f} 秒")
    
    print("\n🎯 檢測測試完成!")

async def main():
    """主函數"""
    try:
        await test_component_detection()
    except KeyboardInterrupt:
        logger.info("🛑 測試被中斷")
    except Exception as e:
        logger.error(f"❌ 測試異常: {e}")

if __name__ == "__main__":
    asyncio.run(main())