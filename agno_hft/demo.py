#!/usr/bin/env python3
"""
HFT Agno系統演示腳本
展示如何使用自然語言與Agent交互
"""

import asyncio
import sys
import os

# 添加當前目錄到Python路徑
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from hft_agents import HFTTeam
from rust_hft_tools import RustHFTTools

async def demo_basic_functionality():
    """演示基本功能"""
    print("🎯 HFT Agno Agent 系統演示")
    print("=" * 50)
    
    try:
        # 1. 初始化系統
        print("\n1️⃣ 初始化HFT Agent團隊...")
        hft_team = HFTTeam()
        print("✅ Agent團隊初始化完成")
        
        # 2. 測試系統狀態
        print("\n2️⃣ 檢查系統狀態...")
        status = await hft_team.get_status()
        print(f"📊 系統狀態: {status}")
        
        # 3. 測試主控Agent對話
        print("\n3️⃣ 測試Agent對話功能...")
        print("💬 用戶請求: '檢查系統狀態'")
        
        try:
            response = await hft_team.master_agent.process_user_request("檢查系統狀態")
            print(f"🤖 Agent回應: {response[:200]}...")
        except Exception as e:
            print(f"⚠️ Agent對話測試跳過 (需要網絡): {e}")
        
        # 4. 測試風險評估
        print("\n4️⃣ 測試風險評估功能...")
        print("💰 評估參數: BTCUSDT, 1000 USDT")
        
        try:
            risk_result = await hft_team.risk_agent.assess_trading_risk("BTCUSDT", 1000.0)
            if risk_result.get('success'):
                print("✅ 風險評估完成")
                print(f"📋 推薦參數: {risk_result.get('recommendations', {})}")
            else:
                print("⚠️ 風險評估需要網絡連接")
        except Exception as e:
            print(f"⚠️ 風險評估測試跳過: {e}")
        
        # 5. 展示支持的交易對
        print("\n5️⃣ 支持的交易對:")
        tools = RustHFTTools()
        symbols = tools.get_supported_symbols()
        for symbol in symbols:
            is_valid = tools.validate_symbol(symbol)
            status = "✅" if is_valid else "❌"
            print(f"   {status} {symbol}")
        
        print("\n🎉 系統演示完成！")
        print("\n📝 使用方法:")
        print("   python start_hft.py")
        print("   然後使用自然語言與Agent對話")
        
        return True
        
    except Exception as e:
        print(f"❌ 演示失敗: {e}")
        import traceback
        traceback.print_exc()
        return False

async def demo_conversation_examples():
    """演示對話示例"""
    print("\n💬 對話示例:")
    print("-" * 30)
    
    examples = [
        "檢查系統狀態",
        "訓練SOLUSDT模型", 
        "用500 USDT交易BTCUSDT",
        "評估模型性能",
        "停止所有任務",
        "緊急停止"
    ]
    
    for i, example in enumerate(examples, 1):
        print(f"{i}. 💬 您: {example}")
        print(f"   🤖 Agent: [根據請求執行相應操作]")
    
    print("\n✨ 系統特性:")
    print("• 🧠 智能理解: 自動識別用戶意圖")
    print("• 🔄 完整流程: 訓練→評估→測試→部署")
    print("• 🛡️ 風險控制: 多層安全防護")
    print("• ⚡ 高性能: Rust核心引擎")
    print("• 🤖 多Agent: 專業化團隊協作")

async def main():
    """主演示函數"""
    success = await demo_basic_functionality()
    await demo_conversation_examples()
    
    if success:
        print("\n🚀 系統準備就緒！運行 'python start_hft.py' 開始使用")
    else:
        print("\n⚠️ 請檢查系統配置")

if __name__ == "__main__":
    asyncio.run(main())