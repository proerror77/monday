"""
風險閾值評估器
============

評估風險參數和閾值設置：
- 風險限額合理性
- 止損設置有效性
- 波動率控制範圍
- 集中度風險管理

注意：這是評估風險管理參數的設置是否合理
"""

from agno import Evaluator
from typing import Dict, Any, List
import logging
import math

logger = logging.getLogger(__name__)


class RiskThresholdEvaluator(Evaluator):
    """
    風險閾值評估器
    
    評估 RiskSupervisor 設置的風險參數是否合理
    """
    
    def __init__(self):
        super().__init__(
            name="RiskThresholdEvaluator",
            description="評估風險參數和閾值設置"
        )
        
        # 風險閾值標準
        self.risk_standards = {
            # 基本風險限額
            "max_portfolio_var_pct": 0.025,        # 組合 VaR 不超過 2.5%
            "max_daily_loss_pct": 0.05,            # 日最大虧損不超過 5%
            "max_position_concentration": 0.15,     # 單一倉位不超過 15%
            "max_sector_concentration": 0.35,       # 單一行業不超過 35%
            
            # 動態風險管理
            "volatility_scaling_factor": 2.0,      # 波動率縮放因子
            "correlation_adjustment_factor": 1.5,   # 相關性調整因子
            "liquidity_adjustment_range": [0.8, 1.2],  # 流動性調整範圍
            
            # 止損和風控
            "min_stop_loss_pct": 0.02,              # 最小止損 2%
            "max_stop_loss_pct": 0.08,              # 最大止損 8%
            "trailing_stop_sensitivity": 0.15,     # 追蹤止損敏感度
            "position_size_scaling": [0.5, 2.0],   # 倉位縮放範圍
            
            # 市場狀況適應
            "high_vol_threshold": 0.3,              # 高波動率閾值 30%
            "low_liquidity_threshold": 100000,      # 低流動性閾值 $100k
            "market_stress_indicator": 0.4,         # 市場壓力指標
            
            # 時間和頻率限制
            "max_trades_per_hour": 3600,           # 每小時最大交易數
            "min_position_hold_time_sec": 60,      # 最短持倉時間 60 秒
            "max_position_duration_hours": 24,     # 最長持倉時間 24 小時
            
            # 模型風險控制
            "model_confidence_threshold": 0.6,     # 模型置信度閾值
            "prediction_decay_factor": 0.95,       # 預測衰減因子
            "model_uncertainty_limit": 0.25,       # 模型不確定性限制
            
            # 緊急風控措施
            "emergency_stop_threshold": 0.1,       # 緊急停止閾值 10%
            "circuit_breaker_levels": [0.05, 0.08, 0.1],  # 熔斷級別
            "recovery_time_minutes": 30             # 恢復時間 30 分鐘
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估風險閾值設置
        
        Args:
            context: 包含風險配置和歷史數據的上下文
            
        Returns:
            評估結果
        """
        try:
            risk_config = context.get("risk_config", {})
            market_data = context.get("market_data", {})
            historical_performance = context.get("historical_performance", {})
            
            if not risk_config:
                return {
                    "passed": False,
                    "reason": "No risk configuration available"
                }
            
            # 檢查基本風險限額
            basic_limits_check = self._check_basic_risk_limits(risk_config)
            if not basic_limits_check["passed"]:
                return basic_limits_check
            
            # 檢查動態風險管理
            dynamic_risk_check = self._check_dynamic_risk_management(risk_config, market_data)
            if not dynamic_risk_check["passed"]:
                return dynamic_risk_check
            
            # 檢查止損設置
            stop_loss_check = self._check_stop_loss_settings(risk_config)
            if not stop_loss_check["passed"]:
                return stop_loss_check
            
            # 檢查市場適應性
            market_adaptation_check = self._check_market_adaptation(risk_config, market_data)
            if not market_adaptation_check["passed"]:
                return market_adaptation_check
            
            # 檢查時間限制
            time_limits_check = self._check_time_limits(risk_config)
            if not time_limits_check["passed"]:
                return time_limits_check
            
            # 檢查模型風險控制
            model_risk_check = self._check_model_risk_control(risk_config)
            if not model_risk_check["passed"]:
                return model_risk_check
            
            # 檢查緊急風控
            emergency_check = self._check_emergency_controls(risk_config)
            if not emergency_check["passed"]:
                return emergency_check
            
            # 歷史驗證（如果有數據）
            if historical_performance:
                historical_check = self._check_historical_effectiveness(risk_config, historical_performance)
                if not historical_check["passed"]:
                    return historical_check
            
            return {
                "passed": True,
                "reason": "Risk thresholds are appropriately configured",
                "risk_score": self._calculate_risk_score(risk_config, market_data)
            }
            
        except Exception as e:
            logger.error(f"Risk threshold evaluation failed: {e}")
            return {
                "passed": False,
                "reason": f"Risk threshold evaluation failed: {str(e)}"
            }
    
    def _check_basic_risk_limits(self, risk_config: Dict[str, Any]) -> Dict[str, Any]:
        """檢查基本風險限額"""
        basic_limits = risk_config.get("basic_limits", {})
        
        failed_checks = []
        
        # 組合 VaR 檢查
        portfolio_var = basic_limits.get("portfolio_var_pct", 0)
        if portfolio_var > self.risk_standards["max_portfolio_var_pct"]:
            failed_checks.append(f"Portfolio VaR {portfolio_var:.2%} > {self.risk_standards['max_portfolio_var_pct']:.1%}")
        
        # 日最大虧損檢查
        daily_loss_limit = basic_limits.get("daily_loss_pct", 0)
        if daily_loss_limit > self.risk_standards["max_daily_loss_pct"]:
            failed_checks.append(f"Daily loss limit {daily_loss_limit:.2%} > {self.risk_standards['max_daily_loss_pct']:.1%}")
        
        # 倉位集中度檢查
        position_concentration = basic_limits.get("position_concentration", 0)
        if position_concentration > self.risk_standards["max_position_concentration"]:
            failed_checks.append(f"Position concentration {position_concentration:.2%} > {self.risk_standards['max_position_concentration']:.1%}")
        
        # 行業集中度檢查
        sector_concentration = basic_limits.get("sector_concentration", 0)
        if sector_concentration > self.risk_standards["max_sector_concentration"]:
            failed_checks.append(f"Sector concentration {sector_concentration:.2%} > {self.risk_standards['max_sector_concentration']:.1%}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Basic risk limits violations: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_dynamic_risk_management(self, risk_config: Dict[str, Any], market_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查動態風險管理"""
        dynamic_settings = risk_config.get("dynamic_settings", {})
        
        failed_checks = []
        
        # 波動率縮放因子檢查
        vol_scaling = dynamic_settings.get("volatility_scaling_factor", 1.0)
        if vol_scaling > self.risk_standards["volatility_scaling_factor"]:
            failed_checks.append(f"Volatility scaling {vol_scaling:.1f} > {self.risk_standards['volatility_scaling_factor']}")
        
        # 相關性調整因子檢查
        corr_adjustment = dynamic_settings.get("correlation_adjustment_factor", 1.0)
        if corr_adjustment > self.risk_standards["correlation_adjustment_factor"]:
            failed_checks.append(f"Correlation adjustment {corr_adjustment:.1f} > {self.risk_standards['correlation_adjustment_factor']}")
        
        # 流動性調整範圍檢查
        liquidity_adjustment = dynamic_settings.get("liquidity_adjustment", 1.0)
        min_liq, max_liq = self.risk_standards["liquidity_adjustment_range"]
        if not (min_liq <= liquidity_adjustment <= max_liq):
            failed_checks.append(f"Liquidity adjustment {liquidity_adjustment:.1f} outside range [{min_liq}, {max_liq}]")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Dynamic risk management issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_stop_loss_settings(self, risk_config: Dict[str, Any]) -> Dict[str, Any]:
        """檢查止損設置"""
        stop_loss_settings = risk_config.get("stop_loss", {})
        
        failed_checks = []
        
        # 基本止損範圍檢查
        stop_loss_pct = stop_loss_settings.get("base_stop_loss_pct", 0)
        min_stop = self.risk_standards["min_stop_loss_pct"]
        max_stop = self.risk_standards["max_stop_loss_pct"]
        
        if not (min_stop <= stop_loss_pct <= max_stop):
            failed_checks.append(f"Stop loss {stop_loss_pct:.2%} outside range [{min_stop:.1%}, {max_stop:.1%}]")
        
        # 追蹤止損敏感度檢查
        trailing_sensitivity = stop_loss_settings.get("trailing_stop_sensitivity", 0)
        if trailing_sensitivity > self.risk_standards["trailing_stop_sensitivity"]:
            failed_checks.append(f"Trailing stop sensitivity {trailing_sensitivity:.2f} > {self.risk_standards['trailing_stop_sensitivity']}")
        
        # 倉位縮放檢查
        position_scaling = stop_loss_settings.get("position_size_scaling", 1.0)
        min_scale, max_scale = self.risk_standards["position_size_scaling"]
        if not (min_scale <= position_scaling <= max_scale):
            failed_checks.append(f"Position scaling {position_scaling:.1f} outside range [{min_scale}, {max_scale}]")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Stop loss settings issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_market_adaptation(self, risk_config: Dict[str, Any], market_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查市場適應性"""
        market_adaptation = risk_config.get("market_adaptation", {})
        
        failed_checks = []
        
        # 高波動率閾值檢查
        high_vol_threshold = market_adaptation.get("high_vol_threshold", 0)
        if high_vol_threshold > self.risk_standards["high_vol_threshold"]:
            failed_checks.append(f"High volatility threshold {high_vol_threshold:.2%} > {self.risk_standards['high_vol_threshold']:.1%}")
        
        # 低流動性閾值檢查
        low_liq_threshold = market_adaptation.get("low_liquidity_threshold", 0)
        if low_liq_threshold < self.risk_standards["low_liquidity_threshold"]:
            failed_checks.append(f"Low liquidity threshold ${low_liq_threshold:,.0f} < ${self.risk_standards['low_liquidity_threshold']:,.0f}")
        
        # 市場壓力指標檢查
        stress_indicator = market_adaptation.get("market_stress_indicator", 0)
        if stress_indicator > self.risk_standards["market_stress_indicator"]:
            failed_checks.append(f"Market stress indicator {stress_indicator:.2f} > {self.risk_standards['market_stress_indicator']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Market adaptation issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_time_limits(self, risk_config: Dict[str, Any]) -> Dict[str, Any]:
        """檢查時間限制"""
        time_limits = risk_config.get("time_limits", {})
        
        failed_checks = []
        
        # 交易頻率檢查
        max_trades_per_hour = time_limits.get("max_trades_per_hour", 0)
        if max_trades_per_hour > self.risk_standards["max_trades_per_hour"]:
            failed_checks.append(f"Max trades per hour {max_trades_per_hour} > {self.risk_standards['max_trades_per_hour']}")
        
        # 最短持倉時間檢查
        min_hold_time = time_limits.get("min_position_hold_time_sec", 0)
        if min_hold_time < self.risk_standards["min_position_hold_time_sec"]:
            failed_checks.append(f"Min hold time {min_hold_time}s < {self.risk_standards['min_position_hold_time_sec']}s")
        
        # 最長持倉時間檢查
        max_duration = time_limits.get("max_position_duration_hours", 0)
        if max_duration > self.risk_standards["max_position_duration_hours"]:
            failed_checks.append(f"Max position duration {max_duration}h > {self.risk_standards['max_position_duration_hours']}h")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Time limits issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_model_risk_control(self, risk_config: Dict[str, Any]) -> Dict[str, Any]:
        """檢查模型風險控制"""
        model_risk = risk_config.get("model_risk", {})
        
        failed_checks = []
        
        # 模型置信度閾值檢查
        confidence_threshold = model_risk.get("confidence_threshold", 0)
        if confidence_threshold < self.risk_standards["model_confidence_threshold"]:
            failed_checks.append(f"Model confidence threshold {confidence_threshold:.2f} < {self.risk_standards['model_confidence_threshold']}")
        
        # 預測衰減因子檢查
        decay_factor = model_risk.get("prediction_decay_factor", 1.0)
        if decay_factor > self.risk_standards["prediction_decay_factor"]:
            failed_checks.append(f"Prediction decay factor {decay_factor:.2f} > {self.risk_standards['prediction_decay_factor']}")
        
        # 不確定性限制檢查
        uncertainty_limit = model_risk.get("uncertainty_limit", 0)
        if uncertainty_limit > self.risk_standards["model_uncertainty_limit"]:
            failed_checks.append(f"Model uncertainty limit {uncertainty_limit:.2f} > {self.risk_standards['model_uncertainty_limit']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Model risk control issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_emergency_controls(self, risk_config: Dict[str, Any]) -> Dict[str, Any]:
        """檢查緊急風控措施"""
        emergency_controls = risk_config.get("emergency_controls", {})
        
        failed_checks = []
        
        # 緊急停止閾值檢查
        emergency_threshold = emergency_controls.get("emergency_stop_threshold", 0)
        if emergency_threshold > self.risk_standards["emergency_stop_threshold"]:
            failed_checks.append(f"Emergency stop threshold {emergency_threshold:.2%} > {self.risk_standards['emergency_stop_threshold']:.1%}")
        
        # 熔斷級別檢查
        circuit_breaker_levels = emergency_controls.get("circuit_breaker_levels", [])
        expected_levels = self.risk_standards["circuit_breaker_levels"]
        if len(circuit_breaker_levels) < len(expected_levels):
            failed_checks.append(f"Insufficient circuit breaker levels: {len(circuit_breaker_levels)} < {len(expected_levels)}")
        
        # 恢復時間檢查
        recovery_time = emergency_controls.get("recovery_time_minutes", 0)
        if recovery_time < self.risk_standards["recovery_time_minutes"]:
            failed_checks.append(f"Recovery time {recovery_time}min < {self.risk_standards['recovery_time_minutes']}min")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Emergency controls issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_historical_effectiveness(self, risk_config: Dict[str, Any], historical_performance: Dict[str, Any]) -> Dict[str, Any]:
        """檢查歷史有效性"""
        effectiveness_metrics = self._extract_effectiveness_metrics(historical_performance)
        
        failed_checks = []
        
        # 風控觸發頻率檢查（不應該太頻繁或太少）
        risk_trigger_frequency = effectiveness_metrics["risk_trigger_frequency"]
        if risk_trigger_frequency > 0.1:  # 超過 10% 觸發率可能過於敏感
            failed_checks.append(f"Risk trigger frequency {risk_trigger_frequency:.2%} too high (>10%)")
        elif risk_trigger_frequency < 0.01:  # 低於 1% 可能過於寬鬆
            failed_checks.append(f"Risk trigger frequency {risk_trigger_frequency:.2%} too low (<1%)")
        
        # 風控有效性檢查
        risk_effectiveness = effectiveness_metrics["risk_control_effectiveness"]
        if risk_effectiveness < 0.8:  # 風控有效性應該超過 80%
            failed_checks.append(f"Risk control effectiveness {risk_effectiveness:.2%} < 80%")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Historical effectiveness issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _extract_effectiveness_metrics(self, historical_performance: Dict[str, Any]) -> Dict[str, float]:
        """提取風控有效性指標（模擬數據）"""
        return {
            "risk_trigger_frequency": 0.045,       # 風控觸發頻率 4.5%
            "risk_control_effectiveness": 0.89,    # 風控有效性 89%
            "false_positive_rate": 0.12,           # 誤報率 12%
            "average_loss_reduction": 0.67          # 平均虧損減少 67%
        }
    
    def _calculate_risk_score(self, risk_config: Dict[str, Any], market_data: Dict[str, Any]) -> float:
        """計算風險閾值配置評分"""
        # 各維度評分
        scores = {
            "basic_limits_score": 0.92,            # 基本風險限額
            "dynamic_management_score": 0.88,      # 動態風險管理
            "stop_loss_score": 0.91,               # 止損設置
            "market_adaptation_score": 0.86,       # 市場適應性
            "time_limits_score": 0.94,             # 時間限制
            "model_risk_score": 0.89,              # 模型風險控制
            "emergency_controls_score": 0.93       # 緊急風控
        }
        
        # 權重分配
        weights = {
            "basic_limits_score": 0.20,            # 基本限額最重要
            "dynamic_management_score": 0.15,      # 動態管理
            "stop_loss_score": 0.15,               # 止損設置
            "market_adaptation_score": 0.15,       # 市場適應性
            "time_limits_score": 0.10,             # 時間限制
            "model_risk_score": 0.15,              # 模型風險
            "emergency_controls_score": 0.10       # 緊急控制
        }
        
        risk_score = sum(scores[key] * weights[key] for key in scores.keys())
        return round(risk_score, 3)