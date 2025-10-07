#!/usr/bin/env python3
"""
交易状态管理系统
管理交易生命周期，防止过度交易，实现智能进出场控制
"""

import numpy as np
from typing import Dict, List, Optional, Any
from dataclasses import dataclass
from enum import Enum
import time

from .signal_aggregator import AggregatedSignal, SignalType

class TradingState(Enum):
    """交易状态"""
    IDLE = "idle"                    # 空闲状态
    SIGNAL_PENDING = "signal_pending"  # 信号待确认
    POSITION_OPEN = "position_open"    # 持仓中
    EXITING = "exiting"               # 准备出场
    COOLDOWN = "cooldown"             # 冷却期

class ExitReason(Enum):
    """出场原因"""
    TIME_BASED = "time_based"
    SIGNAL_BASED = "signal_based"
    STOP_LOSS = "stop_loss"
    TAKE_PROFIT = "take_profit"
    RISK_MANAGEMENT = "risk_management"

@dataclass
class Position:
    """持仓信息"""
    entry_timestamp: int
    entry_price: float
    direction: int  # 1=long, -1=short
    size: float
    expected_exit_time: float
    signal_strength: float
    stop_loss: Optional[float] = None
    take_profit: Optional[float] = None
    max_hold_time_ms: int = 60000  # 最大持仓60秒

@dataclass
class TradeRecord:
    """交易记录"""
    entry_timestamp: int
    exit_timestamp: int
    direction: int
    entry_price: float
    exit_price: float
    size: float
    duration_ms: int
    raw_return: float
    directional_return: float
    gross_pnl: float
    net_pnl: float
    fees: float
    exit_reason: ExitReason
    signal_strength: float

class TradingStateManager:
    """交易状态管理器"""
    
    def __init__(
        self,
        initial_capital: float = 10000.0,
        position_size_pct: float = 0.1,  # 10%资金开仓
        fee_rate: float = 0.0006,
        max_position_time_ms: int = 60000,  # 最大持仓60秒
        cooldown_time_ms: int = 15000,      # 冷却期15秒 (优化)
        stop_loss_pct: float = 0.02,        # 2%止损
        take_profit_pct: float = 0.04,      # 4%止盈
        risk_free_rate: float = 0.02,       # 无风险利率
    ):
        self.initial_capital = initial_capital
        self.current_capital = initial_capital
        self.position_size_pct = position_size_pct
        self.fee_rate = fee_rate
        self.max_position_time_ms = max_position_time_ms
        self.cooldown_time_ms = cooldown_time_ms
        self.stop_loss_pct = stop_loss_pct
        self.take_profit_pct = take_profit_pct
        self.risk_free_rate = risk_free_rate
        
        # 状态管理
        self.current_state = TradingState.IDLE
        self.current_position: Optional[Position] = None
        self.last_trade_timestamp = 0
        self.cooldown_end_timestamp = 0
        
        # 交易记录
        self.trade_history: List[TradeRecord] = []
        self.pending_signals: List[AggregatedSignal] = []
        
        # 风险管理
        self.max_daily_trades = 50  # 每日最大交易数
        self.max_consecutive_losses = 5
        self.consecutive_losses = 0
        self.daily_trade_count = 0
        self.last_reset_date = time.strftime("%Y-%m-%d")
        
    def process_signal(
        self, 
        signal: AggregatedSignal, 
        current_price: float,
        current_timestamp: int
    ) -> Dict[str, Any]:
        """
        处理交易信号
        
        Returns:
            操作结果字典
        """
        result = {
            'action': 'none',
            'state_change': False,
            'trade_executed': False,
            'position_changed': False,
            'message': ''
        }
        
        # 检查每日重置
        self._check_daily_reset()
        
        # 根据当前状态处理信号
        if self.current_state == TradingState.IDLE:
            result = self._handle_idle_state(signal, current_price, current_timestamp)
            
        elif self.current_state == TradingState.SIGNAL_PENDING:
            result = self._handle_signal_pending_state(signal, current_price, current_timestamp)
            
        elif self.current_state == TradingState.POSITION_OPEN:
            result = self._handle_position_open_state(signal, current_price, current_timestamp)
            
        elif self.current_state == TradingState.COOLDOWN:
            result = self._handle_cooldown_state(current_timestamp)
            
        return result
    
    def force_exit_check(self, current_price: float, current_timestamp: int) -> Optional[TradeRecord]:
        """强制出场检查（不依赖信号）"""
        if self.current_state != TradingState.POSITION_OPEN or not self.current_position:
            return None
            
        position = self.current_position
        
        # 检查时间限制
        duration_ms = current_timestamp - position.entry_timestamp
        if duration_ms >= position.max_hold_time_ms:
            return self._execute_exit(current_price, current_timestamp, ExitReason.TIME_BASED)
        
        # 检查止损
        if position.stop_loss:
            if (position.direction == 1 and current_price <= position.stop_loss) or \
               (position.direction == -1 and current_price >= position.stop_loss):
                return self._execute_exit(current_price, current_timestamp, ExitReason.STOP_LOSS)
        
        # 检查止盈
        if position.take_profit:
            if (position.direction == 1 and current_price >= position.take_profit) or \
               (position.direction == -1 and current_price <= position.take_profit):
                return self._execute_exit(current_price, current_timestamp, ExitReason.TAKE_PROFIT)
        
        return None
    
    def _handle_idle_state(self, signal: AggregatedSignal, current_price: float, current_timestamp: int) -> Dict:
        """处理空闲状态"""
        # 检查是否在冷却期
        if current_timestamp < self.cooldown_end_timestamp:
            return {
                'action': 'wait_cooldown',
                'message': f'冷却期剩余: {self.cooldown_end_timestamp - current_timestamp}ms'
            }
        
        # 检查风险限制
        if not self._check_risk_limits():
            return {
                'action': 'risk_block',
                'message': '触发风险限制，暂停交易'
            }
        
        # 尝试开仓
        if self._should_open_position(signal):
            trade_record = self._execute_entry(signal, current_price, current_timestamp)
            return {
                'action': 'open_position',
                'state_change': True,
                'trade_executed': True,
                'position_changed': True,
                'trade_record': trade_record,
                'message': f'开仓: {signal.signal_type.name}'
            }
        
        return {'action': 'wait', 'message': '等待合适信号'}
    
    def _handle_signal_pending_state(self, signal: AggregatedSignal, current_price: float, current_timestamp: int) -> Dict:
        """处理信号待确认状态"""
        # 当前实现中直接从IDLE到POSITION_OPEN，这个状态预留给更复杂的确认逻辑
        return {'action': 'none'}
    
    def _handle_position_open_state(self, signal: AggregatedSignal, current_price: float, current_timestamp: int) -> Dict:
        """处理持仓状态"""
        if not self.current_position:
            self._change_state(TradingState.IDLE)
            return {'action': 'state_fix', 'state_change': True}
        
        # 检查是否应该基于信号出场
        if self._should_exit_on_signal(signal):
            trade_record = self._execute_exit(current_price, current_timestamp, ExitReason.SIGNAL_BASED)
            return {
                'action': 'exit_position',
                'state_change': True,
                'trade_executed': True,
                'position_changed': True,
                'trade_record': trade_record,
                'message': f'信号出场: {signal.signal_type.name}'
            }
        
        return {'action': 'hold', 'message': '持仓中'}
    
    def _handle_cooldown_state(self, current_timestamp: int) -> Dict:
        """处理冷却状态"""
        if current_timestamp >= self.cooldown_end_timestamp:
            self._change_state(TradingState.IDLE)
            return {
                'action': 'cooldown_end',
                'state_change': True,
                'message': '冷却期结束'
            }
        
        return {
            'action': 'wait_cooldown',
            'message': f'冷却期剩余: {self.cooldown_end_timestamp - current_timestamp}ms'
        }
    
    def _should_open_position(self, signal: AggregatedSignal) -> bool:
        """判断是否应该开仓"""
        # 基础条件检查
        if signal.signal_type == SignalType.HOLD:
            return False
            
        # 信号强度检查
        if signal.strength < 0.3:  # 最低强度要求
            return False
            
        # 信号置信度检查
        if signal.confidence < 0.7:  # 最低置信度要求
            return False
            
        return True
    
    def _should_exit_on_signal(self, signal: AggregatedSignal) -> bool:
        """判断是否应该基于信号出场"""
        if not self.current_position:
            return False
            
        # 反向信号出场
        if (self.current_position.direction == 1 and signal.signal_type == SignalType.SELL) or \
           (self.current_position.direction == -1 and signal.signal_type == SignalType.BUY):
            return signal.confidence > 0.8  # 需要高置信度的反向信号
            
        return False
    
    def _execute_entry(self, signal: AggregatedSignal, price: float, timestamp: int) -> Position:
        """执行开仓"""
        direction = 1 if signal.signal_type == SignalType.BUY else -1
        position_size = self.current_capital * self.position_size_pct
        
        # 计算止损止盈
        stop_loss = None
        take_profit = None
        if direction == 1:  # 多头
            stop_loss = price * (1 - self.stop_loss_pct)
            take_profit = price * (1 + self.take_profit_pct)
        else:  # 空头
            stop_loss = price * (1 + self.stop_loss_pct)
            take_profit = price * (1 - self.take_profit_pct)
        
        self.current_position = Position(
            entry_timestamp=timestamp,
            entry_price=price,
            direction=direction,
            size=position_size,
            expected_exit_time=signal.expected_exit_time,
            signal_strength=signal.strength,
            stop_loss=stop_loss,
            take_profit=take_profit,
            max_hold_time_ms=self.max_position_time_ms
        )
        
        self._change_state(TradingState.POSITION_OPEN)
        self.daily_trade_count += 1
        
        return self.current_position
    
    def _execute_exit(self, price: float, timestamp: int, reason: ExitReason) -> TradeRecord:
        """执行平仓"""
        if not self.current_position:
            raise ValueError("没有持仓可以平仓")
            
        position = self.current_position
        duration_ms = timestamp - position.entry_timestamp
        
        # 计算收益
        raw_return = (price - position.entry_price) / position.entry_price
        directional_return = position.direction * raw_return
        
        # 计算PnL
        gross_pnl = directional_return * position.size
        fees = position.size * self.fee_rate  # 双边手续费
        net_pnl = gross_pnl - fees
        
        # 创建交易记录
        trade_record = TradeRecord(
            entry_timestamp=position.entry_timestamp,
            exit_timestamp=timestamp,
            direction=position.direction,
            entry_price=position.entry_price,
            exit_price=price,
            size=position.size,
            duration_ms=duration_ms,
            raw_return=raw_return,
            directional_return=directional_return,
            gross_pnl=gross_pnl,
            net_pnl=net_pnl,
            fees=fees,
            exit_reason=reason,
            signal_strength=position.signal_strength
        )
        
        # 更新资金
        self.current_capital += net_pnl
        
        # 更新连续亏损计数
        if net_pnl <= 0:
            self.consecutive_losses += 1
        else:
            self.consecutive_losses = 0
        
        # 记录交易
        self.trade_history.append(trade_record)
        self.last_trade_timestamp = timestamp
        
        # 清除持仓
        self.current_position = None
        
        # 进入冷却期
        self.cooldown_end_timestamp = timestamp + self.cooldown_time_ms
        self._change_state(TradingState.COOLDOWN)
        
        return trade_record
    
    def _check_risk_limits(self) -> bool:
        """检查风险限制"""
        # 检查每日交易次数
        if self.daily_trade_count >= self.max_daily_trades:
            return False
            
        # 检查连续亏损
        if self.consecutive_losses >= self.max_consecutive_losses:
            return False
            
        # 检查资金水平
        if self.current_capital < self.initial_capital * 0.5:  # 最大50%回撤
            return False
            
        return True
    
    def _check_daily_reset(self):
        """检查是否需要每日重置"""
        current_date = time.strftime("%Y-%m-%d")
        if current_date != self.last_reset_date:
            self.daily_trade_count = 0
            self.last_reset_date = current_date
    
    def _change_state(self, new_state: TradingState):
        """改变交易状态"""
        old_state = self.current_state
        self.current_state = new_state
        
    def get_status(self) -> Dict:
        """获取当前状态"""
        return {
            'state': self.current_state.value,
            'capital': self.current_capital,
            'position': {
                'active': self.current_position is not None,
                'direction': self.current_position.direction if self.current_position else None,
                'size': self.current_position.size if self.current_position else None,
                'entry_price': self.current_position.entry_price if self.current_position else None,
            } if self.current_position else None,
            'daily_trades': self.daily_trade_count,
            'consecutive_losses': self.consecutive_losses,
            'total_trades': len(self.trade_history),
            'cooldown_remaining': max(0, self.cooldown_end_timestamp - int(time.time() * 1000))
        }