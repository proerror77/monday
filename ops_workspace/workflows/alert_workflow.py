#!/usr/bin/env python3
"""
實時告警處理工作流程 - Ops Workspace核心工作流
===========================================

基於PRD第3.2節設計的L2 Ops-Agent告警流水線：
1. 實時監控Rust HFT引擎狀態
2. 處理Redis Channel告警事件
3. 執行風險控制和緊急停止
4. 7x24小時常駐服務

專責：
- 延遲監控 (< 25μs閾值)
- 回撤控制 (< 5%閾值)  
- 系統健康監控
- 緊急停止機制
"""

import asyncio
import redis
import json
import logging
from typing import Dict, Any, List, Optional
from datetime import datetime, timedelta
from enum import Enum
import grpc
from concurrent.futures import ThreadPoolExecutor

from agno import Agent, Workflow, Step
from agno.models import Ollama
from agents.latency_guard import LatencyGuardAgent
from agents.dd_guard import DrawdownGuardAgent
from agents.system_monitor import SystemMonitorAgent

# 導入gRPC客戶端（基於統一的proto定義）
try:
    from protos import hft_control_pb2, hft_control_pb2_grpc
    GRPC_AVAILABLE = True
except ImportError:
    GRPC_AVAILABLE = False
    logging.warning("gRPC客戶端不可用，將使用模擬模式")

logger = logging.getLogger(__name__)

class AlertSeverity(Enum):
    """告警嚴重級別 - 對應PRD第3.2節"""
    LOW = "LOW"
    MEDIUM = "MEDIUM" 
    HIGH = "HIGH"
    CRITICAL = "CRITICAL"

class AlertType(Enum):
    """告警類型"""
    LATENCY_BREACH = "latency_breach"        # PRD 3.2.1 延遲突破
    DRAWDOWN_BREACH = "drawdown_breach"      # PRD 3.2.2 回撤突破
    SYSTEM_ERROR = "system_error"            # 系統錯誤
    MODEL_FAILURE = "model_failure"          # 模型失效
    NETWORK_ISSUE = "network_issue"          # 網路問題
    EMERGENCY_STOP = "emergency_stop"        # 緊急停止

class AlertHandlingWorkflow(Workflow):
    """
    實時告警處理工作流程
    
    PRD對應：第3.2節 L2 Ops-Agent職責
    - 常駐服務：7x24小時運行
    - 快速響應：< 30秒響應時間
    - 零容錯：關鍵告警不可遺漏
    """
    
    def __init__(self, config: Dict[str, Any]):
        super().__init__(
            name="Real_Time_Alert_Handling",
            description="7x24 real-time alert processing and risk control workflow"
        )
        
        self.config = config
        self.redis_client = None
        self.grpc_client = None
        self.running = False
        
        # 初始化連接
        self._initialize_redis()
        self._initialize_grpc()
        
        # 初始化專業化Agent
        self.latency_guard = LatencyGuardAgent()
        self.drawdown_guard = DrawdownGuardAgent()
        self.system_monitor = SystemMonitorAgent()
        
        # 告警處理統計
        self.alert_stats = {
            "total_processed": 0,
            "by_severity": {severity.value: 0 for severity in AlertSeverity},
            "by_type": {alert_type.value: 0 for alert_type in AlertType},
            "response_times": [],
            "false_positives": 0
        }
        
        logger.info("✅ 實時告警處理工作流程初始化完成")
    
    def _initialize_redis(self):
        """初始化Redis連接"""
        try:
            self.redis_client = redis.Redis(
                host=self.config.get("redis_host", "localhost"),
                port=self.config.get("redis_port", 6379),
                db=0,
                decode_responses=True
            )
            self.redis_client.ping()
            logger.info("✅ Redis連接建立成功")
        except Exception as e:
            logger.error(f"❌ Redis連接失敗: {e}")
            raise
    
    def _initialize_grpc(self):
        """初始化gRPC客戶端"""
        if not GRPC_AVAILABLE:
            logger.warning("⚠️ gRPC不可用，告警處理功能受限")
            return
        
        try:
            grpc_host = self.config.get("grpc_host", "localhost")
            grpc_port = self.config.get("grpc_port", 50051)
            
            channel = grpc.insecure_channel(f"{grpc_host}:{grpc_port}")
            self.grpc_client = hft_control_pb2_grpc.HFTControlServiceStub(channel)
            
            logger.info(f"✅ gRPC客戶端連接成功: {grpc_host}:{grpc_port}")
        except Exception as e:
            logger.error(f"❌ gRPC連接失敗: {e}")
            self.grpc_client = None
    
    async def start_alert_monitoring(self):
        """
        啟動告警監控服務
        
        這是L2 Ops-Agent的主要入口點，將持續運行
        """
        logger.info("🚀 啟動7x24小時告警監控服務")
        
        self.running = True
        
        # 創建並發任務
        monitoring_tasks = [
            # Redis Channel監聽
            self._monitor_redis_channels(),
            
            # 系統指標監控
            self._monitor_system_metrics(),
            
            # 週期性健康檢查
            self._periodic_health_check(),
            
            # 告警統計報告
            self._periodic_stats_report()
        ]
        
        try:
            # 並發執行所有監控任務
            await asyncio.gather(*monitoring_tasks)
        except Exception as e:
            logger.error(f"❌ 告警監控服務異常: {e}")
            raise
        finally:
            self.running = False
            logger.info("🛑 告警監控服務已停止")
    
    async def _monitor_redis_channels(self):
        """監控Redis Channel事件"""
        if not self.redis_client:
            logger.error("❌ Redis客戶端不可用，無法監控Redis Channel")
            return
        
        # 訂閱PRD定義的Redis Channel
        channels = [
            "ml.deploy",      # 模型部署通知
            "ml.reject",      # 模型拒絕通知
            "ops.alert",      # 運營告警
            "kill-switch"     # 緊急停止
        ]
        
        pubsub = self.redis_client.pubsub()
        
        try:
            # 訂閱所有channel
            for channel in channels:
                pubsub.subscribe(channel)
            
            logger.info(f"✅ 已訂閱Redis Channels: {channels}")
            
            # 持續監聽
            while self.running:
                try:
                    message = pubsub.get_message(timeout=1.0)
                    
                    if message and message['type'] == 'message':
                        await self._handle_redis_message(
                            message['channel'],
                            message['data']
                        )
                        
                except Exception as e:
                    logger.error(f"❌ Redis消息處理失敗: {e}")
                    await asyncio.sleep(1)
                    
        finally:
            pubsub.close()
            logger.info("🔒 Redis Channel監聽已關閉")
    
    async def _handle_redis_message(self, channel: str, data: str):
        """處理Redis消息"""
        start_time = datetime.now()
        
        try:
            # 解析消息
            message_data = json.loads(data) if data else {}
            
            logger.info(f"📨 收到{channel}消息: {message_data}")
            
            # 根據channel類型處理
            if channel == "ops.alert":
                await self._process_ops_alert(message_data)
            elif channel == "kill-switch":
                await self._process_emergency_stop(message_data)
            elif channel in ["ml.deploy", "ml.reject"]:
                await self._process_ml_notification(channel, message_data)
            
            # 記錄響應時間
            response_time = (datetime.now() - start_time).total_seconds()
            self.alert_stats["response_times"].append(response_time)
            self.alert_stats["total_processed"] += 1
            
            if response_time > 30:  # 超過30秒響應
                logger.warning(f"⚠️ 告警響應時間過長: {response_time:.2f}s")
            
        except Exception as e:
            logger.error(f"❌ Redis消息處理異常: {e}")
    
    async def _process_ops_alert(self, alert_data: Dict[str, Any]):
        """處理運營告警"""
        alert_type = alert_data.get("type", "unknown")
        severity = alert_data.get("severity", "MEDIUM")
        
        logger.info(f"🚨 處理運營告警: {alert_type} - {severity}")
        
        # 更新統計
        if severity in [s.value for s in AlertSeverity]:
            self.alert_stats["by_severity"][severity] += 1
        
        # 根據告警類型分派給專業Agent
        if alert_type == AlertType.LATENCY_BREACH.value:
            await self._handle_latency_alert(alert_data)
        elif alert_type == AlertType.DRAWDOWN_BREACH.value:
            await self._handle_drawdown_alert(alert_data)
        elif alert_type == AlertType.SYSTEM_ERROR.value:
            await self._handle_system_error(alert_data)
        else:
            await self._handle_generic_alert(alert_data)
    
    async def _handle_latency_alert(self, alert_data: Dict[str, Any]):
        """處理延遲告警 - PRD 3.2.1"""
        latency_us = alert_data.get("latency_us", 0)
        threshold_us = alert_data.get("threshold_us", 25)
        
        logger.warning(f"⚡ 延遲告警: {latency_us}μs > {threshold_us}μs")
        
        # 使用LatencyGuard Agent處理
        response = await self.latency_guard.handle_latency_breach(alert_data)
        
        if response.get("action") == "switch_conservative_mode":
            await self._notify_rust_engine_conservative_mode()
        elif response.get("action") == "emergency_stop":
            await self._trigger_emergency_stop("高延遲觸發緊急停止")
    
    async def _handle_drawdown_alert(self, alert_data: Dict[str, Any]):
        """處理回撤告警 - PRD 3.2.2"""
        drawdown_percent = alert_data.get("drawdown_percent", 0)
        threshold_percent = alert_data.get("threshold_percent", 5.0)
        
        logger.critical(f"📉 回撤告警: {drawdown_percent}% > {threshold_percent}%")
        
        # 使用DrawdownGuard Agent處理
        response = await self.drawdown_guard.handle_drawdown_breach(alert_data)
        
        if response.get("action") == "emergency_stop":
            await self._trigger_emergency_stop(f"最大回撤{drawdown_percent}%觸發緊急停止")
        elif response.get("action") == "reduce_position":
            await self._notify_position_reduction(response.get("reduction_ratio", 0.5))
    
    async def _handle_system_error(self, alert_data: Dict[str, Any]):
        """處理系統錯誤告警"""
        error_type = alert_data.get("error_type", "unknown")
        error_message = alert_data.get("message", "")
        
        logger.error(f"🔧 系統錯誤告警: {error_type} - {error_message}")
        
        # 使用SystemMonitor Agent處理
        response = await self.system_monitor.handle_system_error(alert_data)
        
        if response.get("requires_restart"):
            await self._notify_system_restart_required(alert_data)
    
    async def _handle_generic_alert(self, alert_data: Dict[str, Any]):
        """處理通用告警"""
        logger.info(f"📋 處理通用告警: {alert_data}")
        
        # 基本日誌記錄和統計更新
        alert_type = alert_data.get("type", "unknown")
        if alert_type in [t.value for t in AlertType]:
            self.alert_stats["by_type"][alert_type] += 1
    
    async def _process_emergency_stop(self, stop_data: Dict[str, Any]):
        """處理緊急停止請求"""
        reason = stop_data.get("reason", "未知原因")
        force_immediate = stop_data.get("force_immediate", True)
        
        logger.critical(f"🛑 收到緊急停止請求: {reason}")
        
        try:
            if self.grpc_client:
                # 通過gRPC發送緊急停止命令
                request = hft_control_pb2.EmergencyStopRequest(
                    reason=reason,
                    force_immediate=force_immediate
                )
                
                response = self.grpc_client.EmergencyStop(request)
                
                if response.success:
                    logger.info(f"✅ 緊急停止執行成功，停止了{response.stopped_tasks}個任務")
                else:
                    logger.error(f"❌ 緊急停止執行失敗: {response.error_message}")
            else:
                logger.warning("⚠️ gRPC客戶端不可用，無法執行緊急停止")
                
        except Exception as e:
            logger.error(f"❌ 緊急停止執行異常: {e}")
    
    async def _process_ml_notification(self, channel: str, notification_data: Dict[str, Any]):
        """處理ML相關通知"""
        if channel == "ml.deploy":
            # 新模型部署通知
            symbol = notification_data.get("symbol")
            model_path = notification_data.get("model_path")
            
            logger.info(f"🤖 收到模型部署通知: {symbol} - {model_path}")
            
            # 更新監控配置以適應新模型
            await self._update_monitoring_for_new_model(symbol, notification_data)
            
        elif channel == "ml.reject":
            # 模型拒絕通知
            symbol = notification_data.get("symbol")
            reason = notification_data.get("reason", "未知原因")
            
            logger.warning(f"❌ 收到模型拒絕通知: {symbol} - {reason}")
    
    async def _monitor_system_metrics(self):
        """監控系統指標"""
        while self.running:
            try:
                if self.grpc_client:
                    # 獲取系統狀態
                    request = hft_control_pb2.GetSystemStatusRequest(
                        include_detailed_metrics=True
                    )
                    
                    response = self.grpc_client.GetSystemStatus(request)
                    
                    # 檢查關鍵指標
                    await self._check_critical_metrics(response.metrics)
                
                # 每5秒檢查一次
                await asyncio.sleep(5)
                
            except Exception as e:
                logger.error(f"❌ 系統指標監控異常: {e}")
                await asyncio.sleep(10)  # 異常時延長間隔
    
    async def _check_critical_metrics(self, metrics):
        """檢查關鍵指標"""
        if not metrics:
            return
        
        # 檢查執行延遲 - PRD 3.2.1
        if metrics.execution_latency_us > 25.0:
            await self._trigger_latency_alert(metrics.execution_latency_us)
        
        # 檢查錯誤率
        if metrics.error_rate > 0.05:  # 5%錯誤率閾值
            await self._trigger_error_rate_alert(metrics.error_rate)
        
        # 檢查吞吐量異常
        if metrics.throughput_per_second < 100:  # 最低吞吐量閾值
            await self._trigger_throughput_alert(metrics.throughput_per_second)
    
    async def _periodic_health_check(self):
        """週期性健康檢查"""
        while self.running:
            try:
                # 每分鐘執行一次全面健康檢查
                health_report = await self.system_monitor.perform_health_check()
                
                if not health_report.get("healthy", True):
                    logger.warning("⚠️ 系統健康檢查發現問題")
                    await self._handle_health_issues(health_report)
                
                await asyncio.sleep(60)  # 60秒間隔
                
            except Exception as e:
                logger.error(f"❌ 健康檢查異常: {e}")
                await asyncio.sleep(60)
    
    async def _periodic_stats_report(self):
        """週期性統計報告"""
        while self.running:
            try:
                # 每小時生成統計報告
                await asyncio.sleep(3600)  # 1小時
                
                stats_report = self._generate_stats_report()
                logger.info(f"📊 告警處理統計報告:\n{stats_report}")
                
            except Exception as e:
                logger.error(f"❌ 統計報告生成異常: {e}")
    
    def _generate_stats_report(self) -> str:
        """生成統計報告"""
        stats = self.alert_stats
        
        avg_response_time = (
            sum(stats["response_times"]) / len(stats["response_times"])
            if stats["response_times"] else 0
        )
        
        report = f"""
告警處理統計 (過去1小時):
  總處理數量: {stats['total_processed']}
  平均響應時間: {avg_response_time:.2f}s
  
按嚴重程度:
  CRITICAL: {stats['by_severity']['CRITICAL']}
  HIGH: {stats['by_severity']['HIGH']}
  MEDIUM: {stats['by_severity']['MEDIUM']}
  LOW: {stats['by_severity']['LOW']}
  
按類型:
  延遲告警: {stats['by_type']['latency_breach']}
  回撤告警: {stats['by_type']['drawdown_breach']}
  系統錯誤: {stats['by_type']['system_error']}
  
系統狀態: {'✅ 正常' if self.running else '❌ 停止'}
        """
        
        return report.strip()
    
    async def _trigger_emergency_stop(self, reason: str):
        """觸發緊急停止"""
        logger.critical(f"🚨 觸發緊急停止: {reason}")
        
        emergency_data = {
            "reason": reason,
            "timestamp": datetime.now().isoformat(),
            "triggered_by": "ops_agent"
        }
        
        # 發送到kill-switch channel
        if self.redis_client:
            self.redis_client.publish("kill-switch", json.dumps(emergency_data))
        
        # 直接調用緊急停止處理
        await self._process_emergency_stop(emergency_data)
    
    def stop_monitoring(self):
        """停止監控服務"""
        logger.info("🛑 正在停止告警監控服務...")
        self.running = False

# ==========================================
# 工廠函數和工具
# ==========================================

async def create_alert_workflow(config: Dict[str, Any]) -> AlertHandlingWorkflow:
    """創建告警處理工作流程"""
    return AlertHandlingWorkflow(config)

async def start_ops_monitoring_service(config: Dict[str, Any]):
    """啟動Ops監控服務 - L2 Agent主入口點"""
    logger.info("🚀 啟動L2 Ops-Agent監控服務")
    
    # 創建告警工作流程
    workflow = await create_alert_workflow(config)
    
    try:
        # 啟動監控
        await workflow.start_alert_monitoring()
    except KeyboardInterrupt:
        logger.info("👋 收到中斷信號，正在優雅關閉...")
        workflow.stop_monitoring()
    except Exception as e:
        logger.error(f"❌ 監控服務異常終止: {e}")
        raise

# ==========================================
# CLI入口點
# ==========================================

async def main():
    """Ops Workspace告警工作流程入口點"""
    import argparse
    
    parser = argparse.ArgumentParser(description="HFT Ops Alert Monitoring Service")
    parser.add_argument("--redis-host", default="localhost", help="Redis host")
    parser.add_argument("--redis-port", type=int, default=6379, help="Redis port")
    parser.add_argument("--grpc-host", default="localhost", help="gRPC host")
    parser.add_argument("--grpc-port", type=int, default=50051, help="gRPC port")
    parser.add_argument("--config", help="Configuration file")
    
    args = parser.parse_args()
    
    config = {
        "redis_host": args.redis_host,
        "redis_port": args.redis_port,
        "grpc_host": args.grpc_host,
        "grpc_port": args.grpc_port
    }
    
    # 啟動L2 Ops-Agent服務
    await start_ops_monitoring_service(config)

if __name__ == "__main__":
    asyncio.run(main())