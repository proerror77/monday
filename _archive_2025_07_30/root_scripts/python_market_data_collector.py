#!/usr/bin/env python3
"""
Python 市場數據收集器
===================

模擬真實市場數據收集並存儲到 ClickHouse
"""

import asyncio
import time
import random
import json
from datetime import datetime, timezone
import redis
import aiohttp
import logging

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class MarketDataCollector:
    def __init__(self):
        self.redis_client = redis.Redis(host='localhost', port=6379, decode_responses=True)
        self.symbols = ['BTCUSDT', 'ETHUSDT', 'ADAUSDT', 'BNBUSDT', 'SOLUSDT']
        self.message_count = 0
        self.start_time = time.time()
        
    async def create_tables(self):
        """創建 ClickHouse 數據表"""
        logger.info("🏗️ 創建數據表...")
        
        create_sql = """
        CREATE TABLE IF NOT EXISTS market_data (
            timestamp DateTime64(3),
            symbol String,
            price Decimal64(8),
            volume Decimal64(8),
            side Enum8('buy' = 1, 'sell' = 2),
            data_source String DEFAULT 'python_collector'
        ) ENGINE = MergeTree()
        ORDER BY (symbol, timestamp)
        """
        
        async with aiohttp.ClientSession() as session:
            try:
                async with session.post(
                    'http://localhost:8123/',
                    data=create_sql,
                    headers={'X-ClickHouse-User': 'default'}
                ) as response:
                    if response.status == 200:
                        logger.info("✅ 數據表創建成功")
                        return True
                    else:
                        logger.error(f"❌ 創建表失敗: {response.status}")
                        return False
            except Exception as e:
                logger.error(f"❌ 連接 ClickHouse 失敗: {e}")
                return False

    async def collect_market_data(self):
        """收集市場數據"""
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
                
                # 存儲到 ClickHouse
                await self.store_to_clickhouse(current_time, symbol, price, volume, side)
                
                # 發布到 Redis
                await self.publish_to_redis(current_time, symbol, price, volume, side)
                
                self.message_count += 1
            
            # 等待 1 秒
            await asyncio.sleep(1)
            
            # 每 30 秒報告一次狀態
            elapsed = time.time() - self.start_time
            if int(elapsed) % 30 == 0:
                rate = self.message_count / elapsed
                logger.info(f"📊 數據收集統計: {self.message_count} 條消息, {rate:.2f} msg/s")
            
            # 10 分鐘後停止
            if elapsed > 600:  # 10 minutes
                logger.info("⏰ 收集完成，運行了 10 分鐘")
                break

    async def store_to_clickhouse(self, timestamp, symbol, price, volume, side):
        """存儲數據到 ClickHouse"""
        query = f"""
        INSERT INTO market_data (timestamp, symbol, price, volume, side) VALUES 
        ('{timestamp.strftime("%Y-%m-%d %H:%M:%S.%f")[:-3]}', '{symbol}', {price:.8f}, {volume:.8f}, '{side}')
        """
        
        async with aiohttp.ClientSession() as session:
            try:
                async with session.post(
                    'http://localhost:8123/', 
                    data=query,
                    headers={'X-ClickHouse-User': 'default'}
                ) as response:
                    if response.status != 200:
                        logger.warning(f"⚠️ ClickHouse 插入警告: {response.status}")
            except Exception as e:
                logger.warning(f"⚠️ ClickHouse 存儲失敗: {e}")

    async def publish_to_redis(self, timestamp, symbol, price, volume, side):
        """發布數據到 Redis"""
        try:
            market_data = {
                'timestamp': timestamp.isoformat(),
                'symbol': symbol,
                'price': price,
                'volume': volume,
                'side': side,
                'source': 'python_collector'
            }
            
            # 發布到通用頻道
            self.redis_client.publish('market_data', json.dumps(market_data))
            
            # 發布到特定符號頻道
            self.redis_client.publish(f'market_data:{symbol}', json.dumps(market_data))
            
        except Exception as e:
            logger.warning(f"⚠️ Redis 發布失敗: {e}")

    async def run(self):
        """運行數據收集器"""
        logger.info("🚀 啟動 Python 市場數據收集器")
        
        # 測試 Redis 連接
        try:
            self.redis_client.ping()
            logger.info("✅ Redis 連接成功")
        except Exception as e:
            logger.error(f"❌ Redis 連接失敗: {e}")
            return
        
        # 創建數據表
        if not await self.create_tables():
            return
        
        # 開始數據收集
        logger.info("📊 開始收集市場數據...")
        await self.collect_market_data()
        
        # 統計信息
        elapsed = time.time() - self.start_time
        rate = self.message_count / elapsed
        logger.info(f"🎉 數據收集完成！")
        logger.info(f"📈 總共收集: {self.message_count} 條消息")
        logger.info(f"⏱️ 運行時間: {elapsed:.1f} 秒")
        logger.info(f"📊 平均速率: {rate:.2f} msg/s")

async def main():
    collector = MarketDataCollector()
    await collector.run()

if __name__ == "__main__":
    asyncio.run(main())