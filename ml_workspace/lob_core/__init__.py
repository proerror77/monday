#!/usr/bin/env python3
"""
LOB Core - 限价订单簿核心模块

基于事件级数据的HFT微观结构分析和Alpha因子发现系统。
实现了从原始OHLCV数据到事件级LOB数据的完整架构迁移。

主要组件:
- data_structures: LOB事件消息、快照和微观结构数据类型
- data_ingestion: ITCH解析器、LOBSTER加载器和信息驱动采样
- features: HFT级别的微观结构特征工程
- labeling: 三重障碍标签法和Purged K-Fold验证
- alpha_search: 基于遗传编程的Alpha因子自动发现
- models: DeepLOB、Transformer等模型的统一接口
- training_pipeline: 端到端训练管道
"""

__version__ = "1.0.0"
__author__ = "HFT Research Team"

# 核心数据结构
from .data_structures import (
    MessageType, Side, LOBMessage, PriceLevel, LOBSnapshot,
    Trade, InformationBar, LOBDataContainer,
    timestamp_to_datetime, datetime_to_timestamp
)

# 数据摄取
from .data_ingestion import (
    ITCHParser, LOBSTERLoader, InformationBarBuilder,
    ClickHouseLogger, RealTimeDataProcessor
)

# 特征工程
from .features import (
    FeatureConfig, MicrostructureFeatures, OrderFlowFeatures,
    DeepLOBFeatures, FeatureEngineeringPipeline
)

# 标签系统
from .labeling import (
    TripleBarrierConfig, Label, MicropriceDriftLabeler,
    OrderFlowImbalanceLabeler, PurgedKFold, MetaLabelingSystem,
    LabelingPipeline
)

# Alpha搜索引擎
from .alpha_search import (
    OperatorType, Operator, ExpressionNode, AlphaCandidate,
    OperatorLibrary, GeneticProgramming, AlphaSearchEngine
)

# 模型接口
from .models import (
    ModelType, ModelConfig, BaseLOBModel, DeepLOBModel,
    TransformerLOBModel, LightGBMLOBModel, ModelFactory
)

# TCN模型
from .tcn_models import (
    TCNLOBModel, TemporalConvNet, CausalConv1d,
    HybridTCNDeepLOB, benchmark_tcn_vs_lstm
)

# 队列动力学
from .queue_dynamics import (
    QueueEvent, QueueState, QueuePosition, QueueDynamicsAnalyzer,
    QueuePositionPredictor, QueueImbalanceFeatures, AdvancedQueueAnalytics,
    simulate_queue_evolution, compute_queue_efficiency_score
)

# 训练管道
from .training_pipeline import (
    PipelineConfig, PipelineResults, LOBTrainingPipeline,
    run_quick_pipeline
)

# 便捷函数
def create_quick_pipeline(symbol: str = "AAPL",
                         enable_alpha_search: bool = True,
                         output_dir: str = "./lob_results") -> LOBTrainingPipeline:
    """创建快速训练管道"""
    config = PipelineConfig(
        symbol=symbol,
        enable_alpha_search=enable_alpha_search,
        alpha_search_generations=20,
        cv_folds=3,
        output_dir=output_dir
    )
    return LOBTrainingPipeline(config)


def discover_alpha_factors(snapshots, trades, returns,
                          max_generations: int = 30) -> List[AlphaCandidate]:
    """发现Alpha因子的快捷函数"""
    engine = AlphaSearchEngine(FeatureConfig())
    return engine.search_alphas(snapshots, trades, returns, max_generations)


# 导出关键类型
__all__ = [
    # 数据结构
    'MessageType', 'Side', 'LOBMessage', 'PriceLevel', 'LOBSnapshot',
    'Trade', 'InformationBar', 'LOBDataContainer',

    # 数据摄取
    'ITCHParser', 'LOBSTERLoader', 'InformationBarBuilder',

    # 特征工程
    'FeatureConfig', 'FeatureEngineeringPipeline',

    # 标签系统
    'TripleBarrierConfig', 'Label', 'LabelingPipeline', 'PurgedKFold',

    # Alpha搜索
    'AlphaCandidate', 'AlphaSearchEngine', 'ExpressionNode',

    # 模型
    'ModelType', 'ModelConfig', 'ModelFactory', 'BaseLOBModel',

    # 管道
    'PipelineConfig', 'LOBTrainingPipeline', 'run_quick_pipeline',

    # 便捷函数
    'create_quick_pipeline', 'discover_alpha_factors'
]