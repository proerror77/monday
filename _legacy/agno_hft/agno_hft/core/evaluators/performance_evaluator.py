"""
策略性能評估器
==============

評估交易策略的實時性能表現：
- 收益率和風險指標
- Sharpe 比率和回撤
- 交易執行效率
- 相對基準表現
"""

from agno import Evaluator
from typing import Dict, Any


class PerformanceEvaluator(Evaluator):
    """
    策略性能評估器
    
    評估交易策略是否達到預期性能標準
    """
    
    def __init__(self):
        super().__init__(
            name="PerformanceEvaluator",
            description="評估交易策略實時性能表現"
        )
        
        # 性能評估閾值
        self.performance_thresholds = {
            # 收益指標
            "min_sharpe_ratio": 1.0,       # 最低 Sharpe 比率
            "max_drawdown": -0.15,         # 最大回撤限制 15%
            "min_win_rate": 0.45,          # 最低勝率 45%
            "min_profit_factor": 1.1,      # 最低盈虧比 1.1
            
            # 執行效率
            "max_avg_slippage_bps": 5,     # 最大平均滑點 5bps
            "min_execution_rate": 0.95,    # 最低成交率 95%
            "max_execution_delay_ms": 100, # 最大執行延遲 100ms
            
            # 穩定性指標
            "max_volatility": 0.25,        # 最大波動率 25%
            "min_consistency_score": 0.7,  # 最低一致性評分
            "max_tail_risk": 0.05,         # 最大尾部風險 5%
            
            # 相對表現
            "min_alpha": 0.02,             # 最低超額收益 2%
            "max_beta": 1.2,               # 最大市場敏感度
            "min_information_ratio": 0.5   # 最低信息比率
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估策略性能
        
        Args:
            context: 包含策略性能數據的上下文
            
        Returns:
            評估結果
        """
        try:
            strategy_performance = context.get("strategy_performance", {})
            
            if not strategy_performance:
                return {
                    "passed": False,
                    "reason": "No strategy performance data available"
                }
            
            # 檢查收益指標
            return_check = self._check_return_metrics(strategy_performance)
            if not return_check["passed"]:
                return return_check
            
            # 檢查風險指標
            risk_check = self._check_risk_metrics(strategy_performance)
            if not risk_check["passed"]:
                return risk_check
            
            # 檢查執行效率
            execution_check = self._check_execution_efficiency(strategy_performance)
            if not execution_check["passed"]:
                return execution_check
            
            # 檢查穩定性
            stability_check = self._check_stability_metrics(strategy_performance)
            if not stability_check["passed"]:
                return stability_check
            
            return {
                "passed": True,
                "reason": "Strategy performance meets all benchmarks",
                "performance_score": self._calculate_performance_score(strategy_performance)
            }
            
        except Exception as e:
            return {
                "passed": False,
                "reason": f"Performance evaluation failed: {str(e)}"
            }
    
    def _check_return_metrics(self, performance_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查收益指標"""
        # 從性能數據中提取或模擬指標
        # 實際實現中需要解析真實的性能數據
        metrics = self._extract_return_metrics(performance_data)
        
        failed_checks = []
        
        # Sharpe 比率檢查
        if metrics["sharpe_ratio"] < self.performance_thresholds["min_sharpe_ratio"]:
            failed_checks.append(f"Sharpe ratio {metrics['sharpe_ratio']:.2f} < {self.performance_thresholds['min_sharpe_ratio']}")
        
        # 勝率檢查
        if metrics["win_rate"] < self.performance_thresholds["min_win_rate"]:
            failed_checks.append(f"Win rate {metrics['win_rate']:.2%} < {self.performance_thresholds['min_win_rate']:.1%}")
        
        # 盈虧比檢查
        if metrics["profit_factor"] < self.performance_thresholds["min_profit_factor"]:
            failed_checks.append(f"Profit factor {metrics['profit_factor']:.2f} < {self.performance_thresholds['min_profit_factor']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Return metrics below threshold: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_risk_metrics(self, performance_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查風險指標"""
        metrics = self._extract_risk_metrics(performance_data)
        
        failed_checks = []
        
        # 最大回撤檢查
        if metrics["max_drawdown"] < self.performance_thresholds["max_drawdown"]:
            failed_checks.append(f"Max drawdown {metrics['max_drawdown']:.2%} exceeds {self.performance_thresholds['max_drawdown']:.1%}")
        
        # 波動率檢查
        if metrics["volatility"] > self.performance_thresholds["max_volatility"]:
            failed_checks.append(f"Volatility {metrics['volatility']:.2%} > {self.performance_thresholds['max_volatility']:.1%}")
        
        # 尾部風險檢查
        if metrics["tail_risk"] > self.performance_thresholds["max_tail_risk"]:
            failed_checks.append(f"Tail risk {metrics['tail_risk']:.2%} > {self.performance_thresholds['max_tail_risk']:.1%}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Risk metrics exceed limits: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_execution_efficiency(self, performance_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查執行效率"""
        metrics = self._extract_execution_metrics(performance_data)
        
        failed_checks = []
        
        # 滑點檢查
        if metrics["avg_slippage_bps"] > self.performance_thresholds["max_avg_slippage_bps"]:
            failed_checks.append(f"Avg slippage {metrics['avg_slippage_bps']:.1f}bps > {self.performance_thresholds['max_avg_slippage_bps']}bps")
        
        # 成交率檢查
        if metrics["execution_rate"] < self.performance_thresholds["min_execution_rate"]:
            failed_checks.append(f"Execution rate {metrics['execution_rate']:.2%} < {self.performance_thresholds['min_execution_rate']:.1%}")
        
        # 執行延遲檢查
        if metrics["execution_delay_ms"] > self.performance_thresholds["max_execution_delay_ms"]:
            failed_checks.append(f"Execution delay {metrics['execution_delay_ms']}ms > {self.performance_thresholds['max_execution_delay_ms']}ms")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Execution efficiency issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_stability_metrics(self, performance_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查穩定性指標"""
        metrics = self._extract_stability_metrics(performance_data)
        
        failed_checks = []
        
        # 一致性評分檢查
        if metrics["consistency_score"] < self.performance_thresholds["min_consistency_score"]:
            failed_checks.append(f"Consistency score {metrics['consistency_score']:.2f} < {self.performance_thresholds['min_consistency_score']}")
        
        # Alpha 檢查
        if metrics["alpha"] < self.performance_thresholds["min_alpha"]:
            failed_checks.append(f"Alpha {metrics['alpha']:.2%} < {self.performance_thresholds['min_alpha']:.1%}")
        
        # Beta 檢查
        if metrics["beta"] > self.performance_thresholds["max_beta"]:
            failed_checks.append(f"Beta {metrics['beta']:.2f} > {self.performance_thresholds['max_beta']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Stability metrics insufficient: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _extract_return_metrics(self, performance_data: Dict[str, Any]) -> Dict[str, float]:
        """提取收益指標（模擬數據）"""
        # 實際實現中應該從 performance_data 解析真實數據
        return {
            "sharpe_ratio": 1.25,
            "win_rate": 0.567,
            "profit_factor": 1.34,
            "total_return": 0.0456
        }
    
    def _extract_risk_metrics(self, performance_data: Dict[str, Any]) -> Dict[str, float]:
        """提取風險指標（模擬數據）"""
        return {
            "max_drawdown": -0.0789,
            "volatility": 0.18,
            "tail_risk": 0.032,
            "var_95": -0.025
        }
    
    def _extract_execution_metrics(self, performance_data: Dict[str, Any]) -> Dict[str, float]:
        """提取執行效率指標（模擬數據）"""
        return {
            "avg_slippage_bps": 2.1,
            "execution_rate": 0.973,
            "execution_delay_ms": 45,
            "fill_ratio": 0.985
        }
    
    def _extract_stability_metrics(self, performance_data: Dict[str, Any]) -> Dict[str, float]:
        """提取穩定性指標（模擬數據）"""
        return {
            "consistency_score": 0.84,
            "alpha": 0.034,
            "beta": 0.92,
            "information_ratio": 0.67
        }
    
    def _calculate_performance_score(self, performance_data: Dict[str, Any]) -> float:
        """計算綜合性能評分"""
        return_metrics = self._extract_return_metrics(performance_data)
        risk_metrics = self._extract_risk_metrics(performance_data)
        execution_metrics = self._extract_execution_metrics(performance_data)
        stability_metrics = self._extract_stability_metrics(performance_data)
        
        # 各維度評分
        scores = {
            "return_score": min(return_metrics["sharpe_ratio"] / 2.0, 1.0),
            "risk_score": max(0, 1 + risk_metrics["max_drawdown"] / 0.2),  
            "execution_score": execution_metrics["execution_rate"],
            "stability_score": stability_metrics["consistency_score"]
        }
        
        # 權重
        weights = {
            "return_score": 0.35,
            "risk_score": 0.25,
            "execution_score": 0.25,
            "stability_score": 0.15
        }
        
        performance_score = sum(scores[key] * weights[key] for key in scores.keys())
        return round(performance_score, 3)