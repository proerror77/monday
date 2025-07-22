"""
重新設計的評估器架構 v2
=======================

基於真實業務需求的評估器設計：

**系統評估器**：
- SystemHealthEvaluator: 系統健康狀況評估
- PerformanceEvaluator: 策略性能評估

**研發評估器**：
- ResearchQualityEvaluator: 研究質量評估
- StrategyViabilityEvaluator: 策略可行性評估

**模型評估器**：
- ModelQualityEvaluator: 模型質量評估
- DeploymentReadinessEvaluator: 部署就緒度評估

**合規評估器**：
- ComplianceEvaluator: 合規性評估
- RiskThresholdEvaluator: 風險閾值評估
"""

from .system_health_evaluator import SystemHealthEvaluator
from .performance_evaluator import PerformanceEvaluator
from .research_quality_evaluator import ResearchQualityEvaluator
from .strategy_viability_evaluator import StrategyViabilityEvaluator
from .model_quality_evaluator import ModelQualityEvaluator
from .deployment_readiness_evaluator import DeploymentReadinessEvaluator
from .compliance_evaluator import ComplianceEvaluator
from .risk_threshold_evaluator import RiskThresholdEvaluator

__all__ = [
    'SystemHealthEvaluator',        # 系統健康評估器
    'PerformanceEvaluator',         # 性能評估器  
    'ResearchQualityEvaluator',     # 研究質量評估器
    'StrategyViabilityEvaluator',   # 策略可行性評估器
    'ModelQualityEvaluator',        # 模型質量評估器
    'DeploymentReadinessEvaluator', # 部署就緒評估器
    'ComplianceEvaluator',          # 合規性評估器
    'RiskThresholdEvaluator'        # 風險閾值評估器
]