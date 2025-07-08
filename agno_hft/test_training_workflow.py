#!/usr/bin/env python3
"""
測試完整的訓練工作流程
"""

import asyncio
import sys
import os

# 添加當前目錄到Python路徑
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hft_agents import HFTTeam
from rust_hft_tools import RustHFTTools

async def test_training_workflow():
    """測試訓練工作流程"""
    print("🧪 測試HFT Agent訓練工作流程")
    print("=" * 50)
    
    try:
        # 1. 初始化系統
        print("\n1️⃣ 初始化HFT Agent團隊...")
        hft_team = HFTTeam()
        print("✅ Agent團隊初始化完成")
        
        # 2. 測試訓練Agent
        print("\n2️⃣ 測試訓練Agent...")
        symbol = "SOLUSDT"
        hours = 1  # 使用較短時間進行測試
        
        print(f"📊 訓練參數: {symbol}, {hours}小時")
        
        # 直接測試RustHFTTools
        print("\n3️⃣ 測試Rust工具調用...")
        tools = RustHFTTools()
        
        # 測試命令生成
        test_command = await tools._run_rust_command("train_lob_transformer", {
            "symbol": symbol,
            "training-hours": hours,
            "help": True  # 只獲取幫助信息，不實際訓練
        })
        
        print(f"🔧 Rust命令測試結果: {test_command.get('success', False)}")
        
        # 4. 測試Agent調用
        print("\n4️⃣ 測試Agent調用...")
        training_result = await hft_team.training_agent.train_model(symbol, hours)
        
        print(f"🧠 Agent訓練結果: {training_result}")
        
        print("\n🎉 測試完成！")
        
        return True
        
    except Exception as e:
        print(f"❌ 測試失敗: {e}")
        import traceback
        traceback.print_exc()
        return False

async def main():
    """主測試函數"""
    success = await test_training_workflow()
    
    if success:
        print("\n✅ 系統測試通過！")
        print("🚀 可以正常運行: python start_hft.py")
    else:
        print("\n❌ 系統測試失敗，請檢查配置")

if __name__ == "__main__":
    asyncio.run(main())