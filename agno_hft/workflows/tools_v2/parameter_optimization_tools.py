"""
參數優化工具集
==============

提供智能參數調整的具體實現：
- 基於績效反饋的參數優化
- 自適應學習算法
- A/B 測試參數對比
- 參數敏感性分析
"""

from agno.tools import Tool
from typing import Dict, Any, Optional, List, Tuple
import numpy as np
import logging
from datetime import datetime
import json

logger = logging.getLogger(__name__)


class ParameterOptimizationTools(Tool):
    """
    參數優化工具集
    
    提供智能化的參數調整策略
    """
    
    def __init__(self):
        super().__init__()
        # 參數優化歷史記錄
        self.optimization_history = {}
        # 參數範圍定義
        self.parameter_bounds = self._define_parameter_bounds()
        
    def _define_parameter_bounds(self) -> Dict[str, Dict[str, float]]:
        """定義各類參數的合理範圍"""
        return {
            "risk_parameters": {
                "stop_loss_pct": {"min": 0.005, "max": 0.05, "default": 0.02},
                "take_profit_pct": {"min": 0.01, "max": 0.1, "default": 0.03},
                "max_position_size": {"min": 100, "max": 10000, "default": 1000},
                "var_limit": {"min": -5000, "max": -500, "default": -2000}
            },
            "trading_parameters": {
                "signal_threshold": {"min": 0.6, "max": 0.95, "default": 0.75},
                "min_confidence": {"min": 0.5, "max": 0.9, "default": 0.7},
                "position_hold_time_sec": {"min": 10, "max": 3600, "default": 300},
                "slippage_tolerance_bps": {"min": 1, "max": 20, "default": 5}
            },
            "model_parameters": {
                "prediction_decay": {"min": 0.1, "max": 0.9, "default": 0.5},
                "ensemble_weight": {"min": 0.1, "max": 1.0, "default": 0.6},
                "feature_importance_threshold": {"min": 0.01, "max": 0.1, "default": 0.05}
            }
        }
    
    async def optimize_parameters_bayesian(
        self,
        symbol: str,
        current_performance: Dict[str, float],
        parameter_category: str,
        optimization_target: str = "sharpe_ratio"
    ) -> Dict[str, Any]:
        """
        使用貝葉斯優化調整參數
        
        Args:
            symbol: 交易對
            current_performance: 當前績效指標
            parameter_category: 參數類別
            optimization_target: 優化目標
            
        Returns:
            優化結果和建議參數
        """
        try:
            # 獲取當前參數邊界
            bounds = self.parameter_bounds.get(parameter_category, {})
            if not bounds:
                return {"success": False, "error": f"Unknown parameter category: {parameter_category}"}
            
            # 分析當前績效
            current_score = current_performance.get(optimization_target, 0)
            
            # 貝葉斯優化邏輯（簡化版）
            optimized_params = {}
            improvement_rationale = []
            
            for param_name, param_bounds in bounds.items():
                current_value = current_performance.get(f"current_{param_name}", param_bounds["default"])
                
                # 基於績效決定調整方向
                if current_score < 1.0:  # 表現不佳
                    if "risk" in param_name.lower():
                        # 風險參數：表現不佳時收緊風險
                        new_value = self._adjust_towards_conservative(current_value, param_bounds)
                    else:
                        # 交易參數：表現不佳時提高門檻
                        new_value = self._adjust_towards_selective(current_value, param_bounds)
                else:  # 表現良好
                    # 適度積極調整
                    new_value = self._adjust_towards_aggressive(current_value, param_bounds)
                
                optimized_params[param_name] = new_value
                improvement_rationale.append(f"{param_name}: {current_value:.4f} -> {new_value:.4f}")
            
            # 預估改進效果
            expected_improvement = self._estimate_improvement(
                current_performance, optimized_params, parameter_category
            )
            
            return {
                "success": True,
                "symbol": symbol,
                "parameter_category": parameter_category,
                "optimized_parameters": optimized_params,
                "current_score": current_score,
                "expected_improvement": expected_improvement,
                "rationale": improvement_rationale,
                "confidence_score": 0.75,  # 優化信心度
                "implementation_priority": "high" if expected_improvement > 0.1 else "medium"
            }
            
        except Exception as e:
            logger.error(f"Bayesian optimization failed: {e}")
            return {"success": False, "error": str(e)}
    
    async def analyze_parameter_sensitivity(
        self,
        symbol: str,
        performance_history: List[Dict[str, Any]],
        parameter_name: str
    ) -> Dict[str, Any]:
        """
        分析參數敏感性
        
        Args:
            symbol: 交易對
            performance_history: 歷史績效數據
            parameter_name: 要分析的參數名
            
        Returns:
            敏感性分析結果
        """
        try:
            if len(performance_history) < 5:
                return {"success": False, "error": "Insufficient historical data"}
            
            # 提取參數值和對應績效
            param_values = []
            performance_scores = []
            
            for record in performance_history:
                if parameter_name in record and "sharpe_ratio" in record:
                    param_values.append(record[parameter_name])
                    performance_scores.append(record["sharpe_ratio"])
            
            if len(param_values) < 3:
                return {"success": False, "error": f"Insufficient data for parameter {parameter_name}"}
            
            # 計算相關性
            correlation = np.corrcoef(param_values, performance_scores)[0, 1]
            
            # 找最優區間
            sorted_data = sorted(zip(param_values, performance_scores), key=lambda x: x[1], reverse=True)
            top_performers = sorted_data[:len(sorted_data)//3]  # 前1/3表現最好的
            
            optimal_range = {
                "min": min(p[0] for p in top_performers),
                "max": max(p[0] for p in top_performers),
                "avg": sum(p[0] for p in top_performers) / len(top_performers)
            }
            
            # 敏感性等級
            sensitivity_level = "high" if abs(correlation) > 0.7 else "medium" if abs(correlation) > 0.4 else "low"
            
            return {
                "success": True,
                "symbol": symbol,
                "parameter_name": parameter_name,
                "correlation": correlation,
                "sensitivity_level": sensitivity_level,
                "optimal_range": optimal_range,
                "current_recommendation": optimal_range["avg"],
                "confidence": min(len(param_values) / 20, 1.0),  # 基於樣本數量的信心度
                "adjustment_suggestion": self._generate_adjustment_suggestion(correlation, optimal_range)
            }
            
        except Exception as e:
            logger.error(f"Parameter sensitivity analysis failed: {e}")
            return {"success": False, "error": str(e)}
    
    async def implement_parameter_change(
        self,
        symbol: str,
        parameter_updates: Dict[str, float],
        change_strategy: str = "gradual"
    ) -> Dict[str, Any]:
        """
        實施參數變更
        
        Args:
            symbol: 交易對
            parameter_updates: 要更新的參數
            change_strategy: 變更策略 (gradual, immediate, ab_test)
            
        Returns:
            實施結果
        """
        try:
            implementation_plan = {
                "symbol": symbol,
                "change_strategy": change_strategy,
                "parameter_updates": parameter_updates,
                "implementation_steps": [],
                "rollback_plan": {},
                "monitoring_metrics": []
            }
            
            if change_strategy == "gradual":
                # 漸進式調整：分3步調整到目標值
                for param_name, target_value in parameter_updates.items():
                    current_value = self._get_current_parameter_value(symbol, param_name)
                    
                    steps = [
                        current_value + (target_value - current_value) * 0.33,
                        current_value + (target_value - current_value) * 0.67,
                        target_value
                    ]
                    
                    implementation_plan["implementation_steps"].append({
                        "parameter": param_name,
                        "current_value": current_value,
                        "target_value": target_value,
                        "gradual_steps": steps,
                        "step_interval_minutes": 30
                    })
                    
                    implementation_plan["rollback_plan"][param_name] = current_value
            
            elif change_strategy == "immediate":
                # 立即調整
                for param_name, target_value in parameter_updates.items():
                    current_value = self._get_current_parameter_value(symbol, param_name)
                    
                    implementation_plan["implementation_steps"].append({
                        "parameter": param_name,
                        "current_value": current_value,
                        "target_value": target_value,
                        "change_type": "immediate"
                    })
                    
                    implementation_plan["rollback_plan"][param_name] = current_value
            
            elif change_strategy == "ab_test":
                # A/B 測試模式
                implementation_plan["ab_test_config"] = {
                    "test_duration_hours": 6,
                    "traffic_split": 0.5,
                    "success_criteria": {"min_improvement": 0.05},
                    "auto_promote": True
                }
            
            # 設置監控指標
            implementation_plan["monitoring_metrics"] = [
                "sharpe_ratio", "max_drawdown", "win_rate", 
                "avg_trade_duration", "slippage", "fill_rate"
            ]
            
            return {
                "success": True,
                "implementation_plan": implementation_plan,
                "estimated_implementation_time_minutes": self._estimate_implementation_time(change_strategy),
                "risk_assessment": self._assess_implementation_risk(parameter_updates),
                "next_steps": [
                    "Execute parameter changes via Rust tools",
                    "Monitor performance metrics", 
                    "Evaluate results after implementation",
                    "Decide on rollback if performance degrades"
                ]
            }
            
        except Exception as e:
            logger.error(f"Parameter implementation planning failed: {e}")
            return {"success": False, "error": str(e)}
    
    def _adjust_towards_conservative(self, current_value: float, bounds: Dict[str, float]) -> float:
        """向保守方向調整參數"""
        # 對風險參數，保守意味著更小的風險敞口
        conservative_target = bounds["min"] + (bounds["default"] - bounds["min"]) * 0.5
        adjustment = conservative_target - current_value
        return current_value + adjustment * 0.3  # 30% 調整幅度
    
    def _adjust_towards_selective(self, current_value: float, bounds: Dict[str, float]) -> float:
        """向選擇性方向調整參數"""
        # 對交易參數，選擇性意味著更高的門檻
        selective_target = bounds["default"] + (bounds["max"] - bounds["default"]) * 0.3
        adjustment = selective_target - current_value
        return current_value + adjustment * 0.2  # 20% 調整幅度
    
    def _adjust_towards_aggressive(self, current_value: float, bounds: Dict[str, float]) -> float:
        """向積極方向調整參數"""
        # 表現好時適度積極
        aggressive_target = bounds["default"] + (bounds["max"] - bounds["default"]) * 0.2
        adjustment = aggressive_target - current_value
        return current_value + adjustment * 0.15  # 15% 調整幅度
    
    def _estimate_improvement(
        self,
        current_performance: Dict[str, float],
        optimized_params: Dict[str, float],
        category: str
    ) -> float:
        """預估參數調整的改進效果"""
        # 簡化的改進預估邏輯
        base_improvement = 0.05  # 基礎改進預期
        
        # 根據當前表現調整預期
        current_sharpe = current_performance.get("sharpe_ratio", 1.0)
        if current_sharpe < 0.5:
            return base_improvement * 1.5  # 表現差時改進空間大
        elif current_sharpe > 1.5:
            return base_improvement * 0.5  # 表現好時改進空間小
        else:
            return base_improvement
    
    def _generate_adjustment_suggestion(
        self,
        correlation: float,
        optimal_range: Dict[str, float]
    ) -> str:
        """生成調整建議"""
        if abs(correlation) > 0.7:
            direction = "increase" if correlation > 0 else "decrease"
            return f"Strong correlation detected. Recommend to {direction} parameter towards {optimal_range['avg']:.4f}"
        elif abs(correlation) > 0.4:
            return f"Moderate correlation. Consider adjusting towards optimal range: {optimal_range['min']:.4f} - {optimal_range['max']:.4f}"
        else:
            return "Low correlation. Parameter adjustment may not significantly impact performance"
    
    def _get_current_parameter_value(self, symbol: str, param_name: str) -> float:
        """獲取當前參數值（模擬）"""
        # 實際實現中應該從 Rust 系統獲取當前參數
        bounds = self.parameter_bounds
        for category in bounds.values():
            if param_name in category:
                return category[param_name]["default"]
        return 0.0
    
    def _estimate_implementation_time(self, strategy: str) -> int:
        """預估實施時間"""
        time_map = {
            "immediate": 5,
            "gradual": 90, 
            "ab_test": 360
        }
        return time_map.get(strategy, 30)
    
    def _assess_implementation_risk(self, parameter_updates: Dict[str, float]) -> str:
        """評估實施風險"""
        if len(parameter_updates) > 3:
            return "HIGH - Multiple parameter changes simultaneously"
        elif any("risk" in param.lower() for param in parameter_updates.keys()):
            return "MEDIUM - Risk parameter adjustments"
        else:
            return "LOW - Minor parameter adjustments"
    
    def get_info(self) -> Dict[str, str]:
        """獲取工具信息"""
        return {
            "name": "ParameterOptimizationTools",
            "description": "智能參數優化和調整工具",
            "available_functions": [
                "optimize_parameters_bayesian",
                "analyze_parameter_sensitivity",
                "implement_parameter_change"
            ]
        }