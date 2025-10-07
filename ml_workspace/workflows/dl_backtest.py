#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
监督学习模型回测模块
"""

from __future__ import annotations

import argparse
import os
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Tuple, Any

import numpy as np
import pandas as pd
import torch
import matplotlib.pyplot as plt
import json

from utils.clickhouse_client import ClickHouseClient
from utils.time import to_ms
from workflows.dl_predict import load_dl_model, predict_single_horizon


@dataclass
class BacktestConfig:
    """回测配置"""
    taker_fee: float = 0.0006  # 每侧手续费
    slippage: float = 0.0001   # 每侧滑点
    min_edge_bps: float = 5.0  # 最小预期优势(bps)
    max_position_pct: float = 0.1  # 最大仓位比例
    
    def round_trip_cost(self) -> float:
        """往返交易成本"""
        return 2.0 * (self.taker_fee + self.slippage)


def run_simple_backtest(
    predictions_df: pd.DataFrame,
    config: BacktestConfig,
    initial_equity: float = 10000.0
) -> Dict[str, Any]:
    """执行简单回测
    
    Args:
        predictions_df: 包含sec, mid_price, prediction, symbol, horizon的DataFrame
        config: 回测配置
        initial_equity: 初始资金
    
    Returns:
        回测结果字典
    """
    if predictions_df.empty:
        return {
            'final_equity': initial_equity,
            'total_return': 0.0,
            'max_drawdown': 0.0,
            'trades': [],
            'sharpe_ratio': 0.0,
            'win_rate': 0.0
        }
    
    df = predictions_df.copy().sort_values('sec').reset_index(drop=True)
    
    # 构造每秒索引，便于稳健查找退出时的 mid_price（参考无监督回测）
    sec_to_price = dict(zip(df['sec'].astype(int).tolist(), df['mid_price'].astype(float).tolist()))
    
    equity = initial_equity
    max_equity = equity
    trades = []
    
    # 简化回测：基于预测信号开平仓
    min_edge = config.min_edge_bps / 10000.0  # 转换为小数
    round_trip_cost = config.round_trip_cost()
    
    open_position = None
    
    for i in range(len(df)):
        row = df.iloc[i]
        current_sec = int(row['sec'])
        current_price = float(row['mid_price'])
        pred = float(row['prediction'])
        horizon = int(row['horizon'])
        
        # 检查是否需要平仓（持仓到期）
        if open_position is not None:
            if current_sec >= open_position['exit_sec']:
                # 稳健查找退出价格：先尝试精确时间，然后尝试就近几秒（参考无监督回测）
                exit_price = None
                for off in [0, 1, 2, 3]:
                    exit_price = sec_to_price.get(open_position['exit_sec'] + off)
                    if exit_price is not None:
                        break
                
                if exit_price is None:
                    # 无法找到合适的退出价格，跳过这笔交易（实务中可延后退出）
                    continue
                
                entry_price = open_position['entry_price']
                direction = open_position['direction']
                
                if direction > 0:  # 做多
                    gross_return = (exit_price - entry_price) / entry_price
                else:  # 做空
                    gross_return = (entry_price - exit_price) / entry_price
                
                net_return = gross_return - round_trip_cost
                position_size = initial_equity * config.max_position_pct
                pnl = position_size * net_return
                equity += pnl
                max_equity = max(max_equity, equity)
                
                trades.append({
                    'entry_sec': open_position['entry_sec'],
                    'exit_sec': open_position['exit_sec'],  # 使用预期退出时间而不是实际遍历到的时间
                    'entry_price': entry_price,
                    'exit_price': exit_price,
                    'direction': direction,
                    'gross_return': gross_return,
                    'net_return': net_return,
                    'pnl': pnl,
                    'equity': equity
                })
                
                open_position = None
        
        # 检查是否开新仓
        if open_position is None:
            # 计算净预期收益
            expected_gross = abs(pred)
            expected_net = expected_gross - round_trip_cost
            
            if expected_net > min_edge:
                # 开仓
                direction = 1 if pred > 0 else -1
                exit_sec = current_sec + horizon
                
                open_position = {
                    'entry_sec': current_sec,
                    'exit_sec': exit_sec,
                    'entry_price': current_price,
                    'direction': direction,
                    'expected_return': pred
                }
    
    # 计算最终指标
    total_return = (equity - initial_equity) / initial_equity
    
    # 计算最大回撤
    max_dd = 0.0
    if trades:
        equity_curve = [initial_equity] + [t['equity'] for t in trades]
        peak = equity_curve[0]
        for eq in equity_curve[1:]:
            peak = max(peak, eq)
            dd = (peak - eq) / peak
            max_dd = max(max_dd, dd)
    
    # 计算胜率
    win_rate = 0.0
    if trades:
        wins = sum(1 for t in trades if t['net_return'] > 0)
        win_rate = wins / len(trades)
    
    # 计算夏普比率（简化版）
    sharpe_ratio = 0.0
    if trades and len(trades) > 1:
        returns = [t['net_return'] for t in trades]
        mean_return = np.mean(returns)
        std_return = np.std(returns)
        if std_return > 0:
            sharpe_ratio = mean_return / std_return * np.sqrt(252)  # 年化
    
    return {
        'final_equity': equity,
        'total_return': total_return,
        'max_drawdown': max_dd,
        'trades': trades,
        'sharpe_ratio': sharpe_ratio,
        'win_rate': win_rate,
        'num_trades': len(trades)
    }

def plot_equity_curve(trades: List[Dict], initial_equity: float, output_path: str):
    """繪製權益曲線圖並儲存"""
    if not trades:
        print("沒有交易可供繪圖。")
        return

    equity_curve = [initial_equity] + [t['equity'] for t in trades]
    
    plt.style.use('seaborn-v0_8-darkgrid')
    plt.figure(figsize=(12, 7))
    plt.plot(equity_curve, marker='o', markersize=3, linestyle='-')
    plt.title('Equity Curve / Cumulative Profit')
    plt.xlabel('Trade Number')
    plt.ylabel('Equity ($)')
    plt.grid(True)
    plt.tight_layout()
    
    try:
        plt.savefig(output_path)
        print(f"📈 權益曲線圖已儲存至: {output_path}")
    except Exception as e:
        print(f"❌ 無法儲存圖表: {e}")
    finally:
        plt.close()


def main():
    parser = argparse.ArgumentParser(description='监督学习模型回测')
    parser.add_argument('--model', required=True, help='模型文件路径(.pt)')
    parser.add_argument('--symbol', required=True, help='交易品种')
    parser.add_argument('--start', required=True, help='开始时间 (ISO format)')
    parser.add_argument('--end', required=True, help='结束时间 (ISO format)')
    parser.add_argument('--horizon', type=int, default=5, help='预测时间地平线(秒)')
    parser.add_argument('--min_edge_bps', type=float, default=5.0, help='最小预期优势(bps)')
    parser.add_argument('--taker_fee', type=float, default=0.0006, help='手续费率')
    parser.add_argument('--slippage', type=float, default=0.0001, help='滑点率')
    parser.add_argument('--initial_equity', type=float, default=10000.0, help='初始资金')
    parser.add_argument('--max_position_pct', type=float, default=0.1, help='最大仓位比例')
    parser.add_argument('--host', default=os.getenv('CLICKHOUSE_HOST'))
    parser.add_argument('--user', default=os.getenv('CLICKHOUSE_USERNAME', 'default'))
    parser.add_argument('--password', default=os.getenv('CLICKHOUSE_PASSWORD', ''))
    parser.add_argument('--database', default=os.getenv('CLICKHOUSE_DATABASE', 'hft'))
    parser.add_argument('--plot-output', type=str, default='equity_curve.png', help='儲存權益曲線圖的路徑')
    parser.add_argument('--trades-output', type=str, default='trades.json', help='儲存詳細交易紀錄的JSON檔案路徑')
    
    args = parser.parse_args()
    
    # 验证模型文件
    model_path = Path(args.model)
    if not model_path.exists():
        raise FileNotFoundError(f"Model file not found: {model_path}")
    
    # 解析时间
    start_dt = datetime.fromisoformat(args.start.replace('Z', '+00:00'))
    end_dt = datetime.fromisoformat(args.end.replace('Z', '+00:00'))
    start_ms = to_ms(start_dt)
    end_ms = to_ms(end_dt)
    
    # 设备设置
    device = torch.device(
        'mps' if torch.backends.mps.is_available() else 
        ('cuda' if torch.cuda.is_available() else 'cpu')
    )
    
    # 加载模型
    model, metadata = load_dl_model(str(model_path), device)
    seq_len = metadata['sequence_length']
    
    # 连接ClickHouse
    client = ClickHouseClient(
        host=args.host,
        username=args.user,
        password=args.password,
        database=args.database
    )
    
    if not client.test_connection():
        raise RuntimeError('Cannot connect to ClickHouse')
    
    # 生成预测
    print(f"生成预测 - 品种: {args.symbol}, 时间范围: {args.start} 到 {args.end}")
    predictions = predict_single_horizon(
        client=client,
        model=model,
        symbol=args.symbol,
        start_ms=start_ms,
        end_ms=end_ms,
        seq_len=seq_len,
        horizon=args.horizon,
        device=device
    )
    
    if predictions.empty:
        print("没有生成预测数据 - 数据不足或结果为空")
        return
    
    print(f"生成了 {len(predictions)} 个预测")
    
    # 回测配置
    config = BacktestConfig(
        taker_fee=args.taker_fee,
        slippage=args.slippage,
        min_edge_bps=args.min_edge_bps,
        max_position_pct=args.max_position_pct
    )
    
    # 执行回测
    results = run_simple_backtest(predictions, config, args.initial_equity)
    
    # 输出结果
    print("\n=== 回测结果 ===")
    print(f"初始资金: {args.initial_equity:,.2f}")
    print(f"期末资金: {results['final_equity']:,.2f}")
    print(f"总收益率: {results['total_return']*100:.2f}%")
    print(f"最大回撤: {results['max_drawdown']*100:.2f}%")
    print(f"交易次数: {results['num_trades']}")
    print(f"胜率: {results['win_rate']*100:.2f}%")
    print(f"夏普比率: {results['sharpe_ratio']:.2f}")
    
    if results['trades']:
        print(f"\n前5笔交易:")
        for i, trade in enumerate(results['trades'][:5]):
            print(f"  {i+1}. 方向:{'多' if trade['direction']>0 else '空'} "
                  f"收益: {trade['net_return']*100:.2f}% "
                  f"PnL: {trade['pnl']:+.2f}")


if __name__ == '__main__':
    main()