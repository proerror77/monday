#!/usr/bin/env python3
"""
集成測試腳本
驗證Agno HFT系統的各個組件
"""

import asyncio
import sys
import os
import logging

# 添加當前目錄到Python路徑
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from rust_hft_tools import RustHFTTools
from hft_agents import HFTTeam

# 設置日誌
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

async def test_rust_hft_tools():
    """測試Rust HFT工具"""
    print("\n🔧 測試Rust HFT工具...")
    
    try:
        tools = RustHFTTools()
        
        # 測試系統狀態
        status = await tools.get_system_status()
        print(f"✅ 系統狀態: {status}")
        
        # 測試支持的交易對
        symbols = tools.get_supported_symbols()
        print(f"✅ 支持的交易對: {symbols}")
        
        # 測試符號驗證
        is_valid = tools.validate_symbol("BTCUSDT")
        print(f"✅ BTCUSDT驗證: {is_valid}")
        
        return True
        
    except Exception as e:
        print(f"❌ Rust工具測試失敗: {e}")
        return False

async def test_hft_agents():
    """測試HFT Agent團隊"""
    print("\n🤖 測試HFT Agent團隊...")
    
    try:
        team = HFTTeam()
        
        # 測試主控Agent
        response = await team.master_agent.process_user_request("檢查系統狀態")
        print(f"✅ 主控Agent響應: {response[:100]}...")
        
        # 測試風險評估
        risk_assessment = await team.risk_agent.assess_trading_risk("BTCUSDT", 1000.0)
        print(f"✅ 風險評估: {risk_assessment.get('success', False)}")
        
        return True
        
    except Exception as e:
        print(f"❌ Agent團隊測試失敗: {e}")
        return False

async def test_workflow_simulation():
    """測試工作流程模擬"""
    print("\n🔄 測試工作流程...")
    
    try:
        team = HFTTeam()
        
        # 模擬小規模工作流程（僅到評估階段）
        print("📊 模擬風險評估...")
        risk_result = await team.risk_agent.assess_trading_risk("SOLUSDT", 100.0)
        
        if risk_result.get('success'):
            print("✅ 風險評估通過")
            print(f"   建議: {risk_result.get('recommendations', {})}")
        else:
            print("❌ 風險評估失敗")
        
        return True
        
    except Exception as e:
        print(f"❌ 工作流程測試失敗: {e}")
        return False

async def main():
    """主測試函數"""
    print("🧪 開始HFT系統集成測試")
    print("="*50)
    
    test_results = []
    
    # 測試各個組件
    test_results.append(("Rust HFT工具", await test_rust_hft_tools()))
    test_results.append(("HFT Agent團隊", await test_hft_agents()))
    test_results.append(("工作流程模擬", await test_workflow_simulation()))
    
    # 顯示測試結果
    print("\n" + "="*50)
    print("📊 測試結果總結:")
    print("="*50)
    
    all_passed = True
    for test_name, result in test_results:
        status = "✅ 通過" if result else "❌ 失敗"
        print(f"{test_name}: {status}")
        if not result:
            all_passed = False
    
    if all_passed:
        print("\n🎉 所有測試通過！HFT系統準備就緒")
        print("\n💡 使用方法:")
        print("   python start_hft.py")
        print("   或者")
        print("   python main.py")
    else:
        print("\n⚠️ 部分測試失敗，請檢查系統配置")
    
    return all_passed

if __name__ == "__main__":
    try:
        success = asyncio.run(main())
        sys.exit(0 if success else 1)
    except KeyboardInterrupt:
        print("\n👋 測試中斷")
        sys.exit(1)
    except Exception as e:
        print(f"❌ 測試異常: {e}")
        sys.exit(1)