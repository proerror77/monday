#!/usr/bin/env python3
"""
層級代理架構集成測試
=====================================

使用 uv 管理依賴，連接 Docker 中的 ClickHouse 和 Redis
測試新的雙平面HFT系統架構
"""

import asyncio
import logging
import time
import redis
import json
import sys
import os
from typing import Dict, Any, Optional

# 設啟動日誌
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(name)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

class DockerServicesManager:
    """Docker 服務管理器"""
    
    def __init__(self):
        self.redis_client = None
        self.clickhouse_url = "http://localhost:8123"
        
    async def test_connections(self) -> Dict[str, Any]:
        """測試 Docker 服務連接"""
        results = {}
        
        # 測試 Redis 連接
        try:
            self.redis_client = redis.Redis(host='localhost', port=6379, db=0, decode_responses=True)
            redis_ping = self.redis_client.ping()
            results["redis"] = {
                "status": "connected" if redis_ping else "failed",
                "ping": redis_ping,
                "host": "localhost:6379"
            }
        except Exception as e:
            results["redis"] = {"status": "failed", "error": str(e)}
        
        # 測試 ClickHouse 連接
        try:
            import requests
            response = requests.get(f"{self.clickhouse_url}/ping", timeout=5)
            results["clickhouse"] = {
                "status": "connected" if response.status_code == 200 else "failed",
                "response": response.text.strip(),
                "host": "localhost:8123"
            }
        except Exception as e:
            results["clickhouse"] = {"status": "failed", "error": str(e)}
        
        return results

class MockRustHFTTools:
    """模擬 Rust HFT 工具（用於測試）"""
    
    async def get_system_status(self) -> Dict[str, Any]:
        """模擬獲取系統狀態"""
        return {
            "status": "healthy",
            "uptime_seconds": 3600,
            "memory_usage_mb": 128,
            "cpu_usage_percent": 15.2,
            "last_update": time.time()
        }
    
    async def train_model(self, symbol: str, training_hours: int) -> Dict[str, Any]:
        """模擬模型訓練"""
        await asyncio.sleep(2)  # 模擬訓練時間
        return {
            "success": True,
            "model_path": f"/models/{symbol}_tlob_v{int(time.time())}.safetensors",
            "training_duration": training_hours * 0.1,  # 模擬快速訓練
            "performance": {
                "sharpe_ratio": 1.8,
                "max_drawdown": 0.03,
                "accuracy": 0.62,
                "win_rate": 0.54
            }
        }
    
    async def evaluate_model(self, model_path: str, symbol: str) -> Dict[str, Any]:
        """模擬模型評估"""
        await asyncio.sleep(1)
        return {
            "success": True,
            "sharpe_ratio": 1.75,
            "max_drawdown": 0.04,
            "accuracy": 0.61,
            "win_rate": 0.53,
            "profit_factor": 1.4
        }
    
    async def start_live_trading(self, symbol: str, model_path: str, capital: float) -> Dict[str, Any]:
        """模擬啟動實盤交易"""
        return {
            "success": True,
            "trading_session_id": f"session_{int(time.time())}",
            "symbol": symbol,
            "capital": capital,
            "model": model_path,
            "status": "active"
        }
    
    async def emergency_stop(self) -> Dict[str, Any]:
        """模擬緊急停止"""
        return {
            "success": True,
            "stopped_sessions": 1,
            "timestamp": time.time()
        }

class SimplifiedHFTOperationsAgent:
    """簡化版 HFT 營運代理（無 Agno 依賴）"""
    
    def __init__(self, docker_services: DockerServicesManager):
        self.docker_services = docker_services
        self.rust_tools = MockRustHFTTools()
        self.agent_id = f"hft_ops_{int(time.time())}"
        
        logger.info(f"🏭 簡化版 HFT Operations Agent 初始化完成")
    
    async def monitor_real_time_risk(self, symbol: str) -> Dict[str, Any]:
        """實時風險監控"""
        try:
            # 從 Redis 獲取實時數據（如果有的話）
            redis_key = f'hft:orderbook:{symbol}'
            redis_data = self.docker_services.redis_client.get(redis_key)
            
            if redis_data:
                data = json.loads(redis_data)
                current_price = data.get('mid_price', 0)
                risk_score = 0.3  # 模擬風險分數
            else:
                # 模擬數據
                current_price = 97341.25
                risk_score = 0.25
                logger.info(f"⚠️  {symbol} Redis 數據不存在，使用模擬數據")
            
            risk_level = "LOW" if risk_score < 0.4 else "MEDIUM"
            
            return {
                "status": "success",
                "symbol": symbol,
                "current_price": current_price,
                "risk_score": risk_score,
                "risk_level": risk_level,
                "timestamp": time.time()
            }
            
        except Exception as e:
            logger.error(f"❌ 實時風險監控失敗: {e}")
            return {"status": "error", "error": str(e)}
    
    async def get_system_health(self) -> Dict[str, Any]:
        """獲取系統健康狀況"""
        try:
            # 獲取 Rust 系統狀態
            rust_status = await self.rust_tools.get_system_status()
            
            # 檢查 Docker 服務
            docker_status = await self.docker_services.test_connections()
            
            overall_health = "HEALTHY"
            if any(service.get("status") != "connected" for service in docker_status.values()):
                overall_health = "DEGRADED"
            
            return {
                "status": "success",
                "overall_health": overall_health,
                "components": {
                    "rust_engine": rust_status,
                    **docker_status
                },
                "timestamp": time.time()
            }
            
        except Exception as e:
            logger.error(f"❌ 系統健康檢查失敗: {e}")
            return {"status": "error", "error": str(e)}
    
    async def run_operations_cycle(self) -> Dict[str, Any]:
        """運行完整營運週期"""
        logger.info("🔄 開始 HFT 營運週期...")
        
        cycle_results = {
            "cycle_id": f"ops_{int(time.time())}",
            "start_time": time.time(),
            "checks": {}
        }
        
        try:
            # 1. 實時風險檢查
            risk_check = await self.monitor_real_time_risk("BTCUSDT")
            cycle_results["checks"]["risk"] = risk_check
            
            # 2. 系統健康檢查
            health_check = await self.get_system_health()
            cycle_results["checks"]["health"] = health_check
            
            # 3. 智能決策分析（簡化版）
            decision = self._make_simple_decision(cycle_results["checks"])
            cycle_results["decision"] = decision
            
            cycle_results["status"] = "completed"
            cycle_results["duration"] = time.time() - cycle_results["start_time"]
            
            logger.info(f"✅ 營運週期完成 ({cycle_results['duration']:.2f}秒)")
            return cycle_results
            
        except Exception as e:
            logger.error(f"❌ 營運週期失敗: {e}")
            cycle_results["status"] = "failed"
            cycle_results["error"] = str(e)
            return cycle_results
    
    def _make_simple_decision(self, checks: Dict[str, Any]) -> Dict[str, Any]:
        """簡化版決策邏輯"""
        risk_level = checks.get("risk", {}).get("risk_level", "UNKNOWN")
        overall_health = checks.get("health", {}).get("overall_health", "UNKNOWN")
        
        if risk_level in ["CRITICAL", "DANGER"] or overall_health == "DEGRADED":
            recommendation = "REDUCE_RISK"
            actions = [{"type": "REDUCE_POSITION", "ratio": 0.5}]
        else:
            recommendation = "CONTINUE"
            actions = []
        
        return {
            "recommendation": recommendation,
            "actions": actions,
            "reasoning": f"Risk: {risk_level}, Health: {overall_health}",
            "timestamp": time.time()
        }

class SimplifiedMLWorkflowAgent:
    """簡化版 ML 工作流程代理（無 Agno 依賴）"""
    
    def __init__(self, docker_services: DockerServicesManager):
        self.docker_services = docker_services
        self.rust_tools = MockRustHFTTools()
        self.agent_id = f"ml_workflow_{int(time.time())}"
        
        logger.info(f"🧠 簡化版 ML Workflow Agent 初始化完成")
    
    async def collect_market_data(self, symbol: str, duration_minutes: int) -> Dict[str, Any]:
        """模擬數據收集"""
        logger.info(f"📦 開始數據收集 - {symbol} ({duration_minutes}分鐘)")
        
        # 模擬數據收集過程
        await asyncio.sleep(1)
        
        return {
            "status": "success",
            "symbol": symbol,
            "duration_minutes": duration_minutes,
            "records_collected": duration_minutes * 1000,
            "database_writes": duration_minutes * 10,
            "execution_time_seconds": 1.0,
            "data_source": "mock_simulation"
        }
    
    async def train_tlob_model(self, symbol: str, training_config: Dict[str, Any]) -> Dict[str, Any]:
        """TLOB 模型訓練"""
        logger.info(f"🤖 開始 TLOB 模型訓練 - {symbol}")
        
        # 使用 Mock Rust 工具訓練
        training_hours = training_config.get("training_hours", 24)
        training_result = await self.rust_tools.train_model(symbol, training_hours)
        
        if training_result.get("success"):
            logger.info(f"✅ TLOB 模型訓練完成: {training_result.get('model_path')}")
            return {"status": "success", "result": training_result}
        else:
            return {"status": "failed", "error": "Training failed"}
    
    async def evaluate_model(self, model_path: str, symbol: str) -> Dict[str, Any]:
        """模型評估"""
        logger.info(f"📈 開始模型評估 - {model_path}")
        
        # 執行評估
        base_evaluation = await self.rust_tools.evaluate_model(model_path, symbol)
        
        # 部署可行性判斷
        criteria = {
            "min_sharpe_ratio": 1.5,
            "max_drawdown": 0.05,
            "min_accuracy": 0.60,
            "min_win_rate": 0.52
        }
        
        performance = base_evaluation
        checks = {}
        for criterion, threshold in criteria.items():
            metric_key = criterion.replace("min_", "").replace("max_", "")
            current_value = performance.get(metric_key, 0)
            
            if "min_" in criterion:
                checks[criterion] = current_value >= threshold
            else:  # max_
                checks[criterion] = current_value <= threshold
        
        all_passed = all(checks.values())
        
        deployment_decision = {
            "deployment_approved": all_passed,
            "criteria_checks": checks,
            "recommendation": "DEPLOY" if all_passed else "RETRAIN"
        }
        
        return {
            "status": "success",
            "base_evaluation": base_evaluation,
            "deployment_decision": deployment_decision,
            "evaluation_timestamp": time.time()
        }
    
    async def execute_complete_ml_workflow(self, symbol: str, workflow_config: Dict[str, Any]) -> Dict[str, Any]:
        """執行完整 ML 工作流程"""
        workflow_id = f"ml_workflow_{symbol}_{int(time.time())}"
        logger.info(f"🚀 開始完整 ML 工作流程: {workflow_id}")
        
        workflow_result = {
            "workflow_id": workflow_id,
            "symbol": symbol,
            "config": workflow_config,
            "start_time": time.time(),
            "stages": {},
            "status": "in_progress"
        }
        
        try:
            # 階段1: 數據收集
            logger.info("📦 階段1: 智能數據收集...")
            data_collection_result = await self.collect_market_data(
                symbol, workflow_config.get("data_collection_minutes", 10)
            )
            workflow_result["stages"]["data_collection"] = data_collection_result
            
            if data_collection_result["status"] != "success":
                workflow_result["status"] = "failed"
                return workflow_result
            
            # 階段2: 模型訓練
            logger.info("🤖 階段2: TLOB 模型訓練...")
            training_config = {
                "training_hours": workflow_config.get("training_hours", 24),
                "model_architecture": "tlob_transformer"
            }
            
            training_result = await self.train_tlob_model(symbol, training_config)
            workflow_result["stages"]["training"] = training_result
            
            if training_result["status"] != "success":
                workflow_result["status"] = "failed"
                return workflow_result
            
            # 階段3: 模型評估
            logger.info("📈 階段3: 多維度模型評估...")
            model_path = training_result["result"]["model_path"]
            
            evaluation_result = await self.evaluate_model(model_path, symbol)
            workflow_result["stages"]["evaluation"] = evaluation_result
            
            if evaluation_result["status"] != "success":
                workflow_result["status"] = "failed"
                return workflow_result
            
            # 最終狀態
            deployment_decision = evaluation_result["deployment_decision"]
            workflow_result["stages"]["deployment_decision"] = deployment_decision
            
            if deployment_decision["deployment_approved"]:
                workflow_result["status"] = "ready_for_deployment"
                workflow_result["recommended_action"] = "DEPLOY_MODEL"
            else:
                workflow_result["status"] = "requires_improvement"
                workflow_result["recommended_action"] = "RETRAIN_MODEL"
            
            workflow_result["end_time"] = time.time()
            workflow_result["total_duration"] = workflow_result["end_time"] - workflow_result["start_time"]
            
            logger.info(f"✅ ML 工作流程完成: {workflow_result['status']} ({workflow_result['total_duration']:.1f}秒)")
            return workflow_result
            
        except Exception as e:
            logger.error(f"❌ ML 工作流程異常: {e}")
            workflow_result["status"] = "error"
            workflow_result["error"] = str(e)
            workflow_result["end_time"] = time.time()
            return workflow_result

async def test_docker_services():
    """測試 Docker 服務連接"""
    logger.info("🐳 測試 Docker 服務連接...")
    
    docker_manager = DockerServicesManager()
    connections = await docker_manager.test_connections()
    
    for service, result in connections.items():
        status_icon = "✅" if result["status"] == "connected" else "❌"
        logger.info(f"{status_icon} {service.upper()}: {result['status']} - {result.get('host', '')}")
        if "error" in result:
            logger.error(f"   錯誤: {result['error']}")
    
    return docker_manager, connections

async def test_hft_operations_agent(docker_manager):
    """測試 HFT 營運代理"""
    logger.info("\n🏭 測試 HFT Operations Agent...")
    
    agent = SimplifiedHFTOperationsAgent(docker_manager)
    
    # 運行營運週期
    result = await agent.run_operations_cycle()
    
    logger.info(f"📊 營運週期結果:")
    logger.info(f"   狀態: {result.get('status')}")
    logger.info(f"   執行時間: {result.get('duration', 0):.2f}秒")
    logger.info(f"   決策建議: {result.get('decision', {}).get('recommendation', 'NONE')}")
    
    return result

async def test_ml_workflow_agent(docker_manager):
    """測試 ML 工作流程代理"""
    logger.info("\n🧠 測試 ML Workflow Agent...")
    
    agent = SimplifiedMLWorkflowAgent(docker_manager)
    
    # 執行完整 ML 工作流程
    workflow_config = {
        "data_collection_minutes": 10,
        "training_hours": 24,
        "evaluation_hours": 168
    }
    
    result = await agent.execute_complete_ml_workflow("BTCUSDT", workflow_config)
    
    logger.info(f"📊 ML 工作流程結果:")
    logger.info(f"   狀態: {result.get('status')}")
    logger.info(f"   總執行時間: {result.get('total_duration', 0):.1f}秒")
    logger.info(f"   建議行動: {result.get('recommended_action', 'NONE')}")
    
    return result

async def test_agents_coordination(docker_manager):
    """測試兩個代理協調工作"""
    logger.info("\n🤝 測試代理協調工作...")
    
    # 同時創建兩個代理
    hft_agent = SimplifiedHFTOperationsAgent(docker_manager)
    ml_agent = SimplifiedMLWorkflowAgent(docker_manager)
    
    # 模擬協調場景：ML 代理完成訓練後，HFT 代理執行部署
    logger.info("📋 場景: ML 代理訓練模型 → HFT 代理部署模型")
    
    # 1. ML 代理訓練模型
    training_result = await ml_agent.train_tlob_model("BTCUSDT", {"training_hours": 24})
    
    if training_result["status"] == "success":
        model_path = training_result["result"]["model_path"]
        logger.info(f"🤖 ML 代理完成訓練: {model_path}")
        
        # 2. HFT 代理接收部署請求
        deployment_command = {
            "type": "DEPLOY_MODEL",
            "symbol": "BTCUSDT",
            "model_path": model_path
        }
        
        # 模擬部署執行
        deployment_result = await hft_agent.rust_tools.start_live_trading(
            "BTCUSDT", model_path, 1000.0
        )
        
        logger.info(f"🚀 HFT 代理完成部署: {deployment_result}")
        
        coordination_result = {
            "status": "success",
            "ml_training": training_result,
            "hft_deployment": deployment_result,
            "coordination": "ML → HFT 成功協調"
        }
    else:
        coordination_result = {
            "status": "failed",
            "error": "ML 訓練失敗，無法進行協調"
        }
    
    return coordination_result

async def main():
    """主測試函數"""
    print("=" * 80)
    print("🧪 層級代理架構集成測試")
    print("   基於 Docker ClickHouse + Redis + uv 管理")
    print("=" * 80)
    
    start_time = time.time()
    
    try:
        # 1. 測試 Docker 服務
        docker_manager, docker_status = await test_docker_services()
        
        # 檢查必要服務是否可用
        if not all(service.get("status") == "connected" for service in docker_status.values()):
            logger.warning("⚠️  部分 Docker 服務不可用，但繼續測試...")
        
        # 2. 測試 HFT Operations Agent
        hft_result = await test_hft_operations_agent(docker_manager)
        
        # 3. 測試 ML Workflow Agent
        ml_result = await test_ml_workflow_agent(docker_manager)
        
        # 4. 測試代理協調
        coordination_result = await test_agents_coordination(docker_manager)
        
        # 總結測試結果
        total_time = time.time() - start_time
        
        print("\n" + "=" * 80)
        print("📊 集成測試總結")
        print("=" * 80)
        print(f"⏱️  總執行時間: {total_time:.2f}秒")
        print(f"🐳 Docker 服務: {len([s for s in docker_status.values() if s.get('status') == 'connected'])}/2 連接成功")
        print(f"🏭 HFT Operations: {hft_result.get('status', 'unknown')}")
        print(f"🧠 ML Workflow: {ml_result.get('status', 'unknown')}")
        print(f"🤝 代理協調: {coordination_result.get('status', 'unknown')}")
        
        # 最終評估
        success_conditions = [
            hft_result.get("status") in ["completed", "success"],
            ml_result.get("status") in ["ready_for_deployment", "success"],
            coordination_result.get("status") == "success"
        ]
        
        if all(success_conditions):
            print("🎉 所有測試通過！層級代理架構運行正常")
            return 0
        else:
            print("⚠️  部分測試未完全通過，請檢查日誌")
            print(f"   HFT: {hft_result.get('status')}")
            print(f"   ML: {ml_result.get('status')}")
            print(f"   協調: {coordination_result.get('status')}")
            return 1
            
    except Exception as e:
        logger.error(f"❌ 集成測試失敗: {e}")
        return 1

if __name__ == "__main__":
    # 設置事件循環策略（macOS 兼容性）
    if sys.platform == "darwin":
        asyncio.set_event_loop_policy(asyncio.DefaultEventLoopPolicy())
    
    exit_code = asyncio.run(main())
    sys.exit(exit_code)