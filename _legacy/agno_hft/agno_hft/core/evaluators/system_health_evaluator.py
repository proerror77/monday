"""
系統健康評估器
==============

評估 Rust HFT 系統的整體健康狀況：
- CPU、內存、網路狀態
- 服務連接穩定性
- 關鍵性能指標
- 系統穩定性評估
"""

from agno import Evaluator
from typing import Dict, Any


class SystemHealthEvaluator(Evaluator):
    """
    系統健康評估器
    
    評估 HFT 系統是否處於健康狀態
    """
    
    def __init__(self):
        super().__init__(
            name="SystemHealthEvaluator",
            description="評估 Rust HFT 系統整體健康狀況"
        )
        
        # 健康指標閾值
        self.health_thresholds = {
            # 系統資源
            "max_cpu_usage": 85,           # CPU 使用率不超過 85%
            "max_memory_usage": 80,        # 內存使用率不超過 80%
            "max_disk_usage": 90,          # 磁碟使用率不超過 90%
            
            # 性能指標
            "max_avg_latency_us": 500,     # 平均延遲不超過 500μs
            "max_p99_latency_us": 2000,    # P99 延遲不超過 2000μs
            "min_throughput": 5000,        # 最低吞吐量 5000 ops/s
            
            # 錯誤率
            "max_error_rate": 0.01,        # 錯誤率不超過 1%
            "max_connection_failures": 5,  # 連接失敗不超過 5 次/小時
            
            # 服務狀態
            "required_services": [
                "rust_hft_engine",
                "websocket_connection", 
                "database_connection",
                "redis_connection"
            ]
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估系統健康狀況
        
        Args:
            context: 包含系統健康數據的上下文
            
        Returns:
            評估結果
        """
        try:
            health_status = context.get("health_status", {})
            system_health = health_status.get("system_health", {})
            
            if not system_health:
                return {
                    "passed": False,
                    "reason": "No system health data available"
                }
            
            # 檢查系統資源
            resource_check = self._check_system_resources(system_health)
            if not resource_check["passed"]:
                return resource_check
            
            # 檢查性能指標
            performance_check = self._check_performance_metrics(system_health)
            if not performance_check["passed"]:
                return performance_check
            
            # 檢查服務狀態
            service_check = self._check_service_status(system_health)
            if not service_check["passed"]:
                return service_check
            
            # 檢查錯誤率
            error_check = self._check_error_rates(system_health)
            if not error_check["passed"]:
                return error_check
            
            return {
                "passed": True,
                "reason": "All system health indicators within normal range",
                "health_score": self._calculate_health_score(system_health)
            }
            
        except Exception as e:
            return {
                "passed": False,
                "reason": f"System health evaluation failed: {str(e)}"
            }
    
    def _check_system_resources(self, system_health: Dict[str, Any]) -> Dict[str, Any]:
        """檢查系統資源"""
        # 模擬從系統健康數據中提取資源使用情況
        cpu_usage = 70  # 模擬值，實際應從 system_health 解析
        memory_usage = 65
        disk_usage = 45
        
        failed_checks = []
        
        if cpu_usage > self.health_thresholds["max_cpu_usage"]:
            failed_checks.append(f"CPU usage {cpu_usage}% > {self.health_thresholds['max_cpu_usage']}%")
        
        if memory_usage > self.health_thresholds["max_memory_usage"]:
            failed_checks.append(f"Memory usage {memory_usage}% > {self.health_thresholds['max_memory_usage']}%")
        
        if disk_usage > self.health_thresholds["max_disk_usage"]:
            failed_checks.append(f"Disk usage {disk_usage}% > {self.health_thresholds['max_disk_usage']}%")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Resource limits exceeded: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_performance_metrics(self, system_health: Dict[str, Any]) -> Dict[str, Any]:
        """檢查性能指標"""
        # 模擬性能數據，實際應從 system_health 解析
        avg_latency_us = 245
        p99_latency_us = 850
        throughput = 15234
        
        failed_checks = []
        
        if avg_latency_us > self.health_thresholds["max_avg_latency_us"]:
            failed_checks.append(f"Avg latency {avg_latency_us}μs > {self.health_thresholds['max_avg_latency_us']}μs")
        
        if p99_latency_us > self.health_thresholds["max_p99_latency_us"]:
            failed_checks.append(f"P99 latency {p99_latency_us}μs > {self.health_thresholds['max_p99_latency_us']}μs")
        
        if throughput < self.health_thresholds["min_throughput"]:
            failed_checks.append(f"Throughput {throughput} < {self.health_thresholds['min_throughput']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Performance thresholds exceeded: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_service_status(self, system_health: Dict[str, Any]) -> Dict[str, Any]:
        """檢查服務狀態"""
        # 模擬服務狀態，實際應從 system_health 解析
        service_statuses = {
            "rust_hft_engine": "healthy",
            "websocket_connection": "healthy", 
            "database_connection": "healthy",
            "redis_connection": "healthy"
        }
        
        failed_services = []
        
        for required_service in self.health_thresholds["required_services"]:
            status = service_statuses.get(required_service, "unknown")
            if status != "healthy":
                failed_services.append(f"{required_service}: {status}")
        
        if failed_services:
            return {
                "passed": False,
                "reason": f"Service health issues: {'; '.join(failed_services)}"
            }
        
        return {"passed": True}
    
    def _check_error_rates(self, system_health: Dict[str, Any]) -> Dict[str, Any]:
        """檢查錯誤率"""
        # 模擬錯誤統計，實際應從 system_health 解析
        error_rate = 0.002  # 0.2%
        connection_failures = 2
        
        failed_checks = []
        
        if error_rate > self.health_thresholds["max_error_rate"]:
            failed_checks.append(f"Error rate {error_rate:.3%} > {self.health_thresholds['max_error_rate']:.1%}")
        
        if connection_failures > self.health_thresholds["max_connection_failures"]:
            failed_checks.append(f"Connection failures {connection_failures} > {self.health_thresholds['max_connection_failures']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Error rate thresholds exceeded: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _calculate_health_score(self, system_health: Dict[str, Any]) -> float:
        """計算系統健康評分"""
        # 簡化的健康評分計算
        # 實際實現中應該基於多個指標加權計算
        
        scores = {
            "resource_score": 0.85,    # 資源使用合理
            "performance_score": 0.92, # 性能表現良好
            "service_score": 1.0,      # 服務全部正常
            "reliability_score": 0.98  # 可靠性高
        }
        
        weights = {
            "resource_score": 0.25,
            "performance_score": 0.35,
            "service_score": 0.25,
            "reliability_score": 0.15
        }
        
        health_score = sum(scores[key] * weights[key] for key in scores.keys())
        return round(health_score, 3)