"""
交易機會評估器
==============

評估交易機會是否值得執行：
- 信號強度評估
- 市場條件評估
- 風險回報比評估
- 流動性評估
"""

from agno import Evaluator
from typing import Dict, Any


class TradingOpportunityEvaluator(Evaluator):
    """
    交易機會評估器
    
    評估當前市場條件下交易信號是否構成有效的交易機會
    """
    
    def __init__(self):
        super().__init__(
            name="TradingOpportunityEvaluator",
            description="評估交易機會是否值得執行"
        )
        
        # 評估閾值
        self.opportunity_thresholds = {
            # 信號質量
            "min_signal_confidence": 0.65,    # 最低信號置信度 65%
            "min_signal_strength": 0.6,       # 最低信號強度 60%
            
            # 市場條件
            "max_volatility": 0.05,           # 最大波動率 5%
            "min_liquidity": 0.7,             # 最低流動性 70%
            "max_spread_bps": 15,             # 最大價差 15bps
            
            # 風險回報
            "min_risk_reward_ratio": 1.5,     # 最低風險回報比 1.5:1
            "max_position_risk": 0.02,        # 最大倉位風險 2%
            
            # 技術指標
            "min_trend_strength": 0.4,        # 最低趋勢強度 40%
            "max_noise_level": 0.3,           # 最大噪音水平 30%
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估交易機會
        
        Args:
            context: 評估上下文，包含交易信號和市場分析
            
        Returns:
            評估結果
        """
        try:
            trading_signals = context.get("trading_signals", {})
            market_analysis = context.get("market_analysis", {})
            
            if not trading_signals or not market_analysis:
                return {
                    "passed": False,
                    "reason": "Missing trading signals or market analysis"
                }
            
            # 檢查信號質量
            signal_check = self._check_signal_quality(trading_signals)
            if not signal_check["passed"]:
                return signal_check
            
            # 檢查市場條件
            market_check = self._check_market_conditions(market_analysis)
            if not market_check["passed"]:
                return market_check
            
            # 檢查風險回報比
            risk_reward_check = self._check_risk_reward_ratio(trading_signals, market_analysis)
            if not risk_reward_check["passed"]:
                return risk_reward_check
            
            # 檢查技術指標確認
            technical_check = self._check_technical_confirmation(trading_signals, market_analysis)
            if not technical_check["passed"]:
                return technical_check
            
            return {
                "passed": True,
                "reason": "Strong trading opportunity identified",
                "opportunity_score": self._calculate_opportunity_score(trading_signals, market_analysis)
            }
            
        except Exception as e:
            return {
                "passed": False,
                "reason": f"Trading opportunity evaluation failed: {str(e)}"
            }
    
    def _check_signal_quality(self, trading_signals: Dict[str, Any]) -> Dict[str, Any]:
        """檢查交易信號質量"""
        signals_data = trading_signals.get("signals", {})
        
        # 從信號數據中提取或模擬關鍵指標
        # 實際實現中應該解析真實的信號數據
        signal_confidence = 0.72  # 模擬值
        signal_strength = 0.68    # 模擬值
        signal_direction = signals_data.get("direction", "hold")
        
        failed_checks = []
        
        # 檢查信號置信度
        if signal_confidence < self.opportunity_thresholds["min_signal_confidence"]:
            failed_checks.append(f"signal_confidence {signal_confidence:.3f} < {self.opportunity_thresholds['min_signal_confidence']}")
        
        # 檢查信號強度
        if signal_strength < self.opportunity_thresholds["min_signal_strength"]:
            failed_checks.append(f"signal_strength {signal_strength:.3f} < {self.opportunity_thresholds['min_signal_strength']}")
        
        # 檢查信號方向有效性
        if signal_direction == "hold":
            failed_checks.append("No directional signal - hold recommendation")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Signal quality checks failed: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_market_conditions(self, market_analysis: Dict[str, Any]) -> Dict[str, Any]:
        """檢查市場條件"""
        analysis_data = market_analysis.get("analysis", {})
        
        # 模擬市場條件數據
        # 實際實現中應該從市場分析中提取真實數據
        current_volatility = 0.035    # 模擬當前波動率
        liquidity_score = 0.75        # 模擬流動性評分
        bid_ask_spread_bps = 12       # 模擬買賣價差
        
        failed_checks = []
        
        # 檢查波動率
        if current_volatility > self.opportunity_thresholds["max_volatility"]:
            failed_checks.append(f"volatility {current_volatility:.3f} > {self.opportunity_thresholds['max_volatility']}")
        
        # 檢查流動性
        if liquidity_score < self.opportunity_thresholds["min_liquidity"]:
            failed_checks.append(f"liquidity {liquidity_score:.3f} < {self.opportunity_thresholds['min_liquidity']}")
        
        # 檢查價差
        if bid_ask_spread_bps > self.opportunity_thresholds["max_spread_bps"]:
            failed_checks.append(f"spread {bid_ask_spread_bps}bps > {self.opportunity_thresholds['max_spread_bps']}bps")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Market conditions unfavorable: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_risk_reward_ratio(self, trading_signals: Dict[str, Any], market_analysis: Dict[str, Any]) -> Dict[str, Any]:
        """檢查風險回報比"""
        # 模擬風險回報計算
        # 實際實現中應該基於信號和市場分析計算真實的風險回報比
        expected_return = 0.025      # 預期回報 2.5%
        expected_risk = 0.015        # 預期風險 1.5%
        position_risk = 0.018        # 倉位風險 1.8%
        
        failed_checks = []
        
        # 計算風險回報比
        if expected_risk > 0:
            risk_reward_ratio = expected_return / expected_risk
            if risk_reward_ratio < self.opportunity_thresholds["min_risk_reward_ratio"]:
                failed_checks.append(f"risk_reward_ratio {risk_reward_ratio:.2f} < {self.opportunity_thresholds['min_risk_reward_ratio']}")
        
        # 檢查倉位風險
        if position_risk > self.opportunity_thresholds["max_position_risk"]:
            failed_checks.append(f"position_risk {position_risk:.3f} > {self.opportunity_thresholds['max_position_risk']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Risk-reward profile inadequate: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_technical_confirmation(self, trading_signals: Dict[str, Any], market_analysis: Dict[str, Any]) -> Dict[str, Any]:
        """檢查技術指標確認"""
        # 模擬技術指標分析
        # 實際實現中應該分析真實的技術指標
        trend_strength = 0.65        # 趋勢強度
        noise_level = 0.25           # 噪音水平
        confirmation_signals = 3     # 確認信號數量
        
        failed_checks = []
        
        # 檢查趋勢強度
        if trend_strength < self.opportunity_thresholds["min_trend_strength"]:
            failed_checks.append(f"trend_strength {trend_strength:.3f} < {self.opportunity_thresholds['min_trend_strength']}")
        
        # 檢查噪音水平
        if noise_level > self.opportunity_thresholds["max_noise_level"]:
            failed_checks.append(f"noise_level {noise_level:.3f} > {self.opportunity_thresholds['max_noise_level']}")
        
        # 檢查技術確認信號數量
        if confirmation_signals < 2:  # 至少需要2個確認信號
            failed_checks.append(f"insufficient_confirmations {confirmation_signals} < 2")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Technical confirmation inadequate: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _calculate_opportunity_score(self, trading_signals: Dict[str, Any], market_analysis: Dict[str, Any]) -> float:
        """計算交易機會評分"""
        # 模擬機會評分計算
        # 實際實現中應該基於多個因素計算綜合評分
        
        signal_score = 0.72      # 信號質量評分
        market_score = 0.68      # 市場條件評分
        risk_reward_score = 0.75 # 風險回報評分
        technical_score = 0.70   # 技術確認評分
        
        # 加權平均
        weights = {"signal": 0.3, "market": 0.2, "risk_reward": 0.3, "technical": 0.2}
        
        opportunity_score = (
            signal_score * weights["signal"] +
            market_score * weights["market"] +
            risk_reward_score * weights["risk_reward"] +
            technical_score * weights["technical"]
        )
        
        return round(opportunity_score, 3)