#!/usr/bin/env python3
"""
快速測試 Agno Workflows v2 MLOps 系統
====================================

驗證完整系統功能（使用短時間參數）
"""

import asyncio
import sys
from pathlib import Path

# 添加當前目錄到 Python 路徑
sys.path.insert(0, str(Path(__file__).parent))

from agno_v2_mlops_workflow import AgnoV2MLOpsWorkflow

async def quick_test():
    """快速測試完整工作流程"""
    print("🧪 Agno Workflows v2 MLOps 快速測試")
    print("=" * 60)
    
    # 創建 MLOps 系統（使用快速測試配置）
    mlops_system = AgnoV2MLOpsWorkflow()
    
    # 覆蓋配置為快速測試模式
    mlops_system.config = {
        "data_collection": {
            "default_duration_minutes": 0.1,  # 6秒測試
            "quality_threshold": 0.95
        },
        "optimization": {
            "max_iterations": 2,  # 只測試2次迭代
            "target_sharpe": 1.5,
            "max_drawdown_threshold": 0.05
        },
        "evaluation": {
            "profit_factor_min": 1.5,
            "sharpe_ratio_min": 1.5,
            "win_rate_min": 0.52,
            "latency_max_us": 100
        },
        "deployment": {
            "strategy": "blue_green",
            "enable_ab_testing": False
        }
    }
    
    # 執行快速測試
    print("🚀 開始快速測試執行...")
    result = await mlops_system.execute_mlops_pipeline(
        symbol="BTCUSDT",
        custom_config={
            "duration_minutes": 0.1,  # 6秒數據收集
            "training_hours": 0.1     # 快速訓練
        }
    )
    
    print("\n📊 測試結果:")
    print(f"狀態: {result['status']}")
    print(f"執行時間: {result['execution_time']:.2f} 秒")
    
    if result.get('result'):
        workflow_result = result['result']
        if workflow_result.get('results'):
            # 顯示各組件執行結果
            results = workflow_result['results']
            
            print("\n📋 組件執行摘要:")
            for component_name, component_result in results.items():
                success = "✅" if component_result.success else "❌"
                print(f"  {success} {component_name}")
                
                if component_name == "collect_data" and component_result.content:
                    records = component_result.content.get('records_collected', 0)
                    print(f"      - 收集記錄: {records:,} 條")
                
                elif component_name == "model_optimization_loop" and component_result.content:
                    iterations = component_result.content.get('total_iterations', 0)
                    print(f"      - 優化迭代: {iterations} 次")
                    
                    best_result = component_result.content.get('best_result')
                    if best_result and 'evaluate_model' in best_result:
                        eval_result = best_result['evaluate_model']
                        if eval_result.success and eval_result.content:
                            dashboard = eval_result.content.get('evaluation_dashboard', {})
                            print(f"      - Sharpe比率: {dashboard.get('sharpe_ratio', 0):.2f}")
                            print(f"      - 勝率: {dashboard.get('win_rate', 0):.1%}")
                
                elif component_name == "deployment_decision_router" and component_result.content:
                    route = component_result.metadata.get('selected_route', 'unknown')
                    print(f"      - 部署決策: {route}")
    
    print(f"\n{'='*60}")
    if result['status'] == 'success':
        print("✅ Agno Workflows v2 MLOps 系統測試通過！")
        print("🔧 所有核心組件功能正常：")
        print("   - Step 執行引擎 ✅")
        print("   - Loop 優化循環 ✅") 
        print("   - Router 智能路由 ✅")
        print("   - Workflow 工作流程 ✅")
        print("   - 多維度模型評估 ✅")
        return True
    else:
        print("❌ 測試失敗，請檢查錯誤日誌")
        return False

if __name__ == "__main__":
    # 設置事件循環策略（macOS 兼容性）
    import sys
    if sys.platform == "darwin":
        asyncio.set_event_loop_policy(asyncio.DefaultEventLoopPolicy())
    
    success = asyncio.run(quick_test())
    sys.exit(0 if success else 1)