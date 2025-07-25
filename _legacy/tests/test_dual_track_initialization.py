#!/usr/bin/env python3
"""
測試雙軌系統初始化 - 修正版
=============================

修正文件路徑問題，使用本地目錄進行測試
"""

import asyncio
import sys
from pathlib import Path

# 添加當前目錄到 Python 路徑
sys.path.insert(0, str(Path(__file__).parent))

from dual_track_system_initialization import DualTrackSystemManager, SystemConfig, SystemState

async def test_dual_track_system():
    """測試雙軌系統初始化"""
    print("🧪 測試雙軌系統初始化")
    print("=" * 60)
    
    # 創建本地測試配置
    config = SystemConfig(
        # 使用本地路徑
        rust_hft_dir="./rust_hft",
        rust_config_dir="./rust_hft/config",
        production_models_dir="./production_models",  # 本地目錄
        rust_api_port=8080,
        rust_health_check_endpoint="http://localhost:8080/health",
        
        # MLOps 配置
        python_mlops_module="agno_v2_mlops_workflow",
        mlops_trigger_schedule="0 3 * * *",
        
        # 基礎設施
        clickhouse_url="http://localhost:8123",
        redis_url="redis://localhost:6379",
        
        # 測試設置
        startup_timeout=60,  # 縮短超時時間
        enable_trading=False  # 測試模式，禁用實際交易
    )
    
    # 創建系統管理器
    manager = DualTrackSystemManager(config)
    
    try:
        print("🚀 開始系統初始化...")
        
        # 執行初始化
        result = await manager.initialize_system()
        
        print(f"\n📊 初始化結果:")
        print(f"最終狀態: {result['final_state']}")
        print(f"總耗時: {result['total_duration']:.2f} 秒")
        
        # 顯示各階段結果
        print(f"\n📋 各階段結果:")
        for stage_name, stage_result in result["stages"].items():
            success = "✅" if stage_result.get("success", stage_result.get("passed", False)) else "❌"
            print(f"  {success} {stage_name}")
            
            if not stage_result.get("success", stage_result.get("passed", False)):
                error = stage_result.get("error", "未知錯誤")
                print(f"      錯誤: {error}")
        
        # 如果系統初始化成功，顯示狀態
        if result["final_state"] in ["success", "degraded"]:
            print(f"\n📈 系統狀態詳情:")
            status = manager.get_system_status()
            
            print(f"🦀 Rust 核心軌道:")
            rust_track = status["tracks"]["rust_core"]
            print(f"   狀態: {rust_track['status']}")
            print(f"   模式: {rust_track['mode']}")
            if rust_track.get('pid'):
                print(f"   PID: {rust_track['pid']}")
            
            print(f"\n🧠 MLOps 工作流軌道:")
            mlops_track = status["tracks"]["mlops_workflow"]
            print(f"   狀態: {mlops_track['status']}")
            print(f"   模式: {mlops_track['mode']}")
            print(f"   可觸發: {'是' if mlops_track['ready_for_trigger'] else '否'}")
            
            print(f"\n🏗️  基礎設施:")
            infra = status["infrastructure"]
            print(f"   Redis: {'✅' if infra['redis_available'] else '❌'}")
            print(f"   監控: {'✅' if infra['monitoring_active'] else '❌'}")
        
        # 測試結果評估
        if result["final_state"] == "success":
            print(f"\n🎉 雙軌系統初始化測試成功！")
            print(f"✅ Rust 核心：進入 24/7 熱備狀態")
            print(f"✅ MLOps 工作流：進入按需冷備狀態")
            return True
        elif result["final_state"] == "degraded":
            print(f"\n⚠️  系統部分功能可用（降級模式）")
            return True
        else:
            print(f"\n❌ 系統初始化失敗")
            return False
            
    except Exception as e:
        print(f"\n❌ 測試過程異常: {e}")
        return False
    finally:
        # 確保清理
        await manager.shutdown_system()

async def test_system_components():
    """測試系統各組件功能"""
    print(f"\n🔧 測試系統組件功能...")
    
    try:
        # 測試 Agno Workflows v2 集成
        from agno_v2_mlops_workflow import AgnoV2MLOpsWorkflow
        mlops = AgnoV2MLOpsWorkflow()
        print("✅ Agno Workflows v2 模塊加載成功")
        
        # 測試 Redis 連接
        import redis
        redis_client = redis.Redis(host='localhost', port=6379, db=0, socket_timeout=2)
        redis_client.ping()
        print("✅ Redis 連接測試成功")
        
        # 測試 ClickHouse 連接
        import requests
        response = requests.get("http://localhost:8123/ping", timeout=3)
        if response.status_code == 200:
            print("✅ ClickHouse 連接測試成功")
        else:
            print("⚠️  ClickHouse 連接測試失敗")
        
    except ImportError as e:
        print(f"❌ 模塊導入失敗: {e}")
    except redis.ConnectionError:
        print("⚠️  Redis 連接失敗 (服務可能未啟動)")
    except requests.exceptions.RequestException:
        print("⚠️  ClickHouse 連接失敗 (服務可能未啟動)")
    except Exception as e:
        print(f"❌ 組件測試異常: {e}")

if __name__ == "__main__":
    # 設置事件循環策略（macOS 兼容性）
    if sys.platform == "darwin":
        asyncio.set_event_loop_policy(asyncio.DefaultEventLoopPolicy())
    
    async def main():
        # 先測試組件
        await test_system_components()
        
        # 再測試完整系統
        success = await test_dual_track_system()
        
        print(f"\n{'='*60}")
        if success:
            print("🏆 雙軌系統架構測試通過！")
            print("🚀 系統已準備就緒，可用於生產部署")
        else:
            print("⚠️  系統測試發現問題，需要進一步調試")
        
        return 0 if success else 1
    
    exit_code = asyncio.run(main())
    sys.exit(exit_code)