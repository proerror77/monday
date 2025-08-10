#!/usr/bin/env python3
"""
簡化的 Ops 監控系統
==================

監控 HFT 系統狀態和數據流
"""

import asyncio
import time
import json
import redis
import logging
from datetime import datetime, timezone

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class SimpleOpsMonitor:
    def __init__(self):
        self.redis_client = redis.Redis(host='localhost', port=6379, decode_responses=True)
        self.pubsub = self.redis_client.pubsub()
        self.start_time = time.time()
        self.message_counts = {}
        self.alerts_triggered = 0
        
    async def setup_monitoring(self):
        """設置監控頻道"""
        logger.info("🔧 設置監控頻道...")
        
        # 訂閱關鍵頻道
        channels = [
            'market_data',
            'hft.system_stats', 
            'hft.system_events',
            'ops.alert'
        ]
        
        for channel in channels:
            self.pubsub.subscribe(channel)
            self.message_counts[channel] = 0
        
        logger.info(f"✅ 已訂閱 {len(channels)} 個監控頻道")

    async def monitor_system_health(self):
        """監控系統健康狀態"""
        logger.info("🛡️ 開始系統健康監控...")
        
        last_report_time = time.time()
        
        while True:
            try:
                # 檢查 Redis 連接
                self.redis_client.ping()
                
                # 檢查數據流
                current_time = time.time()
                if current_time - last_report_time >= 30:  # 每 30 秒報告
                    await self.generate_monitoring_report()
                    last_report_time = current_time
                
                # 檢查是否到達 10 分鐘
                elapsed = current_time - self.start_time
                if elapsed > 600:  # 10 minutes
                    logger.info("⏰ 監控完成，運行了 10 分鐘")
                    break
                
                await asyncio.sleep(5)  # 每 5 秒檢查一次
                
            except Exception as e:
                logger.error(f"❌ 監控異常: {e}")
                self.alerts_triggered += 1
                await asyncio.sleep(10)

    async def listen_to_messages(self):
        """監聽消息"""
        logger.info("👂 開始監聽消息...")
        
        while True:
            try:
                message = self.pubsub.get_message(timeout=1.0)
                if message and message['type'] == 'message':
                    channel = message['channel']
                    data = message['data']
                    
                    # 更新消息計數
                    if channel in self.message_counts:
                        self.message_counts[channel] += 1
                    
                    # 處理特定類型的消息
                    if channel == 'hft.system_stats':
                        await self.process_system_stats(data)
                    elif channel == 'hft.system_events':
                        await self.process_system_events(data)
                    elif channel == 'market_data':
                        # 市場數據監控
                        pass
                
                # 檢查運行時間
                elapsed = time.time() - self.start_time
                if elapsed > 600:  # 10 minutes
                    break
                    
            except Exception as e:
                logger.warning(f"⚠️ 消息監聽警告: {e}")
                await asyncio.sleep(1)

    async def process_system_stats(self, data):
        """處理系統統計信息"""
        try:
            stats = json.loads(data)
            if stats.get('status') == 'running':
                rate = stats.get('rate_per_second', 0)
                if rate < 1.0:  # 如果數據速率太低
                    logger.warning(f"⚠️ 數據速率警告: {rate:.2f} msg/s")
                    self.alerts_triggered += 1
        except Exception as e:
            logger.warning(f"⚠️ 統計處理警告: {e}")

    async def process_system_events(self, data):
        """處理系統事件"""
        try:
            event = json.loads(data)
            status = event.get('status', 'unknown')
            logger.info(f"📢 系統事件: {status}")
        except Exception as e:
            logger.warning(f"⚠️ 事件處理警告: {e}")

    async def generate_monitoring_report(self):
        """生成監控報告"""
        elapsed = time.time() - self.start_time
        
        logger.info("📊 === 系統監控報告 ===")
        logger.info(f"⏱️ 運行時間: {elapsed:.1f} 秒")
        logger.info(f"🚨 觸發告警: {self.alerts_triggered} 次")
        
        total_messages = sum(self.message_counts.values())
        logger.info(f"📨 總消息數: {total_messages}")
        
        for channel, count in self.message_counts.items():
            if count > 0:
                rate = count / elapsed
                logger.info(f"  📡 {channel}: {count} 條 ({rate:.2f}/s)")
        
        # 發布監控報告到 Redis
        report = {
            'timestamp': datetime.now(timezone.utc).isoformat(),
            'elapsed_seconds': elapsed,
            'alerts_triggered': self.alerts_triggered,
            'message_counts': self.message_counts,
            'total_messages': total_messages,
            'system_status': 'healthy' if self.alerts_triggered == 0 else 'degraded'
        }
        
        self.redis_client.publish('ops.monitoring_report', json.dumps(report))

    async def run(self):
        """運行監控系統"""
        logger.info("🚀 啟動 Ops 監控系統")
        
        # 測試 Redis 連接
        try:
            self.redis_client.ping()
            logger.info("✅ Redis 連接成功")
        except Exception as e:
            logger.error(f"❌ Redis 連接失敗: {e}")
            return
        
        # 設置監控
        await self.setup_monitoring()
        
        # 並行運行監控任務
        tasks = [
            asyncio.create_task(self.monitor_system_health()),
            asyncio.create_task(self.listen_to_messages())
        ]
        
        try:
            await asyncio.gather(*tasks)
        except Exception as e:
            logger.error(f"❌ 監控任務異常: {e}")
        finally:
            # 清理
            self.pubsub.close()
            
            # 最終報告
            elapsed = time.time() - self.start_time
            total_messages = sum(self.message_counts.values())
            logger.info("🎉 Ops 監控完成！")
            logger.info(f"📊 最終統計:")
            logger.info(f"  ⏱️ 總運行時間: {elapsed:.1f} 秒")
            logger.info(f"  📨 總處理消息: {total_messages}")
            logger.info(f"  🚨 總觸發告警: {self.alerts_triggered}")

async def main():
    monitor = SimpleOpsMonitor()
    await monitor.run()

if __name__ == "__main__":
    asyncio.run(main())