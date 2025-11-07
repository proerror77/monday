#!/usr/bin/env python3
"""
队列动力学分析
实现LOB队列的到达、取消、执行过程建模和预测
"""

import numpy as np
import pandas as pd
from typing import List, Dict, Tuple, Optional, Union, Callable, Any
from dataclasses import dataclass
from enum import Enum
import warnings
from scipy import stats
from scipy.optimize import minimize
from collections import defaultdict, deque
import time

from .data_structures import LOBSnapshot, LOBMessage, Trade, Side, MessageType, PriceLevel


class QueueEvent(Enum):
    """队列事件类型"""
    ARRIVAL = "arrival"      # 订单到达
    CANCELLATION = "cancel"  # 订单取消
    EXECUTION = "execution"  # 订单执行
    MODIFICATION = "modify"  # 订单修改


@dataclass
class QueueState:
    """队列状态快照"""
    timestamp: int
    side: Side
    price_level: int  # 0=best, 1=second best, etc.
    queue_size: int
    order_count: int
    arrival_rate: float      # λ(t) - 订单到达率
    cancellation_rate: float # μ(t) - 订单取消率
    execution_intensity: float # 执行强度
    event_index: Optional[int] = None
    time_bucket: Optional[int] = None


@dataclass
class QueuePosition:
    """队列中的位置信息"""
    order_id: str
    timestamp: int
    side: Side
    price: float
    size: int
    position_in_queue: int  # 队列中的位置
    queue_ahead: int        # 前面的订单数量
    time_in_queue: float    # 在队列中的时间


class QueueDynamicsAnalyzer:
    """队列动力学分析器"""

    def __init__(self, lookback_window: int = 1000,
                 alignment: str = "event",
                 bucket_ns: Optional[int] = None):
        self.lookback_window = lookback_window
        self.queue_states: Dict[Tuple[Side, int], List[QueueState]] = defaultdict(list)
        self.order_positions: Dict[str, QueuePosition] = {}
        self.event_history: List[Tuple[int, QueueEvent, Dict]] = []
        self.alignment = alignment
        self.bucket_ns = bucket_ns
        self._event_counter = 0

        if alignment not in {"event", "time"}:
            raise ValueError("alignment must be 'event' or 'time'")
        if alignment == "time" and not bucket_ns:
            raise ValueError("bucket_ns must be provided when alignment='time'")

    def process_lob_updates(self, snapshots: List[LOBSnapshot],
                          messages: List[LOBMessage]) -> None:
        """处理LOB更新并构建队列状态"""

        # 按时间排序
        snapshots_sorted = sorted(snapshots, key=lambda x: x.timestamp)
        messages_sorted = sorted(messages, key=lambda x: x.timestamp)

        # 合并时间线
        events = []
        for snap in snapshots_sorted:
            events.append(('snapshot', snap.timestamp, snap))
        for msg in messages_sorted:
            events.append(('message', msg.timestamp, msg))

        events.sort(key=lambda x: x[1])

        # 维护当前队列状态
        current_queues = self._initialize_queues()
        self._event_counter = 0

        for event_type, timestamp, data in events:
            current_event_index = self._event_counter
            if event_type == 'message':
                self._process_message(data, current_queues, timestamp)
            elif event_type == 'snapshot':
                self._update_queue_from_snapshot(data, current_queues, current_event_index)
            self._event_counter += 1

    def _initialize_queues(self) -> Dict[Tuple[Side, int], List]:
        """初始化队列结构"""
        queues = {}
        for side in [Side.BID, Side.ASK]:
            for level in range(10):  # 前10档
                queues[(side, level)] = []
        return queues

    def _process_message(self, message: LOBMessage,
                        current_queues: Dict, timestamp: int) -> None:
        """处理单个订单消息"""

        if message.message_type == MessageType.ADD:
            self._handle_order_arrival(message, current_queues, timestamp)
        elif message.message_type == MessageType.CANCEL:
            self._handle_order_cancellation(message, current_queues, timestamp)
        elif message.message_type == MessageType.EXECUTE:
            self._handle_order_execution(message, current_queues, timestamp)
        elif message.message_type == MessageType.MODIFY:
            self._handle_order_modification(message, current_queues, timestamp)

    def _handle_order_arrival(self, message: LOBMessage,
                            current_queues: Dict, timestamp: int) -> None:
        """处理订单到达"""
        if message.order_id:
            # 添加到队列末尾
            queue_key = (message.side, 0)  # 简化：假设都是最优价格
            current_queues[queue_key].append(message.order_id)

            # 记录位置
            position = len(current_queues[queue_key]) - 1
            self.order_positions[message.order_id] = QueuePosition(
                order_id=message.order_id,
                timestamp=timestamp,
                side=message.side,
                price=float(message.price),
                size=message.size,
                position_in_queue=position,
                queue_ahead=position,
                time_in_queue=0.0
            )

            # 记录事件
            self.event_history.append((timestamp, QueueEvent.ARRIVAL, {
                'order_id': message.order_id,
                'side': message.side.value,
                'price': float(message.price),
                'size': message.size
            }))

    def _handle_order_cancellation(self, message: LOBMessage,
                                 current_queues: Dict, timestamp: int) -> None:
        """处理订单取消"""
        if message.order_id and message.order_id in self.order_positions:
            position_info = self.order_positions[message.order_id]

            # 从队列移除
            queue_key = (message.side, 0)
            if message.order_id in current_queues[queue_key]:
                current_queues[queue_key].remove(message.order_id)

            # 更新队列中其他订单的位置
            self._update_queue_positions(queue_key, current_queues, timestamp)

            # 记录事件
            self.event_history.append((timestamp, QueueEvent.CANCELLATION, {
                'order_id': message.order_id,
                'time_in_queue': (timestamp - position_info.timestamp) / 1e9,
                'original_position': position_info.position_in_queue,
                'side': message.side.value
            }))

            del self.order_positions[message.order_id]

    def _handle_order_execution(self, message: LOBMessage,
                              current_queues: Dict, timestamp: int) -> None:
        """处理订单执行"""
        if message.order_id and message.order_id in self.order_positions:
            position_info = self.order_positions[message.order_id]

            # 记录执行事件
            self.event_history.append((timestamp, QueueEvent.EXECUTION, {
                'order_id': message.order_id,
                'executed_size': message.size,
                'execution_price': float(message.price),
                'time_to_execution': (timestamp - position_info.timestamp) / 1e9,
                'queue_position': position_info.position_in_queue,
                'side': message.side.value
            }))

            # 从队列移除
            queue_key = (message.side, 0)
            if message.order_id in current_queues[queue_key]:
                current_queues[queue_key].remove(message.order_id)

            del self.order_positions[message.order_id]

    def _handle_order_modification(self, message: LOBMessage,
                                 current_queues: Dict, timestamp: int) -> None:
        """处理订单修改"""
        # 简化处理：视为取消后重新添加
        self._handle_order_cancellation(message, current_queues, timestamp)
        self._handle_order_arrival(message, current_queues, timestamp)

    def _update_queue_positions(self, queue_key: Tuple[Side, int],
                              current_queues: Dict, timestamp: int) -> None:
        """更新队列中订单位置"""
        queue = current_queues[queue_key]
        for i, order_id in enumerate(queue):
            if order_id in self.order_positions:
                self.order_positions[order_id].position_in_queue = i
                self.order_positions[order_id].queue_ahead = i

    def _update_queue_from_snapshot(self, snapshot: LOBSnapshot,
                                  current_queues: Dict,
                                  event_index: int) -> None:
        """从快照更新队列状态"""
        # 计算队列特征并记录状态
        for side in [Side.BID, Side.ASK]:
            levels = snapshot.bids if side == Side.BID else snapshot.asks
            for level_idx, level in enumerate(levels[:5]):  # 前5档
                if level.size > 0:
                    # 估算到达率和取消率
                    arrival_rate = self._estimate_arrival_rate(side, level_idx)
                    cancel_rate = self._estimate_cancellation_rate(side, level_idx)
                    exec_intensity = self._estimate_execution_intensity(side, level_idx)

                    bucket_id = self._assign_bucket(snapshot.timestamp)

                    state = QueueState(
                        timestamp=snapshot.timestamp,
                        side=side,
                        price_level=level_idx,
                        queue_size=level.size,
                        order_count=level.order_count or 1,
                        arrival_rate=arrival_rate,
                        cancellation_rate=cancel_rate,
                        execution_intensity=exec_intensity,
                        event_index=event_index,
                        time_bucket=bucket_id
                    )

                    self.queue_states[(side, level_idx)].append(state)
                    # 控制队列长度，避免无限增长
                    if len(self.queue_states[(side, level_idx)]) > self.lookback_window:
                        self.queue_states[(side, level_idx)] = self.queue_states[(side, level_idx)][-self.lookback_window:]

    def _assign_bucket(self, timestamp: int) -> Optional[int]:
        if self.bucket_ns:
            return timestamp // self.bucket_ns
        return None

    def export_queue_states(self, side: Optional[Side] = None,
                            level: Optional[int] = None,
                            alignment: Optional[str] = None) -> pd.DataFrame:
        """导出对齐后的队列状态数据"""

        alignment = alignment or self.alignment
        if alignment not in {"event", "time"}:
            raise ValueError("alignment must be 'event' or 'time'")
        if alignment == "time" and not self.bucket_ns:
            raise ValueError("Time alignment requires bucket_ns to be set")

        records = []
        for (state_side, state_level), states in self.queue_states.items():
            if side is not None and state_side != side:
                continue
            if level is not None and state_level != level:
                continue
            for state in states:
                records.append({
                    'timestamp': state.timestamp,
                    'event_index': state.event_index,
                    'time_bucket': state.time_bucket,
                    'side': state.side.value,
                    'price_level': state.price_level,
                    'queue_size': state.queue_size,
                    'order_count': state.order_count,
                    'arrival_rate': state.arrival_rate,
                    'cancellation_rate': state.cancellation_rate,
                    'execution_intensity': state.execution_intensity
                })

        if not records:
            return pd.DataFrame(columns=[
                'timestamp', 'event_index', 'time_bucket', 'side', 'price_level',
                'queue_size', 'order_count', 'arrival_rate',
                'cancellation_rate', 'execution_intensity'
            ])

        df = pd.DataFrame(records)

        if alignment == "event":
            return df.sort_values(['event_index', 'timestamp']).reset_index(drop=True)

        # 时间桶聚合：默认保留每个桶内最新状态
        df_time = df.sort_values('timestamp')
        aggregated = df_time.groupby(['time_bucket', 'side', 'price_level'], as_index=False).last()
        return aggregated.sort_values('time_bucket').reset_index(drop=True)

    def _estimate_arrival_rate(self, side: Side, level: int) -> float:
        """估算订单到达率 λ(t)"""
        window_ns = int(5e9)  # 5秒窗口
        now_ts = self.event_history[-1][0] if self.event_history else None
        if not now_ts:
            return 0.0

        cutoff = now_ts - window_ns
        recent_arrivals = [
            event for event in self.event_history
            if event[0] >= cutoff and
            event[1] == QueueEvent.ARRIVAL and
            event[2].get('side') == side.value
        ]

        if not recent_arrivals:
            return 0.0

        time_span = max((recent_arrivals[-1][0] - recent_arrivals[0][0]) / 1e9, 1e-6)
        return len(recent_arrivals) / time_span

    def _estimate_cancellation_rate(self, side: Side, level: int) -> float:
        """估算订单取消率 μ(t)"""
        window_ns = int(5e9)
        now_ts = self.event_history[-1][0] if self.event_history else None
        if not now_ts:
            return 0.0

        cutoff = now_ts - window_ns
        recent_cancellations = [
            event for event in self.event_history
            if event[0] >= cutoff and
            event[1] == QueueEvent.CANCELLATION and
            event[2].get('side') == side.value
        ]

        if not recent_cancellations:
            return 0.0

        time_span = max((recent_cancellations[-1][0] - recent_cancellations[0][0]) / 1e9, 1e-6)
        return len(recent_cancellations) / time_span

    def _estimate_execution_intensity(self, side: Side, level: int) -> float:
        """估算执行强度"""
        window_ns = int(5e9)
        now_ts = self.event_history[-1][0] if self.event_history else None
        if not now_ts:
            return 0.0

        cutoff = now_ts - window_ns
        recent_executions = [
            event for event in self.event_history
            if event[0] >= cutoff and
            event[1] == QueueEvent.EXECUTION and
            event[2].get('side') == side.value
        ]

        if not recent_executions:
            return 0.0

        time_span = max((recent_executions[-1][0] - recent_executions[0][0]) / 1e9, 1e-6)
        return len(recent_executions) / time_span


class QueuePositionPredictor:
    """队列位置预测器"""

    def __init__(self, staleness_tolerance_ns: int = int(2e9),
                 fallback_rates: Optional[Dict[str, float]] = None):
        self.intensity_models: Dict[Tuple[Side, int], Dict[str, float]] = {}
        self.model_snapshot_ts: Dict[Tuple[Side, int], int] = {}
        self.staleness_tolerance_ns = staleness_tolerance_ns
        self.default_rates = fallback_rates or {'lambda': 1.0, 'mu': 1.0, 'alpha': 0.1}

    def fit_intensity_model(self, queue_states: List[QueueState],
                          side: Side, level: int) -> Dict[str, float]:
        """拟合队列强度模型"""

        if not queue_states:
            return {'lambda': 1.0, 'mu': 1.0, 'alpha': 0.1}

        arrivals = [state.arrival_rate for state in queue_states]
        cancellations = [state.cancellation_rate for state in queue_states]
        queue_sizes = [state.queue_size for state in queue_states]

        # 简单的线性回归拟合
        if len(arrivals) >= 2:
            lambda_mean = np.mean(arrivals)
            mu_mean = np.mean(cancellations)

            # 队列大小对强度的影响
            if len(queue_sizes) >= 2:
                size_effect = np.corrcoef(queue_sizes, arrivals)[0, 1] if np.std(arrivals) > 0 else 0
            else:
                size_effect = 0

            params = {
                'lambda': max(0.1, lambda_mean),
                'mu': max(0.1, mu_mean),
                'alpha': max(0.0, size_effect)  # 队列大小效应
            }
        else:
            params = {'lambda': 1.0, 'mu': 1.0, 'alpha': 0.1}

        self.intensity_models[(side, level)] = params
        if queue_states:
            self.model_snapshot_ts[(side, level)] = queue_states[-1].timestamp
        return params

    def predict_execution_probability(self, current_position: int,
                                    side: Side, level: int,
                                    time_horizon: float = 10.0,
                                    current_timestamp: Optional[int] = None,
                                    return_metadata: bool = False) -> Union[float, Tuple[float, Dict[str, Any]]]:
        """预测在时间窗口内的执行概率"""

        stale = self._is_model_stale(side, level, current_timestamp)
        params = self._get_model_params(side, level, stale)

        lambda_rate = params['lambda']
        mu_rate = params['mu']

        # 简化的马尔可夫队列模型
        # P(执行) ≈ 1 - exp(-effective_rate * time_horizon)
        # effective_rate 考虑当前队列位置

        if current_position == 0:
            # 队列首位，主要受执行强度影响
            effective_rate = lambda_rate * 2  # 优先执行
        else:
            # 需要前面的订单先被处理
            effective_rate = lambda_rate / (current_position + 1)

        execution_prob = 1 - np.exp(-effective_rate * time_horizon)
        execution_prob = min(0.99, max(0.01, execution_prob))

        if return_metadata:
            return execution_prob, {
                'stale_model': stale,
                'lambda_rate': lambda_rate,
                'mu_rate': mu_rate
            }

        return execution_prob

    def predict_queue_evolution(self, initial_size: int, side: Side,
                              level: int, time_steps: int = 100,
                              current_timestamp: Optional[int] = None,
                              return_metadata: bool = False) -> Union[List[int], Tuple[List[int], Dict[str, Any]]]:
        """预测队列大小演化"""

        stale = self._is_model_stale(side, level, current_timestamp)
        params = self._get_model_params(side, level, stale)

        lambda_rate = params['lambda']
        mu_rate = params['mu']

        if stale:
            # 在模型陈旧时适度降低到达率，避免过度乐观
            lambda_rate = max(0.1, lambda_rate * 0.5)

        # 简单的差分方程模拟
        queue_sizes = [initial_size]
        current_size = initial_size

        for t in range(time_steps):
            # 到达
            arrivals = np.random.poisson(lambda_rate * 0.1)  # 0.1秒时间步
            # 离开（取消+执行）
            departures = np.random.poisson(mu_rate * 0.1)

            current_size = max(0, current_size + arrivals - departures)
            queue_sizes.append(current_size)

        if return_metadata:
            return queue_sizes, {
                'stale_model': stale,
                'lambda_rate': lambda_rate,
                'mu_rate': mu_rate
            }

        return queue_sizes

    def _get_model_params(self, side: Side, level: int, stale: bool) -> Dict[str, float]:
        params = self.intensity_models.get((side, level))
        if params is None or stale:
            return dict(self.default_rates)
        return params

    def _is_model_stale(self, side: Side, level: int,
                        current_timestamp: Optional[int]) -> bool:
        if current_timestamp is None:
            return False

        last_ts = self.model_snapshot_ts.get((side, level))
        if last_ts is None:
            return True

        return (current_timestamp - last_ts) > self.staleness_tolerance_ns


class QueueImbalanceFeatures:
    """队列失衡特征计算"""

    def __init__(self):
        self.feature_cache = {}

    def compute_queue_imbalance(self, snapshot: LOBSnapshot,
                              depth: int = 5) -> Dict[str, float]:
        """计算多层队列失衡特征"""
        features = {}

        total_bid_volume = sum(level.size for level in snapshot.bids[:depth])
        total_ask_volume = sum(level.size for level in snapshot.asks[:depth])

        # 基本失衡
        if total_bid_volume + total_ask_volume > 0:
            features['volume_imbalance'] = (total_bid_volume - total_ask_volume) / (
                total_bid_volume + total_ask_volume
            )
        else:
            features['volume_imbalance'] = 0.0

        # 按层级的失衡
        for i in range(min(depth, len(snapshot.bids), len(snapshot.asks))):
            bid_vol = snapshot.bids[i].size
            ask_vol = snapshot.asks[i].size

            if bid_vol + ask_vol > 0:
                level_imbalance = (bid_vol - ask_vol) / (bid_vol + ask_vol)
                features[f'level_{i+1}_imbalance'] = level_imbalance

        # 加权失衡（近端权重更高）
        weighted_bid = sum(
            level.size / (i + 1) for i, level in enumerate(snapshot.bids[:depth])
        )
        weighted_ask = sum(
            level.size / (i + 1) for i, level in enumerate(snapshot.asks[:depth])
        )

        if weighted_bid + weighted_ask > 0:
            features['weighted_imbalance'] = (weighted_bid - weighted_ask) / (
                weighted_bid + weighted_ask
            )
        else:
            features['weighted_imbalance'] = 0.0

        return features

    def compute_queue_slope(self, snapshot: LOBSnapshot,
                          depth: int = 5) -> Dict[str, float]:
        """计算队列斜率特征"""
        features = {}

        # Bid侧斜率
        if len(snapshot.bids) >= depth:
            bid_prices = [float(level.price) for level in snapshot.bids[:depth]]
            bid_volumes = [level.size for level in snapshot.bids[:depth]]

            if len(set(bid_volumes)) > 1:  # 避免所有volume相同
                bid_slope = np.polyfit(bid_prices, bid_volumes, 1)[0]
                features['bid_slope'] = bid_slope
            else:
                features['bid_slope'] = 0.0

        # Ask侧斜率
        if len(snapshot.asks) >= depth:
            ask_prices = [float(level.price) for level in snapshot.asks[:depth]]
            ask_volumes = [level.size for level in snapshot.asks[:depth]]

            if len(set(ask_volumes)) > 1:
                ask_slope = np.polyfit(ask_prices, ask_volumes, 1)[0]
                features['ask_slope'] = ask_slope
            else:
                features['ask_slope'] = 0.0

        return features

    def compute_smart_money_indicators(self, recent_snapshots: List[LOBSnapshot],
                                     window: int = 20) -> Dict[str, float]:
        """计算聪明钱指标"""
        features = {}

        if len(recent_snapshots) < window:
            return features

        # 计算订单簿稳定性
        stability_scores = []
        for i in range(1, len(recent_snapshots)):
            prev_snap = recent_snapshots[i-1]
            curr_snap = recent_snapshots[i]

            # 计算价格稳定性
            if (prev_snap.best_bid and prev_snap.best_ask and
                curr_snap.best_bid and curr_snap.best_ask):

                bid_change = abs(float(curr_snap.best_bid.price) - float(prev_snap.best_bid.price))
                ask_change = abs(float(curr_snap.best_ask.price) - float(prev_snap.best_ask.price))

                stability = 1.0 / (1.0 + bid_change + ask_change)
                stability_scores.append(stability)

        if stability_scores:
            features['price_stability'] = np.mean(stability_scores)

        # 大单检测
        large_orders = []
        for snap in recent_snapshots[-window:]:
            total_volume = sum(level.size for level in snap.bids[:3]) + sum(
                level.size for level in snap.asks[:3]
            )
            if total_volume > 0:
                avg_volume = total_volume / 6  # 3档bid + 3档ask
                # 检测是否有异常大的订单
                max_volume = max(
                    max([level.size for level in snap.bids[:3]], default=0),
                    max([level.size for level in snap.asks[:3]], default=0)
                )
                if max_volume > avg_volume * 3:  # 超过平均3倍视为大单
                    large_orders.append(max_volume / avg_volume)

        features['large_order_ratio'] = np.mean(large_orders) if large_orders else 1.0

        return features


# 高级队列分析工具
class AdvancedQueueAnalytics:
    """高级队列分析"""

    def __init__(self):
        self.ml_models = {}

    def detect_spoofing_patterns(self, snapshots: List[LOBSnapshot],
                               messages: List[LOBMessage]) -> List[Dict]:
        """检测欺骗性订单模式"""
        spoofing_events = []

        # 简化的欺骗检测：快速添加和取消大单
        add_events = {}
        for msg in messages:
            if msg.message_type == MessageType.ADD and msg.order_id:
                add_events[msg.order_id] = {
                    'timestamp': msg.timestamp,
                    'size': msg.size,
                    'price': msg.price
                }
            elif msg.message_type == MessageType.CANCEL and msg.order_id:
                if msg.order_id in add_events:
                    add_event = add_events[msg.order_id]
                    time_diff = (msg.timestamp - add_event['timestamp']) / 1e9

                    # 大单且快速取消
                    if add_event['size'] > 1000 and time_diff < 5.0:
                        spoofing_events.append({
                            'order_id': msg.order_id,
                            'size': add_event['size'],
                            'lifetime': time_diff,
                            'confidence': min(1.0, add_event['size'] / 5000 * (5.0 - time_diff) / 5.0)
                        })

                    del add_events[msg.order_id]

        return spoofing_events

    def compute_liquidity_metrics(self, snapshots: List[LOBSnapshot],
                                depth: int = 5) -> Dict[str, float]:
        """计算流动性指标"""
        if not snapshots:
            return {}

        total_volumes = []
        spreads = []
        depths = []

        for snap in snapshots:
            # 总流动性
            bid_volume = sum(level.size for level in snap.bids[:depth])
            ask_volume = sum(level.size for level in snap.asks[:depth])
            total_volumes.append(bid_volume + ask_volume)

            # 价差
            if snap.spread:
                spreads.append(float(snap.spread))

            # 深度
            if snap.best_bid and snap.best_ask:
                depths.append(len([l for l in snap.bids if l.size > 0]) +
                            len([l for l in snap.asks if l.size > 0]))

        return {
            'avg_liquidity': np.mean(total_volumes) if total_volumes else 0,
            'liquidity_stability': 1 / (1 + np.std(total_volumes)) if total_volumes else 0,
            'avg_spread': np.mean(spreads) if spreads else 0,
            'avg_depth': np.mean(depths) if depths else 0,
            'liquidity_trend': np.polyfit(range(len(total_volumes)), total_volumes, 1)[0]
                              if len(total_volumes) > 1 else 0
        }


# 实用工具函数
def simulate_queue_evolution(initial_state: QueueState,
                           time_horizon: float = 60.0,
                           dt: float = 0.1) -> List[QueueState]:
    """模拟队列演化过程"""
    states = [initial_state]
    current_size = initial_state.queue_size

    steps = int(time_horizon / dt)
    for t in range(steps):
        # 简单的泊松过程模拟
        arrivals = np.random.poisson(initial_state.arrival_rate * dt)
        departures = np.random.poisson(initial_state.cancellation_rate * dt)

        current_size = max(0, current_size + arrivals - departures)

        new_state = QueueState(
            timestamp=initial_state.timestamp + int(t * dt * 1e9),
            side=initial_state.side,
            price_level=initial_state.price_level,
            queue_size=current_size,
            order_count=max(1, current_size),
            arrival_rate=initial_state.arrival_rate,
            cancellation_rate=initial_state.cancellation_rate,
            execution_intensity=initial_state.execution_intensity
        )

        states.append(new_state)

    return states


def compute_queue_efficiency_score(queue_states: List[QueueState]) -> float:
    """计算队列效率得分"""
    if not queue_states:
        return 0.0

    # 考虑队列大小稳定性、处理效率等因素
    sizes = [state.queue_size for state in queue_states]
    arrival_rates = [state.arrival_rate for state in queue_states]
    cancel_rates = [state.cancellation_rate for state in queue_states]

    # 稳定性得分
    size_stability = 1.0 / (1.0 + np.std(sizes)) if len(sizes) > 1 else 1.0

    # 处理效率得分
    if arrival_rates and cancel_rates:
        avg_throughput = np.mean(arrival_rates) + np.mean(cancel_rates)
        efficiency = min(1.0, avg_throughput / 10.0)  # 标准化到[0,1]
    else:
        efficiency = 0.5

    # 综合得分
    return (size_stability * 0.6 + efficiency * 0.4)
