#!/usr/bin/env python3
"""
簡化的市場數據收集器
==================

直接使用 Redis 進行數據模擬，不依賴複雜的 ClickHouse 連接
"""

import asyncio
import time
import random
import json
from datetime import datetime, timezone
import redis
import logging

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class SimpleDataCollector:
    def __init__(self):
        self.redis_client = redis.Redis(host='localhost', port=6379, decode_responses=True)
        self.symbols = ['BTCUSDT', 'ETHUSDT', 'ADAUSDT', 'BNBUSDT', 'SOLUSDT']
        self.message_count = 0
        self.start_time = time.time()
        
    async def collect_and_publish_data(self):
        """收集並發布市場數據到 Redis"""
        logger.info("📊 開始收集和發布市場數據...")
        
        while True:
            current_time = datetime.now(timezone.utc)
            
            for symbol in self.symbols:
                # 模擬市場數據
                base_prices = {
                    'BTCUSDT': 67000,
                    'ETHUSDT': 3500, 
                    'ADAUSDT': 0.45,
                    'BNBUSDT': 600,
                    'SOLUSDT': 180
                }
                
                base_price = base_prices.get(symbol, 100)
                price = base_price + (random.random() - 0.5) * base_price * 0.02
                volume = random.uniform(0.1, 10.0)
                side = random.choice(['buy', 'sell'])
                
                # 創建市場數據
                market_data = {
                    'timestamp': current_time.isoformat(),
                    'symbol': symbol,
                    'price': round(price, 8),
                    'volume': round(volume, 8), 
                    'side': side,
                    'source': 'hft_collector',
                    'message_id': self.message_count
                }
                
                # 發布到 Redis 多個頻道
                try:
                    # 主數據頻道
                    self.redis_client.publish('market_data', json.dumps(market_data))
                    
                    # 特定符號頻道
                    self.redis_client.publish(f'market_data:{symbol}', json.dumps(market_data))
                    
                    # HFT 系統頻道
                    self.redis_client.publish('hft.market_feed', json.dumps(market_data))
                    
                    # 存儲最新價格
                    self.redis_client.hset('latest_prices', symbol, price)
                    
                    # 存儲到時間序列 (用於監控)
                    key = f'timeseries:{symbol}:{current_time.strftime("%Y%m%d%H")}'
                    self.redis_client.lpush(key, json.dumps(market_data))
                    self.redis_client.expire(key, 3600)  # 1小時過期
                    
                except Exception as e:
                    logger.warning(f"⚠️ Redis 發布失敗: {e}")
                
                self.message_count += 1
            
            # 等待 1 秒
            await asyncio.sleep(1)
            
            # 每 30 秒報告一次狀態
            elapsed = time.time() - self.start_time
            if int(elapsed) % 30 == 0:
                rate = self.message_count / elapsed
                logger.info(f"📊 數據收集統計: {self.message_count} 條消息, {rate:.2f} msg/s")
                
                # 發布統計信息
                stats = {
                    'timestamp': current_time.isoformat(),
                    'total_messages': self.message_count,
                    'rate_per_second': rate,
                    'elapsed_seconds': elapsed,
                    'status': 'running'
                }
                self.redis_client.publish('hft.system_stats', json.dumps(stats))
            
            # 10 分鐘後停止
            if elapsed > 600:  # 10 minutes
                logger.info("⏰ 收集完成，運行了 10 分鐘")
                
                # 發布完成狀態
                completion_stats = {
                    'timestamp': current_time.isoformat(),
                    'total_messages': self.message_count,
                    'total_elapsed': elapsed,
                    'average_rate': self.message_count / elapsed,
                    'status': 'completed'
                }
                self.redis_client.publish('hft.system_stats', json.dumps(completion_stats))
                break

    async def run(self):
        """運行數據收集器"""
        logger.info("🚀 啟動簡化市場數據收集器")
        
        # 測試 Redis 連接
        try:
            self.redis_client.ping()
            logger.info("✅ Redis 連接成功")
        except Exception as e:
            logger.error(f"❌ Redis 連接失敗: {e}")
            return
        
        # 清理舊數據
        try:
            self.redis_client.delete('latest_prices')
            logger.info("🧹 清理舊數據完成")
        except Exception as e:
            logger.warning(f"⚠️ 清理數據警告: {e}")
        
        # 發布啟動通知
        start_notification = {
            'timestamp': datetime.now(timezone.utc).isoformat(),
            'status': 'started',
            'symbols': self.symbols,
            'expected_duration_minutes': 10
        }
        self.redis_client.publish('hft.system_events', json.dumps(start_notification))
        
        # 開始數據收集
        await self.collect_and_publish_data()
        
        # 統計信息
        elapsed = time.time() - self.start_time
        rate = self.message_count / elapsed
        logger.info(f"🎉 數據收集完成！")
        logger.info(f"📈 總共收集: {self.message_count} 條消息")
        logger.info(f"⏱️ 運行時間: {elapsed:.1f} 秒")
        logger.info(f"📊 平均速率: {rate:.2f} msg/s")
        logger.info(f"💾 數據已發布到 Redis 多個頻道")

async def main():
    collector = SimpleDataCollector()
    await collector.run()

if __name__ == "__main__":
    asyncio.run(main())