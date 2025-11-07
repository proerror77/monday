#!/usr/bin/env python3
"""
LOB核心数据结构
定义事件级消息、快照和市场数据的标准格式
"""

from dataclasses import dataclass, field
from decimal import Decimal
from typing import List, Dict, Optional, Tuple, Iterator
from enum import Enum
import pandas as pd
import numpy as np
from datetime import datetime

class MessageType(Enum):
    """LOB消息类型"""
    ADD = "add"           # 新增订单
    MODIFY = "modify"     # 修改订单
    CANCEL = "cancel"     # 取消订单
    EXECUTE = "execute"   # 执行订单
    SNAPSHOT = "snapshot" # 快照更新

class Side(Enum):
    """订单方向"""
    BID = "bid"
    ASK = "ask"

@dataclass
class LOBMessage:
    """LOB事件消息结构"""
    timestamp: int              # 纳秒级时间戳
    message_type: MessageType   # 消息类型
    order_id: Optional[int]     # 订单ID（cancel/modify需要）
    side: Side                  # bid/ask
    price: Decimal              # 价格
    size: int                   # 数量
    level: Optional[int] = None # 价格级别（可选）

    def __post_init__(self):
        """后处理验证"""
        if self.price <= 0:
            raise ValueError(f"价格必须大于0: {self.price}")
        if self.size < 0:
            raise ValueError(f"数量不能为负: {self.size}")

@dataclass
class PriceLevel:
    """价格级别"""
    price: Decimal
    size: int
    order_count: Optional[int] = None

    @property
    def is_empty(self) -> bool:
        return self.size == 0

@dataclass
class LOBSnapshot:
    """LOB快照结构"""
    timestamp: int
    symbol: str
    bids: List[PriceLevel]      # 按价格降序排列
    asks: List[PriceLevel]      # 按价格升序排列

    def __post_init__(self):
        """计算衍生字段"""
        self._mid_price = None
        self._microprice = None
        self._spread = None
        self._best_bid = None
        self._best_ask = None

    @property
    def best_bid(self) -> Optional[PriceLevel]:
        if self._best_bid is None and self.bids:
            self._best_bid = self.bids[0]
        return self._best_bid

    @property
    def best_ask(self) -> Optional[PriceLevel]:
        if self._best_ask is None and self.asks:
            self._best_ask = self.asks[0]
        return self._best_ask

    @property
    def mid_price(self) -> Optional[Decimal]:
        """中间价"""
        if self._mid_price is None:
            if self.best_bid and self.best_ask:
                self._mid_price = (self.best_bid.price + self.best_ask.price) / 2
        return self._mid_price

    @property
    def spread(self) -> Optional[Decimal]:
        """价差"""
        if self._spread is None:
            if self.best_bid and self.best_ask:
                self._spread = self.best_ask.price - self.best_bid.price
        return self._spread

    @property
    def microprice(self) -> Optional[Decimal]:
        """
        Microprice - 向较薄队列倾斜的中间价
        标准HFT估计器
        """
        if self._microprice is None:
            if self.best_bid and self.best_ask:
                bid_size = float(self.best_bid.size)
                ask_size = float(self.best_ask.size)
                total_size = bid_size + ask_size

                if total_size > 0:
                    # 按大小加权，向较薄一侧倾斜
                    weight_bid = ask_size / total_size
                    weight_ask = bid_size / total_size

                    self._microprice = (
                        self.best_bid.price * weight_bid +
                        self.best_ask.price * weight_ask
                    )
                else:
                    self._microprice = self.mid_price

        return self._microprice

    def get_levels(self, side: Side, depth: int = 10) -> List[PriceLevel]:
        """获取指定深度的价格级别"""
        if side == Side.BID:
            return self.bids[:depth]
        else:
            return self.asks[:depth]

    def get_total_volume(self, side: Side, depth: int = 10) -> int:
        """获取指定深度的总成交量"""
        levels = self.get_levels(side, depth)
        return sum(level.size for level in levels)

@dataclass
class Trade:
    """成交记录"""
    timestamp: int
    symbol: str
    price: Decimal
    size: int
    side: Side                  # taker方向
    trade_id: Optional[str] = None

@dataclass
class InformationBar:
    """信息驱动的Bar（tick/volume/dollar bars）"""
    timestamp: int
    symbol: str
    bar_type: str              # "tick", "volume", "dollar"
    threshold: float           # bar阈值

    # OHLCV数据
    open: Decimal
    high: Decimal
    low: Decimal
    close: Decimal
    volume: int
    vwap: Decimal

    # LOB特征
    avg_spread: Decimal
    avg_microprice: Decimal
    num_trades: int

    # 微观结构特征
    queue_imbalance_avg: float
    order_flow_imbalance: float
    depth_slope_bid: float
    depth_slope_ask: float

class LOBDataContainer:
    """LOB数据容器，管理消息序列和快照"""

    def __init__(self, symbol: str):
        self.symbol = symbol
        self.messages: List[LOBMessage] = []
        self.snapshots: List[LOBSnapshot] = []
        self.trades: List[Trade] = []

    def add_message(self, message: LOBMessage):
        """添加消息，保持时间顺序"""
        # 验证时间顺序
        if self.messages and message.timestamp < self.messages[-1].timestamp:
            raise ValueError("消息时间戳必须单调递增")

        self.messages.append(message)

    def add_snapshot(self, snapshot: LOBSnapshot):
        """添加快照"""
        if snapshot.symbol != self.symbol:
            raise ValueError(f"符号不匹配: {snapshot.symbol} != {self.symbol}")

        self.snapshots.append(snapshot)

    def add_trade(self, trade: Trade):
        """添加成交记录"""
        if trade.symbol != self.symbol:
            raise ValueError(f"符号不匹配: {trade.symbol} != {self.symbol}")

        self.trades.append(trade)

    def get_messages_in_window(self, start_time: int, end_time: int) -> List[LOBMessage]:
        """获取时间窗口内的消息"""
        return [msg for msg in self.messages
                if start_time <= msg.timestamp <= end_time]

    def get_snapshots_in_window(self, start_time: int, end_time: int) -> List[LOBSnapshot]:
        """获取时间窗口内的快照"""
        return [snap for snap in self.snapshots
                if start_time <= snap.timestamp <= end_time]

    def to_dataframe(self) -> Dict[str, pd.DataFrame]:
        """转换为DataFrame格式"""
        # 消息DataFrame
        messages_data = []
        for msg in self.messages:
            messages_data.append({
                'timestamp': msg.timestamp,
                'message_type': msg.message_type.value,
                'order_id': msg.order_id,
                'side': msg.side.value,
                'price': float(msg.price),
                'size': msg.size,
                'level': msg.level
            })

        # 快照DataFrame
        snapshots_data = []
        for snap in self.snapshots:
            row = {
                'timestamp': snap.timestamp,
                'symbol': snap.symbol,
                'mid_price': float(snap.mid_price) if snap.mid_price else None,
                'microprice': float(snap.microprice) if snap.microprice else None,
                'spread': float(snap.spread) if snap.spread else None
            }

            # 添加bid/ask级别
            for i in range(min(10, len(snap.bids))):
                row[f'bid_price_{i+1}'] = float(snap.bids[i].price)
                row[f'bid_size_{i+1}'] = snap.bids[i].size

            for i in range(min(10, len(snap.asks))):
                row[f'ask_price_{i+1}'] = float(snap.asks[i].price)
                row[f'ask_size_{i+1}'] = snap.asks[i].size

            snapshots_data.append(row)

        # 成交DataFrame
        trades_data = []
        for trade in self.trades:
            trades_data.append({
                'timestamp': trade.timestamp,
                'symbol': trade.symbol,
                'price': float(trade.price),
                'size': trade.size,
                'side': trade.side.value,
                'trade_id': trade.trade_id
            })

        return {
            'messages': pd.DataFrame(messages_data),
            'snapshots': pd.DataFrame(snapshots_data),
            'trades': pd.DataFrame(trades_data)
        }

# 工具函数
def timestamp_to_datetime(timestamp_ns: int) -> datetime:
    """纳秒时间戳转换为datetime"""
    return datetime.fromtimestamp(timestamp_ns / 1_000_000_000)

def datetime_to_timestamp(dt: datetime) -> int:
    """datetime转换为纳秒时间戳"""
    return int(dt.timestamp() * 1_000_000_000)

def validate_price_ladder(levels: List[PriceLevel], side: Side) -> bool:
    """验证价格梯度的单调性"""
    if len(levels) <= 1:
        return True

    for i in range(1, len(levels)):
        if side == Side.BID:
            # bid价格应该递减
            if levels[i].price >= levels[i-1].price:
                return False
        else:
            # ask价格应该递增
            if levels[i].price <= levels[i-1].price:
                return False

    return True

def clean_message_sequence(messages: List[LOBMessage]) -> List[LOBMessage]:
    """清理消息序列，移除破坏的序列"""
    cleaned = []

    for msg in messages:
        # 基本验证
        if msg.timestamp <= 0:
            continue
        if msg.price <= 0:
            continue
        if msg.size < 0:
            continue

        # 保持时间顺序
        if cleaned and msg.timestamp < cleaned[-1].timestamp:
            continue

        cleaned.append(msg)

    return cleaned