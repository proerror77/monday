import asyncio
import websockets
import json
import logging

# 設置日誌
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

async def test_bitget_websocket():
    """測試Bitget WebSocket連接和訂單簿訂閱"""
    # 使用v2 API WebSocket端點 
    ws_url = 'wss://ws.bitget.com/v2/ws/public'
    
    try:
        async with websockets.connect(ws_url) as websocket:
            logger.info("WebSocket v2連接成功")
            
            # 使用v2 API的訂閱格式
            orderbook_msg = {
                "op": "subscribe",
                "args": [{
                    "instType": "SPOT",
                    "channel": "books5",
                    "instId": "BTCUSDT"
                }]
            }
            
            await websocket.send(json.dumps(orderbook_msg))
            logger.info(f"發送訂閱消息: {json.dumps(orderbook_msg)}")
            
            # 監聽消息
            message_count = 0
            while message_count < 10:  # 只接收前10條消息作為測試
                try:
                    message = await websocket.recv()
                    logger.info(f"收到消息 #{message_count + 1}: {message}")
                    
                    data = json.loads(message)
                    
                    # 檢查是否是訂閱確認
                    if 'event' in data and data['event'] == 'subscribe':
                        logger.info("訂閱確認成功")
                    
                    # 檢查是否是訂單簿數據
                    if 'arg' in data and data['arg'].get('channel') == 'books5':
                        if 'data' in data and data['data']:
                            orderbook_data = data['data'][0]
                            bids = orderbook_data.get('bids', [])
                            asks = orderbook_data.get('asks', [])
                            if bids and asks:
                                best_bid = float(bids[0][0])
                                best_ask = float(asks[0][0])
                                logger.info(f"訂單簿數據 - 買一價: {best_bid}, 賣一價: {best_ask}")
                    
                    message_count += 1
                    
                except Exception as e:
                    logger.error(f"處理消息錯誤: {e}")
                    break
                    
    except Exception as e:
        logger.error(f"WebSocket連接失敗: {e}")

if __name__ == "__main__":
    asyncio.run(test_bitget_websocket())