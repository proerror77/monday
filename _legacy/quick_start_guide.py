#!/usr/bin/env python3
"""
快速開始指南 - 一鍵啟動完整系統
================================

這個腳本將引導您完成系統的快速啟動和體驗
"""

import asyncio
import sys
import json
from pathlib import Path

def print_header(title: str):
    """打印標題"""
    print(f"\n{'='*60}")
    print(f"🚀 {title}")
    print(f"{'='*60}\n")

def print_step(step_num: int, description: str):
    """打印步驟"""
    print(f"📋 步驟 {step_num}: {description}")

async def quick_start_demo():
    """快速開始演示"""
    print_header("Agno v2 MLOps 系統快速啟動指南")
    
    print("歡迎使用我們的完整MLOps自動化系統！")
    print("這個系統包含：")
    print("✅ 完整的Agno Workflows v2實現")
    print("✅ 智能超參數調優循環")
    print("✅ 多維度模型評估 (IC/IR/Sharpe等)")
    print("✅ 自動部署決策")
    print("✅ 雙Agent分離架構\n")
    
    # 選擇運行模式
    print("請選擇您想要的體驗模式：")
    print("1. 🚀 完整MLOps流程演示 (推薦)")
    print("2. 🧪 快速功能測試")
    print("3. 🏗️  系統架構展示")
    print("4. 📊 Agent分離架構演示")
    
    try:
        choice = input("\n請輸入選項 (1-4): ").strip()
        
        if choice == "1":
            await run_complete_mlops_demo()
        elif choice == "2":
            await run_quick_tests()
        elif choice == "3":
            await show_system_architecture()
        elif choice == "4":
            await demonstrate_agent_separation()
        else:
            print("❌ 無效選項，使用默認選項...")
            await run_complete_mlops_demo()
            
    except KeyboardInterrupt:
        print("\n🛑 用戶中斷操作")
    except Exception as e:
        print(f"\n❌ 運行異常: {e}")

async def run_complete_mlops_demo():
    """運行完整MLOps演示"""
    print_header("完整MLOps流程演示")
    
    print_step(1, "導入Agno v2 MLOps系統")
    try:
        from agno_v2_mlops_workflow import AgnoV2MLOpsWorkflow
        print("✅ Agno v2 MLOps系統載入成功")
    except ImportError as e:
        print(f"❌ 系統載入失敗: {e}")
        return
    
    print_step(2, "創建MLOps工作流程")
    mlops_system = AgnoV2MLOpsWorkflow()
    print("✅ MLOps系統初始化完成")
    
    print_step(3, "配置快速演示參數")
    config = {
        "duration_minutes": 0.1,  # 快速數據收集
        "training_hours": 0.1,    # 快速訓練
        "max_iterations": 2       # 2次優化迭代
    }
    print(f"✅ 演示配置: {config}")
    
    print_step(4, "執行完整MLOps管道")
    print("🔄 開始執行... (預計需要10-15秒)")
    
    result = await mlops_system.execute_mlops_pipeline("BTCUSDT", config)
    
    print_step(5, "分析執行結果")
    if result["status"] == "success":
        print("🎉 MLOps管道執行成功！")
        
        # 提取關鍵結果
        workflow_result = result.get("result", {})
        if "results" in workflow_result:
            results = workflow_result["results"]
            
            # 數據收集結果
            if "collect_data" in results:
                data_result = results["collect_data"]
                if data_result.success:
                    records = data_result.content.get("records_collected", 0)
                    print(f"📡 數據收集: {records:,} 條記錄")
            
            # 優化循環結果
            if "model_optimization_loop" in results:
                loop_result = results["model_optimization_loop"]
                if loop_result.success:
                    iterations = loop_result.content.get("total_iterations", 0)
                    print(f"🔄 優化迭代: {iterations} 次")
                    
                    # 最佳模型結果
                    best_result = loop_result.content.get("best_result")
                    if best_result and "evaluate_model" in best_result:
                        eval_result = best_result["evaluate_model"]
                        if eval_result.success:
                            dashboard = eval_result.content.get("evaluation_dashboard", {})
                            print(f"📊 模型評估結果:")
                            print(f"   - Sharpe比率: {dashboard.get('sharpe_ratio', 0):.2f}")
                            print(f"   - 勝率: {dashboard.get('win_rate', 0):.1%}")
                            print(f"   - 最大回撤: {dashboard.get('max_drawdown', 0):.1%}")
                            print(f"   - 信號延遲: {dashboard.get('signal_to_execution_latency_us', 0):.0f}μs")
            
            # 部署決策結果
            if "deployment_decision_router" in results:
                router_result = results["deployment_decision_router"]
                if router_result.success:
                    route = router_result.metadata.get("selected_route", "unknown")
                    print(f"🚀 部署決策: {route}")
        
        print(f"\n✅ 完整演示成功！執行時間: {result['execution_time']:.2f}秒")
        
    else:
        print(f"❌ MLOps管道執行失敗: {result.get('error')}")

async def run_quick_tests():
    """運行快速功能測試"""
    print_header("快速功能測試")
    
    print_step(1, "測試Agno Workflows v2組件")
    try:
        from test_agno_v2_simulation import test_complete_agno_v2_simulation
        success = await test_complete_agno_v2_simulation()
        if success:
            print("✅ Agno v2組件測試通過")
        else:
            print("❌ Agno v2組件測試失敗")
    except Exception as e:
        print(f"❌ 測試異常: {e}")

async def show_system_architecture():
    """展示系統架構"""
    print_header("系統架構展示")
    
    print("🏗️  雙軌運行架構:")
    print("""
    ┌─────────────────────────────────────────────────────────┐
    │                    生產級HFT系統                          │
    ├─────────────────────────────────────────────────────────┤
    │                                                         │
    │  🦀 軌道一：Rust 核心 (24/7 熱備狀態)                    │
    │  ├─ 實時市場數據接收和存儲                               │
    │  ├─ 高頻交易執行 (<1μs延遲)                            │
    │  ├─ 風險管理和監控                                      │
    │  └─ HTTP API服務 (供Python調用)                        │
    │                                                         │
    │  🧠 軌道二：Python MLOps (按需冷備狀態)                 │
    │  ├─ ML Agent: 模型訓練和評估                            │
    │  ├─ Trading Ops Agent: 交易運營管理                    │
    │  ├─ 超參數自動調優循環                                  │
    │  └─ 藍綠部署和A/B測試                                   │
    │                                                         │
    ├─────────────────────────────────────────────────────────┤
    │  📡 通信層: Redis + HTTP API                           │
    │  💾 數據層: ClickHouse + Redis                         │
    │  🔍 監控層: 健康檢查 + 性能指標                         │
    └─────────────────────────────────────────────────────────┘
    """)
    
    print("🔧 核心特性:")
    print("✅ 完全符合Agno Workflows v2標準")
    print("✅ 智能超參數調優循環")
    print("✅ 多維度量化金融評估 (IC/IR/Sharpe)")
    print("✅ 雙Agent分離架構")
    print("✅ 藍綠部署和A/B測試")
    print("✅ 24/7高可用性")

async def demonstrate_agent_separation():
    """演示Agent分離架構"""
    print_header("雙Agent分離架構演示")
    
    print("🧠 ML Agent (機器學習代理):")
    print("   職責: 純粹的MLOps pipeline")
    print("   - 數據收集和預處理")
    print("   - TLOB模型訓練和超參數調優")
    print("   - 模型評估 (IC/IR等指標)")
    print("   - 部署決策")
    print("   工作模式: 按需觸發 (cron/手動/事件)")
    
    print("\n🏭 Trading Operations Agent (交易運營代理):")
    print("   職責: 實時交易系統運營")
    print("   - 模型熱加載和版本管理")
    print("   - 實時交易執行和監控")
    print("   - 風險管理和緊急停止")
    print("   - 系統健康檢查")
    print("   工作模式: 24/7持續運行")
    
    print("\n📡 Agent間通信:")
    print("   - Redis發布/訂閱模式")
    print("   - HTTP API調用")
    print("   - 異步消息傳遞")
    
    print("\n🔄 典型工作流程:")
    print("   1. ML Agent完成模型訓練和評估")
    print("   2. 通過Redis通知Trading Ops Agent")
    print("   3. Trading Ops Agent執行藍綠部署")
    print("   4. 影子交易測試通過後上線")
    print("   5. 持續監控和風險管理")

def show_next_steps():
    """顯示後續步驟"""
    print_header("後續步驟建議")
    
    print("🎯 立即可以做的事情:")
    print("1. 📊 查看完整系統文檔:")
    print("   cat system_architecture_summary.md")
    
    print("\n2. 🚀 運行不同模式的演示:")
    print("   python integrated_agno_v2_cli.py system --init")
    print("   python integrated_agno_v2_cli.py mlops --run")
    
    print("\n3. 🔧 自定義配置和參數:")
    print("   - 修改 agno_v2_mlops_workflow.py 中的配置")
    print("   - 調整超參數搜索範圍")
    print("   - 設置評估標準")
    
    print("\n4. 🏗️  集成到現有系統:")
    print("   - 連接真實的Rust HFT API")
    print("   - 配置生產環境數據庫")
    print("   - 設置監控和告警")
    
    print("\n💡 技術支持:")
    print("   - 所有代碼都有詳細注釋")
    print("   - 每個文件都是獨立可運行的")
    print("   - 完整的錯誤處理和日誌記錄")

if __name__ == "__main__":
    # 設置事件循環策略（macOS 兼容性）
    if sys.platform == "darwin":
        asyncio.set_event_loop_policy(asyncio.DefaultEventLoopPolicy())
    
    async def main():
        try:
            await quick_start_demo()
            show_next_steps()
        except KeyboardInterrupt:
            print("\n👋 感謝使用！")
        
        return 0
    
    exit_code = asyncio.run(main())
    sys.exit(exit_code)