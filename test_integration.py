#!/usr/bin/env python3
"""
Rust HFT + Agno Framework 集成測試
測試Python綁定和Agent系統是否正常工作
"""

import sys
import os
import asyncio
import logging

# 設置日誌
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

def test_rust_hft_import():
    """測試Rust HFT Python綁定導入"""
    try:
        # 測試導入模塊（如果已編譯）
        try:
            import rust_hft_py
            logger.info("✅ 成功導入 rust_hft_py 模塊")
            
            # 測試創建HftEngine
            engine = rust_hft_py.HftEngine()
            logger.info("✅ 成功創建 HftEngine 實例")
            
            # 測試獲取系統狀態
            status = engine.get_system_status()
            logger.info(f"✅ 系統狀態: {status.overall_status}")
            
            return True
            
        except ImportError as e:
            logger.warning(f"⚠️ rust_hft_py 模塊未編譯或不可用: {e}")
            logger.info("📋 這是正常的，因為PyO3綁定需要編譯後才能使用")
            return False
            
    except Exception as e:
        logger.error(f"❌ Rust綁定測試失敗: {e}")
        return False

def test_agno_agents_import():
    """測試Agno Agent系統導入"""
    try:
        # 添加agno_hft目錄到路徑
        agno_path = os.path.join(os.path.dirname(__file__), 'agno_hft')
        if os.path.exists(agno_path):
            sys.path.insert(0, agno_path)
            
            # 測試導入Agent系統
            from hft_agents import HFTTeam
            logger.info("✅ 成功導入 HFTTeam")
            
            # 測試導入工具
            from rust_hft_tools import RustHFTTools
            logger.info("✅ 成功導入 RustHFTTools")
            
            return True
        else:
            logger.warning(f"⚠️ agno_hft 目錄不存在: {agno_path}")
            return False
            
    except Exception as e:
        logger.error(f"❌ Agno Agent導入測試失敗: {e}")
        return False

async def test_agent_workflow():
    """測試Agent工作流程"""
    try:
        # 添加agno_hft目錄到路徑
        agno_path = os.path.join(os.path.dirname(__file__), 'agno_hft')
        if not os.path.exists(agno_path):
            logger.warning("⚠️ agno_hft 目錄不存在，跳過工作流程測試")
            return False
            
        sys.path.insert(0, agno_path)
        
        from hft_agents import HFTTeam
        from rust_hft_tools import RustHFTTools
        
        # 創建HFT團隊
        logger.info("🔄 創建 HFT Agent 團隊...")
        hft_team = HFTTeam()
        
        # 測試獲取系統狀態
        logger.info("🔄 測試系統狀態獲取...")
        status = await hft_team.get_status()
        logger.info(f"✅ 系統狀態: {status}")
        
        # 測試主控Agent
        logger.info("🔄 測試主控Agent...")
        response = await hft_team.master_agent.process_user_request("檢查系統狀態")
        logger.info(f"✅ Agent回應: {response[:100]}...")
        
        return True
        
    except Exception as e:
        logger.error(f"❌ Agent工作流程測試失敗: {e}")
        return False

def test_cli_availability():
    """測試CLI可用性"""
    try:
        rust_hft_dir = os.path.join(os.path.dirname(__file__), 'rust_hft')
        if os.path.exists(rust_hft_dir):
            
            # 檢查是否有編譯的CLI二進制文件
            target_dir = os.path.join(rust_hft_dir, 'target', 'debug')
            cli_binary = os.path.join(target_dir, 'hft-cli')
            
            if os.path.exists(cli_binary):
                logger.info(f"✅ 發現CLI二進制文件: {cli_binary}")
                return True
            else:
                logger.info(f"📋 CLI二進制文件不存在，需要運行 'cargo build --bin hft-cli'")
                
                # 檢查源文件
                cli_source = os.path.join(rust_hft_dir, 'src', 'bin', 'hft-cli.rs')
                if os.path.exists(cli_source):
                    logger.info(f"✅ CLI源文件存在: {cli_source}")
                    return True
                else:
                    logger.warning(f"⚠️ CLI源文件不存在: {cli_source}")
                    return False
        else:
            logger.warning(f"⚠️ rust_hft 目錄不存在: {rust_hft_dir}")
            return False
            
    except Exception as e:
        logger.error(f"❌ CLI可用性測試失敗: {e}")
        return False

def test_project_structure():
    """測試項目結構完整性"""
    try:
        base_dir = os.path.dirname(__file__)
        
        # 檢查關鍵目錄和文件
        required_paths = [
            'rust_hft/src/lib.rs',
            'rust_hft/src/python_bindings/mod.rs',
            'rust_hft/src/python_bindings/hft_engine.rs',
            'rust_hft/Cargo.toml',
            'agno_hft/hft_agents.py',
            'agno_hft/rust_hft_tools.py',
            'agno_hft/main.py',
        ]
        
        all_exists = True
        for path in required_paths:
            full_path = os.path.join(base_dir, path)
            if os.path.exists(full_path):
                logger.info(f"✅ {path}")
            else:
                logger.warning(f"⚠️ 缺失: {path}")
                all_exists = False
        
        return all_exists
        
    except Exception as e:
        logger.error(f"❌ 項目結構測試失敗: {e}")
        return False

async def main():
    """主測試函數"""
    logger.info("🚀 開始 Rust HFT + Agno Framework 集成測試")
    logger.info("=" * 60)
    
    results = {}
    
    # 測試項目結構
    logger.info("📁 測試項目結構...")
    results['project_structure'] = test_project_structure()
    
    # 測試CLI可用性
    logger.info("⚡ 測試CLI可用性...")
    results['cli_availability'] = test_cli_availability()
    
    # 測試Agno Agent導入
    logger.info("🤖 測試Agno Agent導入...")
    results['agno_import'] = test_agno_agents_import()
    
    # 測試Rust綁定導入
    logger.info("🦀 測試Rust綁定導入...")
    results['rust_import'] = test_rust_hft_import()
    
    # 測試Agent工作流程
    if results['agno_import']:
        logger.info("🔄 測試Agent工作流程...")
        results['agent_workflow'] = await test_agent_workflow()
    else:
        results['agent_workflow'] = False
    
    # 總結結果
    logger.info("=" * 60)
    logger.info("📊 測試結果總結:")
    
    for test_name, passed in results.items():
        status = "✅ 通過" if passed else "❌ 失敗"
        logger.info(f"  {test_name}: {status}")
    
    passed_count = sum(results.values())
    total_count = len(results)
    
    logger.info(f"📈 總體結果: {passed_count}/{total_count} 測試通過")
    
    # 提供建議
    logger.info("=" * 60)
    logger.info("💡 後續建議:")
    
    if not results['rust_import']:
        logger.info("  1. 編譯Rust Python綁定: cd rust_hft && maturin develop --features python")
    
    if not results['cli_availability']:
        logger.info("  2. 編譯CLI工具: cd rust_hft && cargo build --bin hft-cli")
    
    if not results['agno_import']:
        logger.info("  3. 安裝Python依賴: cd agno_hft && pip install -r requirements.txt")
    
    if passed_count == total_count:
        logger.info("🎉 所有測試通過！系統準備就緒。")
    else:
        logger.info("🔧 請根據上述建議完成缺失的組件。")
    
    return passed_count == total_count

if __name__ == "__main__":
    asyncio.run(main())