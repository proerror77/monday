import asyncio
import json
import redis
from typing import Dict, Any
from agno.workflow import Workflow, WorkflowStep
from agents.latency_guard import get_latency_guard
from agents.dd_guard import get_dd_guard
from agents.alert_manager import get_alert_manager


class AlertWorkflow:
    """HFT 系統告警處理工作流程"""
    
    def __init__(self):
        self.redis_client = redis.Redis(host='localhost', port=6379, decode_responses=True)
        self.alert_manager = get_alert_manager()
        self.latency_guard = get_latency_guard()
        self.dd_guard = get_dd_guard()
        
    async def listen_for_alerts(self):
        """監聽 Redis ops.alert 頻道中的告警事件"""
        pubsub = self.redis_client.pubsub()
        pubsub.subscribe('ops.alert')
        
        print("🔊 Alert Workflow 已啟動，正在監聽 ops.alert 頻道...")
        
        try:
            for message in pubsub.listen():
                if message['type'] == 'message':
                    try:
                        alert_data = json.loads(message['data'])
                        await self.process_alert(alert_data)
                    except json.JSONDecodeError as e:
                        print(f"❌ 告警數據解析失敗: {e}")
                    except Exception as e:
                        print(f"❌ 告警處理異常: {e}")
        except KeyboardInterrupt:
            print("⏹️ Alert Workflow 停止")
        finally:
            pubsub.close()
    
    async def process_alert(self, alert_data: Dict[str, Any]):
        """處理告警事件並分派給相應的代理"""
        alert_type = alert_data.get('type')
        alert_value = alert_data.get('value')
        timestamp = alert_data.get('ts')
        
        print(f"🚨 收到告警: {alert_type} = {alert_value} @ {timestamp}")
        
        # 根據告警類型分派處理
        if alert_type == 'latency':
            await self.handle_latency_alert(alert_data)
        elif alert_type == 'dd':
            await self.handle_drawdown_alert(alert_data)
        elif alert_type == 'infra':
            await self.handle_infrastructure_alert(alert_data)
        else:
            print(f"⚠️ 未知告警類型: {alert_type}")
    
    async def handle_latency_alert(self, alert_data: Dict[str, Any]):
        """處理延遲告警"""
        value = alert_data.get('value')
        p99 = alert_data.get('p99', 25.0)  # 預設 p99 閾值 25µs
        
        prompt = f"""
        系統延遲告警觸發：
        - 當前延遲: {value}µs
        - P99 閾值: {p99}µs
        - 超標倍數: {value/p99:.2f}x
        
        請分析延遲情況並執行相應的響應措施。
        """
        
        response = await self.latency_guard.arun(prompt)
        print(f"🛡️ Latency Guard 響應: {response.content}")
    
    async def handle_drawdown_alert(self, alert_data: Dict[str, Any]):
        """處理回撤告警"""
        dd_value = alert_data.get('value')
        
        prompt = f"""
        資金回撤告警觸發：
        - 當前回撤: {dd_value}%
        
        請評估風險級別並執行相應的保護措施。
        根據回撤水平決定是否需要觸發 kill-switch。
        """
        
        response = await self.dd_guard.arun(prompt)
        print(f"🛡️ DD Guard 響應: {response.content}")
        
        # 如果回撤達到危險級別，觸發 kill-switch
        if dd_value >= 5.0:
            kill_switch_message = {
                "cause": f"Drawdown exceeded {dd_value}%",
                "ts": alert_data.get('ts')
            }
            self.redis_client.publish('kill-switch', json.dumps(kill_switch_message))
            print(f"🛑 Kill-switch 已觸發: DD = {dd_value}%")
    
    async def handle_infrastructure_alert(self, alert_data: Dict[str, Any]):
        """處理基礎設施告警"""
        prompt = f"""
        基礎設施告警：
        {json.dumps(alert_data, indent=2)}
        
        請檢查系統基礎設施狀態並執行修復措施。
        """
        
        response = await self.alert_manager.arun(prompt)
        print(f"🔧 Alert Manager 響應: {response.content}")


# 工作流程入口點
async def start_alert_workflow():
    """啟動告警處理工作流程"""
    workflow = AlertWorkflow()
    await workflow.listen_for_alerts()


if __name__ == "__main__":
    # 直接運行告警工作流程
    asyncio.run(start_alert_workflow())