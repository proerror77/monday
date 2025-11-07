#!/usr/bin/env python3
"""
LOB-native训练管道
整合数据摄取、特征工程、标签生成、Alpha搜索和模型训练的完整流程
"""

import numpy as np
import pandas as pd
from typing import List, Dict, Tuple, Optional, Union, Any
from dataclasses import dataclass
from pathlib import Path
import logging
import time
import json
from datetime import datetime
import warnings

from .data_structures import LOBSnapshot, LOBMessage, Trade, LOBDataContainer
from .data_ingestion import LOBSTERLoader, InformationBarBuilder
from .features import FeatureEngineeringPipeline, FeatureConfig
from .labeling import (
    MicropriceDriftLabeler, TripleBarrierConfig, Label,
    PurgedKFold, LabelingPipeline, MetaLabelingSystem
)
from .alpha_search import AlphaSearchEngine, AlphaCandidate
from .models import (
    ModelFactory, ModelConfig, ModelType, BaseLOBModel,
    DeepLOBModel, LightGBMLOBModel
)

# 设置日志
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


@dataclass
class PipelineConfig:
    """训练管道配置"""
    # 数据配置
    data_source: str = "LOBSTER"  # LOBSTER, ITCH, etc.
    symbol: str = "AAPL"
    start_date: str = "2024-01-01"
    end_date: str = "2024-01-31"

    # 特征工程配置
    feature_config: FeatureConfig = None

    # 标签配置
    labeling_config: TripleBarrierConfig = None

    # Alpha搜索配置
    enable_alpha_search: bool = True
    alpha_search_generations: int = 30
    alpha_search_population: int = 50

    # 模型配置
    model_configs: List[ModelConfig] = None

    # 交叉验证配置
    cv_folds: int = 5
    purge_period: int = 50
    embargo_period: int = 100

    # 输出配置
    output_dir: str = "./results"
    save_intermediate: bool = True

    def __post_init__(self):
        if self.feature_config is None:
            self.feature_config = FeatureConfig()

        if self.labeling_config is None:
            self.labeling_config = TripleBarrierConfig()

        if self.model_configs is None:
            self.model_configs = [
                # DeepLOB模型
                ModelConfig(
                    model_type=ModelType.DEEPLOB,
                    input_dim=40,  # 将根据实际特征数调整
                    hidden_dims=[128, 64, 32],
                    model_params={'epochs': 50, 'patience': 10}
                ),
                # TCN模型
                ModelConfig(
                    model_type=ModelType.TCN,
                    input_dim=40,
                    sequence_length=100,
                    hidden_dims=[32, 64, 128, 64, 32],
                    model_params={
                        'epochs': 50,
                        'patience': 15,
                        'kernel_size': 3
                    }
                ),
                # 混合TCN-DeepLOB模型
                ModelConfig(
                    model_type=ModelType.HYBRID_TCN_DEEPLOB,
                    input_dim=40,
                    sequence_length=50,
                    hidden_dims=[32, 64, 32],
                    model_params={
                        'epochs': 60,
                        'patience': 15,
                        'use_hybrid': True
                    }
                ),
                # LightGBM模型
                ModelConfig(
                    model_type=ModelType.LIGHTGBM,
                    input_dim=40,
                    model_params={'num_boost_round': 100}
                )
            ]


@dataclass
class PipelineResults:
    """管道执行结果"""
    # 数据统计
    data_stats: Dict[str, Any]

    # 特征统计
    feature_stats: Dict[str, Any]

    # 标签统计
    label_stats: Dict[str, Any]

    # Alpha因子
    discovered_alphas: List[AlphaCandidate]

    # 模型结果
    model_results: Dict[str, Dict[str, Any]]

    # 执行时间
    execution_time: float

    # 最佳模型
    best_model_name: str
    best_model_score: float


class LOBTrainingPipeline:
    """LOB训练管道"""

    def __init__(self, config: PipelineConfig):
        self.config = config
        self.output_dir = Path(config.output_dir)
        self.output_dir.mkdir(parents=True, exist_ok=True)

        # 初始化组件
        self.feature_pipeline = FeatureEngineeringPipeline(config.feature_config)
        self.labeling_pipeline = LabelingPipeline(config.labeling_config)

        if config.enable_alpha_search:
            self.alpha_engine = AlphaSearchEngine(config.feature_config)
        else:
            self.alpha_engine = None

        # 数据容器
        self.data_container: Optional[LOBDataContainer] = None
        self.snapshots: List[LOBSnapshot] = []
        self.trades: List[Trade] = []

        # 结果
        self.results: Optional[PipelineResults] = None

    def run_full_pipeline(self) -> PipelineResults:
        """执行完整训练管道"""
        start_time = time.time()

        logger.info("开始执行LOB训练管道")

        try:
            # 步骤1: 数据加载
            logger.info("步骤1: 加载数据")
            self._load_data()

            # 步骤2: 特征工程
            logger.info("步骤2: 特征工程")
            features = self._extract_features()

            # 步骤3: 标签生成
            logger.info("步骤3: 生成标签")
            labels = self._generate_labels()

            # 步骤4: Alpha因子搜索
            alpha_candidates = []
            if self.config.enable_alpha_search:
                logger.info("步骤4: Alpha因子搜索")
                alpha_candidates = self._search_alpha_factors(labels)

            # 步骤5: 准备训练数据
            logger.info("步骤5: 准备训练数据")
            X, y = self._prepare_training_data(features, labels, alpha_candidates)

            # 步骤6: 模型训练和评估
            logger.info("步骤6: 模型训练和评估")
            model_results = self._train_and_evaluate_models(X, y)

            # 步骤7: 生成结果
            execution_time = time.time() - start_time
            self.results = self._compile_results(
                features, labels, alpha_candidates,
                model_results, execution_time
            )

            # 步骤8: 保存结果
            if self.config.save_intermediate:
                self._save_results()

            logger.info(f"管道执行完成，总耗时: {execution_time:.2f}秒")
            return self.results

        except Exception as e:
            logger.error(f"管道执行失败: {e}")
            raise

    def _load_data(self) -> None:
        """加载数据"""
        if self.config.data_source == "LOBSTER":
            # 使用LOBSTER数据加载器
            loader = LOBSTERLoader()

            # 模拟数据路径（实际应用中需要真实路径）
            message_file = f"data/{self.config.symbol}_{self.config.start_date}_message.csv"
            orderbook_file = f"data/{self.config.symbol}_{self.config.start_date}_orderbook.csv"

            try:
                # 加载数据（简化版本）
                self.data_container = LOBDataContainer(self.config.symbol)

                # 生成模拟数据用于测试
                self._generate_synthetic_data()

            except FileNotFoundError:
                logger.warning("LOBSTER数据文件未找到，生成合成数据用于测试")
                self._generate_synthetic_data()
        else:
            logger.warning(f"不支持的数据源: {self.config.data_source}，生成合成数据")
            self._generate_synthetic_data()

    def _generate_synthetic_data(self) -> None:
        """生成合成数据用于测试"""
        from decimal import Decimal
        import random

        n_snapshots = 1000
        n_trades = 200

        self.snapshots = []
        self.trades = []

        base_price = 100.0
        timestamp = int(time.time() * 1e9)

        # 生成LOB快照
        for i in range(n_snapshots):
            # 模拟价格随机游走
            price_change = random.normalvariate(0, 0.1)
            base_price += price_change

            # 生成bid/ask档位
            bids = []
            asks = []

            for level in range(5):
                bid_price = Decimal(str(round(base_price - 0.01 * (level + 1), 2)))
                ask_price = Decimal(str(round(base_price + 0.01 * (level + 1), 2)))

                bid_size = random.randint(100, 1000)
                ask_size = random.randint(100, 1000)

                from .data_structures import PriceLevel
                bids.append(PriceLevel(bid_price, bid_size))
                asks.append(PriceLevel(ask_price, ask_size))

            snapshot = LOBSnapshot(
                timestamp=timestamp + i * 1000000,  # 1ms间隔
                symbol=self.config.symbol,
                bids=bids,
                asks=asks
            )

            self.snapshots.append(snapshot)

        # 生成交易数据
        for i in range(n_trades):
            trade_time = timestamp + random.randint(0, n_snapshots-1) * 1000000
            trade_price = Decimal(str(round(base_price + random.normalvariate(0, 0.05), 2)))
            trade_size = random.randint(10, 500)
            trade_side = random.choice([Side.BID, Side.ASK])

            trade = Trade(
                timestamp=trade_time,
                symbol=self.config.symbol,
                price=trade_price,
                size=trade_size,
                side=trade_side,
                trade_id=f"T{i}"
            )

            self.trades.append(trade)

        # 按时间排序
        self.snapshots.sort(key=lambda x: x.timestamp)
        self.trades.sort(key=lambda x: x.timestamp)

        logger.info(f"生成合成数据: {len(self.snapshots)}个快照, {len(self.trades)}个交易")

    def _extract_features(self) -> Dict[str, Any]:
        """提取特征"""
        features = self.feature_pipeline.extract_features(self.snapshots, self.trades)

        # 标准化特征
        if self.config.feature_config.enable_normalization:
            features = self.feature_pipeline.normalize_features(features)

        logger.info(f"提取特征完成: 标量特征{len(features.get('scalar_features', {}))}, "
                   f"LOB张量形状{features.get('lob_tensor', np.array([])).shape}")

        return features

    def _generate_labels(self) -> List[Label]:
        """生成标签"""
        label_result = self.labeling_pipeline.generate_labels(self.snapshots, self.trades)
        primary_labels = label_result['primary']

        logger.info(f"生成标签完成: {len(primary_labels)}个主要标签")

        # 标签质量验证
        from .labeling import validate_label_quality
        quality_stats = validate_label_quality(primary_labels)
        logger.info(f"标签质量统计: {quality_stats}")

        return primary_labels

    def _search_alpha_factors(self, labels: List[Label]) -> List[AlphaCandidate]:
        """搜索Alpha因子"""
        if not self.alpha_engine:
            return []

        # 计算目标收益
        target_returns = np.array([label.return_realized for label in labels])

        # 执行搜索
        alpha_candidates = self.alpha_engine.search_alphas(
            self.snapshots[:len(labels)],  # 确保长度匹配
            self.trades,
            target_returns,
            max_generations=self.config.alpha_search_generations
        )

        logger.info(f"Alpha搜索完成: 发现{len(alpha_candidates)}个有效因子")

        # 生成报告
        alpha_report = self.alpha_engine.generate_alpha_report()
        if self.config.save_intermediate:
            alpha_report.to_csv(self.output_dir / "alpha_factors.csv", index=False)

        return alpha_candidates

    def _prepare_training_data(self, features: Dict[str, Any],
                             labels: List[Label],
                             alpha_candidates: List[AlphaCandidate]) -> Tuple[np.ndarray, np.ndarray]:
        """准备训练数据"""

        # 使用特征矩阵
        if 'feature_matrix' in features and features['feature_matrix'].size > 0:
            X_base = features['feature_matrix']
        else:
            # 使用标量特征
            scalar_features = features.get('scalar_features', {})
            if scalar_features:
                feature_values = list(scalar_features.values())
                X_base = np.tile(feature_values, (len(labels), 1))
            else:
                X_base = np.random.randn(len(labels), 20)  # 回退方案

        # 确保维度匹配
        min_length = min(len(X_base), len(labels))
        X_base = X_base[:min_length]
        labels = labels[:min_length]

        # 添加Alpha因子作为额外特征
        alpha_features = []
        if alpha_candidates:
            data_context = self.alpha_engine.prepare_data_context(
                self.snapshots[:min_length], self.trades
            )

            for i, alpha in enumerate(alpha_candidates[:5]):  # 最多使用前5个Alpha因子
                try:
                    alpha_values = alpha.expression.evaluate(data_context)
                    if len(alpha_values) >= min_length:
                        alpha_features.append(alpha_values[:min_length])
                except Exception as e:
                    logger.warning(f"Alpha因子{i}评估失败: {e}")

        # 合并特征
        if alpha_features:
            alpha_matrix = np.column_stack(alpha_features)
            X = np.hstack([X_base, alpha_matrix])
            logger.info(f"添加{len(alpha_features)}个Alpha因子特征")
        else:
            X = X_base

        # 准备标签
        y = np.array([label.label for label in labels])

        # 更新模型配置的输入维度
        for model_config in self.config.model_configs:
            model_config.input_dim = X.shape[1]

        logger.info(f"训练数据准备完成: X.shape={X.shape}, y.shape={y.shape}")
        logger.info(f"标签分布: {np.bincount(y + 1)}")  # +1 to handle -1,0,1 labels

        return X, y

    def _train_and_evaluate_models(self, X: np.ndarray, y: np.ndarray) -> Dict[str, Dict[str, Any]]:
        """训练和评估模型"""

        # 初始化交叉验证
        cv = PurgedKFold(
            n_splits=self.config.cv_folds,
            embargo_period=self.config.embargo_period,
            purge_period=self.config.purge_period
        )

        model_results = {}

        for model_config in self.config.model_configs:
            model_name = f"{model_config.model_type.value}"
            logger.info(f"训练模型: {model_name}")

            # 创建模型
            model = ModelFactory.create_model(model_config)

            # 交叉验证
            cv_scores = []
            cv_fold_results = []

            for fold, (train_idx, val_idx) in enumerate(cv.split(X, y)):
                X_train, X_val = X[train_idx], X[val_idx]
                y_train, y_val = y[train_idx], y[val_idx]

                # 训练模型
                train_result = model.fit(X_train, y_train, X_val, y_val)

                # 评估
                val_metrics = model.evaluate(X_val, y_val)
                cv_scores.append(val_metrics['accuracy'])

                cv_fold_results.append({
                    'fold': fold,
                    'train_size': len(X_train),
                    'val_size': len(X_val),
                    'accuracy': val_metrics['accuracy'],
                    'train_result': train_result
                })

                logger.info(f"  Fold {fold+1}: Accuracy = {val_metrics['accuracy']:.4f}")

            # 汇总结果
            mean_score = np.mean(cv_scores)
            std_score = np.std(cv_scores)

            model_results[model_name] = {
                'mean_accuracy': mean_score,
                'std_accuracy': std_score,
                'cv_scores': cv_scores,
                'cv_fold_results': cv_fold_results,
                'model_config': model_config,
                'feature_importance': getattr(model, 'feature_importance_', None)
            }

            logger.info(f"  模型{model_name}: CV准确率 = {mean_score:.4f} ± {std_score:.4f}")

            # 保存模型
            if self.config.save_intermediate:
                model_path = self.output_dir / f"{model_name}_model"
                try:
                    model.save_model(str(model_path))
                except Exception as e:
                    logger.warning(f"保存模型{model_name}失败: {e}")

        return model_results

    def _compile_results(self, features: Dict[str, Any], labels: List[Label],
                        alpha_candidates: List[AlphaCandidate],
                        model_results: Dict[str, Dict[str, Any]],
                        execution_time: float) -> PipelineResults:
        """编译结果"""

        # 数据统计
        data_stats = {
            'n_snapshots': len(self.snapshots),
            'n_trades': len(self.trades),
            'symbol': self.config.symbol,
            'time_span_seconds': (self.snapshots[-1].timestamp - self.snapshots[0].timestamp) / 1e9 if self.snapshots else 0
        }

        # 特征统计
        feature_stats = {
            'n_scalar_features': len(features.get('scalar_features', {})),
            'lob_tensor_shape': str(features.get('lob_tensor', np.array([])).shape),
            'feature_matrix_shape': str(features.get('feature_matrix', np.array([])).shape)
        }

        # 标签统计
        from .labeling import validate_label_quality
        label_stats = validate_label_quality(labels)

        # 找到最佳模型
        best_model_name = ""
        best_model_score = 0.0

        for model_name, results in model_results.items():
            if results['mean_accuracy'] > best_model_score:
                best_model_score = results['mean_accuracy']
                best_model_name = model_name

        return PipelineResults(
            data_stats=data_stats,
            feature_stats=feature_stats,
            label_stats=label_stats,
            discovered_alphas=alpha_candidates,
            model_results=model_results,
            execution_time=execution_time,
            best_model_name=best_model_name,
            best_model_score=best_model_score
        )

    def _save_results(self) -> None:
        """保存结果"""
        if not self.results:
            return

        # 保存主要结果为JSON
        results_summary = {
            'pipeline_config': {
                'symbol': self.config.symbol,
                'start_date': self.config.start_date,
                'end_date': self.config.end_date,
                'alpha_search_enabled': self.config.enable_alpha_search
            },
            'data_stats': self.results.data_stats,
            'feature_stats': self.results.feature_stats,
            'label_stats': self.results.label_stats,
            'model_performance': {
                name: {
                    'mean_accuracy': results['mean_accuracy'],
                    'std_accuracy': results['std_accuracy']
                }
                for name, results in self.results.model_results.items()
            },
            'best_model': {
                'name': self.results.best_model_name,
                'score': self.results.best_model_score
            },
            'execution_time': self.results.execution_time,
            'timestamp': datetime.now().isoformat()
        }

        # 保存JSON结果
        with open(self.output_dir / "pipeline_results.json", 'w') as f:
            json.dump(results_summary, f, indent=2, default=str)

        # 保存Alpha因子
        if self.results.discovered_alphas:
            alpha_data = []
            for alpha in self.results.discovered_alphas:
                alpha_data.append({
                    'expression': alpha.expression.to_string(),
                    'ic': alpha.ic,
                    'ir': alpha.ir,
                    'fitness': alpha.fitness,
                    'complexity': alpha.complexity
                })

            alpha_df = pd.DataFrame(alpha_data)
            alpha_df.to_csv(self.output_dir / "discovered_alphas.csv", index=False)

        logger.info(f"结果已保存到: {self.output_dir}")

    def get_summary_report(self) -> str:
        """获取总结报告"""
        if not self.results:
            return "管道尚未执行"

        report = f"""
LOB-Native训练管道执行报告
==========================

数据统计:
- 符号: {self.results.data_stats['symbol']}
- 快照数量: {self.results.data_stats['n_snapshots']}
- 交易数量: {self.results.data_stats['n_trades']}
- 时间跨度: {self.results.data_stats['time_span_seconds']:.2f}秒

特征工程:
- 标量特征数: {self.results.feature_stats['n_scalar_features']}
- LOB张量形状: {self.results.feature_stats['lob_tensor_shape']}
- 特征矩阵形状: {self.results.feature_stats['feature_matrix_shape']}

标签生成:
- 平均收益: {self.results.label_stats.get('mean_return', 0):.4f}
- 收益波动率: {self.results.label_stats.get('return_volatility', 0):.4f}
- 夏普比率: {self.results.label_stats.get('sharpe_ratio', 0):.4f}

Alpha因子搜索:
- 发现因子数量: {len(self.results.discovered_alphas)}

模型性能:
"""

        for model_name, results in self.results.model_results.items():
            report += f"- {model_name}: {results['mean_accuracy']:.4f} ± {results['std_accuracy']:.4f}\n"

        report += f"""
最佳模型: {self.results.best_model_name} (准确率: {self.results.best_model_score:.4f})
执行时间: {self.results.execution_time:.2f}秒
"""

        return report


# 便捷函数
def run_quick_pipeline(symbol: str = "AAPL",
                      enable_alpha_search: bool = True) -> PipelineResults:
    """快速运行管道的便捷函数"""
    config = PipelineConfig(
        symbol=symbol,
        enable_alpha_search=enable_alpha_search,
        alpha_search_generations=20,  # 较快的设置
        cv_folds=3
    )

    pipeline = LOBTrainingPipeline(config)
    return pipeline.run_full_pipeline()


if __name__ == "__main__":
    # 示例使用
    config = PipelineConfig(
        symbol="AAPL",
        enable_alpha_search=True,
        alpha_search_generations=30,
        output_dir="./lob_results"
    )

    pipeline = LOBTrainingPipeline(config)
    results = pipeline.run_full_pipeline()

    print(pipeline.get_summary_report())