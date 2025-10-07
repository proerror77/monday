#!/usr/bin/env python3
"""
LOB-DSL Alpha因子搜索引擎
基于遗传编程和语义搜索的自动Alpha因子发现系统
"""

import numpy as np
import pandas as pd
from typing import List, Dict, Tuple, Optional, Union, Any, Callable
from dataclasses import dataclass
from abc import ABC, abstractmethod
from enum import Enum
import ast
import operator
import random
import warnings
from copy import deepcopy
import hashlib
from collections import deque
import time

from scipy import stats
from sklearn.metrics import mutual_info_regression
import sympy as sp
from sympy import symbols, sympify, simplify

from .data_structures import LOBSnapshot, Trade, Side
from .features import FeatureEngineeringPipeline, FeatureConfig


class OperatorType(Enum):
    """操作符类型"""
    # 原始数据操作符
    TERMINAL = "terminal"       # bid_price_1, ask_size_2, etc.

    # 时序操作符
    TEMPORAL = "temporal"       # sma, ema, delay, rank, etc.

    # 数学操作符
    ARITHMETIC = "arithmetic"   # +, -, *, /, abs, log, etc.

    # 逻辑操作符
    LOGICAL = "logical"         # <, >, ==, and, or, etc.

    # 聚合操作符
    AGGREGATION = "aggregate"   # sum, mean, std, max, min, etc.


@dataclass
class Operator:
    """操作符定义"""
    name: str
    symbol: str
    operator_type: OperatorType
    arity: int                  # 参数个数
    function: Callable
    constraints: Dict[str, Any] = None

    def __post_init__(self):
        if self.constraints is None:
            self.constraints = {}


class ExpressionNode:
    """表达式树节点"""

    def __init__(self, operator: Operator, children: List['ExpressionNode'] = None):
        self.operator = operator
        self.children = children or []
        self.depth = self._calculate_depth()
        self.hash_value = None

    def _calculate_depth(self) -> int:
        """计算节点深度"""
        if not self.children:
            return 1
        return 1 + max(child.depth for child in self.children)

    def evaluate(self, context: Dict[str, np.ndarray]) -> np.ndarray:
        """评估表达式"""
        if self.operator.operator_type == OperatorType.TERMINAL:
            # 终端节点：直接从context获取数据
            return context.get(self.operator.name, np.zeros(len(next(iter(context.values())))))

        # 非终端节点：递归评估子节点
        child_results = []
        for child in self.children:
            result = child.evaluate(context)
            child_results.append(result)

        try:
            return self.operator.function(*child_results)
        except (ZeroDivisionError, ValueError, RuntimeWarning):
            # 处理数值错误
            return np.full_like(child_results[0], np.nan)

    def to_string(self) -> str:
        """转换为字符串表示"""
        if self.operator.operator_type == OperatorType.TERMINAL:
            return self.operator.name

        if self.operator.arity == 1:
            return f"{self.operator.symbol}({self.children[0].to_string()})"
        elif self.operator.arity == 2:
            return f"({self.children[0].to_string()} {self.operator.symbol} {self.children[1].to_string()})"
        else:
            child_strs = [child.to_string() for child in self.children]
            return f"{self.operator.symbol}({', '.join(child_strs)})"

    def get_hash(self) -> str:
        """获取表达式哈希值"""
        if self.hash_value is None:
            self.hash_value = hashlib.md5(self.to_string().encode()).hexdigest()
        return self.hash_value

    def count_nodes(self) -> int:
        """计算节点总数"""
        if not self.children:
            return 1
        return 1 + sum(child.count_nodes() for child in self.children)

    def get_terminals(self) -> List[str]:
        """获取所有终端节点"""
        if self.operator.operator_type == OperatorType.TERMINAL:
            return [self.operator.name]

        terminals = []
        for child in self.children:
            terminals.extend(child.get_terminals())
        return terminals


@dataclass
class AlphaCandidate:
    """Alpha候选因子"""
    expression: ExpressionNode
    ic: float                   # Information Coefficient
    ir: float                   # Information Ratio
    fitness: float              # 适应度得分
    complexity: int             # 表达式复杂度
    generation: int             # 生成代数
    age: int = 0               # 年龄（迭代次数）

    def __post_init__(self):
        self.hash_value = self.expression.get_hash()

    def is_valid(self) -> bool:
        """检查因子有效性"""
        return (not np.isnan(self.ic) and
                not np.isnan(self.ir) and
                abs(self.ic) > 0.001 and
                self.complexity < 50)


class OperatorLibrary:
    """操作符库"""

    def __init__(self):
        self.operators = {}
        self._initialize_operators()

    def _initialize_operators(self):
        """初始化操作符库"""

        # 终端操作符（LOB数据字段）
        terminal_fields = [
            'bid_price_1', 'bid_size_1', 'ask_price_1', 'ask_size_1',
            'bid_price_2', 'bid_size_2', 'ask_price_2', 'ask_size_2',
            'bid_price_3', 'bid_size_3', 'ask_price_3', 'ask_size_3',
            'spread', 'mid_price', 'microprice', 'volume_imbalance',
            'trade_size', 'trade_intensity', 'ofi'
        ]

        for field in terminal_fields:
            self.operators[field] = Operator(
                name=field,
                symbol=field,
                operator_type=OperatorType.TERMINAL,
                arity=0,
                function=lambda: None  # Terminal nodes don't need functions
            )

        # 数学操作符
        self.operators['add'] = Operator('add', '+', OperatorType.ARITHMETIC, 2,
                                       lambda x, y: x + y)
        self.operators['sub'] = Operator('sub', '-', OperatorType.ARITHMETIC, 2,
                                       lambda x, y: x - y)
        self.operators['mul'] = Operator('mul', '*', OperatorType.ARITHMETIC, 2,
                                       lambda x, y: x * y)
        self.operators['div'] = Operator('div', '/', OperatorType.ARITHMETIC, 2,
                                       lambda x, y: np.divide(x, y, out=np.zeros_like(x), where=(y!=0)))

        self.operators['abs'] = Operator('abs', 'abs', OperatorType.ARITHMETIC, 1,
                                       lambda x: np.abs(x))
        self.operators['log'] = Operator('log', 'log', OperatorType.ARITHMETIC, 1,
                                       lambda x: np.log(np.maximum(x, 1e-8)))
        self.operators['sqrt'] = Operator('sqrt', 'sqrt', OperatorType.ARITHMETIC, 1,
                                        lambda x: np.sqrt(np.maximum(x, 0)))

        # 时序操作符
        self.operators['sma'] = Operator('sma', 'sma', OperatorType.TEMPORAL, 2,
                                       self._sma_function)
        self.operators['ema'] = Operator('ema', 'ema', OperatorType.TEMPORAL, 2,
                                       self._ema_function)
        self.operators['delay'] = Operator('delay', 'delay', OperatorType.TEMPORAL, 2,
                                         self._delay_function)
        self.operators['rank'] = Operator('rank', 'rank', OperatorType.TEMPORAL, 1,
                                        self._rank_function)
        self.operators['std'] = Operator('std', 'std', OperatorType.TEMPORAL, 2,
                                       self._std_function)

        # 聚合操作符
        self.operators['mean'] = Operator('mean', 'mean', OperatorType.AGGREGATION, 1,
                                        lambda x: np.full_like(x, np.nanmean(x)))
        self.operators['max'] = Operator('max', 'max', OperatorType.AGGREGATION, 1,
                                       lambda x: np.full_like(x, np.nanmax(x)))
        self.operators['min'] = Operator('min', 'min', OperatorType.AGGREGATION, 1,
                                       lambda x: np.full_like(x, np.nanmin(x)))

        # 逻辑操作符
        self.operators['gt'] = Operator('gt', '>', OperatorType.LOGICAL, 2,
                                      lambda x, y: (x > y).astype(float))
        self.operators['lt'] = Operator('lt', '<', OperatorType.LOGICAL, 2,
                                      lambda x, y: (x < y).astype(float))

    def _sma_function(self, data: np.ndarray, window: np.ndarray) -> np.ndarray:
        """简单移动平均"""
        window_size = max(1, int(np.median(np.abs(window))) % 20 + 1)
        result = np.convolve(data, np.ones(window_size)/window_size, mode='same')
        return result

    def _ema_function(self, data: np.ndarray, alpha: np.ndarray) -> np.ndarray:
        """指数移动平均"""
        alpha_val = np.clip(np.median(np.abs(alpha)), 0.01, 0.99)
        result = np.zeros_like(data)
        result[0] = data[0]
        for i in range(1, len(data)):
            result[i] = alpha_val * data[i] + (1 - alpha_val) * result[i-1]
        return result

    def _delay_function(self, data: np.ndarray, periods: np.ndarray) -> np.ndarray:
        """延迟函数"""
        period = max(1, int(np.median(np.abs(periods))) % 10 + 1)
        result = np.zeros_like(data)
        result[period:] = data[:-period]
        return result

    def _rank_function(self, data: np.ndarray) -> np.ndarray:
        """排名函数"""
        return stats.rankdata(data, method='ordinal') / len(data)

    def _std_function(self, data: np.ndarray, window: np.ndarray) -> np.ndarray:
        """滚动标准差"""
        window_size = max(1, int(np.median(np.abs(window))) % 20 + 1)
        result = np.zeros_like(data)
        for i in range(len(data)):
            start_idx = max(0, i - window_size + 1)
            result[i] = np.std(data[start_idx:i+1])
        return result

    def get_operator(self, name: str) -> Optional[Operator]:
        """获取操作符"""
        return self.operators.get(name)

    def get_terminals(self) -> List[Operator]:
        """获取所有终端操作符"""
        return [op for op in self.operators.values()
                if op.operator_type == OperatorType.TERMINAL]

    def get_functions(self) -> List[Operator]:
        """获取所有函数操作符"""
        return [op for op in self.operators.values()
                if op.operator_type != OperatorType.TERMINAL]


class GeneticProgramming:
    """遗传编程算法"""

    def __init__(self, operator_library: OperatorLibrary,
                 population_size: int = 100,
                 max_depth: int = 4,
                 mutation_rate: float = 0.1,
                 crossover_rate: float = 0.8,
                 elitism_ratio: float = 0.1):

        self.operator_library = operator_library
        self.population_size = population_size
        self.max_depth = max_depth
        self.mutation_rate = mutation_rate
        self.crossover_rate = crossover_rate
        self.elitism_ratio = elitism_ratio

        self.population: List[AlphaCandidate] = []
        self.generation = 0
        self.best_candidates: List[AlphaCandidate] = []

    def initialize_population(self) -> None:
        """初始化种群"""
        self.population = []

        for _ in range(self.population_size):
            expression = self._generate_random_expression(max_depth=self.max_depth)
            candidate = AlphaCandidate(
                expression=expression,
                ic=0.0,
                ir=0.0,
                fitness=0.0,
                complexity=expression.count_nodes(),
                generation=0
            )
            self.population.append(candidate)

    def _generate_random_expression(self, max_depth: int, current_depth: int = 0) -> ExpressionNode:
        """生成随机表达式"""
        if current_depth >= max_depth or random.random() < 0.3:
            # 生成终端节点
            terminals = self.operator_library.get_terminals()
            operator = random.choice(terminals)
            return ExpressionNode(operator)

        # 生成函数节点
        functions = self.operator_library.get_functions()
        operator = random.choice(functions)

        children = []
        for _ in range(operator.arity):
            child = self._generate_random_expression(max_depth, current_depth + 1)
            children.append(child)

        return ExpressionNode(operator, children)

    def evaluate_population(self, data_context: Dict[str, np.ndarray],
                          target_returns: np.ndarray,
                          cv_splitter: Optional[Any] = None) -> None:
        """评估种群适应度"""
        for candidate in self.population:
            try:
                # 计算Alpha因子值
                alpha_values = candidate.expression.evaluate(data_context)

                # 处理无效值
                if np.any(np.isnan(alpha_values)) or np.any(np.isinf(alpha_values)):
                    candidate.ic = 0.0
                    candidate.ir = 0.0
                    candidate.fitness = 0.0
                    continue

                if len(alpha_values) != len(target_returns):
                    candidate.ic = 0.0
                    candidate.ir = 0.0
                    candidate.fitness = 0.0
                    continue

                if cv_splitter:
                    ic_scores = []
                    ir_scores = []

                    for _, test_idx in cv_splitter.split(np.arange(len(alpha_values))):
                        if len(test_idx) < 3:
                            continue

                        ordered_idx = np.sort(test_idx)
                        alpha_fold = alpha_values[ordered_idx]
                        returns_fold = target_returns[ordered_idx]

                        ic_val = np.corrcoef(alpha_fold, returns_fold)[0, 1]
                        if not np.isnan(ic_val):
                            ic_scores.append(ic_val)

                        alpha_diff = np.diff(alpha_fold)
                        if len(alpha_diff) > 1:
                            ir_val = np.mean(alpha_diff) / (np.std(alpha_diff) + 1e-8)
                            if not np.isnan(ir_val):
                                ir_scores.append(ir_val)

                    candidate.ic = float(np.mean(ic_scores)) if ic_scores else 0.0
                    candidate.ir = float(np.mean(ir_scores)) if ir_scores else 0.0
                else:
                    ic = np.corrcoef(alpha_values, target_returns)[0, 1]
                    candidate.ic = ic if not np.isnan(ic) else 0.0

                    if abs(candidate.ic) > 0.001:
                        alpha_returns = np.diff(alpha_values)
                        if len(alpha_returns) > 1:
                            ir = np.mean(alpha_returns) / (np.std(alpha_returns) + 1e-8)
                            candidate.ir = ir if not np.isnan(ir) else 0.0
                        else:
                            candidate.ir = 0.0
                    else:
                        candidate.ir = 0.0

                # 计算适应度（包含复杂度惩罚）
                complexity_penalty = candidate.complexity / 100.0
                candidate.fitness = abs(candidate.ic) * 0.7 + abs(candidate.ir) * 0.3 - complexity_penalty

            except Exception as e:
                candidate.ic = 0.0
                candidate.ir = 0.0
                candidate.fitness = 0.0
                warnings.warn(f"Error evaluating candidate: {e}")

    def selection(self) -> List[AlphaCandidate]:
        """锦标赛选择"""
        selected = []
        tournament_size = 3

        for _ in range(self.population_size):
            tournament = random.sample(self.population, tournament_size)
            winner = max(tournament, key=lambda x: x.fitness)
            selected.append(deepcopy(winner))

        return selected

    def crossover(self, parent1: AlphaCandidate, parent2: AlphaCandidate) -> Tuple[AlphaCandidate, AlphaCandidate]:
        """交叉操作"""
        if random.random() > self.crossover_rate:
            return deepcopy(parent1), deepcopy(parent2)

        # 深度拷贝父代表达式
        expr1 = deepcopy(parent1.expression)
        expr2 = deepcopy(parent2.expression)

        # 随机选择交叉点
        nodes1 = self._get_all_nodes(expr1)
        nodes2 = self._get_all_nodes(expr2)

        if len(nodes1) > 1 and len(nodes2) > 1:
            node1 = random.choice(nodes1)
            node2 = random.choice(nodes2)

            # 交换子树
            node1.operator, node2.operator = node2.operator, node1.operator
            node1.children, node2.children = node2.children, node1.children

        child1 = AlphaCandidate(
            expression=expr1,
            ic=0.0, ir=0.0, fitness=0.0,
            complexity=expr1.count_nodes(),
            generation=self.generation + 1
        )

        child2 = AlphaCandidate(
            expression=expr2,
            ic=0.0, ir=0.0, fitness=0.0,
            complexity=expr2.count_nodes(),
            generation=self.generation + 1
        )

        return child1, child2

    def mutation(self, candidate: AlphaCandidate) -> AlphaCandidate:
        """变异操作"""
        if random.random() > self.mutation_rate:
            return candidate

        # 深度拷贝
        mutated_expr = deepcopy(candidate.expression)

        # 随机选择变异点
        nodes = self._get_all_nodes(mutated_expr)
        if len(nodes) > 0:
            node = random.choice(nodes)

            # 替换为随机子树
            new_subtree = self._generate_random_expression(max_depth=2)
            node.operator = new_subtree.operator
            node.children = new_subtree.children

        return AlphaCandidate(
            expression=mutated_expr,
            ic=0.0, ir=0.0, fitness=0.0,
            complexity=mutated_expr.count_nodes(),
            generation=self.generation + 1
        )

    def _get_all_nodes(self, expression: ExpressionNode) -> List[ExpressionNode]:
        """获取表达式的所有节点"""
        nodes = [expression]
        for child in expression.children:
            nodes.extend(self._get_all_nodes(child))
        return nodes

    def evolve_generation(self, data_context: Dict[str, np.ndarray],
                         target_returns: np.ndarray,
                         cv_splitter: Optional[Any] = None) -> None:
        """进化一代"""
        # 评估当前种群
        self.evaluate_population(data_context, target_returns, cv_splitter=cv_splitter)

        # 保存最佳个体
        self.population.sort(key=lambda x: x.fitness, reverse=True)
        elite_size = int(self.population_size * self.elitism_ratio)
        elite = self.population[:elite_size]

        # 更新最佳候选
        if elite and (not self.best_candidates or elite[0].fitness > self.best_candidates[0].fitness):
            self.best_candidates.insert(0, deepcopy(elite[0]))
            self.best_candidates = self.best_candidates[:10]  # 保留前10个

        # 选择
        selected = self.selection()

        # 生成新种群
        new_population = []

        # 精英保留
        new_population.extend([deepcopy(ind) for ind in elite])

        # 交叉和变异
        while len(new_population) < self.population_size:
            parent1, parent2 = random.sample(selected, 2)
            child1, child2 = self.crossover(parent1, parent2)

            child1 = self.mutation(child1)
            child2 = self.mutation(child2)

            new_population.extend([child1, child2])

        # 裁剪到目标大小
        self.population = new_population[:self.population_size]
        self.generation += 1

        # 增加年龄
        for candidate in self.population:
            candidate.age += 1


class AlphaSearchEngine:
    """Alpha因子搜索引擎"""

    def __init__(self, feature_config: FeatureConfig):
        self.feature_config = feature_config
        self.operator_library = OperatorLibrary()
        self.gp = GeneticProgramming(self.operator_library)
        self.feature_pipeline = FeatureEngineeringPipeline(feature_config)

        self.discovered_alphas: List[AlphaCandidate] = []
        self.search_history: List[Dict[str, Any]] = []

    def prepare_data_context(self, snapshots: List[LOBSnapshot],
                           trades: List[Trade]) -> Dict[str, np.ndarray]:
        """准备数据上下文"""
        # 提取特征
        features = self.feature_pipeline.extract_features(snapshots, trades)

        # 构建数据上下文
        context = {}

        # 标量特征
        if 'scalar_features' in features:
            for name, value in features['scalar_features'].items():
                if isinstance(value, (int, float)):
                    context[name] = np.full(len(snapshots), value)

        # LOB特征
        for i, snap in enumerate(snapshots):
            if snap.best_bid and snap.best_ask:
                # 前3档价格和数量
                for level in range(1, 4):
                    if len(snap.bids) >= level:
                        context.setdefault(f'bid_price_{level}', np.zeros(len(snapshots)))[i] = float(snap.bids[level-1].price)
                        context.setdefault(f'bid_size_{level}', np.zeros(len(snapshots)))[i] = snap.bids[level-1].size

                    if len(snap.asks) >= level:
                        context.setdefault(f'ask_price_{level}', np.zeros(len(snapshots)))[i] = float(snap.asks[level-1].price)
                        context.setdefault(f'ask_size_{level}', np.zeros(len(snapshots)))[i] = snap.asks[level-1].size

                # 基础特征
                if snap.spread:
                    context.setdefault('spread', np.zeros(len(snapshots)))[i] = float(snap.spread)
                if snap.mid_price:
                    context.setdefault('mid_price', np.zeros(len(snapshots)))[i] = float(snap.mid_price)
                if snap.microprice:
                    context.setdefault('microprice', np.zeros(len(snapshots)))[i] = float(snap.microprice)

        # 交易特征
        trade_intensity = np.zeros(len(snapshots))
        trade_size_avg = np.zeros(len(snapshots))

        for i, snap in enumerate(snapshots):
            # 计算当前时间窗口内的交易强度
            current_trades = [t for t in trades if abs(t.timestamp - snap.timestamp) < 1e9]  # 1秒窗口
            trade_intensity[i] = len(current_trades)
            if current_trades:
                trade_size_avg[i] = np.mean([t.size for t in current_trades])

        context['trade_intensity'] = trade_intensity
        context['trade_size'] = trade_size_avg

        # 订单流失衡
        ofi = np.zeros(len(snapshots))
        for i in range(1, len(snapshots)):
            if (snapshots[i-1].best_bid and snapshots[i-1].best_ask and
                snapshots[i].best_bid and snapshots[i].best_ask):

                # 简化的OFI计算
                bid_delta = snapshots[i].best_bid.size - snapshots[i-1].best_bid.size
                ask_delta = snapshots[i].best_ask.size - snapshots[i-1].best_ask.size
                ofi[i] = bid_delta - ask_delta

        context['ofi'] = ofi

        # 失衡比率
        volume_imbalance = np.zeros(len(snapshots))
        for i, snap in enumerate(snapshots):
            if snap.best_bid and snap.best_ask:
                bid_vol = snap.best_bid.size
                ask_vol = snap.best_ask.size
                volume_imbalance[i] = (bid_vol - ask_vol) / (bid_vol + ask_vol + 1e-8)

        context['volume_imbalance'] = volume_imbalance

        return context

    def search_alphas(self, snapshots: List[LOBSnapshot], trades: List[Trade],
                     target_returns: np.ndarray, max_generations: int = 50,
                     convergence_threshold: float = 0.001,
                     cv_splitter: Optional[Any] = None,
                     min_cv_samples: int = 200) -> List[AlphaCandidate]:
        """搜索Alpha因子"""

        # 准备数据
        data_context = self.prepare_data_context(snapshots, trades)

        # 初始化种群
        self.gp.initialize_population()

        print(f"开始搜索Alpha因子，种群大小: {self.gp.population_size}, 最大代数: {max_generations}")

        best_fitness_history = []
        stagnation_counter = 0

        if cv_splitter is None and len(target_returns) >= min_cv_samples:
            try:
                from .labeling import PurgedKFold
                cv_splitter = PurgedKFold(n_splits=min(5, max(2, len(target_returns) // 200)))
            except Exception:
                warnings.warn("PurgedKFold 不可用，回退到简单评估。")
                cv_splitter = None
        elif cv_splitter is not None and len(target_returns) < min_cv_samples:
            warnings.warn("样本量不足，禁用交叉验证以避免空折。")
            cv_splitter = None

        for generation in range(max_generations):
            # 进化一代
            self.gp.evolve_generation(data_context, target_returns, cv_splitter=cv_splitter)

            # 记录历史
            current_best = self.gp.population[0] if self.gp.population else None
            if current_best:
                best_fitness_history.append(current_best.fitness)

                print(f"第{generation+1}代: 最佳适应度={current_best.fitness:.4f}, "
                      f"IC={current_best.ic:.4f}, IR={current_best.ir:.4f}, "
                      f"复杂度={current_best.complexity}")

                # 检查收敛
                if len(best_fitness_history) >= 10:
                    recent_improvement = (best_fitness_history[-1] - best_fitness_history[-10])
                    if recent_improvement < convergence_threshold:
                        stagnation_counter += 1
                    else:
                        stagnation_counter = 0

                if stagnation_counter >= 5:
                    print(f"收敛检测：第{generation+1}代停止搜索")
                    break

        # 收集有效的Alpha因子
        valid_alphas = [candidate for candidate in self.gp.best_candidates
                       if candidate.is_valid()]

        # 去重（基于表达式结构）
        unique_alphas = self._deduplicate_alphas(valid_alphas)

        # 排序
        unique_alphas.sort(key=lambda x: x.fitness, reverse=True)

        self.discovered_alphas.extend(unique_alphas[:10])  # 保留前10个

        # 记录搜索历史
        self.search_history.append({
            'timestamp': time.time(),
            'generations': generation + 1,
            'best_fitness': best_fitness_history[-1] if best_fitness_history else 0,
            'alphas_found': len(unique_alphas),
            'convergence': stagnation_counter >= 5
        })

        return unique_alphas[:5]  # 返回前5个

    def _deduplicate_alphas(self, alphas: List[AlphaCandidate]) -> List[AlphaCandidate]:
        """去除重复的Alpha因子"""
        seen_hashes = set()
        unique_alphas = []

        for alpha in alphas:
            hash_val = alpha.expression.get_hash()
            if hash_val not in seen_hashes:
                seen_hashes.add(hash_val)
                unique_alphas.append(alpha)

        return unique_alphas

    def generate_alpha_report(self) -> pd.DataFrame:
        """生成Alpha因子报告"""
        if not self.discovered_alphas:
            return pd.DataFrame()

        report_data = []
        for i, alpha in enumerate(self.discovered_alphas[:20]):  # 前20个
            report_data.append({
                'rank': i + 1,
                'expression': alpha.expression.to_string(),
                'ic': alpha.ic,
                'ir': alpha.ir,
                'fitness': alpha.fitness,
                'complexity': alpha.complexity,
                'generation': alpha.generation,
                'terminals': ', '.join(alpha.expression.get_terminals())
            })

        return pd.DataFrame(report_data)

    def export_best_alphas(self, top_k: int = 5) -> List[str]:
        """导出最佳Alpha因子的字符串表示"""
        if not self.discovered_alphas:
            return []

        best_alphas = sorted(self.discovered_alphas, key=lambda x: x.fitness, reverse=True)
        return [alpha.expression.to_string() for alpha in best_alphas[:top_k]]

    def validate_alpha(self, expression_str: str, data_context: Dict[str, np.ndarray],
                      target_returns: np.ndarray) -> Dict[str, float]:
        """验证Alpha因子表达式"""
        try:
            # 这里需要从字符串解析表达式（简化版本）
            # 实际实现需要完整的表达式解析器
            return {
                'ic': 0.0,
                'ir': 0.0,
                'fitness': 0.0,
                'valid': False
            }
        except Exception as e:
            return {
                'error': str(e),
                'valid': False
            }


# 工具函数
def compute_rolling_ic(alpha_values: np.ndarray, returns: np.ndarray,
                      window: int = 50) -> np.ndarray:
    """计算滚动IC"""
    rolling_ic = np.zeros(len(alpha_values))

    for i in range(window, len(alpha_values)):
        alpha_window = alpha_values[i-window:i]
        returns_window = returns[i-window:i]

        if len(alpha_window) == len(returns_window) and len(alpha_window) > 1:
            ic = np.corrcoef(alpha_window, returns_window)[0, 1]
            rolling_ic[i] = ic if not np.isnan(ic) else 0

    return rolling_ic


def evaluate_alpha_stability(alpha_values: np.ndarray, returns: np.ndarray,
                           n_splits: int = 5) -> Dict[str, float]:
    """评估Alpha因子稳定性"""
    split_size = len(alpha_values) // n_splits
    ics = []

    for i in range(n_splits):
        start_idx = i * split_size
        end_idx = (i + 1) * split_size if i < n_splits - 1 else len(alpha_values)

        alpha_split = alpha_values[start_idx:end_idx]
        returns_split = returns[start_idx:end_idx]

        if len(alpha_split) > 1:
            ic = np.corrcoef(alpha_split, returns_split)[0, 1]
            if not np.isnan(ic):
                ics.append(ic)

    if ics:
        return {
            'mean_ic': np.mean(ics),
            'std_ic': np.std(ics),
            'stability_ratio': np.mean(ics) / (np.std(ics) + 1e-8),
            'consistent_splits': sum(1 for ic in ics if ic > 0.01)
        }

    return {'mean_ic': 0, 'std_ic': 0, 'stability_ratio': 0, 'consistent_splits': 0}
