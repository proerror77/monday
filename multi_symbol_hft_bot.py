import asyncio
import websockets
import json
import time
import logging
import hmac
import hashlib
import base64
import aiohttp
from collections import defaultdict, deque
from dataclasses import dataclass
from typing import Dict, List, Optional, Set
import weakref
import yaml

# 配置日誌
logging.basicConfig(level=logging.INFO, format='%(asctime)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)

@dataclass
class TradingPairConfig:
    """交易對配置"""
    symbol: str
    enabled: bool = True
    order_size: float = 0.001
    spread: float = 0.001
    min_depth: float = 1000.0
    max_orders: int = 4
    
@dataclass
class RiskConfig:
    """風險管理配置"""
    total_volume_limit: float = 500000.0
    fee_offset: float = 150.0
    position_limits: Dict[str, float] = None
    max_daily_trades: int = 1000

class RateLimiter:
    """API請求頻率限制器"""
    def __init__(self, max_requests_per_second: int = 10):
        self.max_requests = max_requests_per_second
        self.requests = deque()
        
    async def acquire(self):
        """獲取請求許可"""
        now = time.time()
        # 清理超過1秒的請求記錄
        while self.requests and now - self.requests[0] > 1.0:
            self.requests.popleft()
            
        if len(self.requests) >= self.max_requests:
            sleep_time = 1.0 - (now - self.requests[0])
            if sleep_time > 0:
                await asyncio.sleep(sleep_time)
                
        self.requests.append(now)

class WebSocketManager:
    """WebSocket連接管理器"""
    def __init__(self, config):
        self.config = config
        self.connections: Dict[str, websockets.WebSocketServerProtocol] = {}
        self.reconnect_delays = defaultdict(lambda: 1)
        self.max_reconnect_delay = 60
        self.subscriptions = defaultdict(set)
        self.running = True
        
    async def get_connection(self, url: str) -> websockets.WebSocketServerProtocol:
        """獲取或創建WebSocket連接"""
        if url not in self.connections or self.connections[url].closed:
            await self.create_connection(url)
        return self.connections[url]
        
    async def create_connection(self, url: str):
        """創建WebSocket連接"""
        try:
            websocket = await websockets.connect(url)
            self.connections[url] = websocket
            self.reconnect_delays[url] = 1
            logger.info(f"WebSocket連接成功: {url}")
            
            # 重新訂閱該連接的所有頻道
            await self.resubscribe(url)
            
        except Exception as e:
            logger.error(f"WebSocket連接失敗 {url}: {e}")
            # 指數退避重連
            delay = min(self.reconnect_delays[url], self.max_reconnect_delay)
            self.reconnect_delays[url] *= 2
            await asyncio.sleep(delay)
            
    async def subscribe(self, url: str, subscription: dict):
        """訂閱頻道"""
        self.subscriptions[url].add(json.dumps(subscription, sort_keys=True))
        websocket = await self.get_connection(url)
        await websocket.send(json.dumps(subscription))
        
    async def resubscribe(self, url: str):
        """重新訂閱所有頻道"""
        websocket = self.connections.get(url)
        if websocket and not websocket.closed:
            for sub_str in self.subscriptions[url]:
                subscription = json.loads(sub_str)
                await websocket.send(json.dumps(subscription))
                
    async def listen(self, url: str, message_handler):
        """監聽WebSocket消息"""
        while self.running:
            try:
                websocket = await self.get_connection(url)
                async for message in websocket:
                    try:
                        data = json.loads(message)
                        await message_handler(data)
                    except json.JSONDecodeError as e:
                        logger.error(f"JSON解析錯誤: {e}")
                    except Exception as e:
                        logger.error(f"消息處理錯誤: {e}")
            except websockets.exceptions.ConnectionClosed:
                logger.warning(f"WebSocket連接關閉: {url}")
                await asyncio.sleep(5)
            except Exception as e:
                logger.error(f"WebSocket監聽錯誤: {e}")
                await asyncio.sleep(5)

class AsyncAPIClient:
    """異步API客戶端"""
    def __init__(self, api_key: str, api_secret: str, passphrase: str = ""):
        self.api_key = api_key
        self.api_secret = api_secret
        self.passphrase = passphrase
        self.base_url = "https://api.bitget.com"
        self.session: Optional[aiohttp.ClientSession] = None
        self.rate_limiter = RateLimiter(10)  # 每秒10個請求
        
    async def __aenter__(self):
        connector = aiohttp.TCPConnector(
            limit=100,
            limit_per_host=20,
            ttl_dns_cache=300,
            use_dns_cache=True,
        )
        
        timeout = aiohttp.ClientTimeout(total=30, connect=5)
        
        self.session = aiohttp.ClientSession(
            connector=connector,
            timeout=timeout
        )
        return self
        
    async def __aexit__(self, exc_type, exc_val, exc_tb):
        if self.session:
            await self.session.close()
            
    def sign(self, params: dict, timestamp: str, method: str = "POST", path: str = "") -> str:
        """生成API簽名"""
        body = json.dumps(params) if params else ""
        sign_str = f"{timestamp}{method.upper()}{path}{body}"
        signature = hmac.new(
            self.api_secret.encode('utf-8'),
            sign_str.encode('utf-8'),
            hashlib.sha256
        ).digest()
        return base64.b64encode(signature).decode('utf-8')
        
    async def request(self, method: str, path: str, params: dict = None) -> dict:
        """發送API請求"""
        await self.rate_limiter.acquire()
        
        timestamp = str(int(time.time() * 1000))
        headers = {
            "ACCESS-KEY": self.api_key,
            "ACCESS-SIGN": self.sign(params, timestamp, method, path),
            "ACCESS-TIMESTAMP": timestamp,
            "ACCESS-PASSPHRASE": self.passphrase,
            "Content-Type": "application/json"
        }
        
        url = f"{self.base_url}{path}"
        
        try:
            async with self.session.request(
                method, url, 
                headers=headers, 
                json=params if params else None
            ) as response:
                result = await response.json()
                return result
        except Exception as e:
            logger.error(f"API請求錯誤 {method} {path}: {e}")
            raise

class OrderManager:
    """訂單管理器"""
    def __init__(self, api_client: AsyncAPIClient):
        self.api_client = api_client
        self.active_orders: Dict[str, dict] = {}
        self.order_history = deque(maxlen=10000)
        
    async def place_order(self, symbol: str, side: str, price: float, amount: float) -> Optional[dict]:
        """下單"""
        params = {
            "symbol": symbol,
            "side": side,
            "orderType": "limit",
            "force": "post_only",
            "price": str(round(price, 2)),
            "quantity": str(amount)
        }
        
        try:
            result = await self.api_client.request("POST", "/api/spot/v1/trade/orders", params)
            
            if result.get('code') == '00000':
                order_id = result['data']['orderId']
                order_info = {
                    'orderId': order_id,
                    'symbol': symbol,
                    'side': side,
                    'price': price,
                    'amount': amount,
                    'status': 'live',
                    'timestamp': time.time()
                }
                self.active_orders[order_id] = order_info
                self.order_history.append(order_info.copy())
                logger.info(f"下單成功: {symbol} {side} {price} {amount}")
                return result
            else:
                logger.error(f"下單失敗: {result}")
                return None
                
        except Exception as e:
            logger.error(f"下單錯誤: {e}")
            return None
            
    async def cancel_order(self, symbol: str, order_id: str) -> bool:
        """取消訂單"""
        params = {
            "symbol": symbol,
            "orderId": order_id
        }
        
        try:
            result = await self.api_client.request("POST", "/api/spot/v1/trade/cancel-order", params)
            
            if result.get('code') == '00000':
                if order_id in self.active_orders:
                    del self.active_orders[order_id]
                logger.info(f"取消訂單成功: {order_id}")
                return True
            else:
                logger.error(f"取消訂單失敗: {result}")
                return False
                
        except Exception as e:
            logger.error(f"取消訂單錯誤: {e}")
            return False
            
    async def cancel_old_orders(self, symbol: str, max_age: int = 30):
        """取消超時訂單"""
        current_time = time.time()
        orders_to_cancel = []
        
        for order_id, order_info in self.active_orders.items():
            if (order_info['symbol'] == symbol and 
                current_time - order_info['timestamp'] > max_age):
                orders_to_cancel.append(order_id)
                
        for order_id in orders_to_cancel:
            await self.cancel_order(symbol, order_id)
            
    def get_active_orders_count(self, symbol: str) -> int:
        """獲取活躍訂單數量"""
        return sum(1 for order in self.active_orders.values() 
                  if order['symbol'] == symbol)
                  
    def update_order_status(self, order_id: str, status: str):
        """更新訂單狀態"""
        if order_id in self.active_orders:
            self.active_orders[order_id]['status'] = status
            if status in ['filled', 'cancelled']:
                del self.active_orders[order_id]

class RiskManager:
    """風險管理器"""
    def __init__(self, config: RiskConfig):
        self.config = config
        self.total_volume = 0.0
        self.daily_trades = 0
        self.positions = defaultdict(float)
        self.fee_offset = config.fee_offset
        
    def check_order_risk(self, symbol: str, side: str, amount: float, price: float) -> bool:
        """檢查訂單風險"""
        # 檢查交易量限制
        if self.total_volume >= self.config.total_volume_limit:
            logger.warning("達到總交易量限制")
            return False
            
        # 檢查每日交易次數
        if self.daily_trades >= self.config.max_daily_trades:
            logger.warning("達到每日交易次數限制")
            return False
            
        # 檢查持倉限制
        if self.config.position_limits:
            current_position = self.positions[symbol]
            position_change = amount if side == 'buy' else -amount
            new_position = abs(current_position + position_change)
            
            if symbol in self.config.position_limits:
                if new_position > self.config.position_limits[symbol]:
                    logger.warning(f"超過{symbol}持倉限制")
                    return False
                    
        return True
        
    def update_position(self, symbol: str, side: str, amount: float):
        """更新持倉"""
        if side == 'buy':
            self.positions[symbol] += amount
        else:
            self.positions[symbol] -= amount
            
    def update_volume(self, volume: float):
        """更新交易量"""
        self.total_volume += volume
        self.daily_trades += 1
        
    def update_fee_offset(self, fee: float):
        """更新手續費抵扣"""
        self.fee_offset = max(0, self.fee_offset - fee)
        
    def should_stop_trading(self) -> bool:
        """是否應該停止交易"""
        return self.total_volume >= self.config.total_volume_limit

class SymbolManager:
    """單個交易對管理器"""
    def __init__(self, config: TradingPairConfig, 
                 order_manager: OrderManager, 
                 risk_manager: RiskManager):
        self.config = config
        self.order_manager = order_manager
        self.risk_manager = risk_manager
        self.orderbook = {'bids': [], 'asks': []}
        self.last_order_time = 0
        self.min_order_interval = 1.0  # 最小下單間隔(秒)
        
    async def process_orderbook(self, data: dict):
        """處理訂單簿數據"""
        if not data:
            return
            
        bids = data.get('bids', [])
        asks = data.get('asks', [])
        
        if not bids or not asks:
            return
            
        self.orderbook = {'bids': bids, 'asks': asks}
        
        # 計算最佳買賣價
        best_bid = float(bids[0][0])
        best_ask = float(asks[0][0])
        
        # 計算深度
        bid_depth = sum(float(bid[1]) * float(bid[0]) for bid in bids[:5])
        ask_depth = sum(float(ask[1]) * float(ask[0]) for ask in asks[:5])
        
        # 檢查是否滿足交易條件
        if await self.should_place_orders(best_bid, best_ask, bid_depth, ask_depth):
            await self.place_market_making_orders(best_bid, best_ask)
            
    async def should_place_orders(self, best_bid: float, best_ask: float, 
                                bid_depth: float, ask_depth: float) -> bool:
        """判斷是否應該下單"""
        # 檢查深度是否足夠
        if bid_depth < self.config.min_depth or ask_depth < self.config.min_depth:
            return False
            
        # 檢查價差是否合理
        spread_pct = (best_ask - best_bid) / best_bid
        if spread_pct < 0.0005:  # 最小0.05%價差
            return False
            
        # 檢查活躍訂單數量
        if self.order_manager.get_active_orders_count(self.config.symbol) >= self.config.max_orders:
            return False
            
        # 檢查下單間隔
        current_time = time.time()
        if current_time - self.last_order_time < self.min_order_interval:
            return False
            
        # 檢查風險管理
        if self.risk_manager.should_stop_trading():
            return False
            
        return True
        
    async def place_market_making_orders(self, best_bid: float, best_ask: float):
        """放置市場製造訂單"""
        # 計算下單價格
        buy_price = best_bid + 0.01  # 略高於最佳買價
        sell_price = best_ask - 0.01  # 略低於最佳賣價
        
        # 風險檢查
        if not self.risk_manager.check_order_risk(
            self.config.symbol, 'buy', self.config.order_size, buy_price):
            return
            
        if not self.risk_manager.check_order_risk(
            self.config.symbol, 'sell', self.config.order_size, sell_price):
            return
            
        # 異步下買賣單
        tasks = [
            self.order_manager.place_order(
                self.config.symbol, 'buy', buy_price, self.config.order_size),
            self.order_manager.place_order(
                self.config.symbol, 'sell', sell_price, self.config.order_size)
        ]
        
        try:
            results = await asyncio.gather(*tasks, return_exceptions=True)
            success_count = sum(1 for result in results 
                              if not isinstance(result, Exception) and result)
            
            if success_count > 0:
                self.last_order_time = time.time()
                logger.info(f"{self.config.symbol}: 成功下單 {success_count} 個")
                
        except Exception as e:
            logger.error(f"{self.config.symbol} 下單錯誤: {e}")
            
    async def cleanup(self):
        """清理過期訂單"""
        await self.order_manager.cancel_old_orders(self.config.symbol)

class MultiSymbolHFTBot:
    """多交易對高頻交易機器人"""
    def __init__(self, config_file: str):
        self.config = self.load_config(config_file)
        self.ws_manager = WebSocketManager(self.config)
        self.api_client = None
        self.order_manager = None
        self.risk_manager = RiskManager(
            RiskConfig(**self.config.get('risk_management', {}))
        )
        self.symbol_managers: Dict[str, SymbolManager] = {}
        self.running = True
        
    def load_config(self, config_file: str) -> dict:
        """加載配置文件"""
        try:
            with open(config_file, 'r', encoding='utf-8') as f:
                return yaml.safe_load(f)
        except Exception as e:
            logger.error(f"加載配置文件失敗: {e}")
            return {}
            
    async def initialize(self):
        """初始化"""
        # 初始化API客戶端
        api_config = self.config.get('api', {})
        self.api_client = AsyncAPIClient(
            api_config.get('api_key', ''),
            api_config.get('api_secret', ''),
            api_config.get('passphrase', '')
        )
        
        # 初始化訂單管理器
        self.order_manager = OrderManager(self.api_client)
        
        # 初始化交易對管理器
        trading_pairs = self.config.get('trading_pairs', {})
        for symbol, pair_config in trading_pairs.items():
            if pair_config.get('enabled', True):
                config = TradingPairConfig(symbol=symbol, **pair_config)
                manager = SymbolManager(config, self.order_manager, self.risk_manager)
                self.symbol_managers[symbol] = manager
                
        logger.info(f"初始化完成，支持 {len(self.symbol_managers)} 個交易對")
        
    async def handle_websocket_message(self, data: dict):
        """處理WebSocket消息"""
        if 'arg' not in data or 'data' not in data:
            return
            
        channel = data['arg'].get('channel')
        inst_id = data['arg'].get('instId')
        
        if channel == 'books5' and inst_id in self.symbol_managers:
            await self.symbol_managers[inst_id].process_orderbook(data['data'][0])
        elif channel == 'orders':
            self.process_order_update(data['data'])
        elif channel == 'fill':
            self.process_fill_update(data['data'])
            
    def process_order_update(self, data_list: List[dict]):
        """處理訂單更新"""
        for order in data_list:
            order_id = order.get('orderId')
            status = order.get('status')
            if order_id:
                self.order_manager.update_order_status(order_id, status)
                
    def process_fill_update(self, data_list: List[dict]):
        """處理成交更新"""
        for fill in data_list:
            symbol = fill.get('symbol')
            amount = float(fill.get('amount', 0))
            side = fill.get('side')
            size = float(fill.get('size', 0))
            
            # 更新風險管理
            self.risk_manager.update_volume(amount)
            self.risk_manager.update_position(symbol, side, size)
            
            # 處理手續費
            fee_detail = fill.get('feeDetail', [])
            for fee_info in fee_detail:
                if fee_info.get('deduction') == 'yes':
                    deduction_fee = float(fee_info.get('totalDeductionFee', 0))
                    self.risk_manager.update_fee_offset(deduction_fee)
                    
            logger.info(f"成交: {symbol} {side} {size} @ {amount}")
            
    async def subscribe_channels(self):
        """訂閱WebSocket頻道"""
        ws_url = 'wss://ws.bitget.com/spot/v1/stream'
        
        # 訂閱各交易對的訂單簿
        for symbol in self.symbol_managers.keys():
            await self.ws_manager.subscribe(ws_url, {
                "op": "subscribe",
                "args": [{
                    "instType": "SPOT",
                    "channel": "books5",
                    "instId": symbol
                }]
            })
            
        # 訂閱訂單狀態
        await self.ws_manager.subscribe(ws_url, {
            "op": "subscribe",
            "args": [{
                "instType": "SPOT",
                "channel": "orders",
                "instId": "default"
            }]
        })
        
        # 訂閱成交明細
        await self.ws_manager.subscribe(ws_url, {
            "op": "subscribe",
            "args": [{
                "instType": "SPOT",
                "channel": "fill",
                "instId": "default"
            }]
        })
        
    async def periodic_cleanup(self):
        """定期清理任務"""
        while self.running:
            await asyncio.sleep(30)
            try:
                # 清理各交易對的過期訂單
                for manager in self.symbol_managers.values():
                    await manager.cleanup()
                    
                # 記錄狀態
                active_orders = len(self.order_manager.active_orders)
                total_volume = self.risk_manager.total_volume
                logger.info(f"活躍訂單: {active_orders}, 累計交易量: {total_volume:.2f} USDT")
                
                # 檢查是否需要停止
                if self.risk_manager.should_stop_trading():
                    logger.info("達到交易限制，停止交易")
                    self.running = False
                    
            except Exception as e:
                logger.error(f"定期清理錯誤: {e}")
                
    async def run(self):
        """運行機器人"""
        async with self.api_client:
            await self.initialize()
            await self.subscribe_channels()
            
            logger.info("啟動多交易對高頻交易機器人...")
            
            # 創建任務
            tasks = [
                asyncio.create_task(self.ws_manager.listen(
                    'wss://ws.bitget.com/spot/v1/stream', 
                    self.handle_websocket_message
                )),
                asyncio.create_task(self.periodic_cleanup())
            ]
            
            try:
                await asyncio.gather(*tasks)
            except KeyboardInterrupt:
                logger.info("收到停止信號...")
                self.running = False
                # 取消所有活躍訂單
                for symbol in self.symbol_managers.keys():
                    await self.order_manager.cancel_old_orders(symbol, max_age=0)
            except Exception as e:
                logger.error(f"運行錯誤: {e}")
                self.running = False

if __name__ == "__main__":
    bot = MultiSymbolHFTBot("config.yaml")
    try:
        asyncio.run(bot.run())
    except KeyboardInterrupt:
        logger.info("程序被用戶中斷")
    except Exception as e:
        logger.error(f"程序異常退出: {e}")