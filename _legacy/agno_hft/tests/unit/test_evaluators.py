"""
評估器單元測試
==============

測試各種評估器的核心功能
"""

import pytest
import asyncio
from unittest.mock import AsyncMock, MagicMock

from workflows.evaluators_v2.system_health_evaluator import SystemHealthEvaluator
from workflows.evaluators_v2.model_quality_evaluator import ModelQualityEvaluator
from workflows.evaluators_v2.strategy_performance_evaluator import StrategyPerformanceEvaluator
from workflows.evaluators_v2.compliance_evaluator import ComplianceEvaluator
from workflows.evaluators_v2.risk_threshold_evaluator import RiskThresholdEvaluator


@pytest.mark.unit
class TestSystemHealthEvaluator:
    """系統健康評估器測試"""
    
    def setup_method(self):
        """設置測試"""
        self.evaluator = SystemHealthEvaluator()
    
    @pytest.mark.asyncio
    async def test_evaluate_healthy_system(self):
        """測試健康系統評估"""
        context = {
            "health_status": {
                "system_health": {
                    "cpu_usage": 45,
                    "memory_usage": 60,
                    "disk_usage": 30
                }
            }
        }
        
        result = await self.evaluator.evaluate(context)
        
        assert result["passed"] is True
        assert "health_score" in result
        assert result["health_score"] > 0.8
    
    @pytest.mark.asyncio
    async def test_evaluate_missing_context(self):
        """測試缺少上下文的評估"""
        context = {}
        
        result = await self.evaluator.evaluate(context)
        
        assert result["passed"] is False
        assert "No system health data available" in result["reason"]
    
    def test_check_system_resources_normal(self):
        """測試正常系統資源檢查"""
        system_health = {}
        
        result = self.evaluator._check_system_resources(system_health)
        
        # 模擬數據應該通過檢查
        assert result["passed"] is True
    
    def test_check_performance_metrics_normal(self):
        """測試正常性能指標檢查"""
        system_health = {}
        
        result = self.evaluator._check_performance_metrics(system_health)
        
        # 模擬數據應該通過檢查
        assert result["passed"] is True
    
    def test_check_service_status_normal(self):
        """測試正常服務狀態檢查"""
        system_health = {}
        
        result = self.evaluator._check_service_status(system_health)
        
        # 模擬數據應該通過檢查
        assert result["passed"] is True
    
    def test_calculate_health_score(self):
        """測試健康評分計算"""
        system_health = {}
        
        score = self.evaluator._calculate_health_score(system_health)
        
        assert isinstance(score, float)
        assert 0 <= score <= 1


@pytest.mark.unit
class TestModelQualityEvaluator:
    """模型質量評估器測試"""
    
    def setup_method(self):
        """設置測試"""
        self.evaluator = ModelQualityEvaluator()
    
    @pytest.mark.asyncio
    async def test_evaluate_good_model(self):
        """測試良好模型評估"""
        context = {
            "training_result": {
                "accuracy": 0.75,
                "f1_macro": 0.68,
                "precision_macro": 0.72,
                "recall_macro": 0.65
            },
            "feature_engineering": {
                "feature_count": 25,
                "feature_importance": 0.85
            }
        }
        
        result = await self.evaluator.evaluate(context)
        
        assert result["passed"] is True
        assert "model_score" in result
        assert result["model_score"] > 0.6
    
    @pytest.mark.asyncio
    async def test_evaluate_poor_model(self):
        """測試性能不佳的模型評估"""
        # Mock 返回低性能指標
        self.evaluator._extract_classification_metrics = MagicMock(return_value={
            "accuracy": 0.45,  # 低於最低閾值 0.55
            "f1_macro": 0.40,  # 低於最低閾值 0.52
            "precision_macro": 0.45,
            "recall_macro": 0.42,
            "auc_ovr": 0.55    # 低於最低閾值 0.65
        })
        
        context = {
            "training_result": {
                "accuracy": 0.45,
                "f1_macro": 0.40
            }
        }
        
        result = await self.evaluator.evaluate(context)
        
        assert result["passed"] is False
        assert "Classification metrics below ML standards" in result["reason"]
    
    def test_check_classification_metrics_pass(self):
        """測試分類指標檢查通過"""
        training_result = {}
        
        result = self.evaluator._check_classification_metrics(training_result)
        
        # 使用模擬數據應該通過
        assert result["passed"] is True
    
    def test_extract_classification_metrics(self):
        """測試分類指標提取"""
        training_result = {}
        
        metrics = self.evaluator._extract_classification_metrics(training_result)
        
        # 驗證返回的指標
        assert "accuracy" in metrics
        assert "f1_macro" in metrics
        assert "precision_macro" in metrics
        assert "recall_macro" in metrics
        assert "auc_ovr" in metrics
        
        # 驗證指標值在合理範圍內
        assert 0 <= metrics["accuracy"] <= 1
        assert 0 <= metrics["f1_macro"] <= 1


@pytest.mark.unit
class TestStrategyPerformanceEvaluator:
    """策略性能評估器測試"""
    
    def setup_method(self):
        """設置測試"""
        self.evaluator = StrategyPerformanceEvaluator()
    
    @pytest.mark.asyncio
    async def test_evaluate_good_strategy(self):
        """測試良好策略評估"""
        context = {
            "rust_trading_results": {
                "sharpe_ratio": 1.5,
                "max_drawdown": -0.08,
                "win_rate": 0.6,
                "profit_factor": 1.4
            },
            "system_metrics": {
                "avg_latency_us": 200,
                "error_rate": 0.0005
            }
        }
        
        result = await self.evaluator.evaluate(context)
        
        assert result["passed"] is True
        assert "performance_score" in result
        assert result["performance_score"] > 0.7
    
    @pytest.mark.asyncio
    async def test_evaluate_poor_strategy(self):
        """測試性能不佳的策略評估"""
        # Mock 返回低性能指標
        self.evaluator._extract_trading_metrics = MagicMock(return_value={
            "sharpe_ratio": 0.5,     # 低於最低閾值 1.0
            "max_drawdown": -0.18,   # 超過最大閾值 -0.15
            "win_rate": 0.35,        # 低於最低閾值 0.45
            "profit_factor": 0.9,    # 低於最低閾值 1.1
            "total_return": -0.05,
            "calmar_ratio": 0.3
        })
        
        context = {
            "rust_trading_results": {
                "poor_performance": True
            }
        }
        
        result = await self.evaluator.evaluate(context)
        
        assert result["passed"] is False
        assert "Trading performance below benchmarks" in result["reason"]
    
    def test_check_return_risk_metrics_pass(self):
        """測試收益風險指標檢查通過"""
        trading_results = {}
        
        result = self.evaluator._check_return_risk_metrics(trading_results)
        
        # 使用模擬數據應該通過
        assert result["passed"] is True
    
    def test_extract_trading_metrics(self):
        """測試交易指標提取"""
        trading_results = {}
        
        metrics = self.evaluator._extract_trading_metrics(trading_results)
        
        # 驗證返回的指標
        assert "sharpe_ratio" in metrics
        assert "max_drawdown" in metrics
        assert "win_rate" in metrics
        assert "profit_factor" in metrics
        
        # 驗證指標值在合理範圍內
        assert metrics["sharpe_ratio"] > 0
        assert metrics["max_drawdown"] < 0
        assert 0 <= metrics["win_rate"] <= 1


@pytest.mark.unit
class TestComplianceEvaluator:
    """合規性評估器測試"""
    
    def setup_method(self):
        """設置測試"""
        self.evaluator = ComplianceEvaluator()
    
    @pytest.mark.asyncio
    async def test_evaluate_compliant_system(self):
        """測試合規系統評估"""
        context = {
            "compliance_data": {
                "trading_records": {
                    "position_concentration": 0.08,
                    "leverage_ratio": 2.1
                },
                "system_status": {
                    "monitoring": {
                        "coverage": 0.995,
                        "downtime_pct": 0.002
                    }
                },
                "audit": {
                    "available_fields": [
                        "timestamp", "user_id", "action_type", 
                        "instrument", "quantity", "price", "order_id"
                    ],
                    "log_retention_days": 2800
                },
                "reporting": {
                    "available_reports": [
                        "daily_position_report",
                        "risk_metrics_report", 
                        "trading_volume_report",
                        "compliance_breach_report"
                    ],
                    "max_report_delay_hours": 12
                },
                "data_protection": {
                    "encryption_types": ["data_at_rest", "data_in_transit"],
                    "access_control_score": 0.95
                }
            }
        }
        
        result = await self.evaluator.evaluate(context)
        
        assert result["passed"] is True
        assert "compliance_score" in result
        assert result["compliance_score"] > 0.85
    
    @pytest.mark.asyncio
    async def test_evaluate_missing_compliance_data(self):
        """測試缺少合規數據的評估"""
        context = {}
        
        result = await self.evaluator.evaluate(context)
        
        assert result["passed"] is False
        assert "No compliance data available" in result["reason"]
    
    def test_check_position_compliance_pass(self):
        """測試倉位合規檢查通過"""
        trading_records = {}
        
        result = self.evaluator._check_position_compliance(trading_records)
        
        # 使用模擬數據應該通過
        assert result["passed"] is True
    
    def test_extract_position_metrics(self):
        """測試倉位指標提取"""
        trading_records = {}
        
        metrics = self.evaluator._extract_position_metrics(trading_records)
        
        # 驗證返回的指標
        assert "max_single_position_pct" in metrics
        assert "sector_concentration" in metrics
        assert "diversification_ratio" in metrics
        
        # 驗證指標值在合理範圍內
        assert 0 <= metrics["max_single_position_pct"] <= 1
        assert 0 <= metrics["sector_concentration"] <= 1
        assert 0 <= metrics["diversification_ratio"] <= 1


@pytest.mark.unit
class TestRiskThresholdEvaluator:
    """風險閾值評估器測試"""
    
    def setup_method(self):
        """設置測試"""
        self.evaluator = RiskThresholdEvaluator()
    
    @pytest.mark.asyncio
    async def test_evaluate_good_risk_config(self):
        """測試良好風險配置評估"""
        context = {
            "risk_config": {
                "basic_limits": {
                    "portfolio_var_pct": 0.02,
                    "daily_loss_pct": 0.04,
                    "position_concentration": 0.12,
                    "sector_concentration": 0.25
                },
                "dynamic_settings": {
                    "volatility_scaling_factor": 1.5,
                    "correlation_adjustment_factor": 1.2,
                    "liquidity_adjustment": 1.1
                },
                "stop_loss": {
                    "base_stop_loss_pct": 0.03,
                    "trailing_stop_sensitivity": 0.1,
                    "position_size_scaling": 0.8
                },
                "market_adaptation": {
                    "high_vol_threshold": 0.25,
                    "low_liquidity_threshold": 150000,
                    "market_stress_indicator": 0.35
                },
                "time_limits": {
                    "max_trades_per_hour": 2000,
                    "min_position_hold_time_sec": 120,
                    "max_position_duration_hours": 18
                },
                "model_risk": {
                    "confidence_threshold": 0.7,
                    "prediction_decay_factor": 0.9,
                    "uncertainty_limit": 0.2
                },
                "emergency_controls": {
                    "emergency_stop_threshold": 0.08,
                    "circuit_breaker_levels": [0.05, 0.08, 0.1],
                    "recovery_time_minutes": 45
                }
            },
            "market_data": {
                "volatility": 0.15,
                "liquidity": 500000
            }
        }
        
        result = await self.evaluator.evaluate(context)
        
        assert result["passed"] is True
        assert "risk_score" in result
        assert result["risk_score"] > 0.8
    
    @pytest.mark.asyncio
    async def test_evaluate_missing_risk_config(self):
        """測試缺少風險配置的評估"""
        context = {}
        
        result = await self.evaluator.evaluate(context)
        
        assert result["passed"] is False
        assert "No risk configuration available" in result["reason"]
    
    def test_check_basic_risk_limits_pass(self):
        """測試基本風險限額檢查通過"""
        risk_config = {
            "basic_limits": {
                "portfolio_var_pct": 0.02,
                "daily_loss_pct": 0.04
            }
        }
        
        result = self.evaluator._check_basic_risk_limits(risk_config)
        
        # 配置在閾值內應該通過
        assert result["passed"] is True
    
    def test_calculate_risk_score(self):
        """測試風險評分計算"""
        risk_config = {}
        market_data = {}
        
        score = self.evaluator._calculate_risk_score(risk_config, market_data)
        
        assert isinstance(score, float)
        assert 0 <= score <= 1


@pytest.mark.unit
@pytest.mark.performance
class TestEvaluatorPerformance:
    """評估器性能測試"""
    
    @pytest.mark.asyncio
    async def test_system_health_evaluator_performance(self):
        """測試系統健康評估器性能"""
        import time
        
        evaluator = SystemHealthEvaluator()
        context = {
            "health_status": {
                "system_health": {}
            }
        }
        
        start_time = time.time()
        result = await evaluator.evaluate(context)
        end_time = time.time()
        
        # 評估應該在 0.1 秒內完成
        evaluation_time = end_time - start_time
        assert evaluation_time < 0.1
        assert result is not None
    
    @pytest.mark.asyncio
    async def test_model_quality_evaluator_performance(self):
        """測試模型質量評估器性能"""
        import time
        
        evaluator = ModelQualityEvaluator()
        context = {
            "training_result": {
                "accuracy": 0.75
            }
        }
        
        start_time = time.time()
        result = await evaluator.evaluate(context)
        end_time = time.time()
        
        # 評估應該在 0.1 秒內完成
        evaluation_time = end_time - start_time
        assert evaluation_time < 0.1
        assert result is not None