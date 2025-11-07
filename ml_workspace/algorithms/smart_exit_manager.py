#!/usr/bin/env python3
"""
智能出场管理器
基于市场条件、模型信号和风险管理的智能出场决策
"""

import numpy as np
from typing import Dict, List, Optional, Tuple
from dataclasses import dataclass
from enum import Enum

from .signal_aggregator import AggregatedSignal, SignalType
from .trading_state_manager import Position, ExitReason

class ExitPriority(Enum):
    """出场优先级"""
    LOW = 1
    MEDIUM = 2
    HIGH = 3
    URGENT = 4

@dataclass
class ExitSignal:
    """出场信号"""
    timestamp: int
    reason: ExitReason
    priority: ExitPriority
    confidence: float
    suggested_price: Optional[float] = None
    message: str = ""

class SmartExitManager:
    """智能出场管理器"""
    
    def __init__(
        self,
        lookback_window: int = 10,          # 回望窗口
        volatility_threshold: float = 0.02,  # 波动率阈值
        momentum_threshold: float = 0.01,    # 动量阈值
        risk_reward_ratio: float = 2.0,      # 风险收益比
        trailing_stop_pct: float = 0.005,    # 跟踪止损5bp
        profit_taking_levels: List[float] = None,  # 分批止盈点位
    ):
        self.lookback_window = lookback_window
        self.volatility_threshold = volatility_threshold
        self.momentum_threshold = momentum_threshold
        self.risk_reward_ratio = risk_reward_ratio
        self.trailing_stop_pct = trailing_stop_pct
        self.profit_taking_levels = profit_taking_levels or [0.01, 0.02, 0.03]
        
        # 历史数据缓存
        self.price_history: List[float] = []
        self.signal_history: List[AggregatedSignal] = []
        self.exit_signals: List[ExitSignal] = []
        
        # 动态跟踪
        self.highest_profit = 0.0
        self.trailing_stop_price = None
        self.profit_taken_levels: List[float] = []
        
    def evaluate_exit(
        self,
        position: Position,
        current_price: float,
        current_timestamp: int,
        market_signal: Optional[AggregatedSignal] = None,
        market_features: Optional[np.ndarray] = None
    ) -> Optional[ExitSignal]:
        """
        评估是否应该出场
        
        Args:
            position: 当前持仓
            current_price: 当前价格
            current_timestamp: 当前时间戳
            market_signal: 当前市场信号
            market_features: 市场特征数据
            
        Returns:
            出场信号 (如果应该出场)
        """
        # 更新价格历史
        self._update_price_history(current_price)
        if market_signal:
            self._update_signal_history(market_signal)
        
        # 多维度出场检查
        exit_signals = []
        
        # 1. 时间基础出场检查
        time_exit = self._check_time_based_exit(position, current_timestamp)
        if time_exit:
            exit_signals.append(time_exit)
        
        # 2. 风险管理出场检查
        risk_exit = self._check_risk_based_exit(position, current_price)
        if risk_exit:
            exit_signals.append(risk_exit)
        
        # 3. 技术信号出场检查
        if market_signal:
            signal_exit = self._check_signal_based_exit(position, market_signal)
            if signal_exit:
                exit_signals.append(signal_exit)
        
        # 4. 市场微结构出场检查
        if market_features is not None:
            microstructure_exit = self._check_microstructure_exit(position, market_features)
            if microstructure_exit:
                exit_signals.append(microstructure_exit)
        
        # 5. 动量反转出场检查
        momentum_exit = self._check_momentum_reversal_exit(position, current_price)
        if momentum_exit:
            exit_signals.append(momentum_exit)
        
        # 6. 波动率异常出场检查
        volatility_exit = self._check_volatility_exit(position, current_price)
        if volatility_exit:
            exit_signals.append(volatility_exit)
        
        # 选择最优出场信号
        if exit_signals:
            return self._select_best_exit_signal(exit_signals)
        
        # 更新动态止损
        self._update_trailing_stop(position, current_price)
        
        return None
    
    def _check_time_based_exit(self, position: Position, current_timestamp: int) -> Optional[ExitSignal]:
        """时间基础出场检查"""
        duration_ms = current_timestamp - position.entry_timestamp
        
        # 接近最大持仓时间
        if duration_ms >= position.max_hold_time_ms * 0.9:
            return ExitSignal(
                timestamp=current_timestamp,
                reason=ExitReason.TIME_BASED,
                priority=ExitPriority.HIGH,
                confidence=0.9,
                message=f"接近最大持仓时间: {duration_ms/1000:.1f}s"
            )
        
        # 超过预期出场时间
        if duration_ms >= position.expected_exit_time * 1000:
            return ExitSignal(
                timestamp=current_timestamp,
                reason=ExitReason.TIME_BASED,
                priority=ExitPriority.MEDIUM,
                confidence=0.7,
                message=f"超过预期出场时间: {position.expected_exit_time}s"
            )
        
        return None
    
    def _check_risk_based_exit(self, position: Position, current_price: float) -> Optional[ExitSignal]:
        """风险管理出场检查"""
        current_return = (current_price - position.entry_price) / position.entry_price
        directional_return = position.direction * current_return
        
        # 止损检查
        if position.stop_loss:
            if (position.direction == 1 and current_price <= position.stop_loss) or \
               (position.direction == -1 and current_price >= position.stop_loss):
                return ExitSignal(
                    timestamp=0,
                    reason=ExitReason.STOP_LOSS,
                    priority=ExitPriority.URGENT,
                    confidence=1.0,
                    suggested_price=position.stop_loss,
                    message=f"触发止损: {directional_return:.4f}"
                )
        
        # 止盈检查
        if position.take_profit:
            if (position.direction == 1 and current_price >= position.take_profit) or \
               (position.direction == -1 and current_price <= position.take_profit):
                return ExitSignal(
                    timestamp=0,
                    reason=ExitReason.TAKE_PROFIT,
                    priority=ExitPriority.HIGH,
                    confidence=0.9,
                    suggested_price=position.take_profit,
                    message=f"触发止盈: {directional_return:.4f}"
                )
        
        # 跟踪止损检查
        if self.trailing_stop_price:
            if (position.direction == 1 and current_price <= self.trailing_stop_price) or \
               (position.direction == -1 and current_price >= self.trailing_stop_price):
                return ExitSignal(
                    timestamp=0,
                    reason=ExitReason.STOP_LOSS,
                    priority=ExitPriority.HIGH,
                    confidence=0.85,
                    suggested_price=self.trailing_stop_price,
                    message=f"触发跟踪止损: {directional_return:.4f}"
                )
        
        return None
    
    def _check_signal_based_exit(self, position: Position, signal: AggregatedSignal) -> Optional[ExitSignal]:
        """信号基础出场检查"""
        # 强反向信号
        if (position.direction == 1 and signal.signal_type == SignalType.SELL) or \
           (position.direction == -1 and signal.signal_type == SignalType.BUY):
            
            if signal.confidence > 0.8 and signal.strength > 0.5:
                return ExitSignal(
                    timestamp=signal.timestamp,
                    reason=ExitReason.SIGNAL_BASED,
                    priority=ExitPriority.HIGH,
                    confidence=signal.confidence,
                    message=f"强反向信号: {signal.signal_type.name} 强度={signal.strength:.3f}"
                )
            elif signal.confidence > 0.6:
                return ExitSignal(
                    timestamp=signal.timestamp,
                    reason=ExitReason.SIGNAL_BASED,
                    priority=ExitPriority.MEDIUM,
                    confidence=signal.confidence,
                    message=f"反向信号: {signal.signal_type.name}"
                )
        
        return None
    
    def _check_microstructure_exit(self, position: Position, features: np.ndarray) -> Optional[ExitSignal]:
        """市场微结构出场检查"""
        if len(features) < 10:  # 需要足够的特征
            return None
        
        # 假设特征索引 (需要根据实际特征调整)
        spread_idx = 0  # spread_pct
        imbalance_idx = 5  # imb_1
        volume_idx = 20  # volume_imbalance_1min
        
        try:
            spread = features[spread_idx]
            imbalance = features[imbalance_idx] if len(features) > imbalance_idx else 0
            volume_imb = features[volume_idx] if len(features) > volume_idx else 0
            
            # 流动性恶化检查
            if spread > 0.01:  # 点差过大
                return ExitSignal(
                    timestamp=0,
                    reason=ExitReason.RISK_MANAGEMENT,
                    priority=ExitPriority.MEDIUM,
                    confidence=0.6,
                    message=f"流动性恶化: 点差={spread:.4f}"
                )
            
            # 订单流反转检查
            if abs(imbalance) > 0.5 and \
               ((position.direction == 1 and imbalance < -0.3) or 
                (position.direction == -1 and imbalance > 0.3)):
                return ExitSignal(
                    timestamp=0,
                    reason=ExitReason.SIGNAL_BASED,
                    priority=ExitPriority.MEDIUM,
                    confidence=0.7,
                    message=f"订单流反转: 失衡={imbalance:.3f}"
                )
        
        except (IndexError, TypeError):
            pass  # 特征数据问题，跳过
        
        return None
    
    def _check_momentum_reversal_exit(self, position: Position, current_price: float) -> Optional[ExitSignal]:
        """动量反转出场检查"""
        if len(self.price_history) < self.lookback_window:
            return None
        
        # 计算短期动量
        recent_prices = self.price_history[-5:]
        if len(recent_prices) < 3:
            return None
        
        momentum = (recent_prices[-1] - recent_prices[0]) / recent_prices[0]
        
        # 检查动量反转
        if abs(momentum) > self.momentum_threshold:
            if (position.direction == 1 and momentum < -self.momentum_threshold) or \
               (position.direction == -1 and momentum > self.momentum_threshold):
                return ExitSignal(
                    timestamp=0,
                    reason=ExitReason.SIGNAL_BASED,
                    priority=ExitPriority.MEDIUM,
                    confidence=0.6,
                    message=f"动量反转: {momentum:.4f}"
                )
        
        return None
    
    def _check_volatility_exit(self, position: Position, current_price: float) -> Optional[ExitSignal]:
        """波动率异常出场检查"""
        if len(self.price_history) < self.lookback_window:
            return None
        
        # 计算价格波动率
        returns = np.diff(self.price_history) / self.price_history[:-1]
        if len(returns) < 2:
            return None
        
        volatility = np.std(returns)
        
        # 波动率突然放大
        if volatility > self.volatility_threshold:
            return ExitSignal(
                timestamp=0,
                reason=ExitReason.RISK_MANAGEMENT,
                priority=ExitPriority.MEDIUM,
                confidence=0.5,
                message=f"波动率异常: {volatility:.4f}"
            )
        
        return None
    
    def _update_trailing_stop(self, position: Position, current_price: float):
        """更新跟踪止损"""
        current_return = (current_price - position.entry_price) / position.entry_price
        directional_return = position.direction * current_return
        
        # 更新最高收益
        if directional_return > self.highest_profit:
            self.highest_profit = directional_return
            
            # 设置跟踪止损
            if self.highest_profit > 0.01:  # 盈利超过1%才启动跟踪止损
                trailing_distance = current_price * self.trailing_stop_pct
                if position.direction == 1:
                    self.trailing_stop_price = current_price - trailing_distance
                else:
                    self.trailing_stop_price = current_price + trailing_distance
    
    def _select_best_exit_signal(self, signals: List[ExitSignal]) -> ExitSignal:
        """选择最优出场信号"""
        # 按优先级和置信度排序
        signals.sort(key=lambda s: (s.priority.value, s.confidence), reverse=True)
        return signals[0]
    
    def _update_price_history(self, price: float):
        """更新价格历史"""
        self.price_history.append(price)
        if len(self.price_history) > self.lookback_window * 2:
            self.price_history = self.price_history[-self.lookback_window:]
    
    def _update_signal_history(self, signal: AggregatedSignal):
        """更新信号历史"""
        self.signal_history.append(signal)
        if len(self.signal_history) > 50:
            self.signal_history = self.signal_history[-25:]
    
    def reset_for_new_position(self):
        """为新持仓重置状态"""
        self.highest_profit = 0.0
        self.trailing_stop_price = None
        self.profit_taken_levels.clear()
        self.exit_signals.clear()
    
    def get_exit_analytics(self) -> Dict:
        """获取出场分析数据"""
        return {
            'highest_profit': self.highest_profit,
            'trailing_stop_price': self.trailing_stop_price,
            'recent_volatility': np.std(np.diff(self.price_history) / self.price_history[:-1]) if len(self.price_history) > 2 else 0,
            'price_momentum': (self.price_history[-1] - self.price_history[0]) / self.price_history[0] if len(self.price_history) >= 2 else 0,
            'exit_signals_count': len(self.exit_signals)
        }