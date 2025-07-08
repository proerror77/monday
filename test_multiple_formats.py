import asyncio
import websockets
import json
import logging

logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

async def test_format(ws_url, subscription_msg, format_name):
    """測試特定的訂閱格式"""
    logger.info(f"測試格式: {format_name}")
    logger.info(f"URL: {ws_url}")
    logger.info(f"訂閱消息: {json.dumps(subscription_msg)}")
    
    try:
        async with websockets.connect(ws_url) as websocket:
            await websocket.send(json.dumps(subscription_msg))
            
            # 等待回應
            try:
                response = await asyncio.wait_for(websocket.recv(), timeout=5.0)
                logger.info(f"格式 {format_name} 回應: {response}")
                return True
            except asyncio.TimeoutError:
                logger.warning(f"格式 {format_name} 無回應")
                return False
                
    except Exception as e:
        logger.error(f"格式 {format_name} 錯誤: {e}")
        return False

async def test_all_formats():
    """測試多種可能的格式"""
    
    # 格式1: 原始格式
    format1 = {
        "url": "wss://ws.bitget.com/spot/v1/stream",
        "msg": {
            "op": "subscribe",
            "args": [{
                "instType": "SPOT",
                "channel": "books5",
                "instId": "BTCUSDT"
            }]
        },
        "name": "原始格式"
    }
    
    # 格式2: 不使用instType
    format2 = {
        "url": "wss://ws.bitget.com/spot/v1/stream",
        "msg": {
            "op": "subscribe",
            "args": [{
                "channel": "books5",
                "instId": "BTCUSDT"
            }]
        },
        "name": "無instType"
    }
    
    # 格式3: 使用symbol而不是instId
    format3 = {
        "url": "wss://ws.bitget.com/spot/v1/stream",
        "msg": {
            "op": "subscribe",
            "args": [{
                "instType": "SPOT",
                "channel": "books5",
                "symbol": "BTCUSDT"
            }]
        },
        "name": "使用symbol"
    }
    
    # 格式4: 舊版本格式
    format4 = {
        "url": "wss://ws.bitget.com/spot/v1/stream",
        "msg": {
            "op": "subscribe",
            "args": ["spot/depth5:BTCUSDT"]
        },
        "name": "舊版本格式"
    }
    
    # 格式5: 使用books代替books5
    format5 = {
        "url": "wss://ws.bitget.com/spot/v1/stream",
        "msg": {
            "op": "subscribe",
            "args": [{
                "instType": "SPOT",
                "channel": "books",
                "instId": "BTCUSDT"
            }]
        },
        "name": "使用books"
    }
    
    formats = [format1, format2, format3, format4, format5]
    
    for fmt in formats:
        success = await test_format(fmt["url"], fmt["msg"], fmt["name"])
        if success:
            logger.info(f"✅ 格式 {fmt['name']} 成功!")
            return fmt
        else:
            logger.info(f"❌ 格式 {fmt['name']} 失敗")
        await asyncio.sleep(1)  # 稍等一下再測試下一個
    
    logger.error("所有格式都失敗了")
    return None

if __name__ == "__main__":
    asyncio.run(test_all_formats())