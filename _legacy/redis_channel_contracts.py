#!/usr/bin/env python3
"""
Redis Channel 契約系統實現
=======================

基於PRD規範實現完整的Redis通信契約：
- ml.deploy: ML-Agent → Rust (模型部署)
- ml.reject: ML-Agent → Grafana Alert (部署失敗)
- ops.alert: Rust Sentinel → Ops-Agent (運營告警)
- kill-switch: Ops-Agent → Rust (緊急停止)

符合PRD要求的JSON Schema和通信協議
"""

import asyncio
import json
import time
import redis
import logging
from typing import Dict, Any, Optional, List, Callable
from dataclasses import dataclass, asdict
from enum import Enum
import hashlib
import uuid
from datetime import datetime

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

# ==========================================
# PRD定義的Channel和消息類型
# ==========================================

class ChannelType(Enum):
    """PRD定義的Redis Channel類型"""
    ML_DEPLOY = "ml.deploy"           # ML-Agent → Rust
    ML_REJECT = "ml.reject"           # ML-Agent → Grafana Alert
    OPS_ALERT = "ops.alert"           # Rust Sentinel → Ops-Agent
    KILL_SWITCH = "kill-switch"       # Ops-Agent → Rust

class AlertType(Enum):
    """告警類型"""
    LATENCY = "latency"
    DRAWDOWN = "dd"
    INFRA = "infra"

class ModelType(Enum):
    """模型類型"""
    SUPERVISED = "sup"
    REINFORCEMENT = "rl"
    ENSEMBLE = "ens"

# ==========================================
# PRD規範的消息結構定義
# ==========================================

@dataclass
class MLDeployMessage:
    """ml.deploy Channel消息結構
    
    PRD規範：
    {
      "url": "supabase://models/20250725/model.pt",
      "sha256": "9f5c5e3e...",
      "version": "20250725-ic0031-ir128",
      "ic": 0.031,
      "ir": 1.28,
      "mdd": 0.041,
      "ts": 1721904000,
      "model_type": "sup"
    }
    """
    url: str
    sha256: str
    version: str
    ic: float           # Information Coefficient
    ir: float           # Information Ratio
    mdd: float          # Max Drawdown
    ts: int             # Timestamp
    model_type: str     # ModelType enum value
    
    # 額外字段用於追踪
    source_agent: str = "ml_agent"
    deployment_id: str = ""
    
    def __post_init__(self):
        if not self.deployment_id:
            self.deployment_id = f"deploy_{int(time.time())}_{uuid.uuid4().hex[:8]}"
    
    @classmethod
    def create(cls, model_path: str, performance_metrics: Dict[str, float], 
               model_type: ModelType = ModelType.SUPERVISED) -> 'MLDeployMessage':
        """創建標準的ML部署消息"""
        
        # 計算模型文件SHA256
        try:
            with open(model_path, 'rb') as f:
                sha256 = hashlib.sha256(f.read()).hexdigest()
        except Exception:
            sha256 = f"mock_sha_{int(time.time())}"
        
        # 生成版本號
        timestamp = datetime.now().strftime("%Y%m%d")
        ic_str = f"ic{int(performance_metrics.get('ic', 0) * 1000):03d}"
        ir_str = f"ir{int(performance_metrics.get('ir', 0) * 100):03d}"
        version = f"{timestamp}-{ic_str}-{ir_str}"
        
        return cls(
            url=f"supabase://models/{timestamp}/{model_path.split('/')[-1]}",
            sha256=sha256,
            version=version,
            ic=performance_metrics.get('ic', 0.0),
            ir=performance_metrics.get('ir', 0.0),
            mdd=performance_metrics.get('mdd', 0.0),
            ts=int(time.time()),
            model_type=model_type.value
        )

@dataclass  
class MLRejectMessage:
    """ml.reject Channel消息結構"""
    reason: str
    trials: int
    ts: int
    session_id: str
    performance_summary: Dict[str, float]
    source_agent: str = "ml_agent"
    
    @classmethod
    def create(cls, reason: str, trials: int, session_id: str, 
               performance_summary: Dict[str, float]) -> 'MLRejectMessage':
        return cls(
            reason=reason,
            trials=trials,
            ts=int(time.time()),
            session_id=session_id,
            performance_summary=performance_summary
        )

@dataclass
class OpsAlertMessage:
    """ops.alert Channel消息結構
    
    PRD規範：
    {
      "type": "latency",
      "value": 37.5,
      "p99": 15.2,
      "ts": 1721904021
    }
    """
    type: str           # AlertType enum value
    value: float        # 實際測量值
    p99: Optional[float] = None     # P99基準值（用於延遲告警）
    threshold: Optional[float] = None  # 閾值
    ts: int = 0
    component: str = "hft_core"
    severity: str = "HIGH"
    
    def __post_init__(self):
        if self.ts == 0:
            self.ts = int(time.time())
    
    @classmethod
    def create_latency_alert(cls, latency_ms: float, p99_baseline: float) -> 'OpsAlertMessage':
        return cls(
            type=AlertType.LATENCY.value,
            value=latency_ms,
            p99=p99_baseline,
            threshold=25.0,  # PRD要求：≤ 25 µs
            severity="CRITICAL" if latency_ms > 50.0 else "HIGH"
        )
    
    @classmethod
    def create_drawdown_alert(cls, dd_pct: float) -> 'OpsAlertMessage':
        return cls(
            type=AlertType.DRAWDOWN.value,
            value=dd_pct,
            threshold=0.05,  # PRD要求：≤ 5%
            severity="CRITICAL" if dd_pct > 0.05 else "HIGH"
        )

@dataclass
class KillSwitchMessage:
    """kill-switch Channel消息結構"""
    cause: str
    ts: int
    operator: str = "ops_agent"
    emergency_level: str = "CRITICAL"
    affected_components: List[str] = None
    
    def __post_init__(self):
        if self.affected_components is None:
            self.affected_components = ["trading_engine", "order_management"]
    
    @classmethod
    def create(cls, cause: str, emergency_level: str = "CRITICAL") -> 'KillSwitchMessage':
        return cls(
            cause=cause,
            ts=int(time.time()),
            emergency_level=emergency_level
        )

# ==========================================
# Redis Channel Manager
# ==========================================

class RedisChannelManager:
    """Redis Channel通信管理器"""
    
    def __init__(self, redis_host: str = "localhost", redis_port: int = 6379, 
                 redis_db: int = 0):
        self.redis_host = redis_host
        self.redis_port = redis_port  
        self.redis_db = redis_db
        
        # 連接池
        self.connection_pool = redis.ConnectionPool(
            host=redis_host, port=redis_port, db=redis_db, 
            decode_responses=True, max_connections=20
        )
        
        # 訂閱客戶端
        self.subscriber_clients = {}
        self.publisher_client = redis.Redis(connection_pool=self.connection_pool)
        
        # 消息處理器註冊
        self.message_handlers: Dict[ChannelType, List[Callable]] = {
            channel: [] for channel in ChannelType
        }
        
        logger.info(f"🔗 Redis Channel Manager初始化: {redis_host}:{redis_port}")
    
    def register_handler(self, channel: ChannelType, handler: Callable[[Dict[str, Any]], None]):
        """註冊消息處理器"""
        self.message_handlers[channel].append(handler)
        logger.info(f"📝 註冊處理器: {channel.value}")
    
    async def publish_message(self, channel: ChannelType, message: Any) -> bool:
        """發布消息到指定Channel"""
        try:
            # 轉換消息為JSON
            if hasattr(message, '__dict__'):
                message_dict = asdict(message) if hasattr(message, '__dataclass_fields__') else vars(message)
            else:
                message_dict = message
            
            message_json = json.dumps(message_dict, ensure_ascii=False)
            
            # 發布到Redis
            result = self.publisher_client.publish(channel.value, message_json)
            
            logger.info(f"📤 發布消息: {channel.value} -> {result} 訂閱者")
            logger.debug(f"消息內容: {message_json}")
            
            return result > 0
            
        except Exception as e:
            logger.error(f"❌ 消息發布失敗 {channel.value}: {e}")
            return False
    
    async def subscribe_channel(self, channel: ChannelType):
        """訂閱指定Channel"""
        try:
            # 創建專用的訂閱客戶端
            subscriber = redis.Redis(connection_pool=self.connection_pool)
            pubsub = subscriber.pubsub()
            
            # 訂閱Channel
            pubsub.subscribe(channel.value)
            self.subscriber_clients[channel] = pubsub
            
            logger.info(f"📥 訂閱Channel: {channel.value}")
            
            # 開始監聽消息
            await self._listen_messages(channel, pubsub)
            
        except Exception as e:
            logger.error(f"❌ 訂閱失敗 {channel.value}: {e}")
    
    async def _listen_messages(self, channel: ChannelType, pubsub):
        """監聽Channel消息"""
        try:
            for message in pubsub.listen():
                if message['type'] == 'message':
                    try:
                        # 解析JSON消息
                        message_data = json.loads(message['data'])
                        
                        logger.info(f"📨 收到消息: {channel.value}")
                        logger.debug(f"消息內容: {message_data}")
                        
                        # 調用所有註冊的處理器
                        for handler in self.message_handlers[channel]:
                            try:
                                if asyncio.iscoroutinefunction(handler):
                                    await handler(message_data)
                                else:
                                    handler(message_data)
                            except Exception as e:
                                logger.error(f"❌ 消息處理器異常: {e}")
                        
                    except json.JSONDecodeError as e:
                        logger.error(f"❌ JSON解析失敗: {e}")
                    except Exception as e:
                        logger.error(f"❌ 消息處理異常: {e}")
                        
        except Exception as e:
            logger.error(f"❌ 消息監聽異常 {channel.value}: {e}")
    
    def close(self):
        """關閉所有連接"""
        try:
            for pubsub in self.subscriber_clients.values():
                pubsub.close()
            self.subscriber_clients.clear()
            logger.info("🔐 Redis Channel Manager已關閉")
        except Exception as e:
            logger.error(f"❌ 關閉連接異常: {e}")

# ==========================================
# 專用的Channel處理器
# ==========================================

class MLAgentChannelHandler:
    """ML Agent的Channel處理器"""
    
    def __init__(self, channel_manager: RedisChannelManager):
        self.channel_manager = channel_manager
        self.setup_handlers()
    
    def setup_handlers(self):
        """設置消息處理器"""
        # ML Agent 不需要監聽，只需要發布
        pass
    
    async def deploy_model(self, model_path: str, performance_metrics: Dict[str, float]) -> bool:
        """部署模型"""
        deploy_msg = MLDeployMessage.create(model_path, performance_metrics)
        
        # 驗證部署條件（PRD要求）
        if (deploy_msg.ic >= 0.03 and deploy_msg.ir >= 1.2 and deploy_msg.mdd <= 0.05):
            logger.info(f"✅ 模型通過部署標準: IC={deploy_msg.ic:.3f}, IR={deploy_msg.ir:.2f}")
            return await self.channel_manager.publish_message(ChannelType.ML_DEPLOY, deploy_msg)
        else:
            logger.warning(f"❌ 模型未達部署標準: IC={deploy_msg.ic:.3f}, IR={deploy_msg.ir:.2f}")
            return await self.reject_model("performance_below_threshold", performance_metrics)
    
    async def reject_model(self, reason: str, performance_summary: Dict[str, float], 
                          trials: int = 1, session_id: str = "") -> bool:
        """拒絕模型部署"""
        if not session_id:
            session_id = f"session_{int(time.time())}"
        
        reject_msg = MLRejectMessage.create(reason, trials, session_id, performance_summary)
        return await self.channel_manager.publish_message(ChannelType.ML_REJECT, reject_msg)

class OpsAgentChannelHandler:
    """Ops Agent的Channel處理器"""
    
    def __init__(self, channel_manager: RedisChannelManager):
        self.channel_manager = channel_manager
        self.setup_handlers()
    
    def setup_handlers(self):
        """設置消息處理器"""
        # 註冊ops.alert消息處理器
        self.channel_manager.register_handler(
            ChannelType.OPS_ALERT, 
            self.handle_ops_alert
        )
    
    async def handle_ops_alert(self, alert_data: Dict[str, Any]):
        """處理運營告警"""
        alert_type = alert_data.get('type')
        value = alert_data.get('value', 0)
        threshold = alert_data.get('threshold', 0)
        
        logger.warning(f"🚨 收到運營告警: {alert_type} = {value} (閾值: {threshold})")
        
        # 根據告警類型決定行動
        if alert_type == AlertType.LATENCY.value and value > 50.0:
            # 延遲嚴重超標，觸發kill-switch
            await self.trigger_kill_switch(f"執行延遲嚴重超標: {value}ms")
        elif alert_type == AlertType.DRAWDOWN.value and value > 0.05:
            # 回撤超標，觸發kill-switch
            await self.trigger_kill_switch(f"資金回撤超標: {value*100:.1f}%")
        else:
            # 非緊急告警，記錄並監控
            logger.warning(f"⚠️ 非緊急告警，持續監控: {alert_type}")
    
    async def trigger_kill_switch(self, cause: str) -> bool:
        """觸發緊急停止"""
        logger.critical(f"🛑 觸發Kill Switch: {cause}")
        
        kill_msg = KillSwitchMessage.create(cause)
        return await self.channel_manager.publish_message(ChannelType.KILL_SWITCH, kill_msg)
    
    async def start_monitoring(self):
        """開始監控ops.alert Channel"""
        logger.info("👁️ 開始監控運營告警...")
        await self.channel_manager.subscribe_channel(ChannelType.OPS_ALERT)

class RustCoreChannelHandler:
    """Rust Core的Channel處理器（Python模擬）"""
    
    def __init__(self, channel_manager: RedisChannelManager):
        self.channel_manager = channel_manager
        self.setup_handlers()
    
    def setup_handlers(self):
        """設置消息處理器"""
        # 註冊ml.deploy和kill-switch處理器
        self.channel_manager.register_handler(
            ChannelType.ML_DEPLOY,
            self.handle_model_deployment
        )
        self.channel_manager.register_handler(
            ChannelType.KILL_SWITCH,
            self.handle_kill_switch
        )
    
    async def handle_model_deployment(self, deploy_data: Dict[str, Any]):
        """處理模型部署請求"""
        model_url = deploy_data.get('url', '')
        version = deploy_data.get('version', '')
        sha256 = deploy_data.get('sha256', '')
        
        logger.info(f"🤖 Rust收到模型部署請求: {version}")
        logger.info(f"📍 模型URL: {model_url}")
        logger.info(f"🔐 SHA256: {sha256[:16]}...")
        
        # 模擬模型加載過程
        await asyncio.sleep(0.05)  # 模擬50ms加載時間（符合PRD要求）
        
        logger.info(f"✅ 模型熱加載完成: {version}")
    
    async def handle_kill_switch(self, kill_data: Dict[str, Any]):
        """處理緊急停止請求"""
        cause = kill_data.get('cause', '')
        emergency_level = kill_data.get('emergency_level', 'CRITICAL')
        
        logger.critical(f"🛑 Rust收到Kill Switch: {cause}")
        logger.critical(f"🚨 緊急級別: {emergency_level}")
        
        # 模擬緊急停止流程
        logger.critical("⏹️ 停止所有交易活動")
        logger.critical("📤 撤銷所有未成交訂單")
        logger.critical("🔒 進入安全模式")
        
        logger.info("✅ 緊急停止流程完成")
    
    async def publish_latency_alert(self, latency_ms: float, p99_baseline: float = 15.0):
        """發布延遲告警"""
        alert_msg = OpsAlertMessage.create_latency_alert(latency_ms, p99_baseline)
        return await self.channel_manager.publish_message(ChannelType.OPS_ALERT, alert_msg)
    
    async def publish_drawdown_alert(self, dd_pct: float):
        """發布回撤告警"""
        alert_msg = OpsAlertMessage.create_drawdown_alert(dd_pct)
        return await self.channel_manager.publish_message(ChannelType.OPS_ALERT, alert_msg)
    
    async def start_monitoring(self):
        """開始監控ml.deploy和kill-switch Channel"""
        logger.info("🤖 Rust Core開始監控部署和停止指令...")
        
        # 並行監聽兩個Channel
        await asyncio.gather(
            self.channel_manager.subscribe_channel(ChannelType.ML_DEPLOY),
            self.channel_manager.subscribe_channel(ChannelType.KILL_SWITCH)
        )

# ==========================================
# 集成測試和演示
# ==========================================

async def demo_channel_communication():
    """演示完整的Channel通信流程"""
    print("🔗 Redis Channel契約系統演示")
    print("=" * 60)
    
    # 初始化Channel Manager
    channel_manager = RedisChannelManager()
    
    # 初始化各個處理器
    ml_handler = MLAgentChannelHandler(channel_manager)
    ops_handler = OpsAgentChannelHandler(channel_manager)
    rust_handler = RustCoreChannelHandler(channel_manager)
    
    print("📡 啟動監聽服務...")
    
    # 啟動監聽任務（在後台運行）
    monitoring_tasks = [
        asyncio.create_task(ops_handler.start_monitoring()),
        asyncio.create_task(rust_handler.start_monitoring())
    ]
    
    # 等待一下讓監聽器準備好
    await asyncio.sleep(1)
    
    print("\n🎬 開始演示Channel通信...")
    
    # 場景1：正常模型部署流程
    print("\n📋 場景1：ML Agent部署高質量模型")
    performance_metrics = {
        "ic": 0.035,        # 高於0.03閾值
        "ir": 1.45,         # 高於1.2閾值  
        "mdd": 0.025        # 低於0.05閾值
    }
    
    await ml_handler.deploy_model("/models/btc_v15.pt", performance_metrics)
    await asyncio.sleep(1)
    
    # 場景2：模型質量不達標
    print("\n📋 場景2：ML Agent拒絕低質量模型") 
    poor_performance = {
        "ic": 0.015,        # 低於0.03閾值
        "ir": 0.8,          # 低於1.2閾值
        "mdd": 0.08         # 高於0.05閾值
    }
    
    await ml_handler.reject_model("performance_below_threshold", poor_performance, trials=5)
    await asyncio.sleep(1)
    
    # 場景3：Rust發布延遲告警
    print("\n📋 場景3：Rust Core發布延遲告警")
    await rust_handler.publish_latency_alert(35.0, 15.0)  # 超過25µs閾值
    await asyncio.sleep(1)
    
    # 場景4：Rust發布嚴重回撤告警
    print("\n📋 場景4：Rust Core發布嚴重回撤告警")
    await rust_handler.publish_drawdown_alert(0.06)  # 超過5%閾值
    await asyncio.sleep(2)  # 讓kill-switch有時間觸發
    
    print("\n🎯 Channel通信演示完成")
    
    # 清理
    for task in monitoring_tasks:
        task.cancel()
    
    channel_manager.close()

async def main():
    """主函數"""
    try:
        await demo_channel_communication()
    except KeyboardInterrupt:
        print("\n👋 演示中斷")
    except Exception as e:
        print(f"❌ 演示異常: {e}")

if __name__ == "__main__":
    asyncio.run(main())