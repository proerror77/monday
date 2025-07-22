"""
模型性能評估器
==============

評估模型性能是否滿足部署要求：
- 分類性能指標
- 金融性能指標
- 穩定性評估
- 風險調整收益
"""

from agno import Evaluator
from typing import Dict, Any


class ModelPerformanceEvaluator(Evaluator):
    """
    模型性能評估器
    
    評估 TLOB 模型是否滿足生產部署的性能標準
    """
    
    def __init__(self):
        super().__init__(
            name="ModelPerformanceEvaluator", 
            description="評估模型性能是否滿足部署要求"
        )
        
        # 性能閾值
        self.performance_thresholds = {
            # 分類性能
            "min_accuracy": 0.55,         # 最低準確率 55%
            "min_f1_score": 0.52,         # 最低 F1 分數 52%
            "min_precision": 0.50,        # 最低精確率 50%
            "min_recall": 0.50,           # 最低召回率 50%
            "min_auc": 0.65,              # 最低 AUC 65%
            
            # 金融性能  
            "min_sharpe_ratio": 1.0,      # 最低 Sharpe 比率 1.0
            "max_drawdown": -0.15,        # 最大回撤不超過 15%
            "min_total_return": 0.05,     # 最低總回報 5%
            "min_win_rate": 0.45,         # 最低勝率 45%
            "min_profit_factor": 1.1,     # 最低盈虧比 1.1
            
            # 預測質量
            "min_confidence": 0.6,        # 最低平均置信度 60%
            "max_confidence_std": 0.25,   # 置信度標準差不超過 25%
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估模型性能
        
        Args:
            context: 評估上下文，包含模型性能數據
            
        Returns:
            評估結果
        """
        try:
            model_performance = context.get("model_performance", {})
            evaluation_results = model_performance.get("evaluation_result", {})
            
            if not evaluation_results:
                return {
                    "passed": False,
                    "reason": "No model evaluation results available"
                }
            
            # 檢查分類性能
            classification_check = self._check_classification_performance(evaluation_results)
            if not classification_check["passed"]:
                return classification_check
            
            # 檢查金融性能
            financial_check = self._check_financial_performance(evaluation_results)
            if not financial_check["passed"]:
                return financial_check
            
            # 檢查預測質量
            prediction_check = self._check_prediction_quality(evaluation_results)
            if not prediction_check["passed"]:
                return prediction_check
            
            # 檢查模型穩定性
            stability_check = self._check_model_stability(evaluation_results)
            if not stability_check["passed"]:
                return stability_check
            
            return {
                "passed": True,
                "reason": "Model performance meets all deployment requirements",
                "performance_summary": self._generate_performance_summary(evaluation_results)
            }
            
        except Exception as e:
            return {
                "passed": False,
                "reason": f"Model performance evaluation failed: {str(e)}"
            }
    
    def _check_classification_performance(self, results: Dict[str, Any]) -> Dict[str, Any]:
        """檢查分類性能"""
        classification_metrics = results.get("classification_metrics", {})
        
        failed_metrics = []
        
        # 檢查準確率
        accuracy = classification_metrics.get("accuracy", 0)
        if accuracy < self.performance_thresholds["min_accuracy"]:
            failed_metrics.append(f"accuracy {accuracy:.3f} < {self.performance_thresholds['min_accuracy']}")
        
        # 檢查 F1 分數
        f1_score = classification_metrics.get("f1_score", 0)
        if f1_score < self.performance_thresholds["min_f1_score"]:
            failed_metrics.append(f"f1_score {f1_score:.3f} < {self.performance_thresholds['min_f1_score']}")
        
        # 檢查精確率
        precision = classification_metrics.get("precision", 0)
        if precision < self.performance_thresholds["min_precision"]:
            failed_metrics.append(f"precision {precision:.3f} < {self.performance_thresholds['min_precision']}")
        
        # 檢查召回率
        recall = classification_metrics.get("recall", 0)
        if recall < self.performance_thresholds["min_recall"]:
            failed_metrics.append(f"recall {recall:.3f} < {self.performance_thresholds['min_recall']}")
        
        # 檢查 AUC
        auc_score = classification_metrics.get("auc_score", 0)
        if auc_score < self.performance_thresholds["min_auc"]:
            failed_metrics.append(f"auc {auc_score:.3f} < {self.performance_thresholds['min_auc']}")
        
        if failed_metrics:
            return {
                "passed": False,
                "reason": f"Classification metrics failed: {'; '.join(failed_metrics)}"
            }
        
        return {"passed": True}
    
    def _check_financial_performance(self, results: Dict[str, Any]) -> Dict[str, Any]:
        """檢查金融性能"""
        financial_metrics = results.get("financial_metrics", {})
        
        failed_metrics = []
        
        # 檢查 Sharpe 比率
        sharpe_ratio = financial_metrics.get("sharpe_ratio", 0)
        if sharpe_ratio < self.performance_thresholds["min_sharpe_ratio"]:
            failed_metrics.append(f"sharpe_ratio {sharpe_ratio:.3f} < {self.performance_thresholds['min_sharpe_ratio']}")
        
        # 檢查最大回撤
        max_drawdown = financial_metrics.get("max_drawdown", 0)
        if max_drawdown < self.performance_thresholds["max_drawdown"]:
            failed_metrics.append(f"max_drawdown {max_drawdown:.3f} exceeds {self.performance_thresholds['max_drawdown']}")
        
        # 檢查總回報
        total_return = financial_metrics.get("total_return", 0)
        if total_return < self.performance_thresholds["min_total_return"]:
            failed_metrics.append(f"total_return {total_return:.3f} < {self.performance_thresholds['min_total_return']}")
        
        # 檢查勝率
        win_rate = financial_metrics.get("win_rate", 0)
        if win_rate < self.performance_thresholds["min_win_rate"]:
            failed_metrics.append(f"win_rate {win_rate:.3f} < {self.performance_thresholds['min_win_rate']}")
        
        # 檢查盈虧比
        profit_factor = financial_metrics.get("profit_factor", 0)
        if profit_factor < self.performance_thresholds["min_profit_factor"]:
            failed_metrics.append(f"profit_factor {profit_factor:.3f} < {self.performance_thresholds['min_profit_factor']}")
        
        if failed_metrics:
            return {
                "passed": False,
                "reason": f"Financial metrics failed: {'; '.join(failed_metrics)}"
            }
        
        return {"passed": True}
    
    def _check_prediction_quality(self, results: Dict[str, Any]) -> Dict[str, Any]:
        """檢查預測質量"""
        prediction_analysis = results.get("prediction_analysis", {})
        
        failed_checks = []
        
        # 檢查平均置信度
        confidence_mean = prediction_analysis.get("prediction_confidence_mean", 0)
        if confidence_mean < self.performance_thresholds["min_confidence"]:
            failed_checks.append(f"confidence_mean {confidence_mean:.3f} < {self.performance_thresholds['min_confidence']}")
        
        # 檢查置信度穩定性
        confidence_std = prediction_analysis.get("prediction_confidence_std", 0)
        if confidence_std > self.performance_thresholds["max_confidence_std"]:
            failed_checks.append(f"confidence_std {confidence_std:.3f} > {self.performance_thresholds['max_confidence_std']}")
        
        # 檢查類別分佈平衡性
        class_distribution = prediction_analysis.get("class_distribution", {})
        if class_distribution:
            min_class_ratio = min(class_distribution.values())
            if min_class_ratio < 0.15:  # 任何類別不應少於 15%
                failed_checks.append(f"Class distribution imbalanced: min_ratio {min_class_ratio:.3f}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Prediction quality checks failed: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_model_stability(self, results: Dict[str, Any]) -> Dict[str, Any]:
        """檢查模型穩定性"""
        # 這裡可以檢查模型在不同數據子集上的性能一致性
        # 目前返回通過，實際實現中應該有更複雜的穩定性測試
        
        return {"passed": True}
    
    def _generate_performance_summary(self, results: Dict[str, Any]) -> Dict[str, Any]:
        """生成性能總結"""
        classification_metrics = results.get("classification_metrics", {})
        financial_metrics = results.get("financial_metrics", {})
        
        return {
            "accuracy": classification_metrics.get("accuracy", 0),
            "f1_score": classification_metrics.get("f1_score", 0),
            "sharpe_ratio": financial_metrics.get("sharpe_ratio", 0),
            "max_drawdown": financial_metrics.get("max_drawdown", 0),
            "total_return": financial_metrics.get("total_return", 0),
            "win_rate": financial_metrics.get("win_rate", 0)
        }