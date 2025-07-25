"""
部署就緒度評估器
==============

評估模型和策略的部署準備狀況：
- 模型穩定性和性能
- 基礎設施準備度
- 監控和告警配置
- 部署流程完整性

注意：這是評估部署前的準備狀況，確保安全上線
"""

from agno import Evaluator
from typing import Dict, Any, List
import logging

logger = logging.getLogger(__name__)


class DeploymentReadinessEvaluator(Evaluator):
    """
    部署就緒度評估器
    
    評估系統是否準備好進行生產環境部署
    """
    
    def __init__(self):
        super().__init__(
            name="DeploymentReadinessEvaluator",
            description="評估模型和策略的部署準備狀況"
        )
        
        # 部署就緒度閾值
        self.readiness_thresholds = {
            # 模型穩定性
            "min_model_accuracy": 0.65,           # 最低模型準確率
            "min_model_consistency": 0.8,         # 模型一致性評分
            "max_model_variance": 0.05,           # 模型方差上限
            "min_validation_periods": 5,          # 最少驗證期間
            
            # 系統性能
            "max_inference_latency_ms": 10,       # 最大推理延遲 10ms
            "min_system_availability": 0.9999,    # 系統可用性 99.99%
            "max_memory_usage_pct": 70,           # 最大內存使用 70%
            "max_cpu_usage_pct": 80,              # 最大 CPU 使用 80%
            
            # 基礎設施準備
            "required_infrastructure": [
                "load_balancer",
                "database_backup",
                "monitoring_system",
                "log_aggregation",
                "security_scanning"
            ],
            "min_infrastructure_score": 0.9,      # 基礎設施完整度
            
            # 監控配置
            "required_monitoring": [
                "performance_metrics",
                "error_tracking",
                "business_metrics",
                "resource_usage",
                "security_events"
            ],
            "min_alert_coverage": 0.95,           # 告警覆蓋度
            
            # 部署流程
            "required_deployment_steps": [
                "code_review",
                "testing_validation", 
                "security_scan",
                "performance_test",
                "rollback_plan"
            ],
            "min_automation_level": 0.8,          # 自動化水平
            
            # 安全檢查
            "required_security_checks": [
                "vulnerability_scan",
                "dependency_check",
                "access_control",
                "data_encryption",
                "audit_logging"
            ],
            "min_security_score": 0.85,           # 安全評分
            
            # 測試覆蓋
            "min_test_coverage": 0.8,             # 測試覆蓋率
            "min_integration_tests": 10,          # 最少集成測試數
            "min_performance_tests": 5            # 最少性能測試數
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估部署就緒度
        
        Args:
            context: 包含部署相關信息的上下文
            
        Returns:
            評估結果
        """
        try:
            deployment_context = context.get("deployment_context", {})
            model_metrics = deployment_context.get("model_metrics", {})
            system_status = deployment_context.get("system_status", {})
            
            if not deployment_context:
                return {
                    "passed": False,
                    "reason": "No deployment context available"
                }
            
            # 檢查模型穩定性
            model_check = self._check_model_stability(model_metrics)
            if not model_check["passed"]:
                return model_check
            
            # 檢查系統性能
            performance_check = self._check_system_performance(system_status)
            if not performance_check["passed"]:
                return performance_check
            
            # 檢查基礎設施準備
            infrastructure_check = self._check_infrastructure_readiness(deployment_context)
            if not infrastructure_check["passed"]:
                return infrastructure_check
            
            # 檢查監控配置
            monitoring_check = self._check_monitoring_setup(deployment_context)
            if not monitoring_check["passed"]:
                return monitoring_check
            
            # 檢查部署流程
            deployment_check = self._check_deployment_process(deployment_context)
            if not deployment_check["passed"]:
                return deployment_check
            
            # 檢查安全配置
            security_check = self._check_security_configuration(deployment_context)
            if not security_check["passed"]:
                return security_check
            
            # 檢查測試覆蓋
            testing_check = self._check_testing_coverage(deployment_context)
            if not testing_check["passed"]:
                return testing_check
            
            return {
                "passed": True,
                "reason": "System is ready for production deployment",
                "readiness_score": self._calculate_readiness_score(deployment_context)
            }
            
        except Exception as e:
            logger.error(f"Deployment readiness evaluation failed: {e}")
            return {
                "passed": False,
                "reason": f"Deployment readiness evaluation failed: {str(e)}"
            }
    
    def _check_model_stability(self, model_metrics: Dict[str, Any]) -> Dict[str, Any]:
        """檢查模型穩定性"""
        stability_metrics = self._extract_stability_metrics(model_metrics)
        
        failed_checks = []
        
        # 模型準確率檢查
        if stability_metrics["accuracy"] < self.readiness_thresholds["min_model_accuracy"]:
            failed_checks.append(f"Model accuracy {stability_metrics['accuracy']:.3f} < {self.readiness_thresholds['min_model_accuracy']}")
        
        # 一致性檢查
        if stability_metrics["consistency"] < self.readiness_thresholds["min_model_consistency"]:
            failed_checks.append(f"Model consistency {stability_metrics['consistency']:.3f} < {self.readiness_thresholds['min_model_consistency']}")
        
        # 方差檢查
        if stability_metrics["variance"] > self.readiness_thresholds["max_model_variance"]:
            failed_checks.append(f"Model variance {stability_metrics['variance']:.3f} > {self.readiness_thresholds['max_model_variance']}")
        
        # 驗證期間檢查
        if stability_metrics["validation_periods"] < self.readiness_thresholds["min_validation_periods"]:
            failed_checks.append(f"Validation periods {stability_metrics['validation_periods']} < {self.readiness_thresholds['min_validation_periods']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Model stability issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_system_performance(self, system_status: Dict[str, Any]) -> Dict[str, Any]:
        """檢查系統性能"""
        performance_metrics = self._extract_performance_metrics(system_status)
        
        failed_checks = []
        
        # 推理延遲檢查
        if performance_metrics["inference_latency_ms"] > self.readiness_thresholds["max_inference_latency_ms"]:
            failed_checks.append(f"Inference latency {performance_metrics['inference_latency_ms']}ms > {self.readiness_thresholds['max_inference_latency_ms']}ms")
        
        # 系統可用性檢查
        if performance_metrics["availability"] < self.readiness_thresholds["min_system_availability"]:
            failed_checks.append(f"System availability {performance_metrics['availability']:.4%} < {self.readiness_thresholds['min_system_availability']:.2%}")
        
        # 內存使用檢查
        if performance_metrics["memory_usage_pct"] > self.readiness_thresholds["max_memory_usage_pct"]:
            failed_checks.append(f"Memory usage {performance_metrics['memory_usage_pct']}% > {self.readiness_thresholds['max_memory_usage_pct']}%")
        
        # CPU 使用檢查
        if performance_metrics["cpu_usage_pct"] > self.readiness_thresholds["max_cpu_usage_pct"]:
            failed_checks.append(f"CPU usage {performance_metrics['cpu_usage_pct']}% > {self.readiness_thresholds['max_cpu_usage_pct']}%")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"System performance issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_infrastructure_readiness(self, deployment_context: Dict[str, Any]) -> Dict[str, Any]:
        """檢查基礎設施準備度"""
        infrastructure_status = deployment_context.get("infrastructure", {})
        available_components = infrastructure_status.get("components", [])
        
        failed_checks = []
        
        # 必需基礎設施檢查
        for required_component in self.readiness_thresholds["required_infrastructure"]:
            if required_component not in available_components:
                failed_checks.append(f"Missing infrastructure component: {required_component}")
        
        # 基礎設施完整度檢查
        infrastructure_score = infrastructure_status.get("completeness_score", 0)
        if infrastructure_score < self.readiness_thresholds["min_infrastructure_score"]:
            failed_checks.append(f"Infrastructure score {infrastructure_score:.2f} < {self.readiness_thresholds['min_infrastructure_score']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Infrastructure readiness issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_monitoring_setup(self, deployment_context: Dict[str, Any]) -> Dict[str, Any]:
        """檢查監控配置"""
        monitoring_config = deployment_context.get("monitoring", {})
        configured_monitoring = monitoring_config.get("configured_types", [])
        
        failed_checks = []
        
        # 必需監控檢查
        for required_monitoring in self.readiness_thresholds["required_monitoring"]:
            if required_monitoring not in configured_monitoring:
                failed_checks.append(f"Missing monitoring type: {required_monitoring}")
        
        # 告警覆蓋度檢查
        alert_coverage = monitoring_config.get("alert_coverage", 0)
        if alert_coverage < self.readiness_thresholds["min_alert_coverage"]:
            failed_checks.append(f"Alert coverage {alert_coverage:.2%} < {self.readiness_thresholds['min_alert_coverage']:.1%}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Monitoring setup issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_deployment_process(self, deployment_context: Dict[str, Any]) -> Dict[str, Any]:
        """檢查部署流程"""
        deployment_process = deployment_context.get("deployment_process", {})
        completed_steps = deployment_process.get("completed_steps", [])
        
        failed_checks = []
        
        # 必需部署步驟檢查
        for required_step in self.readiness_thresholds["required_deployment_steps"]:
            if required_step not in completed_steps:
                failed_checks.append(f"Missing deployment step: {required_step}")
        
        # 自動化水平檢查
        automation_level = deployment_process.get("automation_level", 0)
        if automation_level < self.readiness_thresholds["min_automation_level"]:
            failed_checks.append(f"Automation level {automation_level:.2f} < {self.readiness_thresholds['min_automation_level']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Deployment process issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_security_configuration(self, deployment_context: Dict[str, Any]) -> Dict[str, Any]:
        """檢查安全配置"""
        security_config = deployment_context.get("security", {})
        completed_checks = security_config.get("completed_checks", [])
        
        failed_checks = []
        
        # 必需安全檢查
        for required_check in self.readiness_thresholds["required_security_checks"]:
            if required_check not in completed_checks:
                failed_checks.append(f"Missing security check: {required_check}")
        
        # 安全評分檢查
        security_score = security_config.get("security_score", 0)
        if security_score < self.readiness_thresholds["min_security_score"]:
            failed_checks.append(f"Security score {security_score:.2f} < {self.readiness_thresholds['min_security_score']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Security configuration issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_testing_coverage(self, deployment_context: Dict[str, Any]) -> Dict[str, Any]:
        """檢查測試覆蓋度"""
        testing_metrics = deployment_context.get("testing", {})
        
        failed_checks = []
        
        # 測試覆蓋率檢查
        test_coverage = testing_metrics.get("code_coverage", 0)
        if test_coverage < self.readiness_thresholds["min_test_coverage"]:
            failed_checks.append(f"Test coverage {test_coverage:.1%} < {self.readiness_thresholds['min_test_coverage']:.1%}")
        
        # 集成測試檢查
        integration_test_count = testing_metrics.get("integration_tests", 0)
        if integration_test_count < self.readiness_thresholds["min_integration_tests"]:
            failed_checks.append(f"Integration tests {integration_test_count} < {self.readiness_thresholds['min_integration_tests']}")
        
        # 性能測試檢查
        performance_test_count = testing_metrics.get("performance_tests", 0)
        if performance_test_count < self.readiness_thresholds["min_performance_tests"]:
            failed_checks.append(f"Performance tests {performance_test_count} < {self.readiness_thresholds['min_performance_tests']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Testing coverage issues: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _extract_stability_metrics(self, model_metrics: Dict[str, Any]) -> Dict[str, Any]:
        """提取模型穩定性指標（模擬數據）"""
        return {
            "accuracy": 0.678,              # 模型準確率
            "consistency": 0.89,            # 一致性評分
            "variance": 0.032,              # 預測方差
            "validation_periods": 7,        # 驗證期間數
            "stability_score": 0.85         # 穩定性綜合評分
        }
    
    def _extract_performance_metrics(self, system_status: Dict[str, Any]) -> Dict[str, Any]:
        """提取系統性能指標（模擬數據）"""
        return {
            "inference_latency_ms": 6.8,    # 推理延遲
            "availability": 0.99987,        # 系統可用性
            "memory_usage_pct": 55,         # 內存使用率
            "cpu_usage_pct": 68,            # CPU 使用率
            "throughput": 15420             # 吞吐量
        }
    
    def _calculate_readiness_score(self, deployment_context: Dict[str, Any]) -> float:
        """計算部署就緒度綜合評分"""
        # 各維度評分
        scores = {
            "model_stability_score": 0.87,     # 模型穩定性
            "system_performance_score": 0.92,  # 系統性能
            "infrastructure_score": 0.95,      # 基礎設施
            "monitoring_score": 0.89,          # 監控配置
            "deployment_process_score": 0.91,  # 部署流程
            "security_score": 0.88,            # 安全配置
            "testing_score": 0.84              # 測試覆蓋
        }
        
        # 權重分配
        weights = {
            "model_stability_score": 0.20,     # 模型穩定性是關鍵
            "system_performance_score": 0.15,  # 系統性能
            "infrastructure_score": 0.15,      # 基礎設施
            "monitoring_score": 0.15,          # 監控配置
            "deployment_process_score": 0.15,  # 部署流程
            "security_score": 0.10,            # 安全配置
            "testing_score": 0.10               # 測試覆蓋
        }
        
        readiness_score = sum(scores[key] * weights[key] for key in scores.keys())
        return round(readiness_score, 3)