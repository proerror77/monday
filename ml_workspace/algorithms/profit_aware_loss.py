#!/usr/bin/env python3
"""
以获利为导向的HFT损失函数
包含手续费、动态出场时机决策、交易量最大化
"""

import torch
import torch.nn as nn
import numpy as np
from typing import Dict, List, Tuple, Optional

class ProfitAwareLoss(nn.Module):
    """
    以获利为导向的损失函数
    
    核心目标:
    1. 最大化净利润 (扣除手续费)
    2. 最大化交易频率 (在获利前提下)
    3. 动态出场时机决策 (1-60秒)
    4. 风险控制 (最大回撤、夏普比率)
    """
    
    def __init__(
        self,
        fee_rate: float = 0.0006,  # 0.06% 手续费 (买卖各0.03%)
        min_profit_bps: float = 1.0,  # 最小获利要求 (1 basis point)
        max_position_seconds: int = 60,  # 最大持仓时间
        volume_reward_weight: float = 0.3,  # 交易量奖励权重
        risk_penalty_weight: float = 0.2,   # 风险惩罚权重
        sharpe_target: float = 2.0,  # 目标夏普比率
    ):
        super().__init__()
        self.fee_rate = fee_rate
        self.min_profit_bps = min_profit_bps / 10000  # 转换为小数
        self.max_position_seconds = max_position_seconds
        self.volume_reward_weight = volume_reward_weight
        self.risk_penalty_weight = risk_penalty_weight
        self.sharpe_target = sharpe_target
        
    def forward(
        self,
        entry_signals: torch.Tensor,      # [batch, 1] 入场信号强度 (0-1)
        exit_timing: torch.Tensor,        # [batch, max_position_seconds] 各时刻出场概率
        future_returns: torch.Tensor,     # [batch, max_position_seconds] 未来收益率
        current_prices: torch.Tensor,     # [batch, 1] 当前价格
    ) -> Dict[str, torch.Tensor]:
        """
        计算以获利为导向的损失
        
        Args:
            entry_signals: 入场信号强度 (0=不交易, 1=强烈看涨, -1=强烈看跌)
            exit_timing: 各时刻的出场概率分布 (softmax后的概率)
            future_returns: 各时刻的实际收益率
            current_prices: 当前价格水平
        """
        batch_size = entry_signals.size(0)
        device = entry_signals.device
        
        # 1. 计算交易决策
        should_trade = torch.abs(entry_signals) > 0.1  # 信号强度阈值
        trade_direction = torch.sign(entry_signals)     # 1=买入, -1=卖出
        
        # 2. 计算动态出场时机 (基于exit_timing概率分布)
        exit_probs = torch.softmax(exit_timing, dim=-1)  # 概率归一化
        expected_exit_time = torch.sum(
            exit_probs * torch.arange(1, self.max_position_seconds + 1, device=device).float(),
            dim=-1, keepdim=True
        )  # 期望出场时间
        
        # 3. 计算各种收益场景
        net_profits = []
        trade_counts = []
        
        for t in range(self.max_position_seconds):
            # t+1秒后出场的净收益
            raw_return = future_returns[:, t:t+1]  # 原始收益率
            
            # 考虑交易方向
            directional_return = trade_direction * raw_return
            
            # 扣除双边手续费
            net_return = directional_return - self.fee_rate
            
            # 只计算应该交易且获利的情况
            profitable_trades = (net_return > self.min_profit_bps) & should_trade
            
            # 该时刻出场的净利润
            profit_at_t = torch.where(
                profitable_trades,
                net_return * exit_probs[:, t:t+1] * torch.abs(entry_signals),
                torch.zeros_like(net_return)
            )
            
            net_profits.append(profit_at_t)
            trade_counts.append(profitable_trades.float() * exit_probs[:, t:t+1])
        
        # 4. 汇总指标
        total_profit = torch.sum(torch.cat(net_profits, dim=1), dim=1, keepdim=True)
        total_trades = torch.sum(torch.cat(trade_counts, dim=1), dim=1, keepdim=True)
        
        # 5. 计算平均单笔收益和交易频率
        avg_profit_per_trade = torch.where(
            total_trades > 0,
            total_profit / (total_trades + 1e-8),
            torch.zeros_like(total_profit)
        )
        
        # 6. 风险控制指标
        profit_volatility = torch.std(torch.cat(net_profits, dim=1), dim=1, keepdim=True)
        sharpe_ratio = torch.where(
            profit_volatility > 1e-6,
            total_profit / (profit_volatility + 1e-8),
            torch.zeros_like(total_profit)
        )
        
        # 7. 构建复合损失函数 (负值表示要最大化的目标)
        
        # 主要目标: 最大化净利润
        profit_loss = -total_profit.mean()
        
        # 次要目标: 鼓励更多获利交易
        volume_reward = -self.volume_reward_weight * total_trades.mean()
        
        # 风险控制: 鼓励高夏普比率
        sharpe_penalty = self.risk_penalty_weight * torch.relu(self.sharpe_target - sharpe_ratio).mean()
        
        # 出场时机优化: 鼓励在最优时间出场
        timing_loss = 0.1 * torch.mean(torch.abs(expected_exit_time - self._optimal_exit_time(future_returns)))
        
        total_loss = profit_loss + volume_reward + sharpe_penalty + timing_loss
        
        return {
            'total_loss': total_loss,
            'profit_loss': profit_loss,
            'volume_reward': volume_reward,
            'sharpe_penalty': sharpe_penalty,
            'timing_loss': timing_loss,
            'total_profit': total_profit.mean(),
            'total_trades': total_trades.mean(),
            'avg_profit_per_trade': avg_profit_per_trade.mean(),
            'sharpe_ratio': sharpe_ratio.mean(),
            'expected_exit_time': expected_exit_time.mean()
        }
    
    def _optimal_exit_time(self, future_returns: torch.Tensor) -> torch.Tensor:
        """
        计算理论最优出场时间 (收益率最大的时刻)
        """
        # 找到每个样本的最佳出场时刻
        max_return_indices = torch.argmax(future_returns, dim=1, keepdim=True).float() + 1
        return max_return_indices


class TradingMetrics:
    """
    交易性能指标计算器
    """
    
    @staticmethod
    def calculate_pnl(
        signals: np.ndarray,
        returns: np.ndarray,
        exit_times: np.ndarray,
        fee_rate: float = 0.0006
    ) -> Dict[str, float]:
        """
        计算交易盈亏指标
        
        Args:
            signals: 入场信号 [-1, 1]
            returns: 各时刻收益率 [N, max_time]
            exit_times: 出场时间 [N]
            fee_rate: 手续费率
        """
        n_samples = len(signals)
        profits = []
        
        for i in range(n_samples):
            if abs(signals[i]) < 0.1:  # 不交易
                continue
                
            exit_t = int(exit_times[i]) - 1  # 转换为索引
            exit_t = min(exit_t, returns.shape[1] - 1)
            
            # 计算净收益
            raw_return = returns[i, exit_t]
            directional_return = signals[i] * raw_return
            net_return = directional_return - fee_rate
            
            if net_return > 0:  # 只记录获利交易
                profits.append(net_return)
        
        if not profits:
            return {
                'total_profit': 0.0,
                'num_trades': 0,
                'win_rate': 0.0,
                'avg_profit_per_trade': 0.0,
                'sharpe_ratio': 0.0
            }
        
        profits = np.array(profits)
        
        return {
            'total_profit': np.sum(profits),
            'num_trades': len(profits),
            'win_rate': 1.0,  # 只统计获利交易
            'avg_profit_per_trade': np.mean(profits),
            'sharpe_ratio': np.mean(profits) / (np.std(profits) + 1e-8) * np.sqrt(252 * 24 * 3600)  # 年化夏普
        }


class DynamicExitModel(nn.Module):
    """
    动态出场时机决策模型
    结合TCN-GRU输出，预测最优出场时间分布
    """
    
    def __init__(
        self,
        input_dim: int,
        hidden_dim: int = 64,
        max_exit_time: int = 60,
        dropout: float = 0.1
    ):
        super().__init__()
        self.max_exit_time = max_exit_time
        
        # 入场信号预测
        self.entry_head = nn.Sequential(
            nn.Linear(input_dim, hidden_dim),
            nn.ReLU(),
            nn.Dropout(dropout),
            nn.Linear(hidden_dim, 1),
            nn.Tanh()  # [-1, 1] 信号强度
        )
        
        # 出场时机预测
        self.exit_head = nn.Sequential(
            nn.Linear(input_dim, hidden_dim),
            nn.ReLU(), 
            nn.Dropout(dropout),
            nn.Linear(hidden_dim, max_exit_time)
            # 输出未归一化的logits，后续softmax
        )
        
    def forward(self, features: torch.Tensor) -> Tuple[torch.Tensor, torch.Tensor]:
        """
        Args:
            features: [batch, feature_dim] TCN-GRU的输出特征
            
        Returns:
            entry_signals: [batch, 1] 入场信号强度
            exit_logits: [batch, max_exit_time] 出场时机logits
        """
        entry_signals = self.entry_head(features)
        exit_logits = self.exit_head(features)
        
        return entry_signals, exit_logits