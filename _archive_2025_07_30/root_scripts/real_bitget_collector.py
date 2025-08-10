#!/usr/bin/env python3
"""
真實 Bitget 市場數據收集器
========================

連接真實的 Bitget WebSocket API 收集市場數據
"""

import asyncio
import websockets
import json
import time
import logging
from datetime import datetime, timezone
import aiohttp
import clickhouse_connect
from typing import Dict, Any

# 配置日誌
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class RealBitgetCollector:
    def __init__(self):
        self.websocket_url = "wss://ws.bitget.com/v2/ws/public"
        self.symbols = ['BTCUSDT', 'ETHUSDT', 'SOLUSDT']
        self.clickhouse_client = None
        self.message_count = 0
        self.start_time = time.time()
        
    async def setup_clickhouse(self):
        """設置 ClickHouse 連接和表格"""
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
                channel String,
                price Decimal64(8),
                quantity Decimal64(8),
                side String,
                trade_id String DEFAULT '',
                raw_data String
            ) ENGINE = MergeTree()
            ORDER BY (symbol, timestamp)
            """
            
            self.clickhouse_client.command(create_table_sql)
            logger.info("✅ ClickHouse 表格創建成功")
            
        except Exception as e:
            logger.error(f"❌ ClickHouse 設置失敗: {e}")
            raise
    
    async def connect_websocket(self):
        """連接真實的 Bitget WebSocket"""
        logger.info("🔗 連接 Bitget WebSocket...")
        
        try:
            # 添加更多連接參數
            extra_headers = {
                'User-Agent': 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36'
            }
            
            async with websockets.connect(
                self.websocket_url,
                extra_headers=extra_headers,
                ping_interval=20,
                ping_timeout=10,
                close_timeout=10
            ) as websocket:
                logger.info("✅ WebSocket 連接成功")
                
                # 訂閱市場數據
                await self.subscribe_to_channels(websocket)
                
                # 持續接收數據
                await self.listen_for_messages(websocket)
                
        except Exception as e:
            logger.error(f"❌ WebSocket 連接失敗: {e}")
            raise
    
    async def subscribe_to_channels(self, websocket):
        """訂閱市場數據頻道"""
        logger.info("📡 訂閱市場數據頻道...")
        
        # 訂閱 ticker 數據
        ticker_sub = {
            "op": "subscribe",
            "args": [{"instType": "SPOT", "channel": "ticker", "instId": symbol} 
                    for symbol in self.symbols]
        }
        
        # 訂閱交易數據
        trade_sub = {
            "op": "subscribe", 
            "args": [{"instType": "SPOT", "channel": "trade", "instId": symbol}
                    for symbol in self.symbols]
        }
        
        # 訂閱訂單簿數據
        books_sub = {
            "op": "subscribe",
            "args": [{"instType": "SPOT", "channel": "books5", "instId": symbol}
                    for symbol in self.symbols]
        }
        
        await websocket.send(json.dumps(ticker_sub))
        await asyncio.sleep(0.1)
        
        await websocket.send(json.dumps(trade_sub))
        await asyncio.sleep(0.1)
        
        await websocket.send(json.dumps(books_sub))
        
        logger.info(f"✅ 已訂閱 {len(self.symbols)} 個交易對的市場數據")
    
    async def listen_for_messages(self, websocket):
        """監聽並處理消息"""
        logger.info("👂 開始監聽市場數據...")
        
        last_report_time = time.time()
        
        async for message in websocket:
            try:
                data = json.loads(message)
                
                # 處理市場數據
                if 'data' in data and len(data['data']) > 0:
                    await self.process_market_data(data)
                    self.message_count += 1
                
                # 每30秒報告一次狀態
                current_time = time.time()
                if current_time - last_report_time >= 30:
                    await self.report_status()
                    last_report_time = current_time
                
                # 運行10分鐘後停止
                elapsed = current_time - self.start_time
                if elapsed > 600:  # 10 minutes
                    logger.info("⏰ 收集完成，運行了 10 分鐘")
                    break
                    
            except json.JSONDecodeError:
                logger.warning("⚠️ 收到無效的 JSON 數據")
            except Exception as e:
                logger.error(f"❌ 處理消息時出錯: {e}")
    
    async def process_market_data(self, data: Dict[str, Any]):
        """處理並存儲市場數據"""
        try:
            arg = data.get('arg', {})
            channel = arg.get('channel', '')
            symbol = arg.get('instId', '')
            
            for item in data['data']:
                timestamp = datetime.now(timezone.utc)
                
                # 根據不同頻道處理數據
                if channel == 'ticker':
                    await self.store_ticker_data(timestamp, symbol, item, data)
                elif channel == 'trade':
                    await self.store_trade_data(timestamp, symbol, item, data)
                elif channel == 'books5':
                    await self.store_books_data(timestamp, symbol, item, data)
                    
        except Exception as e:
            logger.error(f"❌ 處理市場數據失敗: {e}")
    
    async def store_ticker_data(self, timestamp, symbol, item, raw_data):
        """存儲 ticker 數據"""
        try:
            price = float(item.get('last', 0))
            
            if price > 0:
                self.clickhouse_client.insert('real_market_data', [{
                    'timestamp': timestamp,
                    'symbol': symbol,
                    'channel': 'ticker',
                    'price': price,
                    'quantity': 0,
                    'side': '',
                    'trade_id': '',
                    'raw_data': json.dumps(raw_data)
                }])
                
                logger.debug(f"📊 Ticker: {symbol} = ${price:.2f}")
                
        except Exception as e:
            logger.error(f"❌ 存儲 ticker 數據失敗: {e}")
    
    async def store_trade_data(self, timestamp, symbol, item, raw_data):
        """存儲交易數據"""
        try:
            price = float(item.get('px', 0))
            quantity = float(item.get('sz', 0))
            side = item.get('side', '')
            trade_id = item.get('tradeId', '')
            
            if price > 0 and quantity > 0:
                self.clickhouse_client.insert('real_market_data', [{
                    'timestamp': timestamp,
                    'symbol': symbol,
                    'channel': 'trade',
                    'price': price,
                    'quantity': quantity,
                    'side': side,
                    'trade_id': trade_id,
                    'raw_data': json.dumps(raw_data)
                }])
                
                logger.debug(f"💰 Trade: {symbol} {side} {quantity}@${price:.2f}")
                
        except Exception as e:
            logger.error(f"❌ 存儲交易數據失敗: {e}")
    
    async def store_books_data(self, timestamp, symbol, item, raw_data):
        """存儲訂單簿數據"""
        try:
            # 處理買單
            for bid in item.get('bids', [])[:5]:  # 只取前5檔
                price = float(bid[0])
                quantity = float(bid[1])
                
                if price > 0 and quantity > 0:
                    self.clickhouse_client.insert('real_market_data', [{
                        'timestamp': timestamp,
                        'symbol': symbol,
                        'channel': 'books_bid',
                        'price': price,
                        'quantity': quantity,
                        'side': 'bid',
                        'trade_id': '',
                        'raw_data': json.dumps(raw_data)
                    }])
            
            # 處理賣單
            for ask in item.get('asks', [])[:5]:  # 只取前5檔
                price = float(ask[0])
                quantity = float(ask[1])
                
                if price > 0 and quantity > 0:
                    self.clickhouse_client.insert('real_market_data', [{
                        'timestamp': timestamp,
                        'symbol': symbol,
                        'channel': 'books_ask',
                        'price': price,
                        'quantity': quantity,
                        'side': 'ask',
                        'trade_id': '',
                        'raw_data': json.dumps(raw_data)
                    }])
                    
            logger.debug(f"📖 Books: {symbol} updated")
                    
        except Exception as e:
            logger.error(f"❌ 存儲訂單簿數據失敗: {e}")
    
    async def report_status(self):
        """報告收集狀態"""
        elapsed = time.time() - self.start_time
        rate = self.message_count / elapsed if elapsed > 0 else 0
        
        # 查詢數據庫中的記錄數
        try:
            result = self.clickhouse_client.query("SELECT COUNT(*) as count FROM real_market_data")
            db_count = result.first_row[0] if result.first_row else 0
        except:
            db_count = 0
        
        logger.info(f"📊 狀態報告:")
        logger.info(f"  ⏱️  運行時間: {elapsed:.1f} 秒")
        logger.info(f"  📨 接收消息: {self.message_count}")
        logger.info(f"  📈 消息速率: {rate:.2f} msg/s")
        logger.info(f"  💾 數據庫記錄: {db_count}")
    
    async def run(self):
        """運行數據收集器"""
        logger.info("🚀 啟動真實 Bitget 數據收集器")
        
        try:
            # 設置 ClickHouse
            await self.setup_clickhouse()
            
            # 連接 WebSocket 並開始收集
            await self.connect_websocket()
            
        except Exception as e:
            logger.error(f"❌ 數據收集器運行失敗: {e}")
            raise
        finally:
            # 最終報告
            await self.report_status()
            logger.info("🎉 真實數據收集完成！")

async def main():
    collector = RealBitgetCollector()
    await collector.run()

if __name__ == "__main__":
    asyncio.run(main())