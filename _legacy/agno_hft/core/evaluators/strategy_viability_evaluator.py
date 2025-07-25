"""
策略可行性評估器
==============

評估交易策略的可行性和實用性：
- 理論可行性分析
- 市場適應性評估
- 實施複雜度評估
- 資源需求評估

注意：這是評估策略設計本身的可行性，不是交易績效
"""

from agno import Evaluator
from typing import Dict, Any, List
import logging

logger = logging.getLogger(__name__)


class StrategyViabilityEvaluator(Evaluator):
    """
    策略可行性評估器
    
    評估 TradingStrategist 設計的策略是否具備實施可行性
    """
    
    def __init__(self):
        super().__init__(
            name="StrategyViabilityEvaluator",
            description="評估交易策略的可行性和實用性"
        )
        
        # 可行性評估閾值
        self.viability_thresholds = {
            # 理論可行性
            "min_theoretical_foundation": 0.8,     # 理論基礎評分
            "min_logic_consistency": 0.85,        # 邏輯一致性
            "max_complexity_score": 0.7,          # 策略複雜度上限
            
            # 市場適應性
            "min_market_coverage": 0.6,           # 市場覆蓋度
            "min_liquidity_requirement": 1000000, # 流動性需求（美元）
            "max_market_impact": 0.01,            # 最大市場影響 1%
            "min_diversification": 0.4,           # 分散化程度
            
            # 實施可行性
            "max_implementation_time_days": 30,   # 最大實施時間 30 天
            "min_technical_feasibility": 0.8,    # 技術可行性評分
            "max_latency_sensitivity": 0.5,      # 延遲敏感性上限
            
            # 資源需求
            "max_computational_cost": 1000,      # 最大計算成本（美元/月）
            "max_data_cost": 500,                # 最大數據成本（美元/月）
            "max_infrastructure_cost": 2000,     # 最大基礎設施成本（美元/月）
            
            # 風險可控性
            "min_risk_control_coverage": 0.9,    # 風險控制覆蓋度
            "max_tail_risk_exposure": 0.05,      # 最大尾部風險暴露 5%
            "min_stop_loss_effectiveness": 0.8,  # 止損有效性
            
            # 監管合規
            "required_compliance_features": [
                "position_limits",
                "risk_monitoring", 
                "audit_trail",
                "liquidity_checks"
            ]
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估策略可行性
        
        Args:
            context: 包含策略設計和分析的上下文
            
        Returns:
            評估結果
        """
        try:
            strategy_design = context.get("strategy_design", {})
            market_analysis = context.get("market_analysis", {})
            
            if not strategy_design:
                return {
                    "passed": False,
                    "reason": "No strategy design available"
                }
            
            # 檢查理論可行性
            theoretical_check = self._check_theoretical_viability(strategy_design)
            if not theoretical_check["passed"]:
                return theoretical_check
            
            # 檢查市場適應性
            market_check = self._check_market_adaptability(strategy_design, market_analysis)
            if not market_check["passed"]:
                return market_check
            
            # 檢查實施可行性
            implementation_check = self._check_implementation_feasibility(strategy_design)
            if not implementation_check["passed"]:
                return implementation_check
            
            # 檢查資源需求
            resource_check = self._check_resource_requirements(strategy_design)
            if not resource_check["passed"]:
                return resource_check
            
            # 檢查風險可控性
            risk_check = self._check_risk_controllability(strategy_design)
            if not risk_check["passed"]:
                return risk_check
            
            # 檢查合規性
            compliance_check = self._check_compliance_requirements(strategy_design)
            if not compliance_check["passed"]:
                return compliance_check
            
            return {
                "passed": True,
                "reason": "Strategy design is viable and implementable",
                "viability_score": self._calculate_viability_score(strategy_design, market_analysis)
            }
            
        except Exception as e:
            logger.error(f"Strategy viability evaluation failed: {e}")
            return {
                "passed": False,
                "reason": f"Strategy viability evaluation failed: {str(e)}"
            }
    
    def _check_theoretical_viability(self, strategy_design: Dict[str, Any]) -> Dict[str, Any]:
        """檢查理論可行性"""
        theoretical_metrics = self._extract_theoretical_metrics(strategy_design)
        
        failed_checks = []
        
        # 理論基礎檢查
        if theoretical_metrics["foundation_score"] < self.viability_thresholds["min_theoretical_foundation"]:
            failed_checks.append(f"Theoretical foundation {theoretical_metrics['foundation_score']:.2f} < {self.viability_thresholds['min_theoretical_foundation']}")
        
        # 邏輯一致性檢查
        if theoretical_metrics["logic_consistency"] < self.viability_thresholds["min_logic_consistency"]:
            failed_checks.append(f"Logic consistency {theoretical_metrics['logic_consistency']:.2f} < {self.viability_thresholds['min_logic_consistency']}")
        
        # 複雜度檢查
        if theoretical_metrics["complexity_score"] > self.viability_thresholds["max_complexity_score"]:
            failed_checks.append(f"Strategy complexity {theoretical_metrics['complexity_score']:.2f} > {self.viability_thresholds['max_complexity_score']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Theoretical viability issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_market_adaptability(self, strategy_design: Dict[str, Any], market_analysis: Dict[str, Any]) -> Dict[str, Any]:
        """檢查市場適應性"""
        market_metrics = self._extract_market_metrics(strategy_design, market_analysis)
        
        failed_checks = []
        
        # 市場覆蓋度檢查
        if market_metrics["market_coverage"] < self.viability_thresholds["min_market_coverage"]:
            failed_checks.append(f"Market coverage {market_metrics['market_coverage']:.2f} < {self.viability_thresholds['min_market_coverage']}")
        
        # 流動性需求檢查
        if market_metrics["liquidity_requirement"] > self.viability_thresholds["min_liquidity_requirement"]:
            failed_checks.append(f"Liquidity requirement ${market_metrics['liquidity_requirement']:,.0f} > ${self.viability_thresholds['min_liquidity_requirement']:,.0f}")
        
        # 市場影響檢查
        if market_metrics["market_impact"] > self.viability_thresholds["max_market_impact"]:
            failed_checks.append(f"Market impact {market_metrics['market_impact']:.2%} > {self.viability_thresholds['max_market_impact']:.1%}")
        
        # 分散化檢查
        if market_metrics["diversification"] < self.viability_thresholds["min_diversification"]:
            failed_checks.append(f"Diversification {market_metrics['diversification']:.2f} < {self.viability_thresholds['min_diversification']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Market adaptability issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_implementation_feasibility(self, strategy_design: Dict[str, Any]) -> Dict[str, Any]:
        """檢查實施可行性"""
        implementation_metrics = self._extract_implementation_metrics(strategy_design)
        
        failed_checks = []
        
        # 實施時間檢查
        if implementation_metrics["implementation_time_days"] > self.viability_thresholds["max_implementation_time_days"]:
            failed_checks.append(f"Implementation time {implementation_metrics['implementation_time_days']} days > {self.viability_thresholds['max_implementation_time_days']} days")
        
        # 技術可行性檢查
        if implementation_metrics["technical_feasibility"] < self.viability_thresholds["min_technical_feasibility"]:
            failed_checks.append(f"Technical feasibility {implementation_metrics['technical_feasibility']:.2f} < {self.viability_thresholds['min_technical_feasibility']}")
        
        # 延遲敏感性檢查
        if implementation_metrics["latency_sensitivity"] > self.viability_thresholds["max_latency_sensitivity"]:
            failed_checks.append(f"Latency sensitivity {implementation_metrics['latency_sensitivity']:.2f} > {self.viability_thresholds['max_latency_sensitivity']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Implementation feasibility issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_resource_requirements(self, strategy_design: Dict[str, Any]) -> Dict[str, Any]:
        """檢查資源需求"""
        resource_metrics = self._extract_resource_metrics(strategy_design)
        
        failed_checks = []
        
        # 計算成本檢查
        if resource_metrics["computational_cost"] > self.viability_thresholds["max_computational_cost"]:
            failed_checks.append(f"Computational cost ${resource_metrics['computational_cost']} > ${self.viability_thresholds['max_computational_cost']}")
        
        # 數據成本檢查
        if resource_metrics["data_cost"] > self.viability_thresholds["max_data_cost"]:
            failed_checks.append(f"Data cost ${resource_metrics['data_cost']} > ${self.viability_thresholds['max_data_cost']}")
        
        # 基礎設施成本檢查
        if resource_metrics["infrastructure_cost"] > self.viability_thresholds["max_infrastructure_cost"]:
            failed_checks.append(f"Infrastructure cost ${resource_metrics['infrastructure_cost']} > ${self.viability_thresholds['max_infrastructure_cost']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Resource requirement issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_risk_controllability(self, strategy_design: Dict[str, Any]) -> Dict[str, Any]:
        """檢查風險可控性"""
        risk_metrics = self._extract_risk_metrics(strategy_design)
        
        failed_checks = []
        
        # 風險控制覆蓋度檢查
        if risk_metrics["risk_control_coverage"] < self.viability_thresholds["min_risk_control_coverage"]:
            failed_checks.append(f"Risk control coverage {risk_metrics['risk_control_coverage']:.2f} < {self.viability_thresholds['min_risk_control_coverage']}")
        
        # 尾部風險暴露檢查
        if risk_metrics["tail_risk_exposure"] > self.viability_thresholds["max_tail_risk_exposure"]:
            failed_checks.append(f"Tail risk exposure {risk_metrics['tail_risk_exposure']:.2%} > {self.viability_thresholds['max_tail_risk_exposure']:.1%}")
        
        # 止損有效性檢查
        if risk_metrics["stop_loss_effectiveness"] < self.viability_thresholds["min_stop_loss_effectiveness"]:
            failed_checks.append(f"Stop loss effectiveness {risk_metrics['stop_loss_effectiveness']:.2f} < {self.viability_thresholds['min_stop_loss_effectiveness']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Risk controllability issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_compliance_requirements(self, strategy_design: Dict[str, Any]) -> Dict[str, Any]:
        """檢查合規要求"""
        compliance_features = strategy_design.get("compliance_features", [])
        
        failed_checks = []
        
        # 必需合規功能檢查
        for required_feature in self.viability_thresholds["required_compliance_features"]:
            if required_feature not in compliance_features:
                failed_checks.append(f"Missing compliance feature: {required_feature}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Compliance requirement issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _extract_theoretical_metrics(self, strategy_design: Dict[str, Any]) -> Dict[str, float]:
        """提取理論可行性指標（模擬數據）"""
        return {
            "foundation_score": 0.89,      # 理論基礎評分
            "logic_consistency": 0.92,     # 邏輯一致性評分
            "complexity_score": 0.45,      # 策略複雜度評分
            "innovation_score": 0.76       # 創新性評分
        }
    
    def _extract_market_metrics(self, strategy_design: Dict[str, Any], market_analysis: Dict[str, Any]) -> Dict[str, Any]:
        """提取市場適應性指標（模擬數據）"""
        return {
            "market_coverage": 0.73,         # 市場覆蓋度
            "liquidity_requirement": 750000, # 流動性需求
            "market_impact": 0.006,          # 市場影響
            "diversification": 0.62,         # 分散化程度
            "market_timing_score": 0.78      # 市場時機評分
        }
    
    def _extract_implementation_metrics(self, strategy_design: Dict[str, Any]) -> Dict[str, Any]:
        """提取實施可行性指標（模擬數據）"""
        return {
            "implementation_time_days": 21,   # 實施時間
            "technical_feasibility": 0.87,   # 技術可行性
            "latency_sensitivity": 0.35,     # 延遲敏感性
            "integration_complexity": 0.42   # 集成複雜度
        }
    
    def _extract_resource_metrics(self, strategy_design: Dict[str, Any]) -> Dict[str, int]:
        """提取資源需求指標（模擬數據）"""
        return {
            "computational_cost": 680,      # 計算成本（美元/月）
            "data_cost": 320,               # 數據成本（美元/月）
            "infrastructure_cost": 1200,    # 基礎設施成本（美元/月）
            "maintenance_cost": 450         # 維護成本（美元/月）
        }
    
    def _extract_risk_metrics(self, strategy_design: Dict[str, Any]) -> Dict[str, float]:
        """提取風險可控性指標（模擬數據）"""
        return {
            "risk_control_coverage": 0.94,    # 風險控制覆蓋度
            "tail_risk_exposure": 0.032,      # 尾部風險暴露
            "stop_loss_effectiveness": 0.86,  # 止損有效性
            "risk_diversification": 0.71      # 風險分散化
        }
    
    def _calculate_viability_score(self, strategy_design: Dict[str, Any], market_analysis: Dict[str, Any]) -> float:
        """計算策略可行性綜合評分"""
        theoretical_metrics = self._extract_theoretical_metrics(strategy_design)
        market_metrics = self._extract_market_metrics(strategy_design, market_analysis)
        implementation_metrics = self._extract_implementation_metrics(strategy_design)
        risk_metrics = self._extract_risk_metrics(strategy_design)
        
        # 各維度評分
        scores = {
            "theoretical_score": theoretical_metrics["foundation_score"],
            "market_score": market_metrics["market_timing_score"],
            "implementation_score": implementation_metrics["technical_feasibility"],
            "risk_score": risk_metrics["risk_control_coverage"],
            "resource_score": 0.85  # 基於資源需求的合理性評分
        }
        
        # 權重分配
        weights = {
            "theoretical_score": 0.25,     # 理論基礎
            "market_score": 0.25,          # 市場適應性
            "implementation_score": 0.20,  # 實施可行性
            "risk_score": 0.20,            # 風險可控性
            "resource_score": 0.10          # 資源合理性
        }
        
        viability_score = sum(scores[key] * weights[key] for key in scores.keys())
        return round(viability_score, 3)