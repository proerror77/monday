#!/usr/bin/env python3
"""
Alpha因子自动发现优化器
真正的"自动找因子调参"实现
"""

import numpy as np
import pandas as pd
from typing import Dict, List, Tuple, Any, Optional
from itertools import combinations
import warnings
warnings.filterwarnings('ignore')

try:
    import optuna
    OPTUNA_AVAILABLE = True
except ImportError:
    OPTUNA_AVAILABLE = False
    print("⚠️ Optuna未安装，将使用随机搜索")

class AlphaFactorOptimizer:
    """
    Alpha因子自动发现和优化器

    真正实现"自动找因子调参"：
    1. 自动特征选择
    2. 自动特征组合
    3. 自动技术指标参数优化
    4. 自动Alpha因子构造
    """

    def __init__(self,
                 base_features: pd.DataFrame,
                 target: pd.Series,
                 max_features: int = 20,
                 n_trials: int = 50):
        self.base_features = base_features
        self.target = target
        self.max_features = max_features
        self.n_trials = n_trials
        self.feature_names = list(base_features.columns)

        print(f"🔍 Alpha因子优化器初始化:")
        print(f"   基础特征数: {len(self.feature_names)}")
        print(f"   最大特征数: {max_features}")
        print(f"   优化试验数: {n_trials}")

    def create_ratio_features(self, features: pd.DataFrame,
                            feature_pairs: List[Tuple[str, str]]) -> pd.DataFrame:
        """创建比率特征"""
        ratio_features = pd.DataFrame(index=features.index)

        for feat1, feat2 in feature_pairs:
            if feat1 in features.columns and feat2 in features.columns:
                # 避免除零
                feat2_safe = features[feat2].replace(0, np.nan)
                ratio_name = f"{feat1}_div_{feat2}"
                ratio_features[ratio_name] = features[feat1] / feat2_safe

        return ratio_features

    def create_diff_features(self, features: pd.DataFrame,
                           feature_pairs: List[Tuple[str, str]]) -> pd.DataFrame:
        """创建差值特征"""
        diff_features = pd.DataFrame(index=features.index)

        for feat1, feat2 in feature_pairs:
            if feat1 in features.columns and feat2 in features.columns:
                diff_name = f"{feat1}_minus_{feat2}"
                diff_features[diff_name] = features[feat1] - features[feat2]

        return diff_features

    def create_rolling_features(self, features: pd.DataFrame,
                              selected_features: List[str],
                              windows: List[int]) -> pd.DataFrame:
        """创建滚动窗口特征"""
        rolling_features = pd.DataFrame(index=features.index)

        for feature in selected_features:
            if feature in features.columns:
                for window in windows:
                    # 滚动均值
                    rolling_features[f"{feature}_ma_{window}"] = (
                        features[feature].rolling(window=window, min_periods=1).mean()
                    )
                    # 滚动标准差
                    rolling_features[f"{feature}_std_{window}"] = (
                        features[feature].rolling(window=window, min_periods=1).std()
                    )
                    # 滚动最大值比率
                    rolling_max = features[feature].rolling(window=window, min_periods=1).max()
                    rolling_features[f"{feature}_max_ratio_{window}"] = (
                        features[feature] / rolling_max.replace(0, np.nan)
                    )

        return rolling_features

    def calculate_ic(self, features: pd.DataFrame, target: pd.Series) -> float:
        """计算信息系数(IC)"""
        if features.empty or len(features.columns) == 0:
            return 0.0

        # 组合所有特征为一个信号
        # 简单等权重组合
        valid_features = features.select_dtypes(include=[np.number]).dropna(axis=1)
        if valid_features.empty:
            return 0.0

        # 标准化特征
        feature_signal = valid_features.fillna(0).mean(axis=1)

        # 对齐索引
        aligned_signal, aligned_target = feature_signal.align(target, join='inner')

        if len(aligned_signal) < 10:  # 最少需要10个数据点
            return 0.0

        # 计算相关系数
        try:
            ic = np.corrcoef(aligned_signal, aligned_target)[0, 1]
            return 0.0 if np.isnan(ic) else float(ic)
        except:
            return 0.0

    def optimize_factor_combination(self) -> Dict[str, Any]:
        """优化Alpha因子组合"""

        def objective(trial):
            """Optuna目标函数"""

            # 1. 特征选择
            n_base_features = trial.suggest_int('n_base_features', 5, min(15, len(self.feature_names)))
            selected_base = trial.suggest_categorical('selected_features',
                                                    [tuple(np.random.choice(self.feature_names,
                                                                          n_base_features,
                                                                          replace=False))])
            selected_base = list(selected_base)

            # 2. 是否创建比率特征
            use_ratios = trial.suggest_categorical('use_ratios', [True, False])
            ratio_pairs = []
            if use_ratios:
                n_ratio_pairs = trial.suggest_int('n_ratio_pairs', 1,
                                                min(5, len(selected_base)//2))
                # 随机选择特征对
                if len(selected_base) >= 2:
                    ratio_pairs = [trial.suggest_categorical(f'ratio_pair_{i}',
                                                           list(combinations(selected_base, 2)))
                                 for i in range(n_ratio_pairs)]

            # 3. 是否创建差值特征
            use_diffs = trial.suggest_categorical('use_diffs', [True, False])
            diff_pairs = []
            if use_diffs:
                n_diff_pairs = trial.suggest_int('n_diff_pairs', 1,
                                               min(3, len(selected_base)//2))
                if len(selected_base) >= 2:
                    diff_pairs = [trial.suggest_categorical(f'diff_pair_{i}',
                                                          list(combinations(selected_base, 2)))
                                for i in range(n_diff_pairs)]

            # 4. 滚动窗口参数
            use_rolling = trial.suggest_categorical('use_rolling', [True, False])
            rolling_windows = []
            if use_rolling:
                n_windows = trial.suggest_int('n_windows', 1, 3)
                rolling_windows = [trial.suggest_int(f'window_{i}', 3, 20)
                                 for i in range(n_windows)]
                rolling_features_count = trial.suggest_int('rolling_features', 1,
                                                         min(3, len(selected_base)))
                rolling_selected = selected_base[:rolling_features_count]
            else:
                rolling_selected = []

            # 构造特征
            try:
                # 基础特征
                final_features = self.base_features[selected_base].copy()

                # 比率特征
                if ratio_pairs:
                    ratio_features = self.create_ratio_features(self.base_features, ratio_pairs)
                    final_features = pd.concat([final_features, ratio_features], axis=1)

                # 差值特征
                if diff_pairs:
                    diff_features = self.create_diff_features(self.base_features, diff_pairs)
                    final_features = pd.concat([final_features, diff_features], axis=1)

                # 滚动特征
                if rolling_windows and rolling_selected:
                    rolling_features = self.create_rolling_features(self.base_features,
                                                                  rolling_selected,
                                                                  rolling_windows)
                    final_features = pd.concat([final_features, rolling_features], axis=1)

                # 限制最大特征数
                if len(final_features.columns) > self.max_features:
                    final_features = final_features.iloc[:, :self.max_features]

                # 计算IC
                ic = self.calculate_ic(final_features, self.target)

                return ic

            except Exception as e:
                print(f"⚠️ 试验失败: {e}")
                return 0.0

        # 执行优化
        if OPTUNA_AVAILABLE:
            study = optuna.create_study(direction='maximize',
                                      sampler=optuna.samplers.TPESampler(seed=42))
            study.optimize(objective, n_trials=self.n_trials, show_progress_bar=True)

            best_params = study.best_params
            best_score = study.best_value

            print(f"🏆 Optuna优化完成:")
            print(f"   最佳IC: {best_score:.6f}")
            print(f"   最佳参数: {best_params}")

        else:
            # 随机搜索后备方案
            best_score = 0.0
            best_params = {}

            for trial in range(self.n_trials):
                # 随机参数
                trial_params = {
                    'n_base_features': np.random.randint(5, min(15, len(self.feature_names))),
                    'use_ratios': np.random.choice([True, False]),
                    'use_diffs': np.random.choice([True, False]),
                    'use_rolling': np.random.choice([True, False]),
                }

                # 模拟trial对象进行随机搜索
                class MockTrial:
                    def __init__(self, params):
                        self.params = params

                    def suggest_int(self, name, low, high):
                        if name in self.params:
                            return self.params[name]
                        return np.random.randint(low, high + 1)

                    def suggest_categorical(self, name, choices):
                        if name.startswith('selected_features'):
                            return tuple(np.random.choice(self.feature_names,
                                                        self.params['n_base_features'],
                                                        replace=False))
                        return np.random.choice(choices)

                mock_trial = MockTrial(trial_params)
                score = objective(mock_trial)

                if score > best_score:
                    best_score = score
                    best_params = trial_params

            print(f"🎲 随机搜索完成:")
            print(f"   最佳IC: {best_score:.6f}")

        return {
            'best_ic': best_score,
            'best_params': best_params,
            'optimization_method': 'optuna' if OPTUNA_AVAILABLE else 'random'
        }

# 使用示例
def demo_alpha_factor_optimizer():
    """演示Alpha因子优化器的使用"""
    print("🚀 Alpha因子优化器演示")

    # 生成模拟数据
    np.random.seed(42)
    n_samples = 1000
    n_features = 10

    # 模拟特征数据
    feature_data = pd.DataFrame(
        np.random.randn(n_samples, n_features),
        columns=[f'feature_{i}' for i in range(n_features)]
    )

    # 模拟目标变量（价格变化）
    target_data = pd.Series(
        feature_data['feature_0'] * 0.3 +
        feature_data['feature_1'] * 0.2 +
        np.random.randn(n_samples) * 0.1,
        name='price_change'
    )

    # 创建优化器
    optimizer = AlphaFactorOptimizer(
        base_features=feature_data,
        target=target_data,
        max_features=15,
        n_trials=10  # 演示用少量试验
    )

    # 执行优化
    result = optimizer.optimize_factor_combination()

    print(f"\n📊 优化结果:")
    print(f"   最佳IC: {result['best_ic']:.6f}")
    print(f"   优化方法: {result['optimization_method']}")

    return result

if __name__ == "__main__":
    demo_alpha_factor_optimizer()