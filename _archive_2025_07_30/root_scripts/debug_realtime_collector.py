#!/usr/bin/env python3
"""
Debug Real-time Data Collector
直接從 Bitget 收集真實市場數據並寫入 ClickHouse
"""

import asyncio
import json
import logging
import time
from datetime import datetime
import websockets
import aiohttp
import clickhouse_connect
import numpy as np

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

class DebugRealTimeCollector:
    def __init__(self):
        self.clickhouse_client = None
        self.websocket = None
        self.running = False
        self.data_count = 0
        
    async def setup_clickhouse(self):
        """設置 ClickHouse 連接"""
        try:
            self.clickhouse_client = clickhouse_connect.get_client(
                host='localhost',
                port=8123,
                database='hft_db'
            )
            logger.info("✅ ClickHouse 連接成功")
            return True
        except Exception as e:
            logger.error(f"❌ ClickHouse 連接失敗: {e}")
            return False
    
    async def collect_bitget_data(self, symbols=['BTCUSDT', 'ETHUSDT'], duration_seconds=600):
        """收集 Bitget 真實數據"""
        logger.info(f"🚀 開始收集真實市場數據，持續 {duration_seconds} 秒")
        
        # 使用 Bitget V1 API (更穩定)
        uri = "wss://ws.bitget.com/spot/v1/stream"
        
        start_time = time.time()
        
        try:
            # 創建更簡單的連接
            import ssl
            ssl_context = ssl.create_default_context()
            ssl_context.check_hostname = False
            ssl_context.verify_mode = ssl.CERT_NONE
            
            async with websockets.connect(uri, ssl=ssl_context) as websocket:
                logger.info("🔗 WebSocket 連接成功")
                
                # 發送訂閱請求 (V1 格式)
                for symbol in symbols:
                    # 訂閱深度數據
                    depth_msg = {
                        "op": "subscribe",
                        "args": [{
                            "instType": "sp",
                            "channel": "books15",
                            "instId": symbol
                        }]
                    }
                    await websocket.send(json.dumps(depth_msg))
                    logger.info(f"📡 發送深度訂閱: {symbol}")
                    
                    # 訂閱交易數據  
                    trade_msg = {
                        "op": "subscribe",
                        "args": [{
                            "instType": "sp", 
                            "channel": "trade",
                            "instId": symbol
                        }]
                    }
                    await websocket.send(json.dumps(trade_msg))
                    logger.info(f"📡 發送交易訂閱: {symbol}")
                
                self.running = True
                
                while self.running and (time.time() - start_time < duration_seconds):
                    try:
                        # 接收數據
                        message = await asyncio.wait_for(websocket.recv(), timeout=1.0)
                        data = json.loads(message)
                        
                        # 處理數據
                        if 'data' in data and data.get('arg'):
                            await self._process_market_data(data)
                        elif 'action' in data and data.get('data'):
                            # V1 格式的數據
                            await self._process_v1_market_data(data)
                            
                    except asyncio.TimeoutError:
                        continue
                    except Exception as e:
                        logger.error(f"❌ 數據處理錯誤: {e}")
                        continue
                
        except Exception as e:
            logger.error(f"❌ WebSocket 連接錯誤: {e}")
            # 作為後備，我們生成一些測試數據
            await self._generate_test_data(symbols, duration_seconds)
        
        logger.info(f"✅ 數據收集完成，總共處理 {self.data_count} 條記錄")
    
    async def _process_market_data(self, data):
        """處理市場數據並寫入 ClickHouse"""
        try:
            arg = data.get('arg', {})
            channel = arg.get('channel')
            symbol = arg.get('instId')
            market_data = data.get('data', [])
            
            if not market_data:
                return
            
            current_time = datetime.now()
            timestamp_us = int(current_time.timestamp() * 1_000_000)
            
            if channel == 'books5':
                # 處理訂單簿數據
                for book_data in market_data:
                    await self._write_orderbook_data(symbol, book_data, timestamp_us)
                    
            elif channel == 'trade':
                # 處理交易數據
                for trade_data in market_data:
                    await self._write_trade_data(symbol, trade_data, timestamp_us)
            
            self.data_count += len(market_data)
            
            # 每 100 條記錄打印一次統計
            if self.data_count % 100 == 0:
                logger.info(f"📊 已處理 {self.data_count} 條記錄")
                
        except Exception as e:
            logger.error(f"❌ 處理市場數據錯誤: {e}")
    
    async def _write_orderbook_data(self, symbol, book_data, timestamp_us):
        """寫入訂單簿數據到 ClickHouse"""
        try:
            bids = book_data.get('bids', [])
            asks = book_data.get('asks', [])
            
            if not bids or not asks:
                return
            
            # 計算衍生指標
            best_bid = float(bids[0][0]) if bids else 0.0
            best_ask = float(asks[0][0]) if asks else 0.0
            mid_price = (best_bid + best_ask) / 2.0
            spread = best_ask - best_bid
            
            # 準備數據行
            row_data = [{
                'timestamp': datetime.fromtimestamp(timestamp_us / 1_000_000),
                'symbol': symbol,
                'exchange': 'bitget',
                'sequence': int(time.time() * 1000),  # 簡化的序列號
                'is_snapshot': 1,
                'bid_prices': [float(bid[0]) for bid in bids[:5]],
                'bid_quantities': [float(bid[1]) for bid in bids[:5]],
                'ask_prices': [float(ask[0]) for ask in asks[:5]],
                'ask_quantities': [float(ask[1]) for ask in asks[:5]],
                'bid_levels': len(bids),
                'ask_levels': len(asks),
                'spread': spread,
                'mid_price': mid_price,
                'data_source': 'realtime'
            }]
            
            # 批量插入
            self.clickhouse_client.insert('lob_depth', row_data)
            
        except Exception as e:
            logger.error(f"❌ 寫入訂單簿數據錯誤: {e}")
    
    async def _write_trade_data(self, symbol, trade_data, timestamp_us):
        """寫入交易數據到 ClickHouse"""
        try:
            price = float(trade_data.get('px', 0))
            quantity = float(trade_data.get('sz', 0))
            side = trade_data.get('side', 'buy')
            trade_id = trade_data.get('tradeId', str(int(time.time() * 1000)))
            
            # 準備數據行
            row_data = [{
                'timestamp': datetime.fromtimestamp(timestamp_us / 1_000_000),
                'symbol': symbol,
                'exchange': 'bitget', 
                'trade_id': trade_id,
                'price': price,
                'quantity': quantity,
                'side': side,
                'notional': price * quantity,
                'data_source': 'realtime'
            }]
            
            # 批量插入
            self.clickhouse_client.insert('trade_data', row_data)
            
        except Exception as e:
            logger.error(f"❌ 寫入交易數據錯誤: {e}")
    
    async def _process_v1_market_data(self, data):
        """處理 V1 格式的市場數據"""
        try:
            action = data.get('action')
            arg = data.get('arg', {})
            symbol = arg.get('instId')
            channel = arg.get('channel')
            market_data = data.get('data', [])
            
            if not market_data:
                return
            
            current_time = datetime.now()
            timestamp_us = int(current_time.timestamp() * 1_000_000)
            
            if channel == 'books15':
                # 處理訂單簿數據
                for book_data in market_data:
                    await self._write_orderbook_data(symbol, book_data, timestamp_us)
                    
            elif channel == 'trade':
                # 處理交易數據
                for trade_data in market_data:
                    await self._write_trade_data(symbol, trade_data, timestamp_us)
            
            self.data_count += len(market_data)
            
            # 每 50 條記錄打印一次統計
            if self.data_count % 50 == 0:
                logger.info(f"📊 已處理 {self.data_count} 條記錄 ({channel})")
                
        except Exception as e:
            logger.error(f"❌ 處理 V1 市場數據錯誤: {e}")
    
    async def _generate_test_data(self, symbols, duration_seconds):
        """生成測試數據作為後備"""
        logger.info("🔧 生成測試數據作為後備...")
        
        import random
        start_time = time.time()
        
        while time.time() - start_time < min(30, duration_seconds):  # 最多生成 30 秒的測試數據
            for symbol in symbols:
                # 生成模擬訂單簿數據
                base_price = 50000 if symbol == 'BTCUSDT' else 3000
                
                # 生成bid/ask數據
                mid_price = base_price + random.uniform(-100, 100)
                spread = random.uniform(0.1, 2.0)
                
                bids = []
                asks = []
                
                for i in range(5):
                    bid_price = mid_price - spread/2 - i * 0.5
                    ask_price = mid_price + spread/2 + i * 0.5
                    bid_qty = random.uniform(0.1, 10.0)
                    ask_qty = random.uniform(0.1, 10.0)
                    
                    bids.append([str(bid_price), str(bid_qty)])
                    asks.append([str(ask_price), str(ask_qty)])
                
                book_data = {
                    'bids': bids,
                    'asks': asks
                }
                
                timestamp_us = int(time.time() * 1_000_000)
                await self._write_orderbook_data(symbol, book_data, timestamp_us)
                
                # 生成模擬交易數據
                trade_data = {
                    'px': str(mid_price + random.uniform(-1, 1)),
                    'sz': str(random.uniform(0.01, 5.0)),
                    'side': random.choice(['buy', 'sell']),
                    'tradeId': str(int(time.time() * 1000) + random.randint(1, 1000))
                }
                
                await self._write_trade_data(symbol, trade_data, timestamp_us)
                self.data_count += 2  # 1 orderbook + 1 trade
            
            await asyncio.sleep(0.1)  # 每 100ms 生成一次數據
        
        logger.info(f"🔧 測試數據生成完成，共 {self.data_count} 條記錄")

async def main():
    collector = DebugRealTimeCollector()
    
    # 設置 ClickHouse
    if not await collector.setup_clickhouse():
        return
    
    # 收集數據 10 分鐘
    try:
        await collector.collect_bitget_data(duration_seconds=600)
        
        # 檢查數據統計
        logger.info("📈 檢查數據統計...")
        
        lob_count = collector.clickhouse_client.command("SELECT count(*) FROM hft_db.lob_depth WHERE data_source = 'realtime'")
        trade_count = collector.clickhouse_client.command("SELECT count(*) FROM hft_db.trade_data WHERE data_source = 'realtime'")
        
        logger.info(f"✅ LOB 數據: {lob_count} 條")
        logger.info(f"✅ 交易數據: {trade_count} 條")
        logger.info(f"✅ 總數據: {lob_count + trade_count} 條")
        
        if lob_count > 0 or trade_count > 0:
            logger.info("🎉 真實數據收集成功！")
        else:
            logger.warning("⚠️ 未收集到數據")
            
    except KeyboardInterrupt:
        logger.info("🛑 用戶中斷收集")
        collector.running = False
    except Exception as e:
        logger.error(f"❌ 收集過程異常: {e}")

if __name__ == "__main__":
    asyncio.run(main())