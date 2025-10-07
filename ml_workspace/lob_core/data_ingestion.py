#!/usr/bin/env python3
"""
LOB数据摄取系统
支持ITCH, LOBSTER等数据源的解析和重构
"""

import struct
import gzip
import pandas as pd
import numpy as np
from decimal import Decimal
from typing import Iterator, Dict, List, Optional, Union, BinaryIO
from pathlib import Path
import warnings
from .data_structures import (
    LOBMessage, LOBSnapshot, PriceLevel, Trade, InformationBar,
    MessageType, Side, LOBDataContainer
)

class ITCHParser:
    """
    NASDAQ ITCH 5.0解析器
    解码二进制ITCH流，重建完整深度和订单生命周期
    """

    # ITCH 5.0消息类型
    MESSAGE_TYPES = {
        b'S': 'system_event',
        b'R': 'stock_directory',
        b'H': 'stock_trading_action',
        b'Y': 'reg_sho_restriction',
        b'L': 'market_participant_position',
        b'V': 'mwcb_decline_level',
        b'W': 'mwcb_status',
        b'K': 'ipo_quoting_period_update',
        b'J': 'luld_auction_collar',
        b'A': 'add_order',
        b'F': 'add_order_mpid',
        b'E': 'order_executed',
        b'C': 'order_executed_with_price',
        b'X': 'order_cancel',
        b'D': 'order_delete',
        b'U': 'order_replace',
        b'P': 'trade',
        b'Q': 'cross_trade',
        b'B': 'broken_trade',
        b'I': 'noii'
    }

    def __init__(self):
        self.orders: Dict[int, Dict] = {}  # 活跃订单跟踪
        self.message_count = 0

    def parse_itch_file(self, file_path: Union[str, Path]) -> Iterator[LOBMessage]:
        """
        解析ITCH文件，产生LOB消息流
        """
        file_path = Path(file_path)

        if file_path.suffix == '.gz':
            file_handle = gzip.open(file_path, 'rb')
        else:
            file_handle = open(file_path, 'rb')

        try:
            yield from self._parse_stream(file_handle)
        finally:
            file_handle.close()

    def _parse_stream(self, stream: BinaryIO) -> Iterator[LOBMessage]:
        """解析二进制流"""
        while True:
            # 读取消息长度
            length_data = stream.read(2)
            if len(length_data) < 2:
                break

            message_length = struct.unpack('>H', length_data)[0]

            # 读取消息内容
            message_data = stream.read(message_length)
            if len(message_data) < message_length:
                break

            # 解析消息
            try:
                message = self._parse_message(message_data)
                if message:
                    yield message
                    self.message_count += 1
            except Exception as e:
                warnings.warn(f"消息解析失败: {e}")
                continue

    def _parse_message(self, data: bytes) -> Optional[LOBMessage]:
        """解析单个消息"""
        if len(data) < 1:
            return None

        message_type = data[0:1]

        if message_type == b'A':  # Add Order
            return self._parse_add_order(data)
        elif message_type == b'E':  # Order Executed
            return self._parse_order_executed(data)
        elif message_type == b'X':  # Order Cancel
            return self._parse_order_cancel(data)
        elif message_type == b'D':  # Order Delete
            return self._parse_order_delete(data)
        elif message_type == b'U':  # Order Replace
            return self._parse_order_replace(data)
        elif message_type == b'P':  # Trade
            return self._parse_trade(data)

        return None

    def _parse_add_order(self, data: bytes) -> LOBMessage:
        """解析Add Order消息 (Type A)"""
        # ITCH 5.0 Add Order格式:
        # Message Type (1) + Stock Locate (2) + Tracking Number (2) +
        # Timestamp (6) + Order Reference Number (8) + Buy/Sell (1) +
        # Shares (4) + Stock (8) + Price (4)

        fields = struct.unpack('>cHHQ8sQ1sI8sI', data)
        msg_type, stock_locate, tracking_num, timestamp, order_ref, \
        buy_sell, shares, stock, price = fields

        # 转换字段
        side = Side.BID if buy_sell == b'B' else Side.ASK
        price_decimal = Decimal(price) / Decimal('10000')  # ITCH价格是4位小数

        # 存储订单信息
        self.orders[order_ref] = {
            'price': price_decimal,
            'size': shares,
            'side': side,
            'stock': stock.decode('ascii').strip()
        }

        return LOBMessage(
            timestamp=timestamp,
            message_type=MessageType.ADD,
            order_id=order_ref,
            side=side,
            price=price_decimal,
            size=shares
        )

    def _parse_order_executed(self, data: bytes) -> LOBMessage:
        """解析Order Executed消息 (Type E)"""
        fields = struct.unpack('>cHHQ8sQI', data)
        msg_type, stock_locate, tracking_num, timestamp, order_ref, executed_shares = fields

        if order_ref in self.orders:
            order_info = self.orders[order_ref]

            # 更新订单剩余数量
            self.orders[order_ref]['size'] -= executed_shares
            if self.orders[order_ref]['size'] <= 0:
                del self.orders[order_ref]

            return LOBMessage(
                timestamp=timestamp,
                message_type=MessageType.EXECUTE,
                order_id=order_ref,
                side=order_info['side'],
                price=order_info['price'],
                size=executed_shares
            )

        return None

    def _parse_order_cancel(self, data: bytes) -> LOBMessage:
        """解析Order Cancel消息 (Type X)"""
        fields = struct.unpack('>cHHQ8sQI', data)
        msg_type, stock_locate, tracking_num, timestamp, order_ref, cancelled_shares = fields

        if order_ref in self.orders:
            order_info = self.orders[order_ref]

            # 更新订单剩余数量
            self.orders[order_ref]['size'] -= cancelled_shares
            if self.orders[order_ref]['size'] <= 0:
                del self.orders[order_ref]

            return LOBMessage(
                timestamp=timestamp,
                message_type=MessageType.CANCEL,
                order_id=order_ref,
                side=order_info['side'],
                price=order_info['price'],
                size=cancelled_shares
            )

        return None

    def _parse_order_delete(self, data: bytes) -> LOBMessage:
        """解析Order Delete消息 (Type D)"""
        fields = struct.unpack('>cHHQ8sQ', data)
        msg_type, stock_locate, tracking_num, timestamp, order_ref = fields

        if order_ref in self.orders:
            order_info = self.orders[order_ref]
            del self.orders[order_ref]

            return LOBMessage(
                timestamp=timestamp,
                message_type=MessageType.CANCEL,
                order_id=order_ref,
                side=order_info['side'],
                price=order_info['price'],
                size=order_info['size']
            )

        return None

    def _parse_order_replace(self, data: bytes) -> LOBMessage:
        """解析Order Replace消息 (Type U)"""
        fields = struct.unpack('>cHHQ8sQQII', data)
        msg_type, stock_locate, tracking_num, timestamp, \
        original_order_ref, new_order_ref, shares, price = fields

        if original_order_ref in self.orders:
            old_order = self.orders[original_order_ref]
            del self.orders[original_order_ref]

            # 创建新订单
            price_decimal = Decimal(price) / Decimal('10000')
            self.orders[new_order_ref] = {
                'price': price_decimal,
                'size': shares,
                'side': old_order['side'],
                'stock': old_order['stock']
            }

            return LOBMessage(
                timestamp=timestamp,
                message_type=MessageType.MODIFY,
                order_id=new_order_ref,
                side=old_order['side'],
                price=price_decimal,
                size=shares
            )

        return None

    def _parse_trade(self, data: bytes) -> LOBMessage:
        """解析Trade消息 (Type P)"""
        fields = struct.unpack('>cHHQ8sQ1sI8sI', data)
        msg_type, stock_locate, tracking_num, timestamp, order_ref, \
        buy_sell, shares, stock, price = fields

        side = Side.BID if buy_sell == b'B' else Side.ASK
        price_decimal = Decimal(price) / Decimal('10000')

        return LOBMessage(
            timestamp=timestamp,
            message_type=MessageType.EXECUTE,
            order_id=order_ref,
            side=side,
            price=price_decimal,
            size=shares
        )

class LOBSTERLoader:
    """
    LOBSTER数据加载器
    LOBSTER提供重构好的LOB数据
    """

    def __init__(self):
        self.current_symbol = None

    def load_lobster_data(self, orderbook_file: str, message_file: str,
                         symbol: str) -> LOBDataContainer:
        """
        加载LOBSTER格式的数据文件

        Args:
            orderbook_file: _orderbook_*.csv文件路径
            message_file: _message_*.csv文件路径
            symbol: 股票代码

        Returns:
            LOBDataContainer: 包含消息和快照的容器
        """
        self.current_symbol = symbol
        container = LOBDataContainer(symbol)

        # 加载订单簿数据（快照）
        orderbook_df = pd.read_csv(orderbook_file, header=None)
        snapshots = self._parse_orderbook_data(orderbook_df)

        # 加载消息数据
        message_df = pd.read_csv(message_file, header=None)
        messages = self._parse_message_data(message_df)

        # 添加到容器
        for snapshot in snapshots:
            container.add_snapshot(snapshot)

        for message in messages:
            container.add_message(message)

        return container

    def _parse_orderbook_data(self, df: pd.DataFrame) -> List[LOBSnapshot]:
        """解析LOBSTER订单簿数据"""
        snapshots = []

        for idx, row in df.iterrows():
            timestamp = int(row[0] * 1_000_000_000)  # 转换为纳秒

            # LOBSTER格式：每行包含多个价格级别
            # 格式：Ask Price 1, Ask Size 1, Bid Price 1, Bid Size 1, ...
            bids = []
            asks = []

            # 解析价格级别（假设10级深度）
            num_levels = (len(row) - 1) // 4

            for i in range(num_levels):
                ask_price = Decimal(str(row[1 + i * 4]))
                ask_size = int(row[2 + i * 4])
                bid_price = Decimal(str(row[3 + i * 4]))
                bid_size = int(row[4 + i * 4])

                if ask_price > 0 and ask_size > 0:
                    asks.append(PriceLevel(ask_price, ask_size))

                if bid_price > 0 and bid_size > 0:
                    bids.append(PriceLevel(bid_price, bid_size))

            # 排序：bids降序，asks升序
            bids.sort(key=lambda x: x.price, reverse=True)
            asks.sort(key=lambda x: x.price)

            snapshot = LOBSnapshot(
                timestamp=timestamp,
                symbol=self.current_symbol,
                bids=bids,
                asks=asks
            )

            snapshots.append(snapshot)

        return snapshots

    def _parse_message_data(self, df: pd.DataFrame) -> List[LOBMessage]:
        """解析LOBSTER消息数据"""
        messages = []

        for idx, row in df.iterrows():
            timestamp = int(row[0] * 1_000_000_000)  # 转换为纳秒
            event_type = int(row[1])
            order_id = int(row[2]) if pd.notna(row[2]) else None
            size = int(row[3])
            price = Decimal(str(row[4]))
            direction = int(row[5]) if len(row) > 5 and pd.notna(row[5]) else 1

            # LOBSTER事件类型映射
            if event_type == 1:  # Submission
                msg_type = MessageType.ADD
            elif event_type == 2:  # Cancellation
                msg_type = MessageType.CANCEL
            elif event_type == 3:  # Deletion
                msg_type = MessageType.CANCEL
            elif event_type == 4:  # Execution visible
                msg_type = MessageType.EXECUTE
            elif event_type == 5:  # Execution hidden
                msg_type = MessageType.EXECUTE
            else:
                continue

            # 方向：1=买，-1=卖
            side = Side.BID if direction == 1 else Side.ASK

            message = LOBMessage(
                timestamp=timestamp,
                message_type=msg_type,
                order_id=order_id,
                side=side,
                price=price,
                size=size
            )

            messages.append(message)

        return messages

class InformationBarBuilder:
    """
    信息驱动的Bar构建器
    构建tick/volume/dollar bars来规范化采样
    """

    def __init__(self, symbol: str):
        self.symbol = symbol
        self.reset()

    def reset(self):
        """重置构建器状态"""
        self.current_bar_data = {
            'trades': [],
            'snapshots': [],
            'start_time': None,
            'end_time': None
        }

    def build_tick_bars(self, trades: List[Trade], tick_threshold: int = 100) -> List[InformationBar]:
        """构建tick bars"""
        bars = []
        current_trades = []

        for trade in trades:
            current_trades.append(trade)

            if len(current_trades) >= tick_threshold:
                bar = self._create_bar_from_trades(current_trades, "tick", tick_threshold)
                bars.append(bar)
                current_trades = []

        # 处理剩余交易
        if current_trades:
            bar = self._create_bar_from_trades(current_trades, "tick", tick_threshold)
            bars.append(bar)

        return bars

    def build_volume_bars(self, trades: List[Trade], volume_threshold: int = 10000) -> List[InformationBar]:
        """构建volume bars"""
        bars = []
        current_trades = []
        current_volume = 0

        for trade in trades:
            current_trades.append(trade)
            current_volume += trade.size

            if current_volume >= volume_threshold:
                bar = self._create_bar_from_trades(current_trades, "volume", volume_threshold)
                bars.append(bar)
                current_trades = []
                current_volume = 0

        # 处理剩余交易
        if current_trades:
            bar = self._create_bar_from_trades(current_trades, "volume", volume_threshold)
            bars.append(bar)

        return bars

    def build_dollar_bars(self, trades: List[Trade], dollar_threshold: float = 1000000.0) -> List[InformationBar]:
        """
        构建dollar bars
        使用日均成交额的1/50作为起始阈值
        """
        bars = []
        current_trades = []
        current_dollar_volume = 0.0

        for trade in trades:
            current_trades.append(trade)
            current_dollar_volume += float(trade.price) * trade.size

            if current_dollar_volume >= dollar_threshold:
                bar = self._create_bar_from_trades(current_trades, "dollar", dollar_threshold)
                bars.append(bar)
                current_trades = []
                current_dollar_volume = 0.0

        # 处理剩余交易
        if current_trades:
            bar = self._create_bar_from_trades(current_trades, "dollar", dollar_threshold)
            bars.append(bar)

        return bars

    def _create_bar_from_trades(self, trades: List[Trade], bar_type: str, threshold: float) -> InformationBar:
        """从交易列表创建bar"""
        if not trades:
            raise ValueError("交易列表不能为空")

        # 计算OHLCV
        prices = [float(trade.price) for trade in trades]
        volumes = [trade.size for trade in trades]
        dollar_volumes = [float(trade.price) * trade.size for trade in trades]

        open_price = Decimal(str(prices[0]))
        high_price = Decimal(str(max(prices)))
        low_price = Decimal(str(min(prices)))
        close_price = Decimal(str(prices[-1]))
        total_volume = sum(volumes)

        # 计算VWAP
        total_dollar_volume = sum(dollar_volumes)
        vwap = Decimal(str(total_dollar_volume / total_volume)) if total_volume > 0 else close_price

        # 基础统计
        timestamp = trades[-1].timestamp
        num_trades = len(trades)

        # 创建基础bar（微观结构特征需要LOB快照数据）
        bar = InformationBar(
            timestamp=timestamp,
            symbol=self.symbol,
            bar_type=bar_type,
            threshold=threshold,
            open=open_price,
            high=high_price,
            low=low_price,
            close=close_price,
            volume=total_volume,
            vwap=vwap,
            avg_spread=Decimal('0'),  # 需要LOB数据
            avg_microprice=Decimal('0'),  # 需要LOB数据
            num_trades=num_trades,
            queue_imbalance_avg=0.0,  # 需要LOB数据
            order_flow_imbalance=0.0,  # 需要LOB数据
            depth_slope_bid=0.0,  # 需要LOB数据
            depth_slope_ask=0.0   # 需要LOB数据
        )

        return bar

# 使用示例和工厂类
class LOBDataIngestionFactory:
    """LOB数据摄取工厂"""

    @staticmethod
    def create_itch_parser() -> ITCHParser:
        """创建ITCH解析器"""
        return ITCHParser()

    @staticmethod
    def create_lobster_loader() -> LOBSTERLoader:
        """创建LOBSTER加载器"""
        return LOBSTERLoader()

    @staticmethod
    def create_bar_builder(symbol: str) -> InformationBarBuilder:
        """创建Bar构建器"""
        return InformationBarBuilder(symbol)

    @staticmethod
    def estimate_dollar_bar_threshold(daily_dollar_volume: float) -> float:
        """
        估算dollar bar阈值
        使用日均成交额的1/50作为起始阈值
        """
        return daily_dollar_volume / 50

# 示例用法
def demo_data_ingestion():
    """演示数据摄取功能"""
    print("🚀 LOB数据摄取系统演示")

    # 创建工厂
    factory = LOBDataIngestionFactory()

    # 示例1：LOBSTER数据加载
    print("\n📊 LOBSTER数据加载示例:")
    try:
        lobster_loader = factory.create_lobster_loader()
        # container = lobster_loader.load_lobster_data(
        #     "AAPL_2023-01-03_34200000_57600000_orderbook_10.csv",
        #     "AAPL_2023-01-03_34200000_57600000_message_10.csv",
        #     "AAPL"
        # )
        print("   LOBSTER加载器已准备就绪")
    except Exception as e:
        print(f"   LOBSTER加载器创建失败: {e}")

    # 示例2：信息驱动Bar构建
    print("\n📈 信息驱动Bar构建示例:")
    bar_builder = factory.create_bar_builder("AAPL")

    # 模拟交易数据
    mock_trades = [
        Trade(
            timestamp=1000000000 + i * 1000000,
            symbol="AAPL",
            price=Decimal("150.00") + Decimal(str(i * 0.01)),
            size=100 + i * 10,
            side=Side.BID if i % 2 == 0 else Side.ASK
        )
        for i in range(200)
    ]

    # 构建different types of bars
    tick_bars = bar_builder.build_tick_bars(mock_trades, tick_threshold=50)
    volume_bars = bar_builder.build_volume_bars(mock_trades, volume_threshold=5000)

    # 估算dollar bar阈值
    daily_dollar_volume = 1_000_000_000  # 10亿美元日成交额
    dollar_threshold = factory.estimate_dollar_bar_threshold(daily_dollar_volume)
    dollar_bars = bar_builder.build_dollar_bars(mock_trades, dollar_threshold)

    print(f"   Tick bars生成: {len(tick_bars)} 个")
    print(f"   Volume bars生成: {len(volume_bars)} 个")
    print(f"   Dollar bars生成: {len(dollar_bars)} 个 (阈值: ${dollar_threshold:,.0f})")

    # 示例3：ITCH解析器
    print("\n🔧 ITCH解析器准备:")
    itch_parser = factory.create_itch_parser()
    print(f"   ITCH解析器已初始化，支持 {len(itch_parser.MESSAGE_TYPES)} 种消息类型")

    return True

if __name__ == "__main__":
    demo_data_ingestion()