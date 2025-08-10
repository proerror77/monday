#!/usr/bin/env python3
"""
簡單的真實 Bitget 市場數據收集器
使用 REST API 獲取真實數據
"""

import asyncio
import aiohttp
import json
import time
import logging
from datetime import datetime, timezone
import clickhouse_connect

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class SimpleBitgetCollector:
    def __init__(self):
        self.base_url = "https://api.bitget.com"
        self.symbols = ['BTCUSDT', 'ETHUSDT', 'SOLUSDT']
        self.clickhouse_client = None
        self.total_records = 0
        
    async def setup_clickhouse(self):
        """設置 ClickHouse"""
        try:
            self.clickhouse_client = clickhouse_connect.get_client(
                host='localhost',
                port=8123,
                database='default'
            )
            
            # 創建表格
            create_table_sql = """
            CREATE TABLE IF NOT EXISTS real_market_data (
                timestamp DateTime64(3),
                symbol String,
                data_type String,
                price Decimal64(8),
                volume Decimal64(8),
                side String DEFAULT '',
                raw_data String
            ) ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
            """
            
            self.clickhouse_client.command(create_table_sql)
            logger.info("✅ ClickHouse 表格準備完成")
            
        except Exception as e:
            logger.error(f"❌ ClickHouse 設置失敗: {e}")
            raise
    
    async def fetch_ticker_data(self, session, symbol):
        """獲取實時價格數據"""
        try:
            url = f"{self.base_url}/api/v2/spot/market/ticker"
            params = {'symbol': symbol}
            
            async with session.get(url, params=params) as response:
                if response.status == 200:
                    data = await response.json()
                    
                    if data.get('code') == '00000' and data.get('data'):
                        ticker_data = data['data']
                        timestamp = datetime.now(timezone.utc)
                        
                        # 存儲ticker數據
                        price = float(ticker_data.get('lastPr', 0))
                        volume = float(ticker_data.get('baseVolume', 0))
                        
                        if price > 0:
                            self.clickhouse_client.insert('real_market_data', [{
                                'timestamp': timestamp,
                                'symbol': symbol,
                                'data_type': 'ticker',
                                'price': price,
                                'volume': volume,
                                'side': '',
                                'raw_data': json.dumps(data)
                            }])
                            
                            self.total_records += 1
                            logger.info(f"📊 {symbol}: ${price:.4f} (24h Vol: {volume:.2f})")
                            return True
                        
        except Exception as e:
            logger.error(f"❌ 獲取 {symbol} ticker 數據失敗: {e}")
        
        return False
    
    async def fetch_orderbook_data(self, session, symbol):
        """獲取訂單簿數據"""
        try:
            url = f"{self.base_url}/api/v2/spot/market/orderbook"
            params = {'symbol': symbol, 'limit': '5'}
            
            async with session.get(url, params=params) as response:
                if response.status == 200:
                    data = await response.json()
                    
                    if data.get('code') == '00000' and data.get('data'):
                        orderbook = data['data']
                        timestamp = datetime.now(timezone.utc)
                        
                        # 存儲買單數據
                        for bid in orderbook.get('bids', [])[:3]:
                            price = float(bid[0])
                            volume = float(bid[1])
                            
                            self.clickhouse_client.insert('real_market_data', [{
                                'timestamp': timestamp,
                                'symbol': symbol,
                                'data_type': 'orderbook_bid',
                                'price': price,
                                'volume': volume,
                                'side': 'bid',
                                'raw_data': json.dumps(data)
                            }])
                            self.total_records += 1
                        
                        # 存儲賣單數據
                        for ask in orderbook.get('asks', [])[:3]:
                            price = float(ask[0])
                            volume = float(ask[1])
                            
                            self.clickhouse_client.insert('real_market_data', [{
                                'timestamp': timestamp,
                                'symbol': symbol,
                                'data_type': 'orderbook_ask',
                                'price': price,
                                'volume': volume,
                                'side': 'ask',
                                'raw_data': json.dumps(data)
                            }])
                            self.total_records += 1
                        
                        logger.debug(f"📖 {symbol} 訂單簿數據已更新")
                        return True
                        
        except Exception as e:
            logger.error(f"❌ 獲取 {symbol} 訂單簿數據失敗: {e}")
        
        return False
    
    async def fetch_recent_trades(self, session, symbol):
        """獲取最近交易數據"""
        try:
            url = f"{self.base_url}/api/v2/spot/market/fills"
            params = {'symbol': symbol, 'limit': '10'}
            
            async with session.get(url, params=params) as response:
                if response.status == 200:
                    data = await response.json()
                    
                    if data.get('code') == '00000' and data.get('data'):
                        trades = data['data']
                        timestamp = datetime.now(timezone.utc)
                        
                        for trade in trades[:5]:  # 只取前5筆
                            price = float(trade.get('price', 0))
                            volume = float(trade.get('size', 0))
                            side = trade.get('side', '')
                            
                            if price > 0 and volume > 0:
                                self.clickhouse_client.insert('real_market_data', [{
                                    'timestamp': timestamp,
                                    'symbol': symbol,
                                    'data_type': 'trade',
                                    'price': price,
                                    'volume': volume,
                                    'side': side,
                                    'raw_data': json.dumps(data)
                                }])
                                self.total_records += 1
                        
                        logger.debug(f"💰 {symbol} 交易數據已更新")
                        return True
                        
        except Exception as e:
            logger.error(f"❌ 獲取 {symbol} 交易數據失敗: {e}")
        
        return False
    
    async def collect_data_round(self):
        """執行一輪數據收集"""
        async with aiohttp.ClientSession() as session:
            tasks = []
            
            # 為每個交易對創建數據收集任務
            for symbol in self.symbols:
                tasks.append(self.fetch_ticker_data(session, symbol))
                tasks.append(self.fetch_orderbook_data(session, symbol))
                tasks.append(self.fetch_recent_trades(session, symbol))
            
            # 並發執行所有任務
            results = await asyncio.gather(*tasks, return_exceptions=True)
            
            success_count = sum(1 for r in results if r is True)
            logger.info(f"✅ 本輪收集完成: {success_count}/{len(tasks)} 成功")
    
    async def run_collection(self, duration_minutes=10):
        """運行數據收集"""
        logger.info(f"🚀 開始收集真實市場數據 ({duration_minutes} 分鐘)")
        
        start_time = time.time()
        round_count = 0
        
        while True:
            round_start = time.time()
            round_count += 1
            
            logger.info(f"🔄 第 {round_count} 輪數據收集...")
            await self.collect_data_round()
            
            # 檢查時間
            elapsed = time.time() - start_time
            if elapsed >= duration_minutes * 60:
                logger.info(f"⏰ 收集完成，運行了 {elapsed:.1f} 秒")
                break
            
            # 等待間隔（每10秒收集一次）
            round_elapsed = time.time() - round_start
            sleep_time = max(0, 10 - round_elapsed)
            if sleep_time > 0:
                await asyncio.sleep(sleep_time)
        
        logger.info(f"🎉 數據收集完成！總共收集了 {self.total_records} 條記錄")
    
    async def verify_data(self):
        """驗證收集的數據"""
        try:
            # 查詢總記錄數
            result = self.clickhouse_client.query("SELECT COUNT(*) as count FROM real_market_data")
            total_count = result.first_row[0] if result.first_row else 0
            
            # 查詢每個交易對的數據
            symbol_stats = self.clickhouse_client.query("""
                SELECT symbol, data_type, COUNT(*) as count, 
                       MIN(price) as min_price, MAX(price) as max_price,
                       AVG(price) as avg_price
                FROM real_market_data 
                GROUP BY symbol, data_type 
                ORDER BY symbol, data_type
            """)
            
            logger.info("🔍 數據驗證結果:")
            logger.info(f"📊 總記錄數: {total_count}")
            
            for row in symbol_stats.result_rows:
                symbol, data_type, count, min_price, max_price, avg_price = row
                logger.info(f"  {symbol} {data_type}: {count} 條，價格 ${min_price:.4f}-${max_price:.4f} (平均 ${avg_price:.4f})")
            
            return total_count > 0
            
        except Exception as e:
            logger.error(f"❌ 數據驗證失敗: {e}")
            return False
    
    async def run(self):
        """運行收集器"""
        try:
            # 設置 ClickHouse
            await self.setup_clickhouse()
            
            # 運行數據收集
            await self.run_collection(duration_minutes=10)
            
            # 驗證數據
            success = await self.verify_data()
            
            if success:
                logger.info("✅ 真實數據收集成功完成！")
            else:
                logger.warning("⚠️ 數據驗證未通過")
                
        except Exception as e:
            logger.error(f"❌ 收集器運行失敗: {e}")
            raise

async def main():
    collector = SimpleBitgetCollector()
    await collector.run()

if __name__ == "__main__":
    asyncio.run(main())