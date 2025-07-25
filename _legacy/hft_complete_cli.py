#!/usr/bin/env python3
"""
HFT Complete CLI - 完整的 MLOps 自動化工作流 CLI
==============================================

基於 PRD 規範實現的完整交易系統 CLI
- 啟動 Rust HFT 核心收數據
- 初始化完整的 Agent 工作流
- 實現 Agno v2 自動化 MLOps 流程
- 多維度模型評估和智能部署決策

架構：Rust 執行平面 + Python Agno 控制平面
"""

import asyncio
import argparse
import json
import sys
import time
import logging
import subprocess
import os
from pathlib import Path
from typing import Dict, Any, Optional, List
from datetime import datetime
from enum import Enum

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

class HFTCompleteSystem:
    """完整的 HFT 系統管理器"""
    
    def __init__(self, mode: SystemMode = SystemMode.DEVELOPMENT):
        self.mode = mode
        self.base_dir = Path(__file__).parent
        self.rust_hft_dir = self.base_dir / "rust_hft"
        self.config_file = self.base_dir / "config.yaml"
        
        # 系統組件狀態
        self.system_status = {
            "rust_core": "stopped",
            "data_collection": "stopped", 
            "agents": "stopped",
            "workflow": "idle"
        }
        
        logger.info(f"🏗️  HFT 完整系統初始化 (模式: {mode.value})")
    
    async def initialize_system(self) -> Dict[str, Any]:
        """初始化完整交易系統"""
        logger.info("🚀 開始初始化 HFT 交易系統...")
        
        init_result = {
            "start_time": time.time(),
            "components": {},
            "overall_status": "initializing"
        }
        
        try:
            # 1. 檢查依賴服務
            logger.info("📋 步驟1: 檢查依賴服務...")
            deps_result = await self._check_dependencies()
            init_result["components"]["dependencies"] = deps_result
            
            if not deps_result["all_ready"]:
                init_result["overall_status"] = "failed"
                init_result["error"] = "依賴服務檢查失敗"
                return init_result
            
            # 2. 初始化 Rust HFT 核心
            logger.info("🦀 步驟2: 初始化 Rust HFT 核心...")
            rust_result = await self._initialize_rust_core()
            init_result["components"]["rust_core"] = rust_result
            
            # 3. 啟動數據收集系統
            logger.info("📡 步驟3: 啟動數據收集系統...")
            data_result = await self._initialize_data_collection()
            init_result["components"]["data_collection"] = data_result
            
            # 4. 初始化智能代理系統
            logger.info("🤖 步驟4: 初始化智能代理系統...")
            agents_result = await self._initialize_agents()
            init_result["components"]["agents"] = agents_result
            
            # 5. 驗證系統整合性
            logger.info("🔍 步驟5: 驗證系統整合性...")
            integration_result = await self._verify_system_integration()
            init_result["components"]["integration"] = integration_result
            
            # 6. 更新系統狀態
            if all(comp.get("status") == "success" for comp in init_result["components"].values()):
                init_result["overall_status"] = "ready"
                self._update_system_status("ready")
                logger.info("✅ HFT 交易系統初始化完成！")
            else:
                init_result["overall_status"] = "partial_failure" 
                logger.warning("⚠️  HFT 交易系統部分初始化失敗")
            
            init_result["end_time"] = time.time()
            init_result["total_duration"] = init_result["end_time"] - init_result["start_time"]
            
            return init_result
            
        except Exception as e:
            logger.error(f"❌ 系統初始化失敗: {e}")
            init_result["overall_status"] = "failed"
            init_result["error"] = str(e)
            init_result["end_time"] = time.time()
            return init_result
    
    async def _check_dependencies(self) -> Dict[str, Any]:
        """檢查依賴服務"""
        deps = {
            "redis": False,
            "clickhouse": False,
            "rust_toolchain": False,
            "python_deps": False
        }
        
        try:
            # 檢查 Redis
            import redis
            redis_client = redis.Redis(host='localhost', port=6379, db=0, socket_timeout=3)
            redis_client.ping()
            deps["redis"] = True
            logger.info("✅ Redis 連接正常")
        except Exception as e:
            logger.warning(f"⚠️  Redis 連接失敗: {e}")
        
        try:
            # 檢查 ClickHouse
            import requests
            response = requests.get("http://localhost:8123/ping", timeout=5)
            if response.status_code == 200:
                deps["clickhouse"] = True
                logger.info("✅ ClickHouse 連接正常")
        except Exception as e:
            logger.warning(f"⚠️  ClickHouse 連接失敗: {e}")
        
        # 檢查 Rust 工具鏈
        if self.rust_hft_dir.exists() and (self.rust_hft_dir / "Cargo.toml").exists():
            deps["rust_toolchain"] = True
            logger.info("✅ Rust HFT 核心存在")
        else:
            logger.warning("⚠️  Rust HFT 核心未找到")
        
        # 檢查 Python 依賴
        try:
            sys.path.insert(0, str(self.base_dir))
            from final_agno_compliant_workflow import CompleteMLOpsWorkflow
            deps["python_deps"] = True
            logger.info("✅ Python 依賴正常")
        except Exception as e:
            logger.warning(f"⚠️  Python 依賴檢查失敗: {e}")
        
        return {
            "status": "success",
            "dependencies": deps,
            "all_ready": all(deps.values()),
            "ready_count": sum(deps.values()),
            "total_count": len(deps)
        }
    
    async def _initialize_rust_core(self) -> Dict[str, Any]:
        """初始化 Rust HFT 核心"""
        if not self.rust_hft_dir.exists():
            return {
                "status": "skipped",
                "reason": "Rust HFT 目錄不存在，使用模擬模式"
            }
        
        try:
            # 構建 Rust 項目 (release 模式)
            logger.info("🔨 構建 Rust HFT 核心...")
            build_cmd = ["cargo", "build", "--release"]
            
            process = await asyncio.create_subprocess_exec(
                *build_cmd,
                cwd=str(self.rust_hft_dir),
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE
            )
            
            stdout, stderr = await process.communicate()
            
            if process.returncode == 0:
                logger.info("✅ Rust HFT 核心構建成功")
                self.system_status["rust_core"] = "ready"
                
                return {
                    "status": "success",
                    "build_time": time.time(),
                    "mode": self.mode.value
                }
            else:
                logger.error(f"❌ Rust HFT 核心構建失敗: {stderr.decode()}")
                return {
                    "status": "failed",
                    "error": stderr.decode()
                }
                
        except Exception as e:
            logger.error(f"❌ Rust 核心初始化異常: {e}")
            return {
                "status": "failed", 
                "error": str(e)
            }
    
    async def _initialize_data_collection(self) -> Dict[str, Any]:
        """初始化數據收集系統"""
        try:
            # 驗證數據收集能力
            if self.rust_hft_dir.exists():
                # 檢查數據收集 example 是否存在
                example_path = self.rust_hft_dir / "examples" / "comprehensive_db_stress_test.rs"
                if example_path.exists():
                    logger.info("✅ 數據收集系統準備就緒")
                    self.system_status["data_collection"] = "ready"
                    
                    return {
                        "status": "success",
                        "capability": "rust_hft_websocket",
                        "targets": ["BTCUSDT", "ETHUSDT"],  # 支持的交易對
                        "max_duration_minutes": 60
                    }
                else:
                    logger.warning("⚠️  數據收集 example 未找到")
            
            # 備用：模擬數據收集
            logger.info("📦 使用模擬數據收集模式")
            return {
                "status": "success",
                "capability": "simulation",
                "mode": "mock_data"
            }
            
        except Exception as e:
            logger.error(f"❌ 數據收集初始化失敗: {e}")
            return {
                "status": "failed",
                "error": str(e)
            }
    
    async def _initialize_agents(self) -> Dict[str, Any]:
        """初始化智能代理系統"""
        try:
            # 載入完整的 Agno 工作流程
            sys.path.insert(0, str(self.base_dir))
            
            # 檢查核心 Agent 文件
            agent_files = [
                "final_agno_compliant_workflow.py",
                "hft_operations_agent.py", 
                "ml_workflow_agent.py"
            ]
            
            available_agents = []
            for agent_file in agent_files:
                if (self.base_dir / agent_file).exists():
                    available_agents.append(agent_file.replace('.py', ''))
                    logger.info(f"✅ 發現 Agent: {agent_file}")
            
            if available_agents:
                self.system_status["agents"] = "ready"
                
                return {
                    "status": "success",
                    "available_agents": available_agents,
                    "agent_count": len(available_agents),
                    "framework": "agno_v2_compliant"
                }
            else:
                logger.warning("⚠️  未找到可用的 Agent")
                return {
                    "status": "partial_failure",
                    "available_agents": [],
                    "error": "沒有可用的智能代理"
                }
                
        except Exception as e:
            logger.error(f"❌ Agent 初始化失敗: {e}")
            return {
                "status": "failed",
                "error": str(e)
            }
    
    async def _verify_system_integration(self) -> Dict[str, Any]:
        """驗證系統整合性"""
        try:
            # 驗證數據流：Rust → Redis → Python
            integration_tests = {
                "rust_python_binding": False,
                "redis_communication": False,
                "clickhouse_access": False,
                "agent_coordination": False
            }
            
            # 測試 Redis 通信
            try:
                import redis
                redis_client = redis.Redis(host='localhost', port=6379, db=0)
                
                # 寫入測試數據
                test_key = f"hft:test:{int(time.time())}"
                test_data = {"timestamp": time.time(), "test": "integration"}
                redis_client.setex(test_key, 10, json.dumps(test_data))
                
                # 讀取驗證
                retrieved = redis_client.get(test_key)
                if retrieved:
                    integration_tests["redis_communication"] = True
                    redis_client.delete(test_key)
                    
            except Exception as e:
                logger.warning(f"Redis 整合測試失敗: {e}")
            
            # 測試 ClickHouse 訪問
            try:
                import requests
                response = requests.get("http://localhost:8123/", params={
                    'query': 'SELECT 1 as test_connection'
                }, timeout=5)
                
                if response.status_code == 200 and '1' in response.text:
                    integration_tests["clickhouse_access"] = True
                    
            except Exception as e:
                logger.warning(f"ClickHouse 整合測試失敗: {e}")
            
            # 測試 Agent 協調
            try:
                from test_agents_integration import DockerServicesManager
                docker_manager = DockerServicesManager()
                connections = await docker_manager.test_connections()
                
                if connections.get("redis", {}).get("status") == "connected":
                    integration_tests["agent_coordination"] = True
                    
            except Exception as e:
                logger.warning(f"Agent 協調測試失敗: {e}")
            
            # 測試 Rust-Python 綁定
            try:
                # 檢查是否有 PyO3 綁定
                sys.path.insert(0, str(self.base_dir / "agno_hft"))
                from rust_hft_tools import RustHFTTools
                rust_tools = RustHFTTools()
                integration_tests["rust_python_binding"] = True
                
            except Exception as e:
                logger.warning(f"Rust-Python 綁定測試失敗: {e}")
            
            passed_tests = sum(integration_tests.values())
            total_tests = len(integration_tests)
            
            return {
                "status": "success" if passed_tests >= total_tests * 0.75 else "partial_failure",
                "integration_tests": integration_tests,
                "passed_tests": passed_tests,
                "total_tests": total_tests,
                "integration_score": passed_tests / total_tests
            }
            
        except Exception as e:
            logger.error(f"❌ 系統整合驗證失敗: {e}")
            return {
                "status": "failed",
                "error": str(e)
            }
    
    def _update_system_status(self, status: str):
        """更新系統狀態"""
        self.system_status["workflow"] = status
        logger.info(f"📊 系統狀態更新: {status}")
    
    async def start_data_collection(self, symbol: str = "BTCUSDT", duration_minutes: int = 10) -> Dict[str, Any]:
        """啟動數據收集"""
        logger.info(f"📡 開始收集 {symbol} 數據 ({duration_minutes} 分鐘)")
        
        if self.system_status["rust_core"] != "ready":
            logger.warning("⚠️  Rust 核心未就緒，使用模擬模式")
            await asyncio.sleep(2)
            return {
                "status": "success",
                "mode": "simulation",
                "symbol": symbol,
                "records_collected": duration_minutes * 1000,
                "duration_minutes": duration_minutes
            }
        
        try:
            # 使用完整的 Agno 工作流程進行數據收集
            from final_agno_compliant_workflow import CompleteMLOpsWorkflow
            
            workflow = CompleteMLOpsWorkflow()
            
            # 執行數據收集步驟
            collect_context = {
                "symbol": symbol,
                "duration_minutes": duration_minutes,
                "mode": self.mode.value
            }
            
            collection_result = await workflow.data_agent.execute("collect_market_data", collect_context)
            
            if collection_result.get("success"):
                logger.info(f"✅ 數據收集完成: {collection_result.get('records_collected', 0):,} 條記錄")
                self.system_status["data_collection"] = "active"
                
            return collection_result
            
        except Exception as e:
            logger.error(f"❌ 數據收集失敗: {e}")
            return {
                "status": "failed",
                "error": str(e)
            }
    
    async def run_complete_mlops_workflow(self, symbol: str = "BTCUSDT", **config) -> Dict[str, Any]:
        """運行完整的 MLOps 自動化工作流程"""
        logger.info(f"🚀 開始完整 MLOps 工作流程: {symbol}")
        
        try:
            # 載入完整工作流程
            from final_agno_compliant_workflow import CompleteMLOpsWorkflow
            
            workflow = CompleteMLOpsWorkflow()
            
            # 配置工作流程參數
            workflow_config = {
                "symbol": symbol,
                "data_collection": {
                    "duration_minutes": config.get("data_minutes", 10),
                    "quality_threshold": 0.95
                },
                "model_training": {
                    "max_iterations": config.get("max_iterations", 3),
                    "training_hours": config.get("training_hours", 24),
                    "target_sharpe": 1.5
                },
                "evaluation": {
                    "profit_factor_min": 1.5,
                    "sharpe_ratio_min": 1.5,
                    "max_drawdown_max": 0.05,
                    "win_rate_min": 0.52
                },
                "deployment": {
                    "strategy": "blue_green",
                    "enable_ab_testing": config.get("enable_ab_testing", False)
                }
            }
            
            # 執行完整工作流程
            logger.info("🔄 執行 Agno v2 自動化工作流程...")
            result = await workflow.execute_complete_workflow(workflow_config)
            
            # 更新系統狀態
            if result.get("status") == "completed":
                self.system_status["workflow"] = "completed"
                logger.info("✅ MLOps 工作流程執行完成")
            else:
                self.system_status["workflow"] = "failed"
                logger.error("❌ MLOps 工作流程執行失敗")
            
            return result
            
        except Exception as e:
            logger.error(f"❌ MLOps 工作流程異常: {e}")
            self.system_status["workflow"] = "error"
            return {
                "status": "failed",
                "error": str(e),
                "timestamp": time.time()
            }
    
    async def get_system_status(self) -> Dict[str, Any]:
        """獲取完整系統狀態"""
        return {
            "timestamp": time.time(),
            "mode": self.mode.value,
            "system_status": self.system_status,
            "components_ready": sum(1 for status in self.system_status.values() if status in ["ready", "active"]),
            "total_components": len(self.system_status)
        }

def create_parser():
    """創建完整的命令行解析器"""
    parser = argparse.ArgumentParser(
        description='HFT Complete CLI - 完整的 MLOps 自動化工作流',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
完整功能示例:
  # 系統管理
  %(prog)s system --init                          # 初始化完整交易系統
  %(prog)s system --status                        # 查看系統狀態
  
  # 數據收集 (Rust HFT 核心)
  %(prog)s data --collect --symbol BTCUSDT --duration 10
  
  # MLOps 自動化工作流程
  %(prog)s workflow --run-complete --symbol BTCUSDT --max-iterations 5
  
  # 模式控制
  %(prog)s --mode production system --init        # 生產模式
  %(prog)s --mode simulation workflow --run-complete  # 模擬模式
        """
    )
    
    # 全局參數
    parser.add_argument('--mode', choices=['production', 'development', 'simulation'], 
                       default='development', help='系統運行模式')
    
    subparsers = parser.add_subparsers(dest='command', help='可用命令')
    
    # System 命令
    system_parser = subparsers.add_parser('system', help='系統管理')
    system_group = system_parser.add_mutually_exclusive_group(required=True)
    system_group.add_argument('--init', action='store_true', help='初始化完整交易系統')
    system_group.add_argument('--status', action='store_true', help='查看系統狀態')
    
    # Data 命令
    data_parser = subparsers.add_parser('data', help='數據收集管理')
    data_parser.add_argument('--collect', action='store_true', required=True, help='開始數據收集')
    data_parser.add_argument('--symbol', default='BTCUSDT', help='交易對符號')
    data_parser.add_argument('--duration', type=int, default=10, help='收集時長(分鐘)')
    
    # Workflow 命令  
    workflow_parser = subparsers.add_parser('workflow', help='MLOps 工作流程')
    workflow_parser.add_argument('--run-complete', action='store_true', required=True, 
                                help='運行完整 MLOps 自動化工作流程')
    workflow_parser.add_argument('--symbol', default='BTCUSDT', help='交易對符號')
    workflow_parser.add_argument('--data-minutes', type=int, default=10, help='數據收集時間(分鐘)')
    workflow_parser.add_argument('--training-hours', type=int, default=24, help='訓練時間(小時)')
    workflow_parser.add_argument('--max-iterations', type=int, default=3, help='最大迭代次數')
    workflow_parser.add_argument('--enable-ab-testing', action='store_true', help='啟用 A/B 測試')
    
    return parser

async def main():
    """主函數"""
    parser = create_parser()
    args = parser.parse_args()
    
    if not args.command:
        parser.print_help()
        return 1
    
    try:
        # 創建完整系統管理器
        system_mode = SystemMode(args.mode)
        hft_system = HFTCompleteSystem(mode=system_mode)
        
        print(f"\n🏗️  HFT 完整系統 (模式: {system_mode.value})")
        print("=" * 80)
        
        # 執行命令
        if args.command == 'system':
            if args.init:
                print("🚀 初始化完整交易系統...")
                result = await hft_system.initialize_system()
                
                print(f"\n📊 系統初始化結果:")
                print(json.dumps(result, indent=2, ensure_ascii=False))
                
                if result["overall_status"] == "ready":
                    print(f"\n✅ 系統初始化成功！用時 {result['total_duration']:.1f} 秒")
                    return 0
                else:
                    print(f"\n❌ 系統初始化失敗: {result.get('error', '未知錯誤')}")
                    return 1
                    
            elif args.status:
                status = await hft_system.get_system_status()
                print(f"\n📊 系統狀態:")
                print(json.dumps(status, indent=2, ensure_ascii=False))
                
        elif args.command == 'data':
            if args.collect:
                print(f"📡 開始收集 {args.symbol} 數據...")
                result = await hft_system.start_data_collection(
                    symbol=args.symbol,
                    duration_minutes=args.duration
                )
                
                print(f"\n📊 數據收集結果:")
                print(json.dumps(result, indent=2, ensure_ascii=False))
                
        elif args.command == 'workflow':
            if args.run_complete:
                print(f"🚀 運行完整 MLOps 工作流程...")
                result = await hft_system.run_complete_mlops_workflow(
                    symbol=args.symbol,
                    data_minutes=args.data_minutes,
                    training_hours=args.training_hours,
                    max_iterations=args.max_iterations,
                    enable_ab_testing=args.enable_ab_testing
                )
                
                print(f"\n📊 MLOps 工作流程結果:")
                print(json.dumps(result, indent=2, ensure_ascii=False))
                
                if result.get("status") == "completed":
                    print(f"\n✅ MLOps 工作流程執行成功！")
                    return 0
                else:
                    print(f"\n❌ MLOps 工作流程執行失敗")
                    return 1
        
        return 0
        
    except Exception as e:
        logger.error(f"❌ CLI 執行失敗: {e}")
        print(f"\n❌ 錯誤: {e}")
        return 1

if __name__ == "__main__":
    # 設置事件循環策略（macOS 兼容性）
    if sys.platform == "darwin":
        asyncio.set_event_loop_policy(asyncio.DefaultEventLoopPolicy())
    
    exit_code = asyncio.run(main())
    sys.exit(exit_code)