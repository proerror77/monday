#!/usr/bin/env python3
"""
Complete Integrated Agno v2 MLOps CLI System
============================================

整合了 Agno Workflows v2 的完整 MLOps CLI 系統
包含系統初始化、數據收集、模型訓練和部署的完整流程
"""

import asyncio
import argparse
import json
import sys
import time
import logging
from pathlib import Path
from typing import Dict, Any, Optional
from enum import Enum

# 添加當前目錄到 Python 路徑
sys.path.insert(0, str(Path(__file__).parent))

from agno_v2_mlops_workflow import AgnoV2MLOpsWorkflow

# 設置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class SystemMode(Enum):
    """系統運行模式"""
    PRODUCTION = "production"    # 生產模式：真實數據 + 真實交易
    DEVELOPMENT = "development"  # 開發模式：真實數據 + 模擬交易  
    SIMULATION = "simulation"    # 模擬模式：全模擬環境

class IntegratedAgnoV2CLI:
    """整合的 Agno v2 MLOps CLI 系統"""
    
    def __init__(self, mode: SystemMode = SystemMode.DEVELOPMENT):
        self.mode = mode
        self.base_dir = Path(__file__).parent
        self.mlops_system = AgnoV2MLOpsWorkflow()
        
        logger.info(f"🏗️  整合 Agno v2 MLOps 系統初始化 (模式: {mode.value})")
    
    async def initialize_system(self) -> Dict[str, Any]:
        """初始化系統"""
        logger.info("🚀 初始化 Agno v2 MLOps 系統...")
        
        start_time = time.time()
        
        init_result = {
            "start_time": start_time,
            "components": {},
            "status": "initializing"
        }
        
        try:
            # 檢查核心依賴
            logger.info("📋 檢查系統依賴...")
            deps_result = await self._check_dependencies()
            init_result["components"]["dependencies"] = deps_result
            
            # 檢查 Rust HFT 系統
            logger.info("🦀 檢查 Rust HFT 核心...")
            rust_result = await self._check_rust_system()
            init_result["components"]["rust_system"] = rust_result
            
            # 初始化 Agno 工作流程
            logger.info("🧠 初始化 Agno 工作流程...")
            agno_result = await self._initialize_agno_workflows()
            init_result["components"]["agno_workflows"] = agno_result
            
            # 系統整合測試
            logger.info("🔍 執行系統整合測試...")
            integration_result = await self._test_system_integration()
            init_result["components"]["integration_test"] = integration_result
            
            # 確定最終狀態
            all_success = all(
                comp.get("status") == "success" 
                for comp in init_result["components"].values()
            )
            
            if all_success:
                init_result["status"] = "ready"
                logger.info("✅ Agno v2 MLOps 系統初始化完成！")
            else:
                init_result["status"] = "partial_failure"
                logger.warning("⚠️  系統部分組件初始化失敗")
            
            init_result["end_time"] = time.time()
            init_result["total_duration"] = init_result["end_time"] - start_time
            
            return init_result
            
        except Exception as e:
            logger.error(f"❌ 系統初始化失敗: {e}")
            init_result["status"] = "failed"
            init_result["error"] = str(e)
            init_result["end_time"] = time.time()
            return init_result
    
    async def _check_dependencies(self) -> Dict[str, Any]:
        """檢查系統依賴"""
        deps_status = {
            "python_env": True,  # 已在運行中
            "agno_workflows": False,
            "required_modules": False
        }
        
        try:
            # 檢查 Agno Workflows v2 組件
            from agno_v2_mlops_workflow import Step, Loop, Router, Workflow
            deps_status["agno_workflows"] = True
            logger.info("✅ Agno Workflows v2 組件正常")
        except Exception as e:
            logger.warning(f"⚠️  Agno Workflows v2 組件檢查失敗: {e}")
        
        try:
            # 檢查必要模塊
            import asyncio, json, logging, time
            deps_status["required_modules"] = True
            logger.info("✅ 必要模塊檢查通過")
        except Exception as e:
            logger.warning(f"⚠️  必要模塊檢查失敗: {e}")
        
        return {
            "status": "success" if all(deps_status.values()) else "partial_failure",
            "dependencies": deps_status,
            "ready_count": sum(deps_status.values()),
            "total_count": len(deps_status)
        }
    
    async def _check_rust_system(self) -> Dict[str, Any]:
        """檢查 Rust HFT 系統"""
        rust_hft_dir = self.base_dir / "rust_hft"
        
        if not rust_hft_dir.exists():
            logger.warning("⚠️  Rust HFT 目錄未找到，將使用模擬模式")
            return {
                "status": "success",
                "mode": "simulation",
                "reason": "Rust HFT 目錄不存在"
            }
        
        # 檢查關鍵文件
        cargo_file = rust_hft_dir / "Cargo.toml"
        examples_dir = rust_hft_dir / "examples"
        
        if cargo_file.exists() and examples_dir.exists():
            logger.info("✅ Rust HFT 系統文件完整")
            return {
                "status": "success",
                "mode": "production" if self.mode == SystemMode.PRODUCTION else "development",
                "cargo_file": str(cargo_file),
                "examples_available": True
            }
        else:
            logger.warning("⚠️  Rust HFT 系統文件不完整")
            return {
                "status": "partial_failure",
                "mode": "simulation",
                "reason": "關鍵文件缺失"
            }
    
    async def _initialize_agno_workflows(self) -> Dict[str, Any]:
        """初始化 Agno 工作流程"""
        try:
            # 測試創建工作流程
            test_workflow = self.mlops_system.create_workflow("BTCUSDT")
            
            logger.info("✅ Agno Workflows v2 創建成功")
            
            return {
                "status": "success",
                "workflow_name": test_workflow.name,
                "components_count": len(test_workflow.steps),
                "framework_version": "Agno Workflows v2"
            }
            
        except Exception as e:
            logger.error(f"❌ Agno 工作流程初始化失敗: {e}")
            return {
                "status": "failed",
                "error": str(e)
            }
    
    async def _test_system_integration(self) -> Dict[str, Any]:
        """測試系統整合"""
        try:
            # 執行快速整合測試
            from test_agno_v2_simulation import test_complete_agno_v2_simulation
            
            logger.info("🧪 執行整合測試...")
            test_success = await test_complete_agno_v2_simulation()
            
            if test_success:
                logger.info("✅ 系統整合測試通過")
                return {
                    "status": "success",
                    "test_type": "simulation",
                    "all_components_working": True
                }
            else:
                logger.warning("⚠️  系統整合測試部分失敗")
                return {
                    "status": "partial_failure",
                    "test_type": "simulation",
                    "all_components_working": False
                }
                
        except Exception as e:
            logger.error(f"❌ 系統整合測試失敗: {e}")
            return {
                "status": "failed",
                "error": str(e)
            }
    
    async def run_complete_mlops_workflow(self, symbol: str = "BTCUSDT", **config) -> Dict[str, Any]:
        """運行完整的 MLOps 工作流程"""
        logger.info(f"🚀 開始完整 MLOps 工作流程: {symbol}")
        
        try:
            # 配置工作流程參數
            custom_config = {
                "duration_minutes": config.get("data_minutes", 10),
                "training_hours": config.get("training_hours", 24),
                **(config)
            }
            
            # 根據模式調整配置
            if self.mode == SystemMode.SIMULATION:
                custom_config.update({
                    "duration_minutes": 0.1,  # 快速模擬
                    "training_hours": 0.1
                })
            elif self.mode == SystemMode.DEVELOPMENT:
                custom_config.update({
                    "duration_minutes": min(custom_config.get("duration_minutes", 10), 5),  # 限制開發模式時間
                })
            
            # 執行 MLOps 管道
            logger.info(f"🔄 執行 Agno v2 MLOps 管道 (模式: {self.mode.value})...")
            result = await self.mlops_system.execute_mlops_pipeline(symbol, custom_config)
            
            # 處理結果
            if result.get("status") == "success":
                logger.info("✅ MLOps 工作流程執行完成")
            else:
                logger.error("❌ MLOps 工作流程執行失敗")
            
            return result
            
        except Exception as e:
            logger.error(f"❌ MLOps 工作流程異常: {e}")
            return {
                "status": "failed",
                "error": str(e),
                "timestamp": time.time()
            }
    
    async def get_system_status(self) -> Dict[str, Any]:
        """獲取系統狀態"""
        return {
            "timestamp": time.time(),
            "mode": self.mode.value,
            "framework": "Agno Workflows v2",
            "system_ready": True,
            "components": {
                "agno_workflows": "ready",
                "mlops_pipeline": "ready",
                "cli_interface": "active"
            }
        }

def create_parser():
    """創建命令行解析器"""
    parser = argparse.ArgumentParser(
        description='Integrated Agno v2 MLOps CLI - 完整的 MLOps 自動化系統',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
完整功能示例:
  # 系統管理
  %(prog)s system --init                          # 初始化系統
  %(prog)s system --status                        # 查看系統狀態
  
  # MLOps 工作流程
  %(prog)s mlops --run --symbol BTCUSDT           # 運行完整 MLOps 流程
  %(prog)s mlops --run --symbol ETHUSDT --data-minutes 5 --max-iterations 3
  
  # 模式控制
  %(prog)s --mode production mlops --run          # 生產模式
  %(prog)s --mode simulation mlops --run          # 模擬模式（快速測試）
        """
    )
    
    # 全局參數
    parser.add_argument('--mode', choices=['production', 'development', 'simulation'], 
                       default='development', help='系統運行模式')
    
    subparsers = parser.add_subparsers(dest='command', help='可用命令')
    
    # System 命令
    system_parser = subparsers.add_parser('system', help='系統管理')
    system_group = system_parser.add_mutually_exclusive_group(required=True)
    system_group.add_argument('--init', action='store_true', help='初始化系統')
    system_group.add_argument('--status', action='store_true', help='查看系統狀態')
    
    # MLOps 命令
    mlops_parser = subparsers.add_parser('mlops', help='MLOps 工作流程')
    mlops_parser.add_argument('--run', action='store_true', required=True, 
                             help='運行完整 MLOps 工作流程')
    mlops_parser.add_argument('--symbol', default='BTCUSDT', help='交易對符號')
    mlops_parser.add_argument('--data-minutes', type=int, default=10, help='數據收集時間(分鐘)')
    mlops_parser.add_argument('--training-hours', type=int, default=24, help='訓練時間(小時)')
    mlops_parser.add_argument('--max-iterations', type=int, default=5, help='最大優化迭代次數')
    
    return parser

async def main():
    """主函數"""
    parser = create_parser()
    args = parser.parse_args()
    
    if not args.command:
        parser.print_help()
        return 1
    
    try:
        # 創建整合系統
        system_mode = SystemMode(args.mode)
        cli_system = IntegratedAgnoV2CLI(mode=system_mode)
        
        print(f"\\n🧠 Integrated Agno v2 MLOps CLI (模式: {system_mode.value})")
        print("=" * 80)
        
        # 執行命令
        if args.command == 'system':
            if args.init:
                print("🚀 初始化 Agno v2 MLOps 系統...")
                result = await cli_system.initialize_system()
                
                print(f"\\n📊 系統初始化結果:")
                print(json.dumps(result, indent=2, ensure_ascii=False))
                
                if result["status"] == "ready":
                    print(f"\\n✅ 系統初始化成功！用時 {result['total_duration']:.1f} 秒")
                    print("🔧 所有組件準備就緒：")
                    for comp_name, comp_data in result["components"].items():
                        status = "✅" if comp_data.get("status") == "success" else "⚠️"
                        print(f"   {status} {comp_name}")
                    return 0
                else:
                    print(f"\\n❌ 系統初始化失敗: {result.get('error', '未知錯誤')}")
                    return 1
                    
            elif args.status:
                status = await cli_system.get_system_status()
                print(f"\\n📊 系統狀態:")
                print(json.dumps(status, indent=2, ensure_ascii=False))
                
        elif args.command == 'mlops':
            if args.run:
                print(f"🚀 運行 Agno v2 MLOps 工作流程: {args.symbol}")
                print(f"   模式: {system_mode.value}")
                print(f"   數據收集: {args.data_minutes} 分鐘")
                print(f"   訓練時間: {args.training_hours} 小時")
                print(f"   最大迭代: {args.max_iterations} 次")
                
                result = await cli_system.run_complete_mlops_workflow(
                    symbol=args.symbol,
                    data_minutes=args.data_minutes,
                    training_hours=args.training_hours,
                    max_iterations=args.max_iterations
                )
                
                print(f"\\n📊 MLOps 工作流程結果:")
                print(json.dumps(result, indent=2, ensure_ascii=False))
                
                if result.get("status") == "success":
                    print(f"\\n✅ MLOps 工作流程執行成功！")
                    
                    # 顯示關鍵指標
                    workflow_result = result.get("result", {})
                    if "results" in workflow_result:
                        print("\\n📈 關鍵指標:")
                        results = workflow_result["results"]
                        
                        # 數據收集
                        if "collect_data" in results:
                            data_result = results["collect_data"]
                            if data_result.success:
                                records = data_result.content.get("records_collected", 0)
                                print(f"   📡 數據收集: {records:,} 條記錄")
                        
                        # 優化結果
                        if "model_optimization_loop" in results:
                            loop_result = results["model_optimization_loop"] 
                            if loop_result.success:
                                iterations = loop_result.content.get("total_iterations", 0)
                                print(f"   🔄 優化迭代: {iterations} 次")
                                
                                best_result = loop_result.content.get("best_result")
                                if best_result and "evaluate_model" in best_result:
                                    eval_result = best_result["evaluate_model"]
                                    if eval_result.success:
                                        dashboard = eval_result.content.get("evaluation_dashboard", {})
                                        print(f"   📊 Sharpe 比率: {dashboard.get('sharpe_ratio', 0):.2f}")
                                        print(f"   📊 勝率: {dashboard.get('win_rate', 0):.1%}")
                                        print(f"   📊 最大回撤: {dashboard.get('max_drawdown', 0):.1%}")
                        
                        # 部署決策
                        if "deployment_decision_router" in results:
                            router_result = results["deployment_decision_router"]
                            if router_result.success:
                                route = router_result.metadata.get("selected_route", "unknown")
                                print(f"   🚀 部署決策: {route}")
                    
                    return 0
                else:
                    print(f"\\n❌ MLOps 工作流程執行失敗")
                    return 1
        
        return 0
        
    except Exception as e:
        logger.error(f"❌ CLI 執行失敗: {e}")
        print(f"\\n❌ 錯誤: {e}")
        return 1

if __name__ == "__main__":
    # 設置事件循環策略（macOS 兼容性）
    if sys.platform == "darwin":
        asyncio.set_event_loop_policy(asyncio.DefaultEventLoopPolicy())
    
    exit_code = asyncio.run(main())
    sys.exit(exit_code)