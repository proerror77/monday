import asyncio
import websockets
import json
import requests
import time
import logging
import hmac
import hashlib
import base64

# 設置日誌
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

class BitgetHFTBot:
    def __init__(self, api_key, api_secret, passphrase="", fee_offset=150.0, volume_limit=500000.0):
        self.api_key = api_key
        self.api_secret = api_secret
        self.passphrase = passphrase  # API Passphrase
        self.fee_offset = fee_offset  # 手續費抵扣（USDT）
        self.volume_limit = volume_limit  # 交易量限制（USDT）
        self.total_volume = 0.0  # 累計交易量
        self.symbol = 'BTCUSDT'  # 現貨交易對
        self.spread = 0.001  # 價差（0.1%）
        self.order_size = 0.001  # 每單交易量（BTC）
        self.min_depth = 1000.0  # 訂單簿最小深度（USDT）
        self.max_capital = 1200.0  # 最大資金限制（USDT）
        self.used_capital = 0.0  # 已使用資金
        self.order_timeout = 3.0  # 訂單超時時間（秒）- 快速成交優先
        self.running = True
        self.ws_url = 'wss://ws.bitget.com/v2/ws/public'  # v2 WebSocket端點
        self.api_url = 'https://api.bitget.com'
        self.active_orders = {}  # 活躍訂單追蹤
        self.websocket = None
        self.orderbook = {'bids': [], 'asks': []}
        self.last_price = 0.0  # 最新價格
        self.order_management_task = None

    async def connect_websocket(self):
        """連接到WebSocket並訂閱多個頻道"""
        try:
            async with websockets.connect(self.ws_url) as websocket:
                self.websocket = websocket
                
                # 訂閱訂單簿 - 使用books5獲取5檔深度
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
                logger.info("已訂閱BTCUSDT訂單簿頻道")
                
                # 注意：orders和fill是私有頻道，需要認證WebSocket
                # 暫時只使用公共orderbook頻道

                while self.running:
                    try:
                        message = await websocket.recv()
                        logger.info(f"收到WebSocket消息: {message}")  # 添加完整消息日誌
                        data = json.loads(message)
                        await self.handle_websocket_message(data)
                    except websockets.exceptions.ConnectionClosed:
                        logger.warning("WebSocket連接關閉，嘗試重連...")
                        await asyncio.sleep(5)
                        break
                    except Exception as e:
                        logger.error(f"WebSocket錯誤: {e}")
                        await asyncio.sleep(1)
        except Exception as e:
            logger.error(f"WebSocket連接失敗: {e}")
            if self.running:
                await asyncio.sleep(5)
                await self.connect_websocket()

    async def handle_websocket_message(self, data):
        """處理WebSocket消息"""
        logger.info(f"處理消息結構: {json.dumps(data, ensure_ascii=False)}")
        
        # 檢查不同的消息格式
        if 'event' in data:
            logger.info(f"事件消息: {data}")
            return
        
        if 'arg' not in data or 'data' not in data:
            logger.warning(f"收到非標準消息格式: {data}")
            return
            
        channel = data['arg'].get('channel')
        action = data.get('action')
        
        logger.info(f"處理頻道: {channel}, 動作: {action}")
        
        if channel == 'books5':
            await self.process_orderbook(data['data'], action)
        elif channel == 'orders':
            self.process_order_update(data['data'])
        elif channel == 'fill':
            self.process_fill_update(data['data'])
        else:
            logger.warning(f"未知頻道: {channel}")
    
    async def process_orderbook(self, data_list, action):
        """處理訂單簿數據"""
        if not data_list:
            logger.warning("收到空的訂單簿數據")
            return
            
        data = data_list[0]
        bids = data.get('bids', [])
        asks = data.get('asks', [])
        
        logger.debug(f"訂單簿動作: {action}, 買盤: {len(bids)}, 賣盤: {len(asks)}")
        
        if action == 'snapshot':
            self.orderbook = {'bids': bids, 'asks': asks}
        elif action == 'update':
            # 更新訂單簿
            self.update_orderbook(bids, asks)
        
        if self.orderbook['bids'] and self.orderbook['asks']:
            best_bid = float(self.orderbook['bids'][0][0])
            best_ask = float(self.orderbook['asks'][0][0])
            bid_depth = sum(float(bid[1]) * float(bid[0]) for bid in self.orderbook['bids'][:5])
            ask_depth = sum(float(ask[1]) * float(ask[0]) for ask in self.orderbook['asks'][:5])
            
            # 更新最新價格
            self.last_price = (best_bid + best_ask) / 2
            
            # 積極管理訂單 - 更貼近盤口價格
            tick_size = 0.01  # 最小價格變動單位
            buy_price = best_bid + tick_size  # 緊貼買一價
            sell_price = best_ask - tick_size  # 緊貼賣一價
            
            # 動態選擇最優掛單位置
            optimal_buy_price, optimal_sell_price = self.calculate_optimal_prices(best_bid, best_ask, bids, asks)
            
            # 檢查資金限制
            order_value = self.order_size * self.last_price
            
            logger.info(f"買一價: {best_bid}, 賣一價: {best_ask}, 買盤深度: {bid_depth:.2f}, 賣盤深度: {ask_depth:.2f}")
            logger.info(f"優化價格 - 買: {optimal_buy_price}, 賣: {optimal_sell_price}")
            
            # 快速成交優先，放寬所有條件
            if (bid_depth >= 10.0 and ask_depth >= 10.0 and  # 極低深度要求
                self.used_capital + order_value <= self.max_capital):  # 只檢查資金限制
                
                logger.info(f"條件滿足，準備下單: 買價 {optimal_buy_price}, 賣價 {optimal_sell_price}")
                await self.place_smart_orders(optimal_buy_price, optimal_sell_price)
            elif self.used_capital + order_value > self.max_capital:
                logger.warning(f"超過資金限制 ({self.used_capital + order_value:.2f} > {self.max_capital})，跳過下單")
            else:
                logger.warning(f"深度不足 - 買盤: {bid_depth:.2f}, 賣盤: {ask_depth:.2f}")
    
    def update_orderbook(self, bids, asks):
        """更新訂單簿"""
        # 簡化的訂單簿更新邏輯
        if bids:
            self.orderbook['bids'] = bids
        if asks:
            self.orderbook['asks'] = asks
    
    def process_order_update(self, data_list):
        """處理訂單狀態更新"""
        for order in data_list:
            order_id = order.get('orderId')
            status = order.get('status')
            
            if order_id in self.active_orders:
                order_info = self.active_orders[order_id]
                order_info['status'] = status
                logger.info(f"訂單 {order_id} 狀態更新: {status}")
                
                if status in ['filled', 'cancelled']:
                    # 釋放已使用資金
                    if status == 'cancelled':
                        self.used_capital -= order_info.get('order_value', 0)
                    del self.active_orders[order_id]
    
    def process_fill_update(self, data_list):
        """處理成交更新"""
        for fill in data_list:
            order_id = fill.get('orderId')
            trade_id = fill.get('tradeId')
            side = fill.get('side')
            size = float(fill.get('size', 0))
            amount = float(fill.get('amount', 0))
            fee_detail = fill.get('feeDetail', [])
            
            logger.info(f"成交通知 - 訂單: {order_id}, 交易: {trade_id}, 方向: {side}, 數量: {size}, 金額: {amount}")
            
            # 更新已使用資金（成交後釋放資金）
            self.used_capital = max(0, self.used_capital - amount)
            
            # 更新交易量
            self.total_volume += amount
            
            # 處理手續費
            for fee_info in fee_detail:
                if fee_info.get('deduction') == 'yes':
                    deduction_fee = float(fee_info.get('totalDeductionFee', 0))
                    self.fee_offset = max(0, self.fee_offset - deduction_fee)
                    logger.info(f"使用手續費抵扣: {deduction_fee}, 剩餘: {self.fee_offset}")
            
            logger.info(f"累計交易量: {self.total_volume:.2f} USDT, 已使用資金: {self.used_capital:.2f} USDT")
            
            if self.total_volume >= self.volume_limit:
                self.running = False
                logger.info("達到交易量限制，停止腳本")

    def calculate_optimal_prices(self, best_bid, best_ask, bids, asks):
        """快速成交優先的價格策略"""
        # 快速成交策略：直接使用市場價或略微激進的價格
        spread = best_ask - best_bid
        
        # 如果價差很小(<0.05)，使用市場價
        if spread <= 0.05:
            # 直接吃單，保證成交
            optimal_buy_price = best_ask  # 買入用賣一價
            optimal_sell_price = best_bid  # 賣出用買一價
        else:
            # 價差較大時，用中間價±一個tick
            mid_price = (best_bid + best_ask) / 2
            optimal_buy_price = mid_price + 0.01  # 略高於中間價
            optimal_sell_price = mid_price - 0.01  # 略低於中間價
        
        return optimal_buy_price, optimal_sell_price
    
    async def place_smart_orders(self, buy_price, sell_price):
        """快速成交優先策略"""
        # 積極取消所有舊訂單，重新以市場價下單
        if len(self.active_orders) > 0:
            logger.info("快速成交模式：取消所有舊訂單重新下單")
            await self.cancel_all_orders()
            await asyncio.sleep(0.1)  # 等待取消完成
        
        # 快速成交策略：直接下市價單或接近市價的限價單
        orders_to_place = []
        order_value = self.order_size * self.last_price
        
        # 檢查是否有足夠資金下雙邊訂單
        if self.used_capital + (order_value * 2) <= self.max_capital:
            # 有足夠資金，下雙邊訂單
            orders_to_place = [('buy', buy_price), ('sell', sell_price)]
        elif self.used_capital + order_value <= self.max_capital:
            # 只能下一邊，選擇更有利的一邊
            mid_price = (buy_price + sell_price) / 2
            if self.last_price <= mid_price:
                orders_to_place.append(('buy', buy_price))
            else:
                orders_to_place.append(('sell', sell_price))
        
        if not orders_to_place:
            logger.warning("因持倉限制，跳過下單")
            return
        
        # 並行下單
        tasks = [self.create_order(side, price, self.order_size) for side, price in orders_to_place]
        
        try:
            results = await asyncio.gather(*tasks, return_exceptions=True)
            
            for i, result in enumerate(results):
                if isinstance(result, Exception):
                    logger.error(f"下單錯誤: {result}")
                    continue
                    
                if result and result.get('code') == '00000':
                    side, price = orders_to_place[i]
                    order_id = result['data']['orderId']
                    
                    # 記錄活躍訂單並更新已使用資金
                    order_value = self.order_size * price
                    self.used_capital += order_value
                    
                    self.active_orders[order_id] = {
                        'side': side,
                        'price': price,
                        'size': self.order_size,
                        'status': 'live',
                        'timestamp': time.time(),
                        'reorder_count': 0,
                        'order_value': order_value
                    }
                    
                    logger.info(f"下{side}單成功: 訂單ID {order_id}, 價格 {price}, 數量 {self.order_size} BTC")
                else:
                    logger.error(f"下單失敗: {result}")
        except Exception as e:
            logger.error(f"下單過程出錯: {e}")

    async def create_order(self, side, price, amount):
        """創建限價單"""
        url = f"{self.api_url}/api/v2/spot/trade/place-order"  # v2 API端點
        timestamp = str(int(time.time() * 1000))
        params = {
            "symbol": self.symbol,
            "side": side,
            "orderType": "limit",
            "force": "gtc",  # 根據官方文檔，force是必需參數
            "price": str(round(price, 2)),
            "size": str(amount)
        }
        
        headers = {
            "ACCESS-KEY": self.api_key,
            "ACCESS-SIGN": self.sign(params, timestamp, "/api/v2/spot/trade/place-order"),
            "ACCESS-TIMESTAMP": timestamp,
            "ACCESS-PASSPHRASE": self.passphrase,
            "Content-Type": "application/json"
        }
        
        try:
            loop = asyncio.get_event_loop()
            response = await loop.run_in_executor(
                None, 
                lambda: requests.post(url, headers=headers, data=json.dumps(params), timeout=10)
            )
            
            if response.status_code == 200:
                result = response.json()
                return result
            else:
                logger.error(f"下單失敗 - 狀態碼: {response.status_code}, 響應: {response.text}")
                return None
        except Exception as e:
            logger.error(f"下單錯誤: {e}")
            return None

    def sign(self, params, timestamp, path):
        """生成API簽名"""
        method = "POST"
        body = json.dumps(params)
        sign_str = f"{timestamp}{method.upper()}{path}{body}"
        signature = hmac.new(
            self.api_secret.encode('utf-8'),
            sign_str.encode('utf-8'),
            hashlib.sha256
        ).digest()
        return base64.b64encode(signature).decode('utf-8')

    async def cancel_stale_orders(self):
        """取消超時訂單並重新下單"""
        current_time = time.time()
        orders_to_cancel = []
        reorder_info = []
        
        for order_id, order_info in self.active_orders.items():
            # 超過10秒的訂單取消並準備重新下單
            if current_time - order_info['timestamp'] > self.order_timeout:
                orders_to_cancel.append(order_id)
                # 記錄需要重新下單的信息
                if order_info['reorder_count'] < 3:  # 最多重新下單3次
                    reorder_info.append({
                        'side': order_info['side'],
                        'size': order_info['size'],
                        'reorder_count': order_info['reorder_count'] + 1
                    })
        
        # 取消超時訂單
        for order_id in orders_to_cancel:
            try:
                await self.cancel_order(order_id)
                logger.info(f"取消超時訂單: {order_id}")
            except Exception as e:
                logger.error(f"取消訂單 {order_id} 失敗: {e}")
        
        # 重新下單
        await self.reorder_cancelled_orders(reorder_info)
    
    async def reorder_cancelled_orders(self, reorder_info):
        """重新下單被取消的訂單"""
        if not reorder_info or not self.orderbook['bids'] or not self.orderbook['asks']:
            return
        
        best_bid = float(self.orderbook['bids'][0][0])
        best_ask = float(self.orderbook['asks'][0][0])
        
        tasks = []
        for order_info in reorder_info:
            # 更新價格為當前最優價格
            if order_info['side'] == 'buy':
                new_price = best_bid + 0.01
            else:
                new_price = best_ask - 0.01
            
            # 檢查資金限制
            order_value = order_info['size'] * self.last_price
            if self.used_capital + order_value <= self.max_capital:
                tasks.append(self.create_order(order_info['side'], new_price, order_info['size']))
        
        if tasks:
            try:
                results = await asyncio.gather(*tasks, return_exceptions=True)
                for i, result in enumerate(results):
                    if isinstance(result, Exception):
                        logger.error(f"重新下單錯誤: {result}")
                        continue
                    
                    if result and result.get('code') == '00000':
                        order_info = reorder_info[i]
                        order_id = result['data']['orderId']
                        price = best_bid + 0.01 if order_info['side'] == 'buy' else best_ask - 0.01
                        
                        # 更新已使用資金
                        order_value = order_info['size'] * price
                        self.used_capital += order_value
                        
                        self.active_orders[order_id] = {
                            'side': order_info['side'],
                            'price': price,
                            'size': order_info['size'],
                            'status': 'live',
                            'timestamp': time.time(),
                            'reorder_count': order_info['reorder_count'],
                            'order_value': order_value
                        }
                        
                        logger.info(f"重新下{order_info['side']}單成功: 訂單ID {order_id}, 價格 {price}")
            except Exception as e:
                logger.error(f"重新下單過程出錯: {e}")
    
    async def cancel_order(self, order_id):
        """取消訂單"""
        url = f"{self.api_url}/api/v2/spot/trade/cancel-order"  # v2 API端點
        timestamp = str(int(time.time() * 1000))
        params = {
            "symbol": self.symbol,
            "orderId": order_id
        }
        
        headers = {
            "ACCESS-KEY": self.api_key,
            "ACCESS-SIGN": self.sign(params, timestamp, "/api/v2/spot/trade/cancel-order"),
            "ACCESS-TIMESTAMP": timestamp,
            "ACCESS-PASSPHRASE": self.passphrase,
            "Content-Type": "application/json"
        }
        
        try:
            loop = asyncio.get_event_loop()
            response = await loop.run_in_executor(
                None,
                lambda: requests.post(url, headers=headers, data=json.dumps(params), timeout=10)
            )
            
            if response.status_code == 200:
                logger.info(f"取消訂單成功: {order_id}")
                if order_id in self.active_orders:
                    del self.active_orders[order_id]
            else:
                result = response.json()
                if result.get('code') == '43001':
                    # 訂單不存在通常表示已經成交
                    logger.info(f"訂單 {order_id} 已成交（訂單不存在）")
                    if order_id in self.active_orders:
                        del self.active_orders[order_id]
                else:
                    logger.error(f"取消訂單失敗: {response.text}")
        except Exception as e:
            logger.error(f"取消訂單錯誤: {e}")

    async def run(self):
        """運行腳本"""
        logger.info("啟動高頻交易機器人...")
        logger.info(f"交易對: {self.symbol}")
        logger.info(f"手續費抵扣: {self.fee_offset} USDT")
        logger.info(f"交易量限制: {self.volume_limit} USDT")
        
        # 創建任務
        tasks = [
            asyncio.create_task(self.connect_websocket()),
            asyncio.create_task(self.periodic_cleanup())
        ]
        
        try:
            await asyncio.gather(*tasks)
        except KeyboardInterrupt:
            logger.info("收到停止信號，正在關閉...")
            self.running = False
            # 取消所有活躍訂單
            await self.cancel_all_orders()
        except Exception as e:
            logger.error(f"運行錯誤: {e}")
            self.running = False
    
    async def periodic_cleanup(self):
        """定期清理任務"""
        while self.running:
            await asyncio.sleep(5)  # 每5秒執行一次，更頻繁的管理
            try:
                await self.cancel_stale_orders()
                logger.info(f"活躍訂單數量: {len(self.active_orders)}, 累計交易量: {self.total_volume:.2f} USDT, 已使用資金: {self.used_capital:.2f} USDT")
            except Exception as e:
                logger.error(f"定期清理錯誤: {e}")
    
    async def cancel_all_orders(self):
        """取消所有活躍訂單"""
        logger.info("正在取消所有活躍訂單...")
        for order_id in list(self.active_orders.keys()):
            try:
                await self.cancel_order(order_id)
            except Exception as e:
                logger.error(f"取消訂單 {order_id} 失敗: {e}")

if __name__ == "__main__":
    api_key = "bg_585f4261acbd87b8364c5f8cf872fe85"  # 替換為您的API Key
    api_secret = "9427d57804043465fcd5c73509291f9f8aaab23e76ed6074a8703805f48c026f"  # 替換為您的API Secret
    passphrase = "sogguagua"  # 替換為您的API Passphrase
    
    bot = BitgetHFTBot(
        api_key=api_key, 
        api_secret=api_secret, 
        passphrase=passphrase,
        fee_offset=69.0,  # 150U手續費抵扣
        volume_limit=500000.0  # 50萬U交易量限制
    )
    
    try:
        asyncio.run(bot.run())
    except KeyboardInterrupt:
        logger.info("程序被用戶中斷")
    except Exception as e:
        logger.error(f"程序異常退出: {e}")