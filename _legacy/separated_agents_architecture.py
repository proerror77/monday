#!/usr/bin/env python3
"""
分離式Agent架構 - 基於Agno Workflows v2
==========================================

正確分離交易運營Agent和ML Agent，各司其職：

1. ML Agent: 純粹的MLOps pipeline (數據 → 訓練 → 評估 → 部署決策)
2. Trading Operations Agent: 實時交易系統運營 (模型加載 → 交易執行 → 風險監控)

通信機制：Redis + API
"""

import asyncio
import json
import time
import logging
from dataclasses import dataclass, field
from typing import Dict, Any, Optional, List
from enum import Enum
from pathlib import Path
import redis
import requests

# 設置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

# ==========================================
# 核心通信機制
# ==========================================

class AgentCommunicationBus:
    """Agent間通信總線"""
    
    def __init__(self, redis_url: str = "redis://localhost:6379"):
        self.redis_client = redis.from_url(redis_url)
        self.logger = logging.getLogger("CommunicationBus")
    
    async def publish_message(self, channel: str, message: Dict[str, Any]):
        """發布消息到指定頻道"""
        message_str = json.dumps(message)
        self.redis_client.publish(channel, message_str)
        self.logger.info(f"📡 發布消息到 {channel}: {message.get('type', 'unknown')}")
    
    async def subscribe_channel(self, channel: str, callback):
        """訂閱頻道並處理消息"""
        pubsub = self.redis_client.pubsub()
        pubsub.subscribe(channel)
        
        for message in pubsub.listen():
            if message['type'] == 'message':
                try:
                    data = json.loads(message['data'])
                    await callback(data)
                except Exception as e:
                    self.logger.error(f"❌ 處理消息失敗: {e}")

# ==========================================
# ML Agent - 純粹的MLOps Pipeline
# ==========================================

from agno_v2_mlops_workflow import (
    Step, Loop, Router, Workflow, StepInput, StepOutput,
    AgnoV2MLOpsWorkflow
)

class MLAgentV2:
    """
    ML Agent - 基於Agno Workflows v2的純粹MLOps系統
    
    職責：
    1. 數據收集和預處理
    2. TLOB模型訓練和超參數調優
    3. 模型評估 (IC/IR等指標)
    4. 部署決策 (是否將模型發送給Trading Agent)
    """
    
    def __init__(self, config_path: Optional[str] = None):
        self.name = "MLAgent"
        self.logger = logging.getLogger("MLAgent")
        self.communication_bus = AgentCommunicationBus()
        
        # 使用已有的MLOps workflow
        self.mlops_workflow = AgnoV2MLOpsWorkflow(config_path)
        
        # ML Agent專用配置
        self.config = {
            "hyperparameter_tuning": {
                "max_iterations": 10,
                "target_ic": 0.05,
                "target_ir": 1.2,
                "target_sharpe": 1.5
            },
            "evaluation_criteria": {
                "min_ic": 0.03,
                "min_ir": 0.8,
                "min_sharpe": 1.2,
                "max_drawdown": 0.08
            },
            "deployment_decision": {
                "auto_deploy_threshold": 0.9,  # 評估分數閾值
                "require_manual_approval": False
            }
        }
        
        self.logger.info("🧠 ML Agent 初始化完成")
    
    async def execute_ml_pipeline(self, symbol: str = "BTCUSDT") -> Dict[str, Any]:
        """執行完整的ML Pipeline"""
        self.logger.info(f"🚀 開始執行ML Pipeline: {symbol}")
        
        pipeline_start = time.time()
        
        try:
            # 1. 執行增強的MLOps workflow（包含超參數調優循環）
            result = await self._execute_enhanced_mlops_workflow(symbol)
            
            if result["status"] == "success":
                # 2. 提取最佳模型和評估結果
                best_model_info = self._extract_best_model_info(result)
                
                # 3. 做出部署決策
                deployment_decision = self._make_deployment_decision(best_model_info)
                
                # 4. 如果決定部署，通知Trading Operations Agent
                if deployment_decision["should_deploy"]:
                    await self._notify_trading_agent(best_model_info, deployment_decision)
                
                execution_time = time.time() - pipeline_start
                
                return {
                    "status": "success",
                    "symbol": symbol,
                    "execution_time": execution_time,
                    "best_model": best_model_info,
                    "deployment_decision": deployment_decision,
                    "mlops_result": result
                }
            else:
                return {
                    "status": "failed",
                    "error": result.get("error"),
                    "symbol": symbol
                }
                
        except Exception as e:
            self.logger.error(f"❌ ML Pipeline執行失敗: {e}")
            return {
                "status": "failed",
                "error": str(e),
                "symbol": symbol
            }
    
    async def _execute_enhanced_mlops_workflow(self, symbol: str) -> Dict[str, Any]:
        """執行增強的MLOps workflow（包含超參數調優）"""
        
        # 自定義配置，加入超參數調優
        custom_config = {
            "symbol": symbol,
            "duration_minutes": 10,  # 數據收集時間
            "training_hours": 24,
            "enable_hyperparameter_tuning": True,
            "max_tuning_iterations": self.config["hyperparameter_tuning"]["max_iterations"]
        }
        
        # 執行MLOps管道
        result = await self.mlops_workflow.execute_mlops_pipeline(symbol, custom_config)
        
        return result
    
    def _extract_best_model_info(self, mlops_result: Dict[str, Any]) -> Dict[str, Any]:
        """從MLOps結果中提取最佳模型信息"""
        
        workflow_result = mlops_result.get("result", {})
        results = workflow_result.get("results", {})
        
        # 從優化循環中獲取最佳結果
        if "model_optimization_loop" in results:
            loop_result = results["model_optimization_loop"]
            if loop_result.success:
                best_result = loop_result.content.get("best_result")
                if best_result and "evaluate_model" in best_result:
                    eval_result = best_result["evaluate_model"]
                    if eval_result.success:
                        dashboard = eval_result.content.get("evaluation_dashboard", {})
                        train_result = best_result.get("train_model", {})
                        
                        return {
                            "model_path": train_result.content.get("model_path") if train_result.success else None,
                            "performance_metrics": {
                                "sharpe_ratio": dashboard.get("sharpe_ratio", 0),
                                "information_coefficient": dashboard.get("information_coefficient", 0),
                                "information_ratio": dashboard.get("information_ratio", 0),
                                "max_drawdown": dashboard.get("max_drawdown", 0),
                                "win_rate": dashboard.get("win_rate", 0),
                                "profit_factor": dashboard.get("profit_factor", 0)
                            },
                            "training_info": {
                                "iterations_used": loop_result.content.get("total_iterations", 1),
                                "hyperparameters": train_result.content.get("hyperparameters", {}) if train_result.success else {}
                            },
                            "evaluation_timestamp": time.time()
                        }
        
        # 默認返回空結果
        return {
            "model_path": None,
            "performance_metrics": {},
            "training_info": {},
            "evaluation_timestamp": time.time()
        }
    
    def _make_deployment_decision(self, model_info: Dict[str, Any]) -> Dict[str, Any]:
        """基於評估結果做出部署決策"""
        
        metrics = model_info.get("performance_metrics", {})
        criteria = self.config["evaluation_criteria"]
        
        # 檢查各項指標是否達標
        checks = {
            "ic_check": metrics.get("information_coefficient", 0) >= criteria["min_ic"],
            "ir_check": metrics.get("information_ratio", 0) >= criteria["min_ir"],  
            "sharpe_check": metrics.get("sharpe_ratio", 0) >= criteria["min_sharpe"],
            "drawdown_check": metrics.get("max_drawdown", 1) <= criteria["max_drawdown"]
        }
        
        # 計算整體評估分數
        evaluation_score = sum(checks.values()) / len(checks)
        
        # 部署決策
        should_deploy = (
            evaluation_score >= self.config["deployment_decision"]["auto_deploy_threshold"]
            and model_info.get("model_path") is not None
        )
        
        decision = {
            "should_deploy": should_deploy,
            "evaluation_score": evaluation_score,
            "criteria_checks": checks,
            "decision_reason": self._generate_decision_reason(checks, evaluation_score),
            "model_ready": model_info.get("model_path") is not None,
            "timestamp": time.time()
        }
        
        self.logger.info(f"📊 部署決策: {'✅ 部署' if should_deploy else '❌ 不部署'}")
        self.logger.info(f"   評估分數: {evaluation_score:.2f}")
        
        return decision
    
    def _generate_decision_reason(self, checks: Dict[str, bool], score: float) -> str:
        """生成決策原因"""
        passed_checks = [k for k, v in checks.items() if v]
        failed_checks = [k for k, v in checks.items() if not v]
        
        if score >= 0.9:
            return f"所有關鍵指標優秀 (通過: {len(passed_checks)}/{len(checks)})"
        elif score >= 0.75:
            return f"主要指標達標 (通過: {len(passed_checks)}/{len(checks)})"
        else:
            return f"關鍵指標不達標: {', '.join(failed_checks)}"
    
    async def _notify_trading_agent(self, model_info: Dict[str, Any], decision: Dict[str, Any]):
        """通知Trading Operations Agent新模型可用"""
        
        notification = {
            "type": "NEW_MODEL_AVAILABLE",
            "source": "MLAgent",
            "timestamp": time.time(),
            "model_info": model_info,
            "deployment_decision": decision,
            "message": f"新的TLOB模型訓練完成，評估分數: {decision['evaluation_score']:.2f}"
        }
        
        await self.communication_bus.publish_message("trading_ops_channel", notification)
        self.logger.info("📡 已通知Trading Operations Agent新模型可用")

# ==========================================
# Trading Operations Agent - 實時交易運營
# ==========================================

class TradingOperationsAgentV2:
    """
    Trading Operations Agent - 基於Agno Workflows v2的實時交易運營系統
    
    職責：
    1. 接收ML Agent的新模型
    2. 執行安全的模型部署 (藍綠部署)
    3. 實時交易執行和監控
    4. 風險管理和緊急停止
    5. 系統健康檢查
    """
    
    def __init__(self, config_path: Optional[str] = None):
        self.name = "TradingOperationsAgent"
        self.logger = logging.getLogger("TradingOpsAgent")
        self.communication_bus = AgentCommunicationBus()
        
        # Trading Agent專用配置
        self.config = {
            "risk_management": {
                "max_position_size": 10000,  # 最大倉位
                "max_daily_loss": 1000,      # 最大日損失
                "emergency_stop_threshold": 0.05,  # 緊急停止閾值
                "max_drawdown_alert": 0.03   # 回撤告警閾值
            },
            "deployment": {
                "strategy": "blue_green",
                "shadow_trading_duration": 300,  # 影子交易時間(秒)
                "promotion_criteria": {
                    "min_profit": 10,  # 影子交易最小盈利
                    "max_correlation": 0.8  # 與現有策略的最大相關性
                }
            },
            "monitoring": {
                "health_check_interval": 30,  # 健康檢查間隔(秒)
                "performance_report_interval": 300  # 性能報告間隔(秒)
            }
        }
        
        # 當前交易狀態
        self.trading_state = {
            "active_models": {},
            "current_positions": {},
            "daily_pnl": 0.0,
            "system_health": "healthy",
            "last_health_check": time.time()
        }
        
        self.logger.info("🏭 Trading Operations Agent 初始化完成")
    
    async def start_operations(self):
        """啟動交易運營系統"""
        self.logger.info("🚀 啟動Trading Operations系統...")
        
        # 啟動各個運營組件
        tasks = [
            self._start_model_listener(),
            self._start_health_monitor(),
            self._start_risk_monitor(),
            self._start_performance_reporter()
        ]
        
        await asyncio.gather(*tasks)
    
    async def _start_model_listener(self):
        """監聽ML Agent的新模型通知"""
        self.logger.info("👂 開始監聽ML Agent通知...")
        
        async def handle_ml_notification(message: Dict[str, Any]):
            if message.get("type") == "NEW_MODEL_AVAILABLE":
                await self._handle_new_model_notification(message)
        
        await self.communication_bus.subscribe_channel("trading_ops_channel", handle_ml_notification)
    
    async def _handle_new_model_notification(self, message: Dict[str, Any]):
        """處理新模型通知"""
        model_info = message.get("model_info", {})
        decision = message.get("deployment_decision", {})
        
        self.logger.info("📥 收到新模型通知")
        self.logger.info(f"   模型路徑: {model_info.get('model_path')}")
        self.logger.info(f"   評估分數: {decision.get('evaluation_score', 0):.2f}")
        
        if decision.get("should_deploy", False):
            await self._execute_model_deployment(model_info)
        else:
            self.logger.info("⚠️  模型評估未達部署標準，跳過部署")
    
    async def _execute_model_deployment(self, model_info: Dict[str, Any]):
        """執行安全的模型部署"""
        model_path = model_info.get("model_path")
        
        if not model_path:
            self.logger.error("❌ 模型路徑為空，無法部署")
            return
        
        self.logger.info(f"🚀 開始部署模型: {model_path}")
        
        try:
            # 1. 藍綠部署 - 先在影子環境測試
            shadow_result = await self._deploy_to_shadow_environment(model_info)
            
            if shadow_result["success"]:
                # 2. 影子交易測試通過，提升到生產環境
                production_result = await self._promote_to_production(model_info, shadow_result)
                
                if production_result["success"]:
                    self.logger.info("✅ 模型部署成功並已上線交易")
                    
                    # 3. 更新交易狀態
                    self._update_active_models(model_info)
                    
                    # 4. 通知ML Agent部署成功
                    await self._notify_ml_agent_deployment_success(model_info)
                else:
                    self.logger.error("❌ 生產環境部署失敗")
            else:
                self.logger.error("❌ 影子環境測試失敗，取消部署")
                
        except Exception as e:
            self.logger.error(f"❌ 模型部署過程異常: {e}")
    
    async def _deploy_to_shadow_environment(self, model_info: Dict[str, Any]) -> Dict[str, Any]:
        """部署到影子環境進行測試"""
        self.logger.info("🌑 部署到影子環境...")
        
        # 模擬影子環境部署
        await asyncio.sleep(2)
        
        # 模擬影子交易測試
        shadow_duration = self.config["deployment"]["shadow_trading_duration"]
        self.logger.info(f"📊 開始影子交易測試 ({shadow_duration}秒)...")
        
        # 這裡應該調用實際的Rust HFT系統進行影子交易
        # 目前模擬測試結果
        await asyncio.sleep(3)  # 模擬測試時間
        
        # 模擬測試結果
        test_result = {
            "success": True,
            "shadow_profit": 15.5,  # 影子交易盈利
            "correlation_with_existing": 0.6,  # 與現有策略相關性
            "execution_latency": 85,  # 執行延遲(微秒)
            "test_duration": shadow_duration
        }
        
        # 檢查提升條件
        criteria = self.config["deployment"]["promotion_criteria"]
        meets_criteria = (
            test_result["shadow_profit"] >= criteria["min_profit"] and
            test_result["correlation_with_existing"] <= criteria["max_correlation"]
        )
        
        test_result["meets_promotion_criteria"] = meets_criteria
        
        if meets_criteria:
            self.logger.info("✅ 影子交易測試通過提升條件")
        else:
            self.logger.warning("⚠️  影子交易測試未達提升條件")
        
        return test_result
    
    async def _promote_to_production(self, model_info: Dict[str, Any], shadow_result: Dict[str, Any]) -> Dict[str, Any]:
        """提升到生產環境"""
        
        if not shadow_result.get("meets_promotion_criteria", False):
            return {"success": False, "reason": "未滿足提升條件"}
        
        self.logger.info("🚀 提升到生產環境...")
        
        # 這裡應該調用實際的Rust HFT系統API
        # 例如: rust_api.load_new_model(model_info["model_path"])
        await asyncio.sleep(1)  # 模擬部署時間
        
        return {
            "success": True,
            "deployment_time": time.time(),
            "deployment_strategy": "blue_green",
            "shadow_test_result": shadow_result
        }
    
    def _update_active_models(self, model_info: Dict[str, Any]):
        """更新活躍模型狀態"""
        model_id = f"model_{int(time.time())}"
        
        self.trading_state["active_models"][model_id] = {
            "model_path": model_info.get("model_path"),
            "performance_metrics": model_info.get("performance_metrics", {}),
            "deployment_time": time.time(),
            "status": "active"
        }
        
        self.logger.info(f"📊 更新活躍模型列表: {len(self.trading_state['active_models'])} 個模型")
    
    async def _notify_ml_agent_deployment_success(self, model_info: Dict[str, Any]):
        """通知ML Agent部署成功"""
        notification = {
            "type": "MODEL_DEPLOYED",
            "source": "TradingOperationsAgent",
            "timestamp": time.time(),
            "model_path": model_info.get("model_path"),
            "message": "模型已成功部署到生產環境並開始交易"
        }
        
        await self.communication_bus.publish_message("ml_agent_channel", notification)
    
    async def _start_health_monitor(self):
        """啟動系統健康監控"""
        while True:
            try:
                await self._perform_health_check()
                await asyncio.sleep(self.config["monitoring"]["health_check_interval"])
            except Exception as e:
                self.logger.error(f"❌ 健康檢查異常: {e}")
                await asyncio.sleep(10)
    
    async def _perform_health_check(self):
        """執行系統健康檢查"""
        # 模擬健康檢查
        checks = {
            "rust_hft_connection": True,  # 檢查Rust系統連接
            "database_connection": True,  # 檢查數據庫連接
            "trading_latency": True,      # 檢查交易延遲
            "memory_usage": True          # 檢查內存使用
        }
        
        all_healthy = all(checks.values())
        
        self.trading_state["system_health"] = "healthy" if all_healthy else "degraded"
        self.trading_state["last_health_check"] = time.time()
        
        if not all_healthy:
            self.logger.warning("⚠️  系統健康檢查發現問題")
    
    async def _start_risk_monitor(self):
        """啟動風險監控"""
        while True:
            try:
                await self._check_risk_limits()
                await asyncio.sleep(5)  # 風險檢查更頻繁
            except Exception as e:
                self.logger.error(f"❌ 風險監控異常: {e}")
                await asyncio.sleep(5)
    
    async def _check_risk_limits(self):
        """檢查風險限制"""
        risk_config = self.config["risk_management"]
        
        # 模擬風險檢查
        current_loss = self.trading_state.get("daily_pnl", 0)
        
        if current_loss < -risk_config["max_daily_loss"]:
            self.logger.critical("🚨 觸發緊急停止：日損失超限")
            await self._emergency_stop("日損失超限")
    
    async def _emergency_stop(self, reason: str):
        """緊急停止交易"""
        self.logger.critical(f"🚨 緊急停止交易: {reason}")
        
        # 這裡應該調用Rust系統的緊急停止API
        # rust_api.emergency_stop()
        
        self.trading_state["system_health"] = "emergency_stopped"
        
        # 通知相關方
        emergency_notification = {
            "type": "EMERGENCY_STOP",
            "source": "TradingOperationsAgent", 
            "timestamp": time.time(),
            "reason": reason
        }
        
        await self.communication_bus.publish_message("emergency_channel", emergency_notification)
    
    async def _start_performance_reporter(self):
        """啟動性能報告"""
        while True:
            try:
                await self._generate_performance_report()
                await asyncio.sleep(self.config["monitoring"]["performance_report_interval"])
            except Exception as e:
                self.logger.error(f"❌ 性能報告異常: {e}")
                await asyncio.sleep(60)
    
    async def _generate_performance_report(self):
        """生成性能報告"""
        report = {
            "timestamp": time.time(),
            "active_models": len(self.trading_state["active_models"]),
            "daily_pnl": self.trading_state.get("daily_pnl", 0),
            "system_health": self.trading_state["system_health"],
            "uptime": time.time() - self.trading_state.get("start_time", time.time())
        }
        
        self.logger.info(f"📊 性能報告: PnL={report['daily_pnl']:.2f}, 模型數={report['active_models']}")
        
        # 發送性能報告
        await self.communication_bus.publish_message("performance_channel", {
            "type": "PERFORMANCE_REPORT",
            "source": "TradingOperationsAgent",
            "report": report
        })

# ==========================================
# 統一的CLI管理器
# ==========================================

class DualAgentSystem:
    """雙Agent系統管理器"""
    
    def __init__(self):
        self.ml_agent = MLAgentV2()
        self.trading_agent = TradingOperationsAgentV2()
        self.logger = logging.getLogger("DualAgentSystem")
    
    async def start_system(self):
        """啟動雙Agent系統"""
        self.logger.info("🏗️  啟動雙Agent系統...")
        
        # 並行啟動兩個Agent
        tasks = [
            self.trading_agent.start_operations(),
            self._ml_agent_scheduler()
        ]
        
        await asyncio.gather(*tasks)
    
    async def _ml_agent_scheduler(self):
        """ML Agent調度器 - 定期執行ML Pipeline"""
        while True:
            try:
                self.logger.info("🧠 開始新的ML Pipeline週期...")
                result = await self.ml_agent.execute_ml_pipeline("BTCUSDT")
                
                if result["status"] == "success":
                    self.logger.info("✅ ML Pipeline執行成功")
                else:
                    self.logger.error(f"❌ ML Pipeline執行失敗: {result.get('error')}")
                
                # 等待下一個週期 (例如每4小時執行一次)
                await asyncio.sleep(4 * 3600)
                
            except Exception as e:
                self.logger.error(f"❌ ML Pipeline調度異常: {e}")
                await asyncio.sleep(1800)  # 錯誤後等待30分鐘

# ==========================================
# 測試函數
# ==========================================

async def test_separated_agents():
    """測試分離的Agent架構"""
    print("🧪 測試分離式Agent架構")
    print("=" * 60)
    
    # 創建雙Agent系統
    system = DualAgentSystem()
    
    # 測試ML Agent
    print("🧠 測試ML Agent...")
    ml_result = await system.ml_agent.execute_ml_pipeline("BTCUSDT")
    print(f"ML結果: {ml_result['status']}")
    
    # 測試通信機制
    print("📡 測試Agent間通信...")
    await asyncio.sleep(2)  # 模擬通信延遲
    
    print("✅ 分離式架構測試完成")

if __name__ == "__main__":
    # 設置事件循環策略（macOS 兼容性）
    import sys
    if sys.platform == "darwin":
        asyncio.set_event_loop_policy(asyncio.DefaultEventLoopPolicy())
    
    asyncio.run(test_separated_agents())