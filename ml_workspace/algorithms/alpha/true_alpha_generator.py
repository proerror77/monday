#!/usr/bin/env python3
"""
真正的从零开始Alpha因子生成器
只需要原始OHLCV数据，自动生成所有Alpha因子表达式
"""

import numpy as np
import pandas as pd
from typing import Dict, List, Any, Optional, Union
import random
from abc import ABC, abstractmethod
from dataclasses import dataclass
import warnings
warnings.filterwarnings('ignore')

@dataclass
class MarketData:
    """原始市场数据结构"""
    open: pd.Series
    high: pd.Series
    low: pd.Series
    close: pd.Series
    volume: pd.Series
    vwap: Optional[pd.Series] = None  # Volume Weighted Average Price

    def __post_init__(self):
        """计算VWAP如果没有提供"""
        if self.vwap is None:
            # 简化的VWAP计算：(high + low + close) / 3
            self.vwap = (self.high + self.low + self.close) / 3

class AlphaOperator(ABC):
    """Alpha操作符基类"""

    @abstractmethod
    def apply(self, *args, **kwargs) -> pd.Series:
        pass

    @abstractmethod
    def get_symbol(self) -> str:
        pass

    @abstractmethod
    def get_arity(self) -> int:
        """返回操作符需要的参数数量"""
        pass

# ============= 原子操作符 (叶节点) =============
class OpenOperator(AlphaOperator):
    def apply(self, data: MarketData) -> pd.Series:
        return data.open

    def get_symbol(self) -> str:
        return "open"

    def get_arity(self) -> int:
        return 0

class CloseOperator(AlphaOperator):
    def apply(self, data: MarketData) -> pd.Series:
        return data.close

    def get_symbol(self) -> str:
        return "close"

    def get_arity(self) -> int:
        return 0

class HighOperator(AlphaOperator):
    def apply(self, data: MarketData) -> pd.Series:
        return data.high

    def get_symbol(self) -> str:
        return "high"

    def get_arity(self) -> int:
        return 0

class LowOperator(AlphaOperator):
    def apply(self, data: MarketData) -> pd.Series:
        return data.low

    def get_symbol(self) -> str:
        return "low"

    def get_arity(self) -> int:
        return 0

class VolumeOperator(AlphaOperator):
    def apply(self, data: MarketData) -> pd.Series:
        return data.volume

    def get_symbol(self) -> str:
        return "volume"

    def get_arity(self) -> int:
        return 0

class VwapOperator(AlphaOperator):
    def apply(self, data: MarketData) -> pd.Series:
        return data.vwap

    def get_symbol(self) -> str:
        return "vwap"

    def get_arity(self) -> int:
        return 0

# ============= 二元操作符 =============
class AddOperator(AlphaOperator):
    def apply(self, left: pd.Series, right: pd.Series) -> pd.Series:
        return left + right

    def get_symbol(self) -> str:
        return "+"

    def get_arity(self) -> int:
        return 2

class SubOperator(AlphaOperator):
    def apply(self, left: pd.Series, right: pd.Series) -> pd.Series:
        return left - right

    def get_symbol(self) -> str:
        return "-"

    def get_arity(self) -> int:
        return 2

class MulOperator(AlphaOperator):
    def apply(self, left: pd.Series, right: pd.Series) -> pd.Series:
        return left * right

    def get_symbol(self) -> str:
        return "*"

    def get_arity(self) -> int:
        return 2

class DivOperator(AlphaOperator):
    def apply(self, left: pd.Series, right: pd.Series) -> pd.Series:
        # 避免除零
        right_safe = right.replace(0, np.nan)
        return left / right_safe

    def get_symbol(self) -> str:
        return "/"

    def get_arity(self) -> int:
        return 2

# ============= 一元操作符 =============
class AbsOperator(AlphaOperator):
    def apply(self, series: pd.Series) -> pd.Series:
        return series.abs()

    def get_symbol(self) -> str:
        return "abs"

    def get_arity(self) -> int:
        return 1

class LogOperator(AlphaOperator):
    def apply(self, series: pd.Series) -> pd.Series:
        # 只对正值取对数
        return np.log(series.where(series > 0))

    def get_symbol(self) -> str:
        return "log"

    def get_arity(self) -> int:
        return 1

class RankOperator(AlphaOperator):
    def apply(self, series: pd.Series) -> pd.Series:
        return series.rank(pct=True)  # 百分位排名

    def get_symbol(self) -> str:
        return "rank"

    def get_arity(self) -> int:
        return 1

# ============= 时序操作符 =============
class DelayOperator(AlphaOperator):
    def __init__(self, period: int = 1):
        self.period = period

    def apply(self, series: pd.Series) -> pd.Series:
        return series.shift(self.period)

    def get_symbol(self) -> str:
        return f"delay_{self.period}"

    def get_arity(self) -> int:
        return 1

class SmaOperator(AlphaOperator):
    def __init__(self, window: int = 5):
        self.window = window

    def apply(self, series: pd.Series) -> pd.Series:
        return series.rolling(window=self.window, min_periods=1).mean()

    def get_symbol(self) -> str:
        return f"sma_{self.window}"

    def get_arity(self) -> int:
        return 1

class StdOperator(AlphaOperator):
    def __init__(self, window: int = 5):
        self.window = window

    def apply(self, series: pd.Series) -> pd.Series:
        return series.rolling(window=self.window, min_periods=1).std()

    def get_symbol(self) -> str:
        return f"std_{self.window}"

    def get_arity(self) -> int:
        return 1

class SumOperator(AlphaOperator):
    def __init__(self, window: int = 5):
        self.window = window

    def apply(self, series: pd.Series) -> pd.Series:
        return series.rolling(window=self.window, min_periods=1).sum()

    def get_symbol(self) -> str:
        return f"sum_{self.window}"

    def get_arity(self) -> int:
        return 1

# ============= 表达式树节点 =============
class ExpressionNode:
    """表达式树节点"""

    def __init__(self, operator: AlphaOperator, children: List['ExpressionNode'] = None):
        self.operator = operator
        self.children = children or []

    def evaluate(self, data: MarketData) -> pd.Series:
        """评估表达式树，返回计算结果"""
        if self.operator.get_arity() == 0:
            # 叶节点：直接返回原始数据
            return self.operator.apply(data)
        elif self.operator.get_arity() == 1:
            # 一元操作符
            child_result = self.children[0].evaluate(data)
            return self.operator.apply(child_result)
        elif self.operator.get_arity() == 2:
            # 二元操作符
            left_result = self.children[0].evaluate(data)
            right_result = self.children[1].evaluate(data)
            return self.operator.apply(left_result, right_result)
        else:
            raise ValueError(f"不支持的操作符元数: {self.operator.get_arity()}")

    def to_string(self) -> str:
        """将表达式树转换为可读字符串"""
        if self.operator.get_arity() == 0:
            return self.operator.get_symbol()
        elif self.operator.get_arity() == 1:
            return f"{self.operator.get_symbol()}({self.children[0].to_string()})"
        elif self.operator.get_arity() == 2:
            left_str = self.children[0].to_string()
            right_str = self.children[1].to_string()
            return f"({left_str} {self.operator.get_symbol()} {right_str})"

    def get_depth(self) -> int:
        """获取表达式树深度"""
        if not self.children:
            return 1
        return 1 + max(child.get_depth() for child in self.children)

class TrueAlphaGenerator:
    """真正的从零开始Alpha因子生成器"""

    def __init__(self, max_depth: int = 4, population_size: int = 100):
        self.max_depth = max_depth
        self.population_size = population_size

        # 定义所有可用的操作符
        self.terminal_ops = [
            OpenOperator(), CloseOperator(), HighOperator(),
            LowOperator(), VolumeOperator(), VwapOperator()
        ]

        self.unary_ops = [
            AbsOperator(), LogOperator(), RankOperator(),
            DelayOperator(1), DelayOperator(5), DelayOperator(10),
            SmaOperator(5), SmaOperator(10), SmaOperator(20),
            StdOperator(5), StdOperator(10), StdOperator(20),
            SumOperator(5), SumOperator(10), SumOperator(20)
        ]

        self.binary_ops = [
            AddOperator(), SubOperator(), MulOperator(), DivOperator()
        ]

        print(f"🚀 真正的Alpha因子生成器初始化:")
        print(f"   原始数据源: OHLCV (6个)")
        print(f"   终端操作符: {len(self.terminal_ops)}")
        print(f"   一元操作符: {len(self.unary_ops)}")
        print(f"   二元操作符: {len(self.binary_ops)}")
        print(f"   最大深度: {max_depth}")
        print(f"   种群大小: {population_size}")

    def generate_random_tree(self, depth: int = 0) -> ExpressionNode:
        """生成随机表达式树"""
        if depth >= self.max_depth:
            # 达到最大深度，只能选择终端节点
            operator = random.choice(self.terminal_ops)
            return ExpressionNode(operator)

        # 随机选择操作符类型
        all_ops = self.terminal_ops + self.unary_ops + self.binary_ops
        operator = random.choice(all_ops)

        if operator.get_arity() == 0:
            # 终端节点
            return ExpressionNode(operator)
        elif operator.get_arity() == 1:
            # 一元操作符
            child = self.generate_random_tree(depth + 1)
            return ExpressionNode(operator, [child])
        elif operator.get_arity() == 2:
            # 二元操作符
            left_child = self.generate_random_tree(depth + 1)
            right_child = self.generate_random_tree(depth + 1)
            return ExpressionNode(operator, [left_child, right_child])

    def calculate_ic(self, alpha_values: pd.Series, target_returns: pd.Series) -> float:
        """计算信息系数(IC)"""
        try:
            # 对齐数据
            aligned_alpha, aligned_target = alpha_values.align(target_returns, join='inner')

            # 移除无效值
            valid_mask = ~(aligned_alpha.isna() | aligned_target.isna() |
                          np.isinf(aligned_alpha) | np.isinf(aligned_target))

            if valid_mask.sum() < 10:  # 至少需要10个有效数据点
                return 0.0

            clean_alpha = aligned_alpha[valid_mask]
            clean_target = aligned_target[valid_mask]

            # 计算相关系数
            correlation = np.corrcoef(clean_alpha, clean_target)[0, 1]
            return 0.0 if np.isnan(correlation) else float(correlation)

        except Exception:
            return 0.0

    def generate_alpha_population(self, market_data: MarketData,
                                target_returns: pd.Series) -> List[Dict[str, Any]]:
        """生成Alpha因子种群"""
        population = []

        print(f"🧬 生成{self.population_size}个随机Alpha因子...")

        for i in range(self.population_size):
            try:
                # 生成随机表达式树
                tree = self.generate_random_tree()

                # 评估Alpha因子
                alpha_values = tree.evaluate(market_data)

                # 计算IC
                ic = self.calculate_ic(alpha_values, target_returns)

                # 保存结果
                alpha_info = {
                    'tree': tree,
                    'expression': tree.to_string(),
                    'alpha_values': alpha_values,
                    'ic': ic,
                    'depth': tree.get_depth(),
                    'fitness': abs(ic)  # 使用IC绝对值作为适应度
                }

                population.append(alpha_info)

                if (i + 1) % 20 == 0:
                    print(f"   已生成 {i + 1}/{self.population_size} 个因子...")

            except Exception as e:
                # 生成失败，跳过
                continue

        # 按适应度排序
        population.sort(key=lambda x: x['fitness'], reverse=True)

        print(f"✅ 成功生成 {len(population)} 个有效Alpha因子")
        return population

    def discover_best_alphas(self, market_data: MarketData,
                           target_returns: pd.Series,
                           top_k: int = 10) -> List[Dict[str, Any]]:
        """发现最佳Alpha因子"""
        print("🔍 开始从零自动发现Alpha因子...")

        # 生成初始种群
        population = self.generate_alpha_population(market_data, target_returns)

        if not population:
            print("❌ 未能生成有效的Alpha因子")
            return []

        # 返回最佳因子
        best_alphas = population[:top_k]

        print(f"\n🏆 发现的最佳Alpha因子 (Top {top_k}):")
        print("="*80)

        for i, alpha in enumerate(best_alphas, 1):
            print(f"{i:2d}. IC = {alpha['ic']:8.6f} | "
                  f"深度 = {alpha['depth']} | "
                  f"表达式: {alpha['expression']}")

        return best_alphas

# 使用示例
def demo_true_alpha_generation():
    """演示真正的从零开始Alpha因子生成"""
    print("🚀 真正的从零开始Alpha因子生成演示")
    print("="*60)

    # 1. 生成模拟的原始OHLCV数据
    np.random.seed(42)
    n_days = 500

    dates = pd.date_range(start='2023-01-01', periods=n_days, freq='D')

    # 模拟价格序列（随机游走）
    base_price = 100
    price_changes = np.random.normal(0, 0.02, n_days)
    close_prices = base_price * np.exp(np.cumsum(price_changes))

    # 生成OHLC和成交量
    daily_volatility = 0.01
    high_prices = close_prices * (1 + np.abs(np.random.normal(0, daily_volatility, n_days)))
    low_prices = close_prices * (1 - np.abs(np.random.normal(0, daily_volatility, n_days)))
    open_prices = np.roll(close_prices, 1)  # 开盘价基于前一日收盘价
    volumes = np.random.lognormal(mean=10, sigma=1, size=n_days)

    # 创建MarketData对象
    market_data = MarketData(
        open=pd.Series(open_prices, index=dates),
        high=pd.Series(high_prices, index=dates),
        low=pd.Series(low_prices, index=dates),
        close=pd.Series(close_prices, index=dates),
        volume=pd.Series(volumes, index=dates)
    )

    # 2. 计算目标收益率（未来1日收益）
    target_returns = market_data.close.pct_change().shift(-1)  # 未来1日收益

    print(f"📊 原始数据概况:")
    print(f"   时间范围: {dates[0]} 至 {dates[-1]}")
    print(f"   数据点数: {n_days}")
    print(f"   价格范围: {close_prices.min():.2f} - {close_prices.max():.2f}")

    # 3. 创建Alpha生成器
    generator = TrueAlphaGenerator(
        max_depth=3,        # 限制复杂度
        population_size=50  # 演示用较小种群
    )

    # 4. 发现最佳Alpha因子
    best_alphas = generator.discover_best_alphas(
        market_data=market_data,
        target_returns=target_returns,
        top_k=5
    )

    # 5. 展示最佳Alpha因子的详细信息
    if best_alphas:
        print(f"\n📈 最佳Alpha因子详细分析:")
        print("="*80)

        for i, alpha in enumerate(best_alphas[:3], 1):
            print(f"\n🏆 第{i}名: IC = {alpha['ic']:.6f}")
            print(f"   表达式: {alpha['expression']}")
            print(f"   树深度: {alpha['depth']}")

            # 显示Alpha值的统计信息
            alpha_stats = alpha['alpha_values'].describe()
            print(f"   Alpha值统计:")
            print(f"     均值: {alpha_stats['mean']:.6f}")
            print(f"     标准差: {alpha_stats['std']:.6f}")
            print(f"     范围: [{alpha_stats['min']:.6f}, {alpha_stats['max']:.6f}]")

    return best_alphas

if __name__ == "__main__":
    demo_true_alpha_generation()