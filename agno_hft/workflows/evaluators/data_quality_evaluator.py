"""
數據質量評估器
==============

評估數據質量是否滿足訓練要求：
- 完整性檢查
- 準確性驗證
- 時效性評估
- 一致性檢查
"""

from agno import Evaluator
from typing import Dict, Any


class DataQualityEvaluator(Evaluator):
    """
    數據質量評估器
    
    確保數據質量滿足機器學習訓練的最低標準
    """
    
    def __init__(self):
        super().__init__(
            name="DataQualityEvaluator",
            description="評估數據質量是否滿足訓練要求"
        )
        
        # 數據質量閾值
        self.quality_thresholds = {
            "completeness": 0.95,      # 完整性 >= 95%
            "accuracy": 0.90,          # 準確性 >= 90%  
            "consistency": 0.88,       # 一致性 >= 88%
            "timeliness": 0.75,        # 時效性 >= 75%
            "overall_score": 0.85      # 總體評分 >= 85%
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估數據質量
        
        Args:
            context: 評估上下文，包含數據質量報告
            
        Returns:
            評估結果
        """
        try:
            data_quality = context.get("data_quality", {})
            quality_report = data_quality.get("quality_report", {})
            
            if not quality_report:
                return {
                    "passed": False,
                    "reason": "No data quality report available"
                }
            
            # 檢查總體評分
            overall_score = quality_report.get("overall_score", 0)
            if overall_score < self.quality_thresholds["overall_score"]:
                return {
                    "passed": False,
                    "reason": f"Overall quality score {overall_score:.3f} below threshold {self.quality_thresholds['overall_score']}"
                }
            
            # 檢查詳細指標
            detailed_results = quality_report.get("detailed_results", {})
            failed_checks = []
            
            for metric, threshold in self.quality_thresholds.items():
                if metric == "overall_score":
                    continue
                    
                if metric in detailed_results:
                    score = detailed_results[metric].get("score", 0)
                    if score < threshold:
                        failed_checks.append(f"{metric}: {score:.3f} < {threshold}")
            
            # 檢查關鍵問題
            critical_issues = self._check_critical_issues(quality_report)
            
            if failed_checks or critical_issues:
                reasons = failed_checks + critical_issues
                return {
                    "passed": False,
                    "reason": f"Quality checks failed: {'; '.join(reasons)}"
                }
            
            return {
                "passed": True,
                "reason": "Data quality meets all requirements",
                "quality_score": overall_score
            }
            
        except Exception as e:
            return {
                "passed": False,
                "reason": f"Data quality evaluation failed: {str(e)}"
            }
    
    def _check_critical_issues(self, quality_report: Dict[str, Any]) -> list:
        """檢查關鍵數據質量問題"""
        critical_issues = []
        
        # 檢查是否有嚴重的數據問題
        issues = quality_report.get("issues", [])
        for issue in issues:
            if "delay exceeds" in issue.lower():
                critical_issues.append("Data delay issue detected")
            elif "missing" in issue.lower() and "critical" in issue.lower():
                critical_issues.append("Critical data missing")
            elif "corruption" in issue.lower():
                critical_issues.append("Data corruption detected")
        
        # 檢查數據完整性
        checks_passed = quality_report.get("checks_passed", 0)
        checks_total = quality_report.get("checks_total", 1)
        if checks_total > 0:
            pass_rate = checks_passed / checks_total
            if pass_rate < 0.8:  # 80% 的檢查必須通過
                critical_issues.append(f"Only {pass_rate:.1%} of quality checks passed")
        
        return critical_issues