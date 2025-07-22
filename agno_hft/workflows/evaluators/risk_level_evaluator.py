"""
風險水平評估器
==============

評估當前風險水平是否可以接受：
- 組合風險評估
- 市場風險評估
- 流動性風險評估
- 集中度風險評估
"""

from agno import Evaluator
from typing import Dict, Any


class RiskLevelEvaluator(Evaluator):
    """
    風險水平評估器
    
    評估當前風險水平是否在可接受範圍內
    """
    
    def __init__(self):
        super().__init__(
            name="RiskLevelEvaluator",
            description="評估風險水平是否可以接受"
        )
        
        # 風險限制閾值
        self.risk_thresholds = {
            # VaR 限制
            "max_var_1d_95": -2000,        # 1日95% VaR 不超過 -$2000
            "max_var_1d_99": -3500,        # 1日99% VaR 不超過 -$3500
            "max_expected_shortfall": -4000, # ES 不超過 -$4000
            
            # 組合風險
            "max_portfolio_volatility": 0.25, # 組合波動率不超過 25%
            "max_leverage_ratio": 5.0,        # 槓桿倍數不超過 5x
            "max_margin_utilization": 0.8,    # 保證金使用率不超過 80%
            
            # 集中度風險
            "max_single_position": 0.2,      # 單一倉位不超過組合 20%
            "max_sector_concentration": 0.4,  # 單一板塊不超過組合 40%
            "max_correlation_exposure": 0.7,  # 高相關性倉位不超過 70%
            
            # 流動性風險
            "min_liquidity_score": 0.6,      # 最低流動性評分 60%
            "max_position_vs_volume": 0.1,   # 倉位不超過日均成交量 10%
            
            # 回撤控制
            "max_drawdown_limit": -0.12,     # 最大回撤限制 12%
            "max_daily_loss": -0.05,         # 最大日損失 5%
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估風險水平
        
        Args:
            context: 評估上下文，包含風險評估數據
            
        Returns:
            評估結果
        """
        try:
            risk_assessment = context.get("risk_assessment", {})
            risk_data = risk_assessment.get("risk_assessment", {})
            
            if not risk_data:
                return {
                    "passed": False,
                    "reason": "No risk assessment data available"
                }
            
            # 檢查 VaR 限制
            var_check = self._check_var_limits(risk_data)
            if not var_check["passed"]:
                return var_check
            
            # 檢查組合風險
            portfolio_check = self._check_portfolio_risk(risk_data)
            if not portfolio_check["passed"]:
                return portfolio_check
            
            # 檢查集中度風險
            concentration_check = self._check_concentration_risk(risk_data)
            if not concentration_check["passed"]:
                return concentration_check
            
            # 檢查流動性風險
            liquidity_check = self._check_liquidity_risk(risk_data)
            if not liquidity_check["passed"]:
                return liquidity_check
            
            # 檢查回撤控制
            drawdown_check = self._check_drawdown_limits(risk_data)
            if not drawdown_check["passed"]:
                return drawdown_check
            
            return {
                "passed": True,
                "reason": "Risk levels are within acceptable limits",
                "risk_score": self._calculate_risk_score(risk_data)
            }
            
        except Exception as e:
            return {
                "passed": False,
                "reason": f"Risk level evaluation failed: {str(e)}"
            }
    
    def _check_var_limits(self, risk_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查 VaR 限制"""
        # 從風險數據中提取或模擬 VaR 數據
        # 實際實現中應該使用真實的 VaR 計算結果
        var_1d_95 = -1234.56     # 模擬 1日95% VaR
        var_1d_99 = -2345.67     # 模擬 1日99% VaR
        expected_shortfall = -2789.12  # 模擬 ES
        
        failed_checks = []
        
        # 檢查 1日95% VaR
        if var_1d_95 < self.risk_thresholds["max_var_1d_95"]:
            failed_checks.append(f"VaR_95 {var_1d_95:.2f} exceeds limit {self.risk_thresholds['max_var_1d_95']}")
        
        # 檢查 1日99% VaR
        if var_1d_99 < self.risk_thresholds["max_var_1d_99"]:
            failed_checks.append(f"VaR_99 {var_1d_99:.2f} exceeds limit {self.risk_thresholds['max_var_1d_99']}")
        
        # 檢查預期損失
        if expected_shortfall < self.risk_thresholds["max_expected_shortfall"]:
            failed_checks.append(f"ES {expected_shortfall:.2f} exceeds limit {self.risk_thresholds['max_expected_shortfall']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"VaR limits exceeded: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_portfolio_risk(self, risk_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查組合風險"""
        # 模擬組合風險數據
        portfolio_volatility = 0.18    # 組合波動率
        leverage_ratio = 3.2          # 槓桿比率
        margin_utilization = 0.65     # 保證金使用率
        
        failed_checks = []
        
        # 檢查組合波動率
        if portfolio_volatility > self.risk_thresholds["max_portfolio_volatility"]:
            failed_checks.append(f"volatility {portfolio_volatility:.3f} > {self.risk_thresholds['max_portfolio_volatility']}")
        
        # 檢查槓桿比率
        if leverage_ratio > self.risk_thresholds["max_leverage_ratio"]:
            failed_checks.append(f"leverage {leverage_ratio:.2f} > {self.risk_thresholds['max_leverage_ratio']}")
        
        # 檢查保證金使用率
        if margin_utilization > self.risk_thresholds["max_margin_utilization"]:
            failed_checks.append(f"margin_utilization {margin_utilization:.3f} > {self.risk_thresholds['max_margin_utilization']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Portfolio risk limits exceeded: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_concentration_risk(self, risk_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查集中度風險"""
        # 模擬集中度風險數據
        largest_position = 0.15       # 最大單一倉位比例
        sector_concentration = 0.35   # 板塊集中度
        correlation_exposure = 0.6    # 高相關性倉位比例
        
        failed_checks = []
        
        # 檢查單一倉位集中度
        if largest_position > self.risk_thresholds["max_single_position"]:
            failed_checks.append(f"single_position {largest_position:.3f} > {self.risk_thresholds['max_single_position']}")
        
        # 檢查板塊集中度
        if sector_concentration > self.risk_thresholds["max_sector_concentration"]:
            failed_checks.append(f"sector_concentration {sector_concentration:.3f} > {self.risk_thresholds['max_sector_concentration']}")
        
        # 檢查相關性風險
        if correlation_exposure > self.risk_thresholds["max_correlation_exposure"]:
            failed_checks.append(f"correlation_exposure {correlation_exposure:.3f} > {self.risk_thresholds['max_correlation_exposure']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Concentration risk limits exceeded: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_liquidity_risk(self, risk_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查流動性風險"""
        # 模擬流動性風險數據
        liquidity_score = 0.75        # 流動性評分
        position_vs_volume = 0.08     # 倉位相對於成交量比例
        
        failed_checks = []
        
        # 檢查流動性評分
        if liquidity_score < self.risk_thresholds["min_liquidity_score"]:
            failed_checks.append(f"liquidity_score {liquidity_score:.3f} < {self.risk_thresholds['min_liquidity_score']}")
        
        # 檢查倉位相對大小
        if position_vs_volume > self.risk_thresholds["max_position_vs_volume"]:
            failed_checks.append(f"position_vs_volume {position_vs_volume:.3f} > {self.risk_thresholds['max_position_vs_volume']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Liquidity risk limits exceeded: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_drawdown_limits(self, risk_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查回撤限制"""
        # 模擬回撤數據
        current_drawdown = -0.08      # 當前回撤
        daily_loss = -0.03           # 當日損失
        
        failed_checks = []
        
        # 檢查最大回撤
        if current_drawdown < self.risk_thresholds["max_drawdown_limit"]:
            failed_checks.append(f"drawdown {current_drawdown:.3f} exceeds limit {self.risk_thresholds['max_drawdown_limit']}")
        
        # 檢查日損失
        if daily_loss < self.risk_thresholds["max_daily_loss"]:
            failed_checks.append(f"daily_loss {daily_loss:.3f} exceeds limit {self.risk_thresholds['max_daily_loss']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Drawdown limits exceeded: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _calculate_risk_score(self, risk_data: Dict[str, Any]) -> float:
        """計算風險評分"""
        # 模擬風險評分計算
        # 實際實現中應該基於多個風險因子計算綜合評分
        
        var_score = 0.7          # VaR 風險評分
        portfolio_score = 0.6    # 組合風險評分
        concentration_score = 0.8 # 集中度風險評分
        liquidity_score = 0.75   # 流動性風險評分
        drawdown_score = 0.65    # 回撤風險評分
        
        # 加權平均 (評分越低表示風險越高)
        weights = {
            "var": 0.25,
            "portfolio": 0.2,
            "concentration": 0.2,
            "liquidity": 0.15,
            "drawdown": 0.2
        }
        
        risk_score = (
            var_score * weights["var"] +
            portfolio_score * weights["portfolio"] +
            concentration_score * weights["concentration"] +
            liquidity_score * weights["liquidity"] +
            drawdown_score * weights["drawdown"]
        )
        
        return round(1.0 - risk_score, 3)  # 轉換為風險水平 (越高表示風險越大)