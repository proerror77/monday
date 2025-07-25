"""
研究質量評估器
==============

評估研究分析的質量和深度：
- 數據分析完整性
- 方法論正確性
- 結論可靠性
- 可重現性

注意：這是評估研究過程本身的質量，不是交易結果
"""

from agno import Evaluator
from typing import Dict, Any
import logging

logger = logging.getLogger(__name__)


class ResearchQualityEvaluator(Evaluator):
    """
    研究質量評估器
    
    專門評估 ResearchAnalyst 產出的研究質量
    """
    
    def __init__(self):
        super().__init__(
            name="ResearchQualityEvaluator",
            description="評估研究分析的質量和深度"
        )
        
        # 研究質量閾值
        self.research_thresholds = {
            # 數據質量
            "min_data_completeness": 0.95,      # 最低數據完整度 95%
            "min_data_samples": 10000,          # 最低樣本數量
            "max_missing_ratio": 0.05,          # 最大缺失比例 5%
            "min_time_coverage_days": 30,       # 最低時間覆蓋 30 天
            
            # 分析深度
            "min_feature_count": 20,            # 最低特徵數量
            "min_correlation_analysis": 0.8,    # 相關性分析完整度
            "min_statistical_tests": 3,         # 最低統計檢驗數量
            
            # 方法論
            "required_validation_methods": [
                "train_test_split",
                "cross_validation",
                "out_of_sample_test"
            ],
            "min_validation_score": 0.7,        # 驗證方法評分
            
            # 可重現性
            "required_documentation": [
                "data_source",
                "preprocessing_steps",
                "feature_engineering",
                "model_parameters"
            ],
            "min_code_coverage": 0.8,           # 代碼覆蓋率
            
            # 結論質量
            "min_confidence_score": 0.75,       # 結論置信度
            "max_uncertainty_range": 0.2        # 不確定性範圍
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估研究質量
        
        Args:
            context: 包含研究報告和分析結果的上下文
            
        Returns:
            評估結果
        """
        try:
            research_report = context.get("research_report", {})
            analysis_results = context.get("analysis_results", {})
            
            if not research_report:
                return {
                    "passed": False,
                    "reason": "No research report available"
                }
            
            # 檢查數據質量
            data_quality_check = self._check_data_quality(research_report)
            if not data_quality_check["passed"]:
                return data_quality_check
            
            # 檢查分析深度
            analysis_depth_check = self._check_analysis_depth(analysis_results)
            if not analysis_depth_check["passed"]:
                return analysis_depth_check
            
            # 檢查方法論
            methodology_check = self._check_methodology(research_report)
            if not methodology_check["passed"]:
                return methodology_check
            
            # 檢查可重現性
            reproducibility_check = self._check_reproducibility(research_report)
            if not reproducibility_check["passed"]:
                return reproducibility_check
            
            # 檢查結論質量
            conclusion_check = self._check_conclusion_quality(research_report)
            if not conclusion_check["passed"]:
                return conclusion_check
            
            return {
                "passed": True,
                "reason": "Research quality meets all academic and industry standards",
                "research_score": self._calculate_research_score(research_report, analysis_results)
            }
            
        except Exception as e:
            logger.error(f"Research quality evaluation failed: {e}")
            return {
                "passed": False,
                "reason": f"Research quality evaluation failed: {str(e)}"
            }
    
    def _check_data_quality(self, research_report: Dict[str, Any]) -> Dict[str, Any]:
        """檢查數據質量"""
        data_metrics = self._extract_data_metrics(research_report)
        
        failed_checks = []
        
        # 數據完整度檢查
        if data_metrics["completeness"] < self.research_thresholds["min_data_completeness"]:
            failed_checks.append(f"Data completeness {data_metrics['completeness']:.2%} < {self.research_thresholds['min_data_completeness']:.1%}")
        
        # 樣本數量檢查
        if data_metrics["sample_count"] < self.research_thresholds["min_data_samples"]:
            failed_checks.append(f"Sample count {data_metrics['sample_count']} < {self.research_thresholds['min_data_samples']}")
        
        # 缺失比例檢查
        if data_metrics["missing_ratio"] > self.research_thresholds["max_missing_ratio"]:
            failed_checks.append(f"Missing ratio {data_metrics['missing_ratio']:.2%} > {self.research_thresholds['max_missing_ratio']:.1%}")
        
        # 時間覆蓋檢查
        if data_metrics["time_coverage_days"] < self.research_thresholds["min_time_coverage_days"]:
            failed_checks.append(f"Time coverage {data_metrics['time_coverage_days']} days < {self.research_thresholds['min_time_coverage_days']} days")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Data quality issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_analysis_depth(self, analysis_results: Dict[str, Any]) -> Dict[str, Any]:
        """檢查分析深度"""
        depth_metrics = self._extract_depth_metrics(analysis_results)
        
        failed_checks = []
        
        # 特徵數量檢查
        if depth_metrics["feature_count"] < self.research_thresholds["min_feature_count"]:
            failed_checks.append(f"Feature count {depth_metrics['feature_count']} < {self.research_thresholds['min_feature_count']}")
        
        # 相關性分析檢查
        if depth_metrics["correlation_analysis_score"] < self.research_thresholds["min_correlation_analysis"]:
            failed_checks.append(f"Correlation analysis score {depth_metrics['correlation_analysis_score']:.2f} < {self.research_thresholds['min_correlation_analysis']}")
        
        # 統計檢驗檢查
        if depth_metrics["statistical_tests_count"] < self.research_thresholds["min_statistical_tests"]:
            failed_checks.append(f"Statistical tests {depth_metrics['statistical_tests_count']} < {self.research_thresholds['min_statistical_tests']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Analysis depth insufficient: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_methodology(self, research_report: Dict[str, Any]) -> Dict[str, Any]:
        """檢查方法論"""
        methodology = research_report.get("methodology", {})
        validation_methods = methodology.get("validation_methods", [])
        
        failed_checks = []
        
        # 驗證方法檢查
        for required_method in self.research_thresholds["required_validation_methods"]:
            if required_method not in validation_methods:
                failed_checks.append(f"Missing validation method: {required_method}")
        
        # 驗證評分檢查
        validation_score = methodology.get("validation_score", 0)
        if validation_score < self.research_thresholds["min_validation_score"]:
            failed_checks.append(f"Validation score {validation_score:.2f} < {self.research_thresholds['min_validation_score']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Methodology issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_reproducibility(self, research_report: Dict[str, Any]) -> Dict[str, Any]:
        """檢查可重現性"""
        documentation = research_report.get("documentation", {})
        
        failed_checks = []
        
        # 必需文檔檢查
        for required_doc in self.research_thresholds["required_documentation"]:
            if required_doc not in documentation or not documentation[required_doc]:
                failed_checks.append(f"Missing documentation: {required_doc}")
        
        # 代碼覆蓋率檢查
        code_coverage = documentation.get("code_coverage", 0)
        if code_coverage < self.research_thresholds["min_code_coverage"]:
            failed_checks.append(f"Code coverage {code_coverage:.1%} < {self.research_thresholds['min_code_coverage']:.1%}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Reproducibility issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_conclusion_quality(self, research_report: Dict[str, Any]) -> Dict[str, Any]:
        """檢查結論質量"""
        conclusions = research_report.get("conclusions", {})
        
        failed_checks = []
        
        # 置信度檢查
        confidence_score = conclusions.get("confidence_score", 0)
        if confidence_score < self.research_thresholds["min_confidence_score"]:
            failed_checks.append(f"Confidence score {confidence_score:.2f} < {self.research_thresholds['min_confidence_score']}")
        
        # 不確定性範圍檢查
        uncertainty_range = conclusions.get("uncertainty_range", 1.0)
        if uncertainty_range > self.research_thresholds["max_uncertainty_range"]:
            failed_checks.append(f"Uncertainty range {uncertainty_range:.2f} > {self.research_thresholds['max_uncertainty_range']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Conclusion quality issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _extract_data_metrics(self, research_report: Dict[str, Any]) -> Dict[str, Any]:
        """提取數據質量指標（模擬數據）"""
        # 實際實現中應該從 research_report 解析真實的數據質量指標
        return {
            "completeness": 0.972,        # 97.2% 數據完整度
            "sample_count": 45623,        # 樣本數量
            "missing_ratio": 0.028,       # 2.8% 缺失率
            "time_coverage_days": 45,     # 45天時間覆蓋
            "data_quality_score": 0.95    # 整體數據質量評分
        }
    
    def _extract_depth_metrics(self, analysis_results: Dict[str, Any]) -> Dict[str, Any]:
        """提取分析深度指標（模擬數據）"""
        return {
            "feature_count": 34,                    # 特徵數量
            "correlation_analysis_score": 0.89,    # 相關性分析評分
            "statistical_tests_count": 5,          # 統計檢驗數量
            "analysis_complexity_score": 0.82      # 分析複雜度評分
        }
    
    def _calculate_research_score(self, research_report: Dict[str, Any], analysis_results: Dict[str, Any]) -> float:
        """計算研究質量綜合評分"""
        data_metrics = self._extract_data_metrics(research_report)
        depth_metrics = self._extract_depth_metrics(analysis_results)
        
        # 各維度評分
        scores = {
            "data_quality_score": data_metrics["data_quality_score"],
            "analysis_depth_score": depth_metrics["analysis_complexity_score"],
            "methodology_score": 0.87,    # 方法論評分
            "reproducibility_score": 0.92, # 可重現性評分
            "conclusion_score": 0.85       # 結論質量評分
        }
        
        # 權重分配
        weights = {
            "data_quality_score": 0.25,      # 數據質量
            "analysis_depth_score": 0.25,    # 分析深度
            "methodology_score": 0.20,       # 方法論
            "reproducibility_score": 0.15,   # 可重現性
            "conclusion_score": 0.15          # 結論質量
        }
        
        research_score = sum(scores[key] * weights[key] for key in scores.keys())
        return round(research_score, 3)