#!/usr/bin/env python3
"""
交易信号聚合器
解决过度交易问题，通过信号聚合、确认和过滤机制
"""

import numpy as np
from typing import Dict, List, Optional, Tuple
from collections import deque
from dataclasses import dataclass
from enum import Enum

class SignalType(Enum):
    """信号类型"""
    BUY = 1
    SELL = -1
    HOLD = 0

@dataclass
class RawSignal:
    """原始信号"""
    timestamp: int
    signal_type: SignalType
    strength: float
    expected_exit_time: float
    features: np.ndarray

@dataclass 
class AggregatedSignal:
    """聚合后的信号"""
    timestamp: int
    signal_type: SignalType
    strength: float
    confidence: float
    expected_exit_time: float
    raw_signals_count: int
    duration_ms: int

class SignalAggregator:
    """交易信号聚合器"""
    
    def __init__(
        self,
        confirmation_window_ms: int = 10000,  # 10秒确认窗口 (优化)
        min_signal_strength: float = 0.5,     # 最小信号强度 (优化)
        min_confirmation_count: int = 5,      # 最小确认次数 (优化)
        max_signal_age_ms: int = 30000,       # 最大信号年龄30秒
        consistency_threshold: float = 0.8,   # 一致性阈值 (优化)
        cooldown_period_ms: int = 20000,      # 冷却期20秒 (优化)
    ):
        self.confirmation_window_ms = confirmation_window_ms
        self.min_signal_strength = min_signal_strength
        self.min_confirmation_count = min_confirmation_count
        self.max_signal_age_ms = max_signal_age_ms
        self.consistency_threshold = consistency_threshold
        self.cooldown_period_ms = cooldown_period_ms
        
        # 信号缓冲区
        self.signal_buffer: deque = deque(maxlen=1000)
        self.last_aggregated_signal: Optional[AggregatedSignal] = None
        self.last_signal_timestamp: int = 0
        
    def add_raw_signal(self, raw_signal: RawSignal) -> Optional[AggregatedSignal]:
        """
        添加原始信号并检查是否可以生成聚合信号
        
        Args:
            raw_signal: 原始信号
            
        Returns:
            聚合信号 (如果满足条件) 或 None
        """
        # 过滤低强度信号
        if abs(raw_signal.strength) < self.min_signal_strength:
            return None
            
        # 检查冷却期
        if (raw_signal.timestamp - self.last_signal_timestamp) < self.cooldown_period_ms:
            return None
            
        # 添加到缓冲区
        self.signal_buffer.append(raw_signal)
        
        # 清理过期信号
        self._cleanup_old_signals(raw_signal.timestamp)
        
        # 尝试聚合信号
        aggregated = self._try_aggregate_signals(raw_signal.timestamp)
        
        if aggregated:
            self.last_aggregated_signal = aggregated
            self.last_signal_timestamp = aggregated.timestamp
            
        return aggregated
    
    def _cleanup_old_signals(self, current_timestamp: int):
        """清理过期信号"""
        cutoff_time = current_timestamp - self.max_signal_age_ms
        
        while self.signal_buffer and self.signal_buffer[0].timestamp < cutoff_time:
            self.signal_buffer.popleft()
    
    def _try_aggregate_signals(self, current_timestamp: int) -> Optional[AggregatedSignal]:
        """尝试聚合信号"""
        if len(self.signal_buffer) < self.min_confirmation_count:
            return None
            
        # 获取确认窗口内的信号
        window_start = current_timestamp - self.confirmation_window_ms
        window_signals = [
            s for s in self.signal_buffer 
            if s.timestamp >= window_start
        ]
        
        if len(window_signals) < self.min_confirmation_count:
            return None
            
        # 分析信号一致性
        signal_analysis = self._analyze_signal_consistency(window_signals)
        
        if signal_analysis['consistency'] < self.consistency_threshold:
            return None
            
        # 创建聚合信号
        aggregated = AggregatedSignal(
            timestamp=current_timestamp,
            signal_type=signal_analysis['dominant_signal'],
            strength=signal_analysis['avg_strength'],
            confidence=signal_analysis['consistency'],
            expected_exit_time=signal_analysis['avg_exit_time'],
            raw_signals_count=len(window_signals),
            duration_ms=current_timestamp - window_signals[0].timestamp
        )
        
        return aggregated
    
    def _analyze_signal_consistency(self, signals: List[RawSignal]) -> Dict:
        """分析信号一致性"""
        if not signals:
            return {'consistency': 0.0}
            
        # 统计信号类型
        buy_signals = [s for s in signals if s.signal_type == SignalType.BUY]
        sell_signals = [s for s in signals if s.signal_type == SignalType.SELL]
        
        # 确定主导信号
        if len(buy_signals) > len(sell_signals):
            dominant_signal = SignalType.BUY
            dominant_signals = buy_signals
        elif len(sell_signals) > len(buy_signals):
            dominant_signal = SignalType.SELL
            dominant_signals = sell_signals
        else:
            # 信号冲突，返回低一致性
            return {
                'consistency': 0.0,
                'dominant_signal': SignalType.HOLD,
                'avg_strength': 0.0,
                'avg_exit_time': 30.0
            }
        
        # 计算一致性
        consistency = len(dominant_signals) / len(signals)
        
        # 计算平均强度和出场时间
        avg_strength = np.mean([abs(s.strength) for s in dominant_signals])
        avg_exit_time = np.mean([s.expected_exit_time for s in dominant_signals])
        
        # 检查强度趋势 (最近的信号是否更强)
        if len(dominant_signals) >= 2:
            recent_strength = np.mean([abs(s.strength) for s in dominant_signals[-2:]])
            early_strength = np.mean([abs(s.strength) for s in dominant_signals[:-2]]) if len(dominant_signals) > 2 else recent_strength
            
            # 如果最近信号更强，提高一致性
            if recent_strength > early_strength:
                consistency = min(1.0, consistency * 1.2)
        
        return {
            'consistency': consistency,
            'dominant_signal': dominant_signal,
            'avg_strength': avg_strength,
            'avg_exit_time': avg_exit_time,
            'signal_evolution': self._analyze_signal_evolution(dominant_signals)
        }
    
    def _analyze_signal_evolution(self, signals: List[RawSignal]) -> Dict:
        """分析信号演化趋势"""
        if len(signals) < 2:
            return {'trend': 'stable', 'strength_change': 0.0}
            
        # 计算强度变化趋势
        strengths = [abs(s.strength) for s in signals]
        
        # 简单线性趋势
        x = np.arange(len(strengths))
        slope = np.polyfit(x, strengths, 1)[0]
        
        if slope > 0.01:
            trend = 'strengthening'
        elif slope < -0.01:
            trend = 'weakening'  
        else:
            trend = 'stable'
            
        strength_change = (strengths[-1] - strengths[0]) / (strengths[0] + 1e-10)
        
        return {
            'trend': trend,
            'strength_change': strength_change,
            'slope': slope
        }
    
    def get_signal_statistics(self) -> Dict:
        """获取信号统计信息"""
        if not self.signal_buffer:
            return {}
            
        signals = list(self.signal_buffer)
        
        # 基础统计
        buy_count = sum(1 for s in signals if s.signal_type == SignalType.BUY)
        sell_count = sum(1 for s in signals if s.signal_type == SignalType.SELL)
        
        strengths = [abs(s.strength) for s in signals]
        
        return {
            'total_signals': len(signals),
            'buy_signals': buy_count,
            'sell_signals': sell_count,
            'avg_strength': np.mean(strengths),
            'max_strength': np.max(strengths),
            'min_strength': np.min(strengths),
            'buffer_timespan_ms': signals[-1].timestamp - signals[0].timestamp if len(signals) > 1 else 0,
            'last_aggregated_timestamp': self.last_signal_timestamp
        }
    
    def reset(self):
        """重置聚合器状态"""
        self.signal_buffer.clear()
        self.last_aggregated_signal = None
        self.last_signal_timestamp = 0


class AdaptiveSignalAggregator(SignalAggregator):
    """自适应信号聚合器"""
    
    def __init__(self, **kwargs):
        super().__init__(**kwargs)
        
        # 自适应参数
        self.performance_history: deque = deque(maxlen=100)
        self.adaptation_period = 50  # 50个信号后开始自适应
        
    def add_performance_feedback(self, trade_result: Dict):
        """添加交易结果反馈用于自适应"""
        self.performance_history.append(trade_result)
        
        # 定期调整参数
        if len(self.performance_history) >= self.adaptation_period:
            self._adapt_parameters()
    
    def _adapt_parameters(self):
        """自适应调整参数"""
        recent_trades = list(self.performance_history)[-20:]  # 最近20个交易
        
        if not recent_trades:
            return
            
        # 计算胜率
        win_rate = sum(1 for t in recent_trades if t.get('pnl', 0) > 0) / len(recent_trades)
        
        # 根据胜率调整参数
        if win_rate < 0.3:  # 胜率太低，增加信号确认要求
            self.min_signal_strength = min(0.8, self.min_signal_strength * 1.1)
            self.min_confirmation_count = min(8, self.min_confirmation_count + 1)
            self.consistency_threshold = min(0.9, self.consistency_threshold * 1.05)
            
        elif win_rate > 0.7:  # 胜率很高，可以适当放松要求
            self.min_signal_strength = max(0.2, self.min_signal_strength * 0.95)
            self.min_confirmation_count = max(2, self.min_confirmation_count - 1)
            self.consistency_threshold = max(0.6, self.consistency_threshold * 0.98)