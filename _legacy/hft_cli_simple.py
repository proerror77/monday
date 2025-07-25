#!/usr/bin/env python3
"""
HFT CLI Simple - 無 Agno 依賴的簡化版本
=====================================

統一的命令行界面，用於管理代理工作流程和交易系統
使用簡化的代理實現，去除 Agno 框架依賴

使用方式:
  python hft_cli_simple.py agent --list                    # 列出所有代理
  python hft_cli_simple.py agent --start hft-ops           # 启动 HFT Operations Agent
  python hft_cli_simple.py agent --start ml-workflow       # 启动 ML Workflow Agent
  python hft_cli_simple.py workflow --run complete-ml      # 運行完整 ML 工作流程
  python hft_cli_simple.py trading --start BTCUSDT         # 開始交易
  python hft_cli_simple.py status --all                    # 查看系統狀態
"""

import asyncio
import argparse
import json
import sys
import time
import logging
from pathlib import Path
from typing import Dict, Any, Optional
from datetime import datetime

# 設置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class HFTCLIError(Exception):
    """HFT CLI 專用異常"""
    pass

class SimplifiedHFTCLIManager:
    """簡化版 HFT CLI 管理器 (無 Agno 依賴)"""
    
    def __init__(self):
        self.base_dir = Path(__file__).parent
        self.config_file = self.base_dir / "config.yaml"
        
        # 簡化版代理定義
        self.agents = {
            'hft-ops': {
                'name': 'HFT Operations Agent',
                'description': '24/7 系統監控、實盤交易控制、風險管理',
                'module': 'test_agents_integration',
                'class': 'SimplifiedHFTOperationsAgent'
            },
            'ml-workflow': {
                'name': 'ML Workflow Agent',
                'description': '數據收集、模型訓練、評估部署、實驗管理',
                'module': 'test_agents_integration',
                'class': 'SimplifiedMLWorkflowAgent'
            }
        }
    
    async def list_agents(self) -> Dict[str, Any]:
        """列出所有可用代理"""
        agent_list = {}
        
        for agent_id, agent_info in self.agents.items():
            agent_list[agent_id] = {
                'name': agent_info['name'],
                'description': agent_info['description'],
                'status': 'available'
            }
        
        return agent_list
    
    async def start_agent(self, agent_id: str, mode: str = 'real') -> Dict[str, Any]:
        """启动指定代理"""
        if agent_id not in self.agents:
            raise HFTCLIError(f"未知代理: {agent_id}")
        
        agent_info = self.agents[agent_id]
        
        try:
            logger.info(f"🚀 啟動 {agent_info['name']} (模式: {mode})")
            
            # 載入簡化代理
            sys.path.insert(0, str(self.base_dir))
            
            if agent_id == 'hft-ops':
                result = await self._run_hft_operations_agent(mode)
            elif agent_id == 'ml-workflow':
                result = await self._run_ml_workflow_agent(mode)
            else:
                raise HFTCLIError(f"不支持的代理: {agent_id}")
            
            return {
                'status': 'success',
                'agent_id': agent_id,
                'agent_name': agent_info['name'],
                'mode': mode,
                'result': result,
                'timestamp': time.time()
            }
            
        except Exception as e:
            logger.error(f"❌ 啟動代理失敗: {e}")
            return {
                'status': 'failed',
                'agent_id': agent_id,
                'error': str(e),
                'timestamp': time.time()
            }
    
    async def _run_hft_operations_agent(self, mode: str) -> Dict[str, Any]:
        """運行 HFT Operations Agent"""
        try:
            from test_agents_integration import SimplifiedHFTOperationsAgent, DockerServicesManager
            
            # 創建 Docker 服務管理器
            docker_manager = DockerServicesManager()
            
            # 創建並運行 HFT Operations Agent
            agent = SimplifiedHFTOperationsAgent(docker_manager)
            result = await agent.run_operations_cycle()
            
            logger.info(f"📊 HFT Operations 結果: {result.get('status')}")
            return result
            
        except Exception as e:
            logger.error(f"❌ HFT Operations Agent 執行失敗: {e}")
            return {"status": "failed", "error": str(e)}
    
    async def _run_ml_workflow_agent(self, mode: str) -> Dict[str, Any]:
        """運行 ML Workflow Agent"""
        try:
            from test_agents_integration import SimplifiedMLWorkflowAgent, DockerServicesManager
            
            # 創建 Docker 服務管理器
            docker_manager = DockerServicesManager()
            
            # 創建並運行 ML Workflow Agent
            agent = SimplifiedMLWorkflowAgent(docker_manager)
            
            # 執行完整 ML 工作流程
            workflow_config = {
                "data_collection_minutes": 5,
                "training_hours": 12,
                "evaluation_hours": 24
            }
            
            result = await agent.execute_complete_ml_workflow("BTCUSDT", workflow_config)
            
            logger.info(f"📊 ML Workflow 結果: {result.get('status')}")
            return result
            
        except Exception as e:
            logger.error(f"❌ ML Workflow Agent 執行失敗: {e}")
            return {"status": "failed", "error": str(e)}
    
    async def run_workflow(self, workflow_type: str, **kwargs) -> Dict[str, Any]:
        """運行指定工作流程"""
        logger.info(f"🔄 開始運行工作流程: {workflow_type}")
        
        if workflow_type == 'complete-ml':
            return await self._run_complete_ml_workflow(**kwargs)
        elif workflow_type == 'hft-operations':
            return await self._run_hft_operations_workflow(**kwargs)
        elif workflow_type == 'coordination':
            return await self._run_coordination_workflow(**kwargs)
        else:
            raise HFTCLIError(f"未知工作流程類型: {workflow_type}")
    
    async def _run_complete_ml_workflow(self, symbol: str = "BTCUSDT", **kwargs) -> Dict[str, Any]:
        """運行完整 ML 工作流程"""
        try:
            from test_agents_integration import SimplifiedMLWorkflowAgent, DockerServicesManager
            
            # 創建服務管理器和代理
            docker_manager = DockerServicesManager()
            ml_agent = SimplifiedMLWorkflowAgent(docker_manager)
            
            # 配置工作流程參數
            workflow_config = {
                "data_collection_minutes": kwargs.get("data_minutes", 10),
                "training_hours": kwargs.get("training_hours", 24),
                "evaluation_hours": kwargs.get("evaluation_hours", 168),
                "enable_ab_testing": kwargs.get("enable_ab_testing", False)
            }
            
            logger.info(f"🧠 執行 {symbol} 完整 ML 工作流程...")
            result = await ml_agent.execute_complete_ml_workflow(symbol, workflow_config)
            
            return {
                'workflow_type': 'complete-ml',
                'symbol': symbol,
                'result': result,
                'timestamp': time.time()
            }
            
        except Exception as e:
            logger.error(f"❌ ML 工作流程執行失敗: {e}")
            return {
                'workflow_type': 'complete-ml',
                'status': 'failed',
                'error': str(e),
                'timestamp': time.time()
            }
    
    async def _run_hft_operations_workflow(self, **kwargs) -> Dict[str, Any]:
        """運行 HFT 營運工作流程"""
        try:
            from test_agents_integration import SimplifiedHFTOperationsAgent, DockerServicesManager
            
            # 創建服務管理器和代理
            docker_manager = DockerServicesManager()
            hft_agent = SimplifiedHFTOperationsAgent(docker_manager)
            
            logger.info("🏭 執行 HFT 營運週期...")
            result = await hft_agent.run_operations_cycle()
            
            return {
                'workflow_type': 'hft-operations',
                'result': result,
                'timestamp': time.time()
            }
            
        except Exception as e:
            logger.error(f"❌ HFT 營運工作流程執行失敗: {e}")
            return {
                'workflow_type': 'hft-operations',
                'status': 'failed',
                'error': str(e),
                'timestamp': time.time()
            }
    
    async def _run_coordination_workflow(self, symbol: str = "BTCUSDT", **kwargs) -> Dict[str, Any]:
        """運行代理協調工作流程"""
        try:
            from test_agents_integration import (
                SimplifiedHFTOperationsAgent, 
                SimplifiedMLWorkflowAgent,
                DockerServicesManager
            )
            
            # 創建服務管理器
            docker_manager = DockerServicesManager()
            
            # 創建兩個代理
            hft_agent = SimplifiedHFTOperationsAgent(docker_manager)
            ml_agent = SimplifiedMLWorkflowAgent(docker_manager)
            
            logger.info("🤝 執行代理協調工作流程...")
            
            # 1. ML 代理訓練模型
            training_config = {
                "training_hours": kwargs.get("training_hours", 12),
                "model_architecture": "tlob_transformer"
            }
            
            training_result = await ml_agent.train_tlob_model(symbol, training_config)
            
            if training_result["status"] == "success":
                model_path = training_result["result"]["model_path"]
                logger.info(f"🤖 ML 代理完成訓練: {model_path}")
                
                # 2. 模擬 HFT 代理部署
                deployment_result = await hft_agent.rust_tools.start_live_trading(
                    symbol, model_path, 1000.0
                )
                
                logger.info(f"🚀 HFT 代理完成部署: {deployment_result}")
                
                return {
                    'workflow_type': 'coordination',
                    'symbol': symbol,
                    'training_result': training_result,
                    'deployment_result': deployment_result,
                    'status': 'success',
                    'timestamp': time.time()
                }
            else:
                return {
                    'workflow_type': 'coordination',
                    'status': 'failed',
                    'error': 'ML 訓練失敗',
                    'training_result': training_result,
                    'timestamp': time.time()
                }
                
        except Exception as e:
            logger.error(f"❌ 代理協調工作流程執行失敗: {e}")
            return {
                'workflow_type': 'coordination',
                'status': 'failed',
                'error': str(e),
                'timestamp': time.time()
            }
    
    async def start_trading(self, symbol: str, **kwargs) -> Dict[str, Any]:
        """開始交易"""
        try:
            logger.info(f"📈 開始 {symbol} 交易...")
            
            from test_agents_integration import SimplifiedHFTOperationsAgent, DockerServicesManager
            
            # 創建服務管理器和代理
            docker_manager = DockerServicesManager()
            hft_agent = SimplifiedHFTOperationsAgent(docker_manager)
            
            # 模擬交易啟動
            trading_result = await hft_agent.rust_tools.start_live_trading(
                symbol,
                kwargs.get("model_path", f"/models/{symbol}_default.safetensors"),
                kwargs.get("capital", 1000.0)
            )
            
            return {
                'action': 'start_trading',
                'symbol': symbol,
                'result': trading_result,
                'timestamp': time.time()
            }
            
        except Exception as e:
            logger.error(f"❌ 開始交易失敗: {e}")
            return {
                'action': 'start_trading',
                'symbol': symbol,
                'status': 'failed',
                'error': str(e),
                'timestamp': time.time()
            }
    
    async def get_system_status(self) -> Dict[str, Any]:
        """獲取系統狀態"""
        try:
            from test_agents_integration import DockerServicesManager
            
            # 檢查 Docker 服務
            docker_manager = DockerServicesManager()
            connections = await docker_manager.test_connections()
            
            # 系統狀態摘要
            status = {
                'timestamp': time.time(),
                'system_health': 'healthy' if all(
                    service.get("status") == "connected" 
                    for service in connections.values()
                ) else 'degraded',
                'services': connections,
                'agents': {
                    'hft-operations': 'available',
                    'ml-workflow': 'available'
                },
                'uptime': time.time() - 1753342000  # 啟動時間戳
            }
            
            return status
            
        except Exception as e:
            logger.error(f"❌ 獲取系統狀態失敗: {e}")
            return {
                'timestamp': time.time(),
                'system_health': 'error',
                'error': str(e)
            }

def create_parser():
    """創建命令行解析器"""
    parser = argparse.ArgumentParser(
        description='HFT CLI Simple - 簡化版 (無 Agno 依賴)',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
示例:
  # 代理管理
  %(prog)s agent --list
  %(prog)s agent --start hft-ops --mode real
  %(prog)s agent --start ml-workflow --mode demo
  
  # 工作流程
  %(prog)s workflow --run complete-ml --symbol BTCUSDT
  %(prog)s workflow --run coordination --symbol ETHUSDT
  
  # 交易管理
  %(prog)s trading --start BTCUSDT --capital 1000
  
  # 系統狀態
  %(prog)s status --all
        """
    )
    
    subparsers = parser.add_subparsers(dest='command', help='可用命令')
    
    # Agent 命令
    agent_parser = subparsers.add_parser('agent', help='代理管理')
    agent_group = agent_parser.add_mutually_exclusive_group(required=True)
    agent_group.add_argument('--list', action='store_true', help='列出所有代理')
    agent_group.add_argument('--start', choices=['hft-ops', 'ml-workflow'], help='启动指定代理')
    agent_parser.add_argument('--mode', choices=['real', 'demo', 'dry_run'], default='real', help='運行模式')
    
    # Workflow 命令
    workflow_parser = subparsers.add_parser('workflow', help='工作流程管理')
    workflow_parser.add_argument('--run', choices=['complete-ml', 'hft-operations', 'coordination'], 
                               required=True, help='運行指定工作流程')
    workflow_parser.add_argument('--symbol', default='BTCUSDT', help='交易對符號')
    workflow_parser.add_argument('--data-minutes', type=int, default=10, help='數據收集時間(分鐘)')
    workflow_parser.add_argument('--training-hours', type=int, default=24, help='訓練時間(小時)')
    
    # Trading 命令
    trading_parser = subparsers.add_parser('trading', help='交易管理')
    trading_parser.add_argument('--start', required=True, help='開始交易的交易對')
    trading_parser.add_argument('--capital', type=float, default=1000.0, help='交易資金')
    trading_parser.add_argument('--model-path', help='模型路徑')
    
    # Status 命令
    status_parser = subparsers.add_parser('status', help='系統狀態')
    status_parser.add_argument('--all', action='store_true', help='顯示完整系統狀態')
    
    return parser

async def main():
    """主函數"""
    parser = create_parser()
    args = parser.parse_args()
    
    if not args.command:
        parser.print_help()
        return 1
    
    try:
        # 創建簡化版 CLI 管理器
        cli_manager = SimplifiedHFTCLIManager()
        
        # 執行命令
        if args.command == 'agent':
            if args.list:
                agents = await cli_manager.list_agents()
                print("\n🤖 可用代理列表:")
                print("=" * 60)
                for agent_id, info in agents.items():
                    print(f"  🔹 {agent_id}: {info['name']}")
                    print(f"     {info['description']}")
                    print(f"     狀態: {info['status']}")
                    print()
                    
            elif args.start:
                result = await cli_manager.start_agent(args.start, args.mode)
                print(f"\n🚀 代理執行結果:")
                print("=" * 60)
                print(json.dumps(result, indent=2, ensure_ascii=False))
                
        elif args.command == 'workflow':
            result = await cli_manager.run_workflow(
                args.run,
                symbol=args.symbol,
                data_minutes=args.data_minutes,
                training_hours=args.training_hours
            )
            print(f"\n🔄 工作流程執行結果:")
            print("=" * 60)
            print(json.dumps(result, indent=2, ensure_ascii=False))
            
        elif args.command == 'trading':
            result = await cli_manager.start_trading(
                args.start,
                capital=args.capital,
                model_path=args.model_path
            )
            print(f"\n📈 交易執行結果:")
            print("=" * 60)
            print(json.dumps(result, indent=2, ensure_ascii=False))
            
        elif args.command == 'status':
            if args.all:
                status = await cli_manager.get_system_status()
                print(f"\n📊 系統狀態:")
                print("=" * 60)
                print(json.dumps(status, indent=2, ensure_ascii=False))
        
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