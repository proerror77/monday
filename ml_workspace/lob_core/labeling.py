#!/usr/bin/env python3
"""
LOB微观结构感知标签系统
实现三重障碍法（Triple Barrier Method）和适用于金融时间序列的Purged K-Fold
"""

import numpy as np
import pandas as pd
from typing import List, Tuple, Optional, Dict, Union
from dataclasses import dataclass
from decimal import Decimal
import warnings
from datetime import datetime, timedelta
from sklearn.model_selection import BaseCrossValidator

from .data_structures import LOBSnapshot, Trade, Side, microprice


@dataclass
class TripleBarrierConfig:
    """三重障碍配置"""
    profit_taking_threshold: float = 0.01  # 止盈阈值（百分比）
    stop_loss_threshold: float = 0.01      # 止损阈值（百分比）
    max_holding_period: int = 300          # 最大持有时间（秒）
    min_return_threshold: float = 0.0005   # 最小收益阈值


@dataclass
class Label:
    """标签结构"""
    timestamp: int
    symbol: str
    label: int          # -1, 0, 1
    return_realized: float
    holding_period: int
    barrier_type: str   # "profit", "stop", "time"
    confidence: float   # 标签置信度


class MicropriceDriftLabeler:
    """基于microprice漂移的标签生成器"""

    def __init__(self, config: TripleBarrierConfig):
        self.config = config

    def compute_microprice_drift(self, snapshots: List[LOBSnapshot],
                                window: int = 10) -> pd.Series:
        """计算microprice的短期漂移趋势"""
        if len(snapshots) < window:
            return pd.Series(dtype=float)

        microprices = []
        timestamps = []

        for snap in snapshots:
            if snap.microprice is not None:
                microprices.append(float(snap.microprice))
                timestamps.append(snap.timestamp)

        if len(microprices) < window:
            return pd.Series(dtype=float)

        df = pd.DataFrame({
            'timestamp': timestamps,
            'microprice': microprices
        })

        # 计算滚动线性回归斜率作为漂移指标
        drift = df['microprice'].rolling(window=window).apply(
            lambda x: np.polyfit(range(len(x)), x, 1)[0] if len(x) == window else np.nan
        )

        drift.index = df['timestamp']
        return drift

    def generate_primary_labels(self, snapshots: List[LOBSnapshot],
                               future_snapshots: List[LOBSnapshot]) -> List[Label]:
        """生成基于microprice漂移的主要标签"""
        labels = []

        for i, current_snap in enumerate(snapshots):
            if current_snap.microprice is None:
                continue

            # 应用三重障碍法
            label = self._apply_triple_barrier(
                current_snap,
                future_snapshots[i:i+self.config.max_holding_period]
            )

            if label:
                labels.append(label)

        return labels

    def _apply_triple_barrier(self, entry_snap: LOBSnapshot,
                             future_snaps: List[LOBSnapshot]) -> Optional[Label]:
        """应用三重障碍方法"""
        if not future_snaps or entry_snap.microprice is None:
            return None

        entry_price = float(entry_snap.microprice)
        profit_barrier = entry_price * (1 + self.config.profit_taking_threshold)
        stop_barrier = entry_price * (1 - self.config.stop_loss_threshold)

        for i, snap in enumerate(future_snaps):
            if snap.microprice is None:
                continue

            current_price = float(snap.microprice)
            return_pct = (current_price - entry_price) / entry_price

            # 检查障碍条件
            if current_price >= profit_barrier:
                return Label(
                    timestamp=entry_snap.timestamp,
                    symbol=entry_snap.symbol,
                    label=1,  # 上涨
                    return_realized=return_pct,
                    holding_period=i,
                    barrier_type="profit",
                    confidence=min(abs(return_pct) / self.config.profit_taking_threshold, 1.0)
                )

            elif current_price <= stop_barrier:
                return Label(
                    timestamp=entry_snap.timestamp,
                    symbol=entry_snap.symbol,
                    label=-1,  # 下跌
                    return_realized=return_pct,
                    holding_period=i,
                    barrier_type="stop",
                    confidence=min(abs(return_pct) / self.config.stop_loss_threshold, 1.0)
                )

        # 时间障碍
        if future_snaps:
            final_snap = future_snaps[-1]
            if final_snap.microprice is not None:
                final_price = float(final_snap.microprice)
                return_pct = (final_price - entry_price) / entry_price

                # 根据收益方向和阈值确定标签
                if abs(return_pct) < self.config.min_return_threshold:
                    label_value = 0  # 横盘
                else:
                    label_value = 1 if return_pct > 0 else -1

                return Label(
                    timestamp=entry_snap.timestamp,
                    symbol=entry_snap.symbol,
                    label=label_value,
                    return_realized=return_pct,
                    holding_period=len(future_snaps),
                    barrier_type="time",
                    confidence=abs(return_pct) / max(self.config.min_return_threshold, 0.001)
                )

        return None


class OrderFlowImbalanceLabeler:
    """基于订单流失衡的标签生成器"""

    def __init__(self, lookback_window: int = 50):
        self.lookback_window = lookback_window

    def compute_ofi(self, snapshots: List[LOBSnapshot],
                   trades: List[Trade]) -> pd.Series:
        """计算订单流失衡（Order Flow Imbalance）"""
        ofi_values = []
        timestamps = []

        for i in range(len(snapshots)):
            if i < self.lookback_window:
                continue

            current_snap = snapshots[i]
            prev_snap = snapshots[i-1]

            if (current_snap.best_bid is None or current_snap.best_ask is None or
                prev_snap.best_bid is None or prev_snap.best_ask is None):
                continue

            # OFI = Δbid_volume - Δask_volume
            bid_flow = self._compute_flow_component(
                prev_snap.best_bid, current_snap.best_bid, 'bid'
            )
            ask_flow = self._compute_flow_component(
                prev_snap.best_ask, current_snap.best_ask, 'ask'
            )

            ofi = bid_flow - ask_flow
            ofi_values.append(ofi)
            timestamps.append(current_snap.timestamp)

        return pd.Series(ofi_values, index=timestamps)

    def _compute_flow_component(self, prev_level, curr_level, side: str) -> float:
        """计算单侧流量组件"""
        if prev_level.price == curr_level.price:
            # 价格不变，volume变化
            return curr_level.size - prev_level.size
        elif ((side == 'bid' and curr_level.price > prev_level.price) or
              (side == 'ask' and curr_level.price < prev_level.price)):
            # 价格改善，新volume
            return curr_level.size
        else:
            # 价格恶化，失去volume
            return -prev_level.size


class PurgedKFold(BaseCrossValidator):
    """
    适用于金融时间序列的Purged K-Fold交叉验证
    解决数据泄露问题：
    1. Purging: 移除训练集结束后一段时间的数据
    2. Embargo: 测试集开始前设置禁入期
    """

    def __init__(self, n_splits: int = 5, embargo_period: int = 100,
                 purge_period: int = 50):
        self.n_splits = n_splits
        self.embargo_period = embargo_period  # 禁入期长度
        self.purge_period = purge_period      # 清洗期长度

    def split(self, X, y=None, groups=None):
        """生成train/test索引"""
        n_samples = len(X)
        fold_size = n_samples // self.n_splits

        for i in range(self.n_splits):
            # 测试集范围
            test_start = i * fold_size
            test_end = (i + 1) * fold_size if i < self.n_splits - 1 else n_samples

            # 训练集范围（避开测试集和禁入期）
            train_indices = []

            # 测试集之前的数据
            if test_start > self.embargo_period:
                purge_end = test_start - self.embargo_period
                train_indices.extend(range(0, purge_end - self.purge_period))

            # 测试集之后的数据
            if test_end + self.embargo_period < n_samples:
                train_start_after = test_end + self.embargo_period + self.purge_period
                train_indices.extend(range(train_start_after, n_samples))

            test_indices = list(range(test_start, test_end))

            if train_indices and test_indices:
                yield np.array(train_indices), np.array(test_indices)

    def get_n_splits(self, X=None, y=None, groups=None):
        return self.n_splits


class MetaLabelingSystem:
    """元标签系统：预测模型预测正确的概率"""

    def __init__(self):
        self.base_model_predictions = {}

    def generate_meta_labels(self, primary_labels: List[Label],
                           model_predictions: np.ndarray,
                           prediction_confidence: np.ndarray) -> List[Label]:
        """
        生成元标签：
        1 = 模型预测可信，应该下注
        0 = 模型预测不可信，应该观望
        """
        meta_labels = []

        for i, label in enumerate(primary_labels):
            if i >= len(model_predictions):
                break

            # 实际收益方向
            actual_direction = 1 if label.return_realized > 0 else -1
            predicted_direction = model_predictions[i]
            confidence = prediction_confidence[i]

            # 元标签：预测方向正确且置信度高
            is_correct_and_confident = (
                (actual_direction == predicted_direction) and
                (confidence > 0.6)
            )

            meta_label = Label(
                timestamp=label.timestamp,
                symbol=label.symbol,
                label=1 if is_correct_and_confident else 0,
                return_realized=label.return_realized,
                holding_period=label.holding_period,
                barrier_type="meta",
                confidence=confidence
            )

            meta_labels.append(meta_label)

        return meta_labels


class LabelingPipeline:
    """标签生成管道"""

    def __init__(self, config: TripleBarrierConfig):
        self.config = config
        self.microprice_labeler = MicropriceDriftLabeler(config)
        self.ofi_labeler = OrderFlowImbalanceLabeler()
        self.meta_labeler = MetaLabelingSystem()

    def generate_labels(self, snapshots: List[LOBSnapshot],
                       trades: List[Trade]) -> Dict[str, List[Label]]:
        """生成所有类型的标签"""

        # 按时间排序
        snapshots_sorted = sorted(snapshots, key=lambda x: x.timestamp)
        trades_sorted = sorted(trades, key=lambda x: x.timestamp)

        # 生成主要标签
        primary_labels = self.microprice_labeler.generate_primary_labels(
            snapshots_sorted[:-self.config.max_holding_period],
            snapshots_sorted
        )

        # 计算辅助特征
        ofi_series = self.ofi_labeler.compute_ofi(snapshots_sorted, trades_sorted)

        return {
            'primary': primary_labels,
            'ofi_features': ofi_series,
            'meta': []  # 元标签需要模型预测后生成
        }

    def to_dataframe(self, labels: List[Label]) -> pd.DataFrame:
        """转换标签为DataFrame格式"""
        data = []
        for label in labels:
            data.append({
                'timestamp': label.timestamp,
                'symbol': label.symbol,
                'label': label.label,
                'return': label.return_realized,
                'holding_period': label.holding_period,
                'barrier_type': label.barrier_type,
                'confidence': label.confidence
            })

        return pd.DataFrame(data)


# 工具函数
def sample_weights_by_return(labels: List[Label],
                           decay_factor: float = 0.9) -> np.ndarray:
    """根据收益绝对值计算样本权重"""
    returns = np.array([abs(label.return_realized) for label in labels])
    weights = returns / returns.sum()

    # 时间衰减
    n = len(weights)
    time_weights = np.array([decay_factor ** (n - i - 1) for i in range(n)])

    final_weights = weights * time_weights
    return final_weights / final_weights.sum()


def bootstrap_sample_labels(labels: List[Label],
                          sample_ratio: float = 0.8) -> List[Label]:
    """Bootstrap采样标签，用于模型稳健性测试"""
    n_samples = int(len(labels) * sample_ratio)
    indices = np.random.choice(len(labels), size=n_samples, replace=True)
    return [labels[i] for i in indices]


def validate_label_quality(labels: List[Label]) -> Dict[str, float]:
    """验证标签质量"""
    if not labels:
        return {}

    returns = [label.return_realized for label in labels]
    holding_periods = [label.holding_period for label in labels]
    confidences = [label.confidence for label in labels]

    label_values = [label.label for label in labels]

    return {
        'mean_return': np.mean(returns),
        'return_volatility': np.std(returns),
        'mean_holding_period': np.mean(holding_periods),
        'mean_confidence': np.mean(confidences),
        'label_distribution': {
            'down': label_values.count(-1) / len(label_values),
            'flat': label_values.count(0) / len(label_values),
            'up': label_values.count(1) / len(label_values)
        },
        'sharpe_ratio': np.mean(returns) / np.std(returns) if np.std(returns) > 0 else 0
    }