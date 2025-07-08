#!/usr/bin/env python3
"""
HFT 多執行緒訓練演示
展示如何同時訓練多個商品的模型
"""

import asyncio
import logging
from pipeline_manager import pipeline_manager, PipelineType

# 設置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

async def demo_multi_training():
    """演示多商品同時訓練"""
    print("🚀 HFT 多執行緒訓練演示")
    print("=" * 50)
    
    # 要訓練的商品列表
    symbols = ['BTCUSDT', 'ETHUSDT', 'SOLUSDT']
    
    print(f"📋 即將同時訓練 {len(symbols)} 個商品:")
    for i, symbol in enumerate(symbols, 1):
        print(f"   {i}. {symbol}")
    
    # 提交訓練任務
    print("\n🔄 提交訓練任務...")
    task_ids = []
    
    for symbol in symbols:
        task_id = await pipeline_manager.submit_task(
            PipelineType.TRAINING,
            symbol,
            hours=1  # 演示用，只訓練1小時
        )
        task_ids.append(task_id)
        print(f"✅ {symbol} 訓練任務已提交，任務ID: {task_id}")
    
    print(f"\n📊 總共提交了 {len(task_ids)} 個任務")
    
    # 監控任務狀態
    print("\n🔍 監控任務狀態...")
    for _ in range(5):  # 檢查5次
        await asyncio.sleep(2)
        status = await pipeline_manager.get_status()
        
        print(f"\n📈 當前狀態:")
        print(f"   - 活躍任務: {status.get('active_tasks', 0)}")
        print(f"   - 等待任務: {status.get('pending_tasks', 0)}")
        print(f"   - 完成任務: {status.get('completed_tasks', 0)}")
        
        # 顯示任務詳情
        task_details = status.get('task_details', {})
        if task_details:
            print(f"   📋 任務詳情:")
            for task_id, info in task_details.items():
                status_icon = "🔄" if info['status'] == 'running' else "⏳"
                print(f"      {status_icon} {task_id}: {info['symbol']} ({info['status']})")
    
    print("\n🎯 演示完成！")
    print("💡 實際使用時，您可以通過 main.py 的對話界面進行操作")

async def demo_commands():
    """演示主要命令使用方法"""
    print("\n📚 主要命令使用方法:")
    print("=" * 50)
    
    commands = [
        ("訓練單個商品", "訓練SOLUSDT模型"),
        ("同時訓練多個商品", "同時訓練多個商品"),
        ("檢查系統狀態", "檢查系統狀態"),
        ("停止所有任務", "停止所有任務"),
        ("完整交易流程", "用1000 USDT交易BTCUSDT"),
    ]
    
    for desc, command in commands:
        print(f"🔹 {desc}:")
        print(f"   用戶輸入: '{command}'")
        print()

async def main():
    """主函數"""
    try:
        # 演示多執行緒訓練
        await demo_multi_training()
        
        # 演示命令使用
        await demo_commands()
        
    except KeyboardInterrupt:
        print("\n🛑 演示中斷")
    except Exception as e:
        logger.error(f"❌ 演示過程中發生錯誤: {e}")
        print(f"❌ 錯誤: {e}")
    finally:
        # 清理資源
        await pipeline_manager.stop_all_tasks()
        print("✅ 演示結束，資源已清理")

if __name__ == "__main__":
    asyncio.run(main())