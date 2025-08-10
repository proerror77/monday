#!/usr/bin/env python3
"""
工作的真實數據收集器
====================

使用 CoinGecko API 獲取真實市場數據並存入 ClickHouse
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

class WorkingRealCollector:
    def __init__(self):
        # 改用更可靠的 CoinGecko API
        self.base_url = "https://api.coingecko.com/api/v3"
        self.coins = ['bitcoin', 'ethereum', 'solana']
        self.symbols = ['BTC', 'ETH', 'SOL']
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
            
            # 刪除舊表並重新創建
            self.clickhouse_client.command("DROP TABLE IF EXISTS real_market_data")
            
            # 創建更簡單的表格結構
            create_table_sql = """
            CREATE TABLE real_market_data (
                timestamp DateTime64(3),
                symbol String,
                price Float64,
                volume_24h Float64,
                market_cap Float64,
                price_change_24h Float64
            ) ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
            """
            
            self.clickhouse_client.command(create_table_sql)
            logger.info("✅ ClickHouse 表格創建成功")
            
        except Exception as e:
            logger.error(f"❌ ClickHouse 設置失敗: {e}")
            raise
    
    async def fetch_coin_data(self, session):
        """獲取真實幣價數據"""
        try:
            # 獲取多個幣種的實時數據
            url = f"{self.base_url}/simple/price"
            params = {
                'ids': ','.join(self.coins),
                'vs_currencies': 'usd',
                'include_24hr_vol': 'true',
                'include_24hr_change': 'true',
                'include_market_cap': 'true'
            }
            
            async with session.get(url, params=params) as response:
                if response.status == 200:
                    data = await response.json()
                    timestamp = datetime.now(timezone.utc)
                    
                    records = []
                    for i, coin in enumerate(self.coins):
                        if coin in data:
                            coin_data = data[coin]
                            
                            record = [
                                timestamp,
                                self.symbols[i],
                                float(coin_data.get('usd', 0)),
                                float(coin_data.get('usd_24h_vol', 0)),
                                float(coin_data.get('usd_market_cap', 0)),
                                float(coin_data.get('usd_24h_change', 0))
                            ]
                            records.append(record)
                            
                            price = coin_data.get('usd', 0)
                            change = coin_data.get('usd_24h_change', 0)
                            logger.info(f"💰 {self.symbols[i]}: ${price:,.2f} ({change:+.2f}%)")
                    
                    if records:
                        # 批量插入
                        self.clickhouse_client.insert(
                            'real_market_data',
                            records,
                            column_names=['timestamp', 'symbol', 'price', 'volume_24h', 'market_cap', 'price_change_24h']
                        )
                        self.total_records += len(records)
                        logger.info(f"✅ 成功存儲 {len(records)} 條記錄")
                        return True
                        
        except Exception as e:
            logger.error(f"❌ 獲取幣價數據失敗: {e}")
        
        return False
    
    async def fetch_detailed_data(self, session):
        """獲取詳細市場數據"""
        try:
            # 獲取每個幣種的詳細數據
            for i, coin in enumerate(self.coins):
                url = f"{self.base_url}/coins/{coin}"
                params = {
                    'localization': 'false',
                    'tickers': 'false',
                    'market_data': 'true',
                    'community_data': 'false',
                    'developer_data': 'false',
                    'sparkline': 'false'
                }
                
                async with session.get(url, params=params) as response:
                    if response.status == 200:
                        data = await response.json()
                        timestamp = datetime.now(timezone.utc)
                        
                        market_data = data.get('market_data', {})
                        if market_data:
                            record = [
                                timestamp,
                                f"{self.symbols[i]}_DETAIL",
                                float(market_data.get('current_price', {}).get('usd', 0)),
                                float(market_data.get('total_volume', {}).get('usd', 0)),
                                float(market_data.get('market_cap', {}).get('usd', 0)),
                                float(market_data.get('price_change_percentage_24h', 0))
                            ]
                            
                            self.clickhouse_client.insert(
                                'real_market_data',
                                [record],
                                column_names=['timestamp', 'symbol', 'price', 'volume_24h', 'market_cap', 'price_change_24h']
                            )
                            self.total_records += 1
                            
                            logger.debug(f"📊 {self.symbols[i]} 詳細數據已更新")
                
                # 避免 API 限制
                await asyncio.sleep(1)
                
        except Exception as e:
            logger.error(f"❌ 獲取詳細數據失敗: {e}")
    
    async def run_collection(self, duration_minutes=10):
        """運行數據收集"""
        logger.info(f"🚀 開始收集真實市場數據 ({duration_minutes} 分鐘)")
        
        start_time = time.time()
        round_count = 0
        
        async with aiohttp.ClientSession() as session:
            while True:
                round_count += 1
                logger.info(f"🔄 第 {round_count} 輪數據收集...")
                
                # 獲取基本價格數據
                success1 = await self.fetch_coin_data(session)
                
                # 每5輪獲取一次詳細數據
                if round_count % 5 == 0:
                    await self.fetch_detailed_data(session)
                
                # 檢查時間
                elapsed = time.time() - start_time
                if elapsed >= duration_minutes * 60:
                    logger.info(f"⏰ 收集完成，運行了 {elapsed:.1f} 秒")
                    break
                
                # 等待30秒再收集下一輪
                await asyncio.sleep(30)
        
        logger.info(f"🎉 數據收集完成！總共收集了 {self.total_records} 條記錄")
    
    async def verify_data(self):
        """驗證收集的數據"""
        try:
            # 查詢總記錄數
            result = self.clickhouse_client.query("SELECT COUNT(*) FROM real_market_data")
            total_count = result.first_row[0] if result.first_row else 0
            
            # 查詢每個幣種的最新數據
            latest_data = self.clickhouse_client.query("""
                SELECT symbol, 
                       MAX(timestamp) as latest_time,
                       argMax(price, timestamp) as latest_price,
                       argMax(price_change_24h, timestamp) as latest_change,
                       COUNT(*) as record_count
                FROM real_market_data 
                GROUP BY symbol 
                ORDER BY symbol
            """)
            
            logger.info("🔍 數據驗證結果:")
            logger.info(f"📊 總記錄數: {total_count}")
            
            if total_count > 0:
                logger.info("💰 最新價格數據:")
                for row in latest_data.result_rows:
                    symbol, latest_time, price, change, count = row
                    logger.info(f"  {symbol}: ${price:,.2f} ({change:+.2f}%) | {count} 條記錄 | 最新: {latest_time}")
                
                # 驗證數據的真實性
                btc_data = self.clickhouse_client.query("""
                    SELECT price FROM real_market_data 
                    WHERE symbol = 'BTC' 
                    ORDER BY timestamp DESC 
                    LIMIT 1
                """)
                
                if btc_data.result_rows:
                    btc_price = btc_data.result_rows[0][0]
                    # BTC 價格應該在合理範圍內 (20000-150000 USD)
                    is_realistic = 20000 <= btc_price <= 150000
                    logger.info(f"✅ 數據真實性檢查: {'通過' if is_realistic else '未通過'} (BTC價格: ${btc_price:,.2f})")
                    return is_realistic
            
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
            await self.run_collection(duration_minutes=5)  # 先運行5分鐘測試
            
            # 驗證數據
            success = await self.verify_data()
            
            if success:
                logger.info("✅ 真實數據收集成功完成！")
                return True
            else:
                logger.warning("⚠️ 數據驗證未通過")
                return False
                
        except Exception as e:
            logger.error(f"❌ 收集器運行失敗: {e}")
            return False

async def main():
    collector = WorkingRealCollector()
    success = await collector.run()
    return success

if __name__ == "__main__":
    result = asyncio.run(main())
    if result:
        print("🎉 真實數據收集成功！")
    else:
        print("❌ 數據收集失敗")