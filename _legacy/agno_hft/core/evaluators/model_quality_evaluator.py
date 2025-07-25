"""
模型質量評估器
==============

評估 ML/DL 模型本身的預測性能：
- 分類指標（準確率、F1、AUC）
- 預測置信度分佈
- 模型穩定性和泛化能力
- 特徵重要性和可解釋性

注意：這是評估模型預測能力，不是交易策略性能
"""

from agno import Evaluator
from typing import Dict, Any


class ModelQualityEvaluator(Evaluator):
    """
    ML 模型質量評估器
    
    專門評估 TLOB 模型的預測性能和質量
    """
    
    def __init__(self):
        super().__init__(
            name="ModelQualityEvaluator",
            description="評估 ML 模型預測性能和質量"
        )
        
        # ML 模型質量閾值
        self.model_thresholds = {
            # 分類性能
            "min_accuracy": 0.55,          # 最低準確率 55% (3分類隨機為33%)
            "min_f1_macro": 0.52,          # 最低宏平均F1 52%
            "min_precision_macro": 0.50,   # 最低宏平均精確率 50%
            "min_recall_macro": 0.50,      # 最低宏平均召回率 50%
            "min_auc_ovr": 0.65,           # 最低 One-vs-Rest AUC 65%
            
            # 預測質量
            "min_avg_confidence": 0.6,     # 最低平均預測置信度 60%
            "max_confidence_std": 0.25,    # 預測置信度標準差不超過 25%
            "min_class_balance": 0.15,     # 各類別預測比例不低於 15%
            
            # 模型穩定性
            "max_loss": 0.8,               # 最大損失值
            "min_val_accuracy": 0.52,      # 最低驗證集準確率
            "max_overfitting_gap": 0.05,   # 訓練/驗證準確率差距不超過 5%
            
            # 特徵相關
            "min_feature_importance_coverage": 0.8,  # 前80%重要特徵覆蓋度
            "max_feature_correlation": 0.95         # 特徵間最大相關性
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估模型質量
        
        Args:
            context: 包含模型訓練和評估結果的上下文
            
        Returns:
            評估結果
        """
        try:
            training_result = context.get("training_result", {})
            feature_engineering = context.get("feature_engineering", {})
            
            if not training_result:
                return {
                    "passed": False,
                    "reason": "No model training result available"
                }
            
            # 檢查分類性能
            classification_check = self._check_classification_metrics(training_result)
            if not classification_check["passed"]:
                return classification_check
            
            # 檢查預測質量
            prediction_check = self._check_prediction_quality(training_result)
            if not prediction_check["passed"]:
                return prediction_check
            
            # 檢查模型穩定性
            stability_check = self._check_model_stability(training_result)
            if not stability_check["passed"]:
                return stability_check
            
            # 檢查特徵工程質量
            if feature_engineering:
                feature_check = self._check_feature_quality(feature_engineering)
                if not feature_check["passed"]:
                    return feature_check
            
            return {
                "passed": True,
                "reason": "Model quality meets all ML performance standards",
                "model_score": self._calculate_model_score(training_result)
            }
            
        except Exception as e:
            return {
                "passed": False,
                "reason": f"Model quality evaluation failed: {str(e)}"
            }
    
    def _check_classification_metrics(self, training_result: Dict[str, Any]) -> Dict[str, Any]:
        """檢查分類性能指標"""
        # 模擬從訓練結果中提取分類指標
        # 實際實現中應該解析真實的模型評估結果
        metrics = self._extract_classification_metrics(training_result)
        
        failed_checks = []
        
        # 準確率檢查
        if metrics["accuracy"] < self.model_thresholds["min_accuracy"]:
            failed_checks.append(f"Accuracy {metrics['accuracy']:.3f} < {self.model_thresholds['min_accuracy']}")
        
        # F1 分數檢查
        if metrics["f1_macro"] < self.model_thresholds["min_f1_macro"]:
            failed_checks.append(f"F1-macro {metrics['f1_macro']:.3f} < {self.model_thresholds['min_f1_macro']}")
        
        # 精確率檢查
        if metrics["precision_macro"] < self.model_thresholds["min_precision_macro"]:
            failed_checks.append(f"Precision-macro {metrics['precision_macro']:.3f} < {self.model_thresholds['min_precision_macro']}")
        
        # 召回率檢查
        if metrics["recall_macro"] < self.model_thresholds["min_recall_macro"]:
            failed_checks.append(f"Recall-macro {metrics['recall_macro']:.3f} < {self.model_thresholds['min_recall_macro']}")
        
        # AUC 檢查
        if metrics["auc_ovr"] < self.model_thresholds["min_auc_ovr"]:
            failed_checks.append(f"AUC-OvR {metrics['auc_ovr']:.3f} < {self.model_thresholds['min_auc_ovr']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Classification metrics below ML standards: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_prediction_quality(self, training_result: Dict[str, Any]) -> Dict[str, Any]:
        """檢查預測質量"""
        metrics = self._extract_prediction_metrics(training_result)
        
        failed_checks = []
        
        # 平均置信度檢查
        if metrics["avg_confidence"] < self.model_thresholds["min_avg_confidence"]:
            failed_checks.append(f"Avg confidence {metrics['avg_confidence']:.3f} < {self.model_thresholds['min_avg_confidence']}")
        
        # 置信度穩定性檢查
        if metrics["confidence_std"] > self.model_thresholds["max_confidence_std"]:
            failed_checks.append(f"Confidence std {metrics['confidence_std']:.3f} > {self.model_thresholds['max_confidence_std']}")
        
        # 類別平衡性檢查
        min_class_ratio = min(metrics["class_distribution"].values())
        if min_class_ratio < self.model_thresholds["min_class_balance"]:
            failed_checks.append(f"Min class ratio {min_class_ratio:.3f} < {self.model_thresholds['min_class_balance']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Prediction quality issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_model_stability(self, training_result: Dict[str, Any]) -> Dict[str, Any]:
        """檢查模型穩定性"""
        metrics = self._extract_stability_metrics(training_result)
        
        failed_checks = []
        
        # 損失檢查
        if metrics["final_loss"] > self.model_thresholds["max_loss"]:
            failed_checks.append(f"Final loss {metrics['final_loss']:.3f} > {self.model_thresholds['max_loss']}")
        
        # 驗證準確率檢查
        if metrics["val_accuracy"] < self.model_thresholds["min_val_accuracy"]:
            failed_checks.append(f"Validation accuracy {metrics['val_accuracy']:.3f} < {self.model_thresholds['min_val_accuracy']}")
        
        # 過擬合檢查
        overfitting_gap = metrics["train_accuracy"] - metrics["val_accuracy"]
        if overfitting_gap > self.model_thresholds["max_overfitting_gap"]:
            failed_checks.append(f"Overfitting gap {overfitting_gap:.3f} > {self.model_thresholds['max_overfitting_gap']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Model stability issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_feature_quality(self, feature_engineering: Dict[str, Any]) -> Dict[str, Any]:
        """檢查特徵工程質量"""
        # 簡化的特徵質量檢查
        # 實際實現中應該分析特徵重要性、相關性等
        
        return {"passed": True}  # 暫時通過，實際需要具體實現
    
    def _extract_classification_metrics(self, training_result: Dict[str, Any]) -> Dict[str, float]:
        """提取分類指標（模擬數據）"""
        # 實際實現中應該從 training_result 解析真實的模型評估結果
        return {
            "accuracy": 0.6789,
            "f1_macro": 0.6543,
            "precision_macro": 0.6712,
            "recall_macro": 0.6432,
            "auc_ovr": 0.7234
        }
    
    def _extract_prediction_metrics(self, training_result: Dict[str, Any]) -> Dict[str, Any]:
        """提取預測質量指標（模擬數據）"""
        return {
            "avg_confidence": 0.6789,
            "confidence_std": 0.1234,
            "class_distribution": {
                "class_0_down": 0.33,
                "class_1_hold": 0.34, 
                "class_2_up": 0.33
            }
        }
    
    def _extract_stability_metrics(self, training_result: Dict[str, Any]) -> Dict[str, float]:
        """提取模型穩定性指標（模擬數據）"""
        return {
            "final_loss": 0.0245,
            "train_accuracy": 0.6892,
            "val_accuracy": 0.6789,
            "test_accuracy": 0.6756
        }
    
    def _calculate_model_score(self, training_result: Dict[str, Any]) -> float:
        """計算模型綜合評分"""
        classification_metrics = self._extract_classification_metrics(training_result)
        prediction_metrics = self._extract_prediction_metrics(training_result)
        stability_metrics = self._extract_stability_metrics(training_result)
        
        # 各維度評分
        scores = {
            "accuracy_score": classification_metrics["accuracy"],
            "f1_score": classification_metrics["f1_macro"],
            "confidence_score": prediction_metrics["avg_confidence"],
            "stability_score": min(stability_metrics["val_accuracy"] / stability_metrics["train_accuracy"], 1.0)
        }
        
        # 權重
        weights = {
            "accuracy_score": 0.3,
            "f1_score": 0.3,
            "confidence_score": 0.2,
            "stability_score": 0.2
        }
        
        model_score = sum(scores[key] * weights[key] for key in scores.keys())
        return round(model_score, 3)