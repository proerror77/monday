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
        self.order_size = 0.001  # 每單交易量（BTC）- scalping用固定小量
        self.spread = 0.001  # 價差（0.1%）
        self.min_depth = 10.0  # 訂單簿最小深度（USDT）- 極低門檻
        self.max_capital = 1200.0  # 最大資金限制（USDT）
        self.max_position_value = 1200.0  # 最大持倉價值限制（USDT）
        self.used_capital = 0.0  # 已使用資金
        self.btc_balance = 0.0  # BTC持倉數量（用於1200U限制）
        self.usdt_balance = 0.0  # USDT餘額
        self.order_timeout = 1.0  # 訂單超時時間（秒）- pure scalping極速成交
        
        # 交易損益統計
        self.total_trades = 0  # 總交易次數
        self.successful_trades = 0  # 成功交易次數
        self.total_profit = 0.0  # 總利潤（USDT）
        self.total_fees = 0.0  # 總手續費（USDT）
        self.initial_usdt = 0.0  # 初始USDT餘額
        self.initial_btc = 0.0  # 初始BTC持倉
        self.trade_history = []  # 交易歷史記錄
        self.running = True
        self.ws_url = 'wss://ws.bitget.com/v2/ws/private'  # v2 私有WebSocket端點
        self.public_ws_url = 'wss://ws.bitget.com/v2/ws/public'  # v2 公共WebSocket端點
        self.api_url = 'https://api.bitget.com'
        self.active_orders = {}  # 活躍訂單追蹤
        self.websocket = None
        self.orderbook = {'bids': [], 'asks': []}
        self.last_price = 0.0  # 最新價格
        self.order_management_task = None
    
    @property
    def position_value(self):
        """計算當前持倉價值"""
        return abs(self.btc_balance) * self.last_price
    
    async def authenticate_websocket(self, websocket):
        """WebSocket 認證"""
        timestamp = str(int(time.time() * 1000))
        sign_str = f"{timestamp}GET/user/verify"
        signature = hmac.new(
            self.api_secret.encode('utf-8'),
            sign_str.encode('utf-8'),
            hashlib.sha256
        ).digest()
        sign = base64.b64encode(signature).decode('utf-8')
        
        auth_msg = {
            "op": "login",
            "args": [{
                "apiKey": self.api_key,
                "passphrase": self.passphrase,
                "timestamp": timestamp,
                "sign": sign
            }]
        }
        
        await websocket.send(json.dumps(auth_msg))
        logger.info("發送 WebSocket 認證請求")
        
        # 等待認證回應
        auth_response = await websocket.recv()
        auth_data = json.loads(auth_response)
        
        if auth_data.get('event') == 'login' and auth_data.get('code') == 0:
            logger.info("WebSocket 認證成功")
        else:
            logger.error(f"WebSocket 認證失敗: {auth_data}")
            raise Exception("WebSocket authentication failed")

    async def connect_websocket(self):
        """連接到WebSocket並訂閱頻道"""
        try:
            async with websockets.connect(self.ws_url) as websocket:
                self.websocket = websocket
                
                # 先進行WebSocket認證
                await self.authenticate_websocket(websocket)
                
                # 認證成功後訂閱私有頻道
                # 訂閱訂單狀態頻道
                orders_msg = {
                    "op": "subscribe",
                    "args": [{
                        "instType": "SPOT",
                        "channel": "orders",
                        "instId": "BTCUSDT"
                    }]
                }
                await websocket.send(json.dumps(orders_msg))
                logger.info("已訂閱BTCUSDT訂單狀態頻道")
                
                # 訂閱賬戶餘額頻道
                account_msg = {
                    "op": "subscribe",
                    "args": [{
                        "instType": "SPOT",
                        "channel": "account",
                        "coin": "BTC"
                    }]
                }
                await websocket.send(json.dumps(account_msg))
                logger.info("已訂閱BTC賬戶餘額頻道")
                
                # 訂閱USDT賬戶餘額頻道  
                usdt_account_msg = {
                    "op": "subscribe",
                    "args": [{
                        "instType": "SPOT",
                        "channel": "account",
                        "coin": "USDT"
                    }]
                }
                await websocket.send(json.dumps(usdt_account_msg))
                logger.info("已訂閱USDT賬戶餘額頻道")

                while self.running:
                    try:
                        message = await websocket.recv()
                        logger.info(f"收到WebSocket消息: {message}")
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
    
    async def connect_public_websocket(self):
        """連接到公共WebSocket訂閱市場數據"""
        retry_count = 0
        max_retries = 3
        
        while retry_count < max_retries and self.running:
            try:
                logger.info(f"嘗試連接公共WebSocket... (第{retry_count + 1}次)")
                async with websockets.connect(self.public_ws_url, timeout=10) as websocket:
                    logger.info("公共WebSocket連接成功")
                    
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
                    logger.info("已訂閱BTCUSDT訂單簿頻道（公共）")

                    # 重置重試計數器，因為連接成功了
                    retry_count = 0
                    
                    while self.running:
                        try:
                            message = await websocket.recv()
                            logger.debug(f"收到公共WebSocket消息: {message}")
                            data = json.loads(message)
                            await self.handle_public_websocket_message(data)
                        except websockets.exceptions.ConnectionClosed:
                            logger.warning("公共WebSocket連接關閉，嘗試重連...")
                            await asyncio.sleep(5)
                            break
                        except Exception as e:
                            logger.error(f"公共WebSocket錯誤: {e}")
                            await asyncio.sleep(1)
                    
                    # 如果到這裡，說明連接被正常關閉，退出重試循環
                    break
                    
            except Exception as e:
                retry_count += 1
                logger.error(f"公共WebSocket連接失敗 (第{retry_count}次): {e}")
                if retry_count < max_retries:
                    logger.info(f"等待5秒後重試...")
                    await asyncio.sleep(5)
                else:
                    logger.error("公共WebSocket重試次數已用完，放棄連接")
                    break

    async def handle_public_websocket_message(self, data):
        """處理公共WebSocket消息"""
        if 'event' in data:
            logger.debug(f"公共事件消息: {data}")
            return
        
        if 'arg' not in data or 'data' not in data:
            logger.warning(f"收到非標準公共消息格式: {data}")
            return
            
        channel = data['arg'].get('channel')
        action = data.get('action')
        
        if channel == 'books5':
            await self.process_orderbook(data['data'], action)
        else:
            logger.warning(f"未知公共頻道: {channel}")

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
        
        logger.debug(f"處理頻道: {channel}, 動作: {action}")
        
        if channel == 'books5':
            await self.process_orderbook(data['data'], action)
        elif channel == 'orders':
            self.process_order_update(data['data'])
        elif channel == 'fill':
            self.process_fill_update(data['data'])
        elif channel == 'account':
            self.process_account_update(data['data'])
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
            self.update_orderbook(bids, asks)
        
        if self.orderbook['bids'] and self.orderbook['asks']:
            best_bid = float(self.orderbook['bids'][0][0])
            best_ask = float(self.orderbook['asks'][0][0])
            bid_depth = sum(float(bid[1]) * float(bid[0]) for bid in self.orderbook['bids'][:5])
            ask_depth = sum(float(ask[1]) * float(ask[0]) for ask in self.orderbook['asks'][:5])
            
            # 更新最新價格
            self.last_price = (best_bid + best_ask) / 2
            
            # Pure Scalping 策略：積極做市，賺取價差
            tick_size = 0.01  # 最小價格變動單位
            
            # Scalping 價格策略：緊貼盤口，快速成交
            optimal_buy_price, optimal_sell_price = self.calculate_optimal_prices(best_bid, best_ask, self.orderbook['bids'], self.orderbook['asks'])
            
            logger.info(f"Scalping - 買一: {best_bid}, 賣一: {best_ask}, 優化價格 - 買: {optimal_buy_price}, 賣: {optimal_sell_price}")
            logger.info(f"持倉狀況 - BTC: {self.btc_balance:.6f} ({self.position_value:.2f} USDT), 已用資金: {self.used_capital:.2f} USDT")
            
            # 如果BTC持倉很少但不為零，記錄但不強制清零（保持真實餘額）
            if self.btc_balance > 0 and self.btc_balance < 0.001:
                logger.warning(f"BTC持倉 {self.btc_balance:.6f} 小於最小交易量 0.001，暫時無法交易")
            
            # Pure Scalping 策略：風險控制優先
            order_value = self.order_size * self.last_price
            current_btc_value = self.btc_balance * self.last_price
            total_account_value = self.usdt_balance + current_btc_value
            btc_risk_ratio = current_btc_value / total_account_value if total_account_value > 0 else 0
            
            logger.info(f"風險分析 - BTC佔比: {btc_risk_ratio:.1%}, 總資產: {total_account_value:.2f} USDT")
            
            # 風險控制：BTC持倉超過30%時優先賣出
            if btc_risk_ratio > 0.3 and self.btc_balance >= self.order_size:
                logger.info(f"BTC佔比過高 ({btc_risk_ratio:.1%})，優先賣出平衡風險")
                await self.place_risk_control_sell(optimal_sell_price)
                
            # Scalping 條件：深度足夠且風險可控
            elif (bid_depth >= self.min_depth and ask_depth >= self.min_depth and
                  btc_risk_ratio <= 0.4):  # BTC佔比不超過40%
                
                logger.info(f"Scalping 條件滿足，準備下雙邊訂單: 買價 {optimal_buy_price}, 賣價 {optimal_sell_price}")
                await self.place_smart_orders(optimal_buy_price, optimal_sell_price)
                
            elif btc_risk_ratio > 0.4:
                logger.warning(f"BTC佔比過高 ({btc_risk_ratio:.1%})，暫停買入")
            else:
                logger.warning(f"深度不足 - 買盤: {bid_depth:.2f}, 賣盤: {ask_depth:.2f}")
    
    def update_orderbook(self, bids, asks):
        """更新訂單簿"""
        if bids:
            self.orderbook['bids'] = bids
        if asks:
            self.orderbook['asks'] = asks
    
    def calculate_optimal_prices(self, best_bid, best_ask, bids=None, asks=None):
        """Pure Scalping 價格策略：緊貼盤口，快速成交"""
        # Pure Scalping：積極搶佔最優位置
        tick_size = 0.01  # 最小價格變動
        
        # 檢查價差大小決定策略
        spread = best_ask - best_bid
        
        if spread <= 0.02:  # 價差很小時，直接吃單
            # 超窄價差：直接市價成交
            optimal_buy_price = best_ask  # 買入吃賣一
            optimal_sell_price = best_bid  # 賣出吃買一
        else:
            # 正常價差：緊貼盤口掛單
            optimal_buy_price = best_bid + tick_size  # 買單緊貼買一上方
            optimal_sell_price = best_ask - tick_size  # 賣單緊貼賣一下方
        
        return optimal_buy_price, optimal_sell_price
    
    async def place_smart_orders(self, buy_price, sell_price):
        """快速成交優先策略"""
        # 檢查是否需要取消訂單
        best_bid = float(self.orderbook['bids'][0][0]) if self.orderbook['bids'] else 0
        best_ask = float(self.orderbook['asks'][0][0]) if self.orderbook['asks'] else 0
        
        # 只取消價格不合理的訂單，保留有希望成交的賣單
        orders_to_cancel = []
        for order_id, order_info in self.active_orders.items():
            side = order_info.get('side')
            price = order_info.get('price', 0)
            
            # 取消條件：買單價格偏離太多，或賣單價格不合理
            if side == 'buy' and price < best_bid - 0.05:  # 買單太低
                orders_to_cancel.append(order_id)
            elif side == 'sell' and price > best_ask + 0.05:  # 賣單太高
                orders_to_cancel.append(order_id)
        
        # 只取消必要的訂單
        if orders_to_cancel:
            logger.info(f"Scalping模式：取消{len(orders_to_cancel)}個不合理訂單")
            for order_id in orders_to_cancel:
                try:
                    await self.cancel_order(order_id)
                except Exception as e:
                    logger.error(f"取消訂單失敗: {e}")
            await asyncio.sleep(0.1)  # 等待取消完成
        
        # 快速成交策略：直接下市價單或接近市價的限價單
        orders_to_place = []
        order_value = self.order_size * self.last_price
        
        # 風險控制：檢查BTC持倉是否過多
        btc_position_value = self.btc_balance * self.last_price
        max_btc_position = self.max_capital * 0.8  # 最大BTC持倉不超過80%資金
        
        # 智能選擇策略：根據持倉狀況決定
        should_prefer_sell = btc_position_value > max_btc_position or self.btc_balance > 0.003
        
        if should_prefer_sell:
            # BTC持倉過多，優先賣出，更激進的賣價
            aggressive_sell_price = best_bid + 0.01  # 比買一價高0.01，確保快速成交
            orders_to_place = [('sell', aggressive_sell_price)]
            logger.info(f"風險控制：BTC持倉過多 ({btc_position_value:.2f} USDT)，優先賣出 價格: {aggressive_sell_price}")
        elif self.used_capital + (order_value * 2) <= self.max_capital and btc_position_value < max_btc_position * 0.5:
            # 持倉安全且資金充足，下雙邊訂單
            orders_to_place = [('buy', buy_price), ('sell', sell_price)]
        elif self.used_capital + order_value <= self.max_capital:
            # 資金有限，選擇更有利的一邊
            if btc_position_value > self.max_capital * 0.3:  # 持倉超過30%，優先賣
                orders_to_place.append(('sell', sell_price))
            else:
                orders_to_place.append(('buy', buy_price))
        
        # 並行下單
        if orders_to_place:
            tasks = []
            for side, price in orders_to_place:
                tasks.append(self.create_order(side, price, self.order_size))
            
            try:
                results = await asyncio.gather(*tasks, return_exceptions=True)
                
                for i, result in enumerate(results):
                    side, price = orders_to_place[i]
                    if isinstance(result, Exception):
                        logger.error(f"Scalping {side}單錯誤: {result}")
                        continue
                        
                    if result and result.get('code') == '00000':
                        order_id = result['data']['orderId']
                        order_value = self.order_size * price
                        
                        # 記錄活躍訂單
                        self.active_orders[order_id] = {
                            'side': side,
                            'price': price,
                            'size': self.order_size,
                            'status': 'live',
                            'timestamp': time.time(),
                            'order_value': order_value
                        }
                        
                        if side == 'buy':
                            self.used_capital += order_value
                        
                        logger.info(f"Scalping {side}單成功: 訂單ID {order_id}, 價格 {price:.2f}, 數量 {self.order_size:.6f} BTC")
                    else:
                        logger.error(f"Scalping {side}單失敗: {result}")
            except Exception as e:
                logger.error(f"Scalping下單過程出錯: {e}")
        else:
            logger.info("資金不足，暫停scalping")

    def calculate_optimal_prices_original(self, best_bid, best_ask, bids, asks):
        """根據訂單簿計算最優掛單價格"""
        # 分析訂單簿深度分佈
        bid_volumes = [float(bid[1]) for bid in bids[:3]] if len(bids) >= 3 else [float(bid[1]) for bid in bids]
        ask_volumes = [float(ask[1]) for ask in asks[:3]] if len(asks) >= 3 else [float(ask[1]) for ask in asks]
        
        # 如果買一檔量很大，可以更激進地掛在買一價上方
        if len(bids) >= 2 and len(bid_volumes) >= 2 and bid_volumes[0] > bid_volumes[1] * 2:
            # 買一檔量大，激進掛單
            optimal_buy_price = best_bid + 0.01
        else:
            # 保守一點，掛在買二價上方
            optimal_buy_price = float(bids[1][0]) + 0.01 if len(bids) >= 2 else best_bid + 0.01
        
        # 賣盤同理
        if len(asks) >= 2 and len(ask_volumes) >= 2 and ask_volumes[0] > ask_volumes[1] * 2:
            optimal_sell_price = best_ask - 0.01
        else:
            optimal_sell_price = float(asks[1][0]) - 0.01 if len(asks) >= 2 else best_ask - 0.01
        
        return optimal_buy_price, optimal_sell_price
    
    def process_order_update(self, data_list):
        """處理訂單狀態更新"""
        for order in data_list:
            order_id = order.get('orderId')
            status = order.get('status')
            side = order.get('side')
            
            if order_id in self.active_orders:
                order_info = self.active_orders[order_id]
                old_status = order_info.get('status', 'unknown')
                order_info['status'] = status
                
                logger.info(f"訂單 {order_id} 狀態更新: {old_status} → {status} ({side})")
                
                # 任何狀態變化都立即更新餘額，確保數據準確
                if old_status != status:
                    asyncio.create_task(self.query_account_balance())
                
                # 處理訂單完成（成交或取消）
                if status in ['filled', 'cancelled']:
                    # 釋放已用資金（買單）
                    if side == 'buy' and 'order_value' in order_info:
                        self.used_capital = max(0, self.used_capital - order_info['order_value'])
                        logger.info(f"釋放買單資金: {order_info['order_value']:.2f} USDT, 剩餘已用資金: {self.used_capital:.2f} USDT")
                    
                    del self.active_orders[order_id]
            else:
                # 新訂單（可能是從WebSocket快照獲得的）
                logger.info(f"發現新訂單: {order_id} ({side}) 狀態: {status}")
                
                # 如果是live或partially_filled狀態，添加到活躍訂單中
                if status in ['live', 'partially_filled']:
                    self.active_orders[order_id] = {
                        'side': side,
                        'price': 0,  # WebSocket數據中獲取
                        'size': 0,
                        'status': status,
                        'timestamp': time.time(),
                        'order_value': 0
                    }
                    logger.debug(f"新訂單 {order_id} 添加到活躍列表")
                elif status in ['filled', 'cancelled']:
                    logger.debug(f"新訂單 {order_id} 已完成，不添加到活躍列表")
                
                asyncio.create_task(self.query_account_balance())
    
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
            
            # 記錄交易統計
            self.total_trades += 1
            trade_fee = 0.0
            
            # 處理手續費
            for fee_info in fee_detail:
                fee_amount = float(fee_info.get('fee', 0))
                trade_fee += abs(fee_amount)  # 手續費記為正數
                self.total_fees += abs(fee_amount)
                
                if fee_info.get('deduction') == 'yes':
                    deduction_fee = float(fee_info.get('totalDeductionFee', 0))
                    self.fee_offset = max(0, self.fee_offset - deduction_fee)
                    logger.info(f"使用手續費抵扣: {deduction_fee}, 剩餘: {self.fee_offset}")
            
            # 記錄交易歷史
            trade_record = {
                'timestamp': time.time(),
                'order_id': order_id,
                'trade_id': trade_id,
                'side': side,
                'size': size,
                'price': amount / size if size > 0 else 0,
                'amount': amount,
                'fee': trade_fee
            }
            self.trade_history.append(trade_record)
            
            # 計算當前總價值和利潤
            current_total_value = self.usdt_balance + (self.btc_balance * self.last_price)
            initial_total_value = self.initial_usdt + (self.initial_btc * self.last_price)
            self.total_profit = current_total_value - initial_total_value
            
            # 更新成功交易計數（簡單統計）
            if len(self.trade_history) >= 2:
                # 如果有買賣配對，視為成功交易
                recent_trades = self.trade_history[-2:]
                if len(set([t['side'] for t in recent_trades])) == 2:  # 有買有賣
                    self.successful_trades += 1
            
            # 更新交易量
            self.total_volume += amount
            
            logger.info(f"累計交易量: {self.total_volume:.2f} USDT, BTC持倉: {self.btc_balance:.6f}")
            logger.info(f"交易統計 - 總次數: {self.total_trades}, 成功: {self.successful_trades}, 利潤: {self.total_profit:.2f} USDT, 手續費: {self.total_fees:.4f} USDT")
            
            if self.total_volume >= self.volume_limit:
                self.running = False
                logger.info("達到交易量限制，停止腳本")
    
    def process_account_update(self, data_list):
        """處理賬戶更新"""
        for account in data_list:
            coin = account.get('coin')
            available = float(account.get('available', 0))
            
            if coin == 'BTC':
                old_balance = self.btc_balance
                self.btc_balance = available
                logger.info(f"BTC餘額更新: {old_balance:.6f} → {available:.6f}")
            elif coin == 'USDT':
                logger.info(f"USDT餘額: {available:.2f}")
            
            logger.debug(f"賬戶更新 - {coin}: 可用 {available}")

    async def place_maker_orders(self, order_tasks):
        """並行下做市商訂單"""
        tasks = [self.create_order(side, price, size) for side, price, size in order_tasks]
        
        try:
            results = await asyncio.gather(*tasks, return_exceptions=True)
            
            for i, result in enumerate(results):
                if isinstance(result, Exception):
                    logger.error(f"做市商下單錯誤: {result}")
                    continue
                    
                if result and result.get('code') == '00000':
                    side, price, size = order_tasks[i]
                    order_id = result['data']['orderId']
                    order_value = size * price
                    
                    # 記錄活躍訂單
                    self.active_orders[order_id] = {
                        'side': side,
                        'price': price,
                        'size': size,
                        'status': 'live',
                        'timestamp': time.time(),
                        'order_value': order_value
                    }
                    
                    if side == 'buy':
                        self.used_capital += order_value
                        # 買單成功，立即更新BTC餘額
                        await self.query_account_balance()
                    
                    logger.info(f"做市商{side}單成功: 訂單ID {order_id}, 價格 {price:.2f}, 數量 {size:.6f} BTC")
                elif result is None:
                    side, price, size = order_tasks[i]
                    logger.error(f"做市商{side}單失敗: 無回應 (價格: {price:.2f}, 數量: {size:.6f})")
                else:
                    side, price, size = order_tasks[i]
                    logger.error(f"做市商{side}單失敗: {result} (價格: {price:.2f}, 數量: {size:.6f})")
        except Exception as e:
            logger.error(f"做市商下單過程出錯: {e}")

    async def place_single_order(self, side, price, size):
        """下單個訂單（買入或賣出）"""
        try:
            result = await self.create_order(side, price, size)
            
            if result and result.get('code') == '00000':
                order_id = result['data']['orderId']
                order_value = size * price
                
                # 記錄活躍訂單
                self.active_orders[order_id] = {
                    'side': side,
                    'price': price,
                    'size': size,
                    'status': 'live',
                    'timestamp': time.time(),
                    'reorder_count': 0,
                    'order_value': order_value
                }
                
                logger.info(f"下{side}單成功: 訂單ID {order_id}, 價格 {price}, 數量 {size:.6f} BTC, 價值 {order_value:.2f} USDT")
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
            "size": str(round(amount, 6))  # BTC 數量保留6位小數
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

    def sign(self, params, timestamp, path, method="POST"):
        """生成API簽名"""
        if method == "GET" and params:
            # GET 請求需要將參數加到 path 中
            query_string = "&".join([f"{k}={v}" for k, v in params.items()])
            sign_path = f"{path}?{query_string}"
            body = ""
        else:
            sign_path = path
            body = json.dumps(params) if params else ""
            
        sign_str = f"{timestamp}{method.upper()}{sign_path}{body}"
        signature = hmac.new(
            self.api_secret.encode('utf-8'),
            sign_str.encode('utf-8'),
            hashlib.sha256
        ).digest()
        return base64.b64encode(signature).decode('utf-8')

    async def cancel_stale_orders(self):
        """取消超時訂單"""
        current_time = time.time()
        orders_to_cancel = []
        
        for order_id, order_info in self.active_orders.items():
            # 超過10秒的訂單取消
            if current_time - order_info['timestamp'] > self.order_timeout:
                orders_to_cancel.append(order_id)
        
        # 取消超時訂單
        for order_id in orders_to_cancel:
            try:
                await self.cancel_order(order_id)
                logger.info(f"取消超時訂單: {order_id}")
            except Exception as e:
                logger.error(f"取消訂單 {order_id} 失敗: {e}")
    
    async def check_order_status(self):
        """檢查活躍訂單狀態並查詢賬戶餘額"""
        # 總是查詢賬戶餘額
        await self.query_account_balance()
        
        # 如果有活躍訂單，檢查訂單狀態
        if self.active_orders:
            for order_id in list(self.active_orders.keys()):
                try:
                    order_status = await self.query_order_status(order_id)
                    if order_status:
                        await self.process_order_status_result(order_id, order_status)
                except Exception as e:
                    logger.error(f"查詢訂單 {order_id} 狀態失敗: {e}")
    
    async def query_account_balance(self):
        """查詢賬戶餘額"""
        url = f"{self.api_url}/api/v2/spot/account/assets"
        timestamp = str(int(time.time() * 1000))
        params = {}
        
        headers = {
            "ACCESS-KEY": self.api_key,
            "ACCESS-SIGN": self.sign(params, timestamp, "/api/v2/spot/account/assets", "GET"),
            "ACCESS-TIMESTAMP": timestamp,
            "ACCESS-PASSPHRASE": self.passphrase,
            "Content-Type": "application/json"
        }
        
        try:
            loop = asyncio.get_event_loop()
            response = await loop.run_in_executor(
                None,
                lambda: requests.get(url, headers=headers, timeout=10)
            )
            
            if response.status_code == 200:
                result = response.json()
                if result.get('code') == '00000':
                    btc_found = False
                    usdt_found = False
                    for asset in result.get('data', []):
                        if asset.get('coin') == 'BTC':
                            btc_found = True
                            old_balance = self.btc_balance
                            available = float(asset.get('available', 0))
                            frozen = float(asset.get('frozen', 0))
                            total = available + frozen
                            
                            self.btc_balance = available  # 只使用可用餘額
                            
                            # 記錄初始餘額（僅第一次）
                            if self.initial_btc == 0.0:
                                self.initial_btc = self.btc_balance
                            
                            if abs(old_balance - self.btc_balance) > 0.000001:
                                logger.info(f"BTC餘額更新: {old_balance:.6f} → {self.btc_balance:.6f} (可用: {available:.6f}, 凍結: {frozen:.6f}, 總計: {total:.6f})")
                        elif asset.get('coin') == 'USDT':
                            usdt_found = True
                            old_usdt = self.usdt_balance
                            available = float(asset.get('available', 0))
                            frozen = float(asset.get('frozen', 0))
                            total = available + frozen
                            
                            self.usdt_balance = available  # 只使用可用餘額
                            
                            # 記錄初始餘額（僅第一次）
                            if self.initial_usdt == 0.0:
                                self.initial_usdt = self.usdt_balance
                            
                            if abs(old_usdt - self.usdt_balance) > 0.01:
                                logger.info(f"USDT餘額更新: {old_usdt:.2f} → {self.usdt_balance:.2f} (可用: {available:.2f}, 凍結: {frozen:.2f}, 總計: {total:.2f})")
                    
                    if not btc_found:
                        logger.debug("賬戶中無BTC餘額")
                    if not usdt_found:
                        logger.debug("賬戶中無USDT餘額")
                else:
                    logger.error(f"查詢餘額失敗: {result}")
        except Exception as e:
            logger.error(f"查詢餘額錯誤: {e}")
    
    async def query_order_status(self, order_id):
        """查詢單個訂單狀態"""
        url = f"{self.api_url}/api/v2/spot/trade/orderInfo"
        timestamp = str(int(time.time() * 1000))
        params = {
            "symbol": self.symbol,
            "orderId": order_id
        }
        
        headers = {
            "ACCESS-KEY": self.api_key,
            "ACCESS-SIGN": self.sign(params, timestamp, "/api/v2/spot/trade/orderInfo", "GET"),
            "ACCESS-TIMESTAMP": timestamp,
            "ACCESS-PASSPHRASE": self.passphrase,
            "Content-Type": "application/json"
        }
        
        try:
            loop = asyncio.get_event_loop()
            response = await loop.run_in_executor(
                None,
                lambda: requests.get(url, headers=headers, params=params, timeout=10)
            )
            
            if response.status_code == 200:
                result = response.json()
                if result.get('code') == '00000':
                    data = result.get('data')
                    # API返回的data可能是列表，取第一個元素
                    if isinstance(data, list) and len(data) > 0:
                        return data[0]
                    elif isinstance(data, dict):
                        return data
                    else:
                        logger.warning(f"訂單查詢返回空數據: {data}")
                        return None
                else:
                    logger.error(f"查詢訂單失敗: {result}")
        except Exception as e:
            logger.error(f"查詢訂單錯誤: {e}")
        return None
    
    async def process_order_status_result(self, order_id, order_data):
        """處理訂單狀態查詢結果"""
        status = order_data.get('status')
        side = order_data.get('side')
        size = float(order_data.get('size', 0))
        filled_size = float(order_data.get('fillSize', 0))
        avg_price = float(order_data.get('priceAvg', 0))
        
        if status == 'filled':
            logger.info(f"訂單 {order_id} 已完全成交: {side} {filled_size:.6f} BTC @ {avg_price:.2f}")
            
            # 更新交易量
            amount = filled_size * avg_price
            self.total_volume += amount
            
            # 移除已成交訂單
            if order_id in self.active_orders:
                del self.active_orders[order_id]
                
        elif status == 'cancelled':
            logger.info(f"訂單 {order_id} 已取消")
            if order_id in self.active_orders:
                del self.active_orders[order_id]
                
        elif status in ['new', 'partial_filled']:
            logger.debug(f"訂單 {order_id} 狀態: {status}, 已成交: {filled_size:.6f}/{size:.6f}")
    
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
        logger.info(f"持倉價值限制: {self.max_position_value} USDT")
        
        # 初始化：查詢賬戶餘額和重置狀態
        logger.info("初始化賬戶餘額...")
        self.used_capital = 0.0  # 重置已用資金
        await self.query_account_balance()
        
        # 創建任務 - 同時運行公共和私有WebSocket
        tasks = [
            asyncio.create_task(self.connect_websocket()),        # 私有WebSocket (orders, account)
            asyncio.create_task(self.connect_public_websocket()), # 公共WebSocket (orderbook)
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
            await asyncio.sleep(0.5)  # 每0.5秒執行一次，pure scalping極頻繁管理
            try:
                # 更頻繁地更新餘額
                await self.query_account_balance()
                await self.cancel_stale_orders()
                await self.check_order_status()  # 檢查訂單狀態
                # 計算勝率和實時利潤
                win_rate = (self.successful_trades / max(1, self.total_trades // 2)) * 100  # 粗略計算勝率
                current_total_value = self.usdt_balance + (self.btc_balance * self.last_price)
                initial_total_value = self.initial_usdt + (self.initial_btc * self.last_price) if self.last_price > 0 else self.initial_usdt
                unrealized_pnl = current_total_value - initial_total_value
                
                logger.info(f"活躍訂單: {len(self.active_orders)}, 累計交易量: {self.total_volume:.2f} USDT")
                logger.info(f"持倉 - BTC: {self.btc_balance:.6f} ({self.position_value:.2f} USDT), USDT: {self.usdt_balance:.2f}")
                logger.info(f"交易統計 - 次數: {self.total_trades}, 成功: {self.successful_trades}, 勝率: {win_rate:.1f}%")
                logger.info(f"損益 - 未實現: {unrealized_pnl:.2f} USDT, 手續費: {self.total_fees:.4f} USDT, 淨利潤: {unrealized_pnl - self.total_fees:.2f} USDT")
            except Exception as e:
                logger.error(f"定期清理錯誤: {e}")
    
    async def cancel_stale_orders(self):
        """Pure Scalping: 取消超時訂單並立即重新下單"""
        current_time = time.time()
        orders_to_cancel = []
        reorder_info = []
        
        for order_id, order_info in self.active_orders.items():
            # Pure Scalping: 1秒超時，極速換單
            if current_time - order_info['timestamp'] > self.order_timeout:
                orders_to_cancel.append(order_id)
                # 記錄需要重新下單的信息
                if order_info.get('reorder_count', 0) < 5:  # 最多重新下單5次
                    reorder_info.append({
                        'side': order_info['side'],
                        'size': order_info['size'],
                        'reorder_count': order_info.get('reorder_count', 0) + 1
                    })
        
        # 取消超時訂單
        for order_id in orders_to_cancel:
            try:
                await self.cancel_order(order_id)
                logger.info(f"Pure Scalping: 取消超時訂單 {order_id}")
            except Exception as e:
                logger.error(f"取消訂單 {order_id} 失敗: {e}")
        
        # 立即重新下單
        await self.reorder_cancelled_orders(reorder_info)
    
    async def reorder_cancelled_orders(self, reorder_info):
        """Pure Scalping: 重新下單被取消的訂單"""
        if not reorder_info or not self.orderbook['bids'] or not self.orderbook['asks']:
            return
        
        best_bid = float(self.orderbook['bids'][0][0])
        best_ask = float(self.orderbook['asks'][0][0])
        
        # 重新計算最優價格
        optimal_buy_price, optimal_sell_price = self.calculate_optimal_prices(
            best_bid, best_ask, self.orderbook['bids'], self.orderbook['asks']
        )
        
        tasks = []
        for order_info in reorder_info:
            # 使用最新的最優價格
            if order_info['side'] == 'buy':
                new_price = optimal_buy_price
            else:
                new_price = optimal_sell_price
            
            # 檢查資金限制
            order_value = order_info['size'] * self.last_price
            if self.used_capital + order_value <= self.max_capital:
                tasks.append(self.create_order(order_info['side'], new_price, order_info['size']))
        
        if tasks:
            try:
                results = await asyncio.gather(*tasks, return_exceptions=True)
                for i, result in enumerate(results):
                    if isinstance(result, Exception):
                        logger.error(f"Pure Scalping重新下單錯誤: {result}")
                        continue
                    
                    if result and result.get('code') == '00000':
                        order_info = reorder_info[i]
                        order_id = result['data']['orderId']
                        price = optimal_buy_price if order_info['side'] == 'buy' else optimal_sell_price
                        
                        # 更新已使用資金
                        order_value = order_info['size'] * price
                        if order_info['side'] == 'buy':
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
                        
                        logger.info(f"Pure Scalping重新下{order_info['side']}單成功: 訂單ID {order_id}, 價格 {price:.2f}")
            except Exception as e:
                logger.error(f"Pure Scalping重新下單過程出錯: {e}")

    async def place_risk_control_sell(self, sell_price):
        """風險控制：優先賣出BTC平衡持倉"""
        try:
            # 風險控制賣單：賣出部分BTC降低單邊風險
            sell_amount = min(self.order_size * 2, self.btc_balance * 0.5)  # 賣出一半或固定量
            sell_amount = max(self.order_size, sell_amount)  # 至少賣出最小單位
            
            result = await self.create_order("sell", sell_price, sell_amount)
            
            if result and result.get('code') == '00000':
                order_id = result['data']['orderId']
                
                self.active_orders[order_id] = {
                    'side': 'sell',
                    'price': sell_price,
                    'size': sell_amount,
                    'status': 'live',
                    'timestamp': time.time(),
                    'risk_control': True  # 標記為風險控制訂單
                }
                
                logger.info(f"風險控制賣單成功: 訂單ID {order_id}, 價格 {sell_price:.2f}, 數量 {sell_amount:.6f} BTC")
            else:
                logger.error(f"風險控制賣單失敗: {result}")
                
        except Exception as e:
            logger.error(f"風險控制賣單錯誤: {e}")

    async def cancel_all_orders(self):
        """取消所有活躍訂單"""
        if not self.active_orders:
            return
            
        logger.info("正在取消所有活躍訂單...")
        orders_to_cancel = []
        
        # 只取消狀態為live的訂單
        for order_id, order_info in self.active_orders.items():
            status = order_info.get('status', 'unknown')
            if status in ['live', 'partially_filled']:
                orders_to_cancel.append(order_id)
            else:
                logger.debug(f"跳過訂單 {order_id}，狀態: {status}")
        
        for order_id in orders_to_cancel:
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
        fee_offset=69.0,  # 69U手續費抵扣
        volume_limit=500000.0  # 50萬U交易量限制
    )
    
    try:
        asyncio.run(bot.run())
    except KeyboardInterrupt:
        logger.info("程序被用戶中斷")
    except Exception as e:
        logger.error(f"程序異常退出: {e}")