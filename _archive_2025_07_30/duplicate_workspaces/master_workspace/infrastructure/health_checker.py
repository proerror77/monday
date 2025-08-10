#!/usr/bin/env python3
"""
Infrastructure Health Checker - 基礎設施健康檢查器
===============================================

提供全面的基礎設施健康監控：
1. 服務可用性檢查 (Redis, ClickHouse, Docker)
2. 性能指標監控 (延遲, 吞吐量, 資源使用)
3. 故障檢測和自動恢復
4. 健康報告和告警

設計模式：Observer Pattern + Health Check Pattern
"""

import asyncio
import logging
from typing import Dict, Any, List, Optional, Callable
from datetime import datetime, timedelta
import time
import psutil
import json

logger = logging.getLogger(__name__)

class InfrastructureHealthChecker:
    """
    基礎設施健康檢查器
    
    功能：
    - 週期性健康檢查
    - 性能指標收集
    - 故障檢測和告警
    - 自動恢復機制
    """
    
    def __init__(
        self,
        redis_proxy=None,
        clickhouse_proxy=None,
        docker_manager=None,
        check_interval: int = 30
    ):
        self.redis_proxy = redis_proxy
        self.clickhouse_proxy = clickhouse_proxy
        self.docker_manager = docker_manager
        self.check_interval = check_interval
        
        # 運行狀態
        self.is_running = False
        self.health_checks_enabled = True
        
        # 健康狀態
        self.overall_health = True
        self.service_health = {}
        self.last_check_time = None
        
        # 性能指標
        self.metrics = {
            "system": {},
            "services": {},
            "alerts": []
        }
        
        # 告警配置
        self.alert_thresholds = {
            "cpu_usage": 80.0,
            "memory_usage": 80.0,
            "disk_usage": 85.0,
            "redis_latency_ms": 10.0,
            "clickhouse_latency_ms": 100.0,
            "service_downtime_minutes": 5.0
        }
        
        # 回調函數
        self.alert_callbacks: List[Callable] = []
        self.recovery_callbacks: List[Callable] = []
        
        # 統計數據
        self.stats = {
            "total_checks": 0,
            "failed_checks": 0,
            "alerts_triggered": 0,
            "auto_recoveries": 0,
            "uptime_start": datetime.now()
        }
        
        logger.info("✅ 基礎設施健康檢查器初始化完成")
    
    async def initialize(self):
        """初始化健康檢查器"""
        logger.info("🔍 初始化健康檢查器...")
        
        # 執行初始健康檢查
        await self.perform_comprehensive_check()
        
        logger.info("✅ 健康檢查器初始化完成")
    
    async def start_monitoring(self):
        """開始健康監控"""
        if self.is_running:
            logger.warning("⚠️ 健康監控已在運行")
            return
        
        logger.info(f"🚀 開始健康監控 (間隔: {self.check_interval}s)")
        
        self.is_running = True
        
        # 啟動監控任務
        monitoring_tasks = [
            self._health_check_loop(),
            self._metrics_collection_loop(),
            self._alert_processing_loop()
        ]
        
        try:
            await asyncio.gather(*monitoring_tasks)
        except Exception as e:
            logger.error(f"❌ 健康監控異常: {e}")
            raise
        finally:
            self.is_running = False
    
    async def stop_monitoring(self):
        """停止健康監控"""
        logger.info("🛑 停止健康監控...")
        
        self.is_running = False
        
        logger.info("✅ 健康監控已停止")
    
    async def _health_check_loop(self):
        """健康檢查循環"""
        while self.is_running:
            try:
                if self.health_checks_enabled:
                    await self.perform_comprehensive_check()
                
                await asyncio.sleep(self.check_interval)
                
            except Exception as e:
                logger.error(f"❌ 健康檢查循環異常: {e}")
                await asyncio.sleep(self.check_interval * 2)  # 異常時延長間隔
    
    async def _metrics_collection_loop(self):
        """指標收集循環"""
        while self.is_running:
            try:
                await self._collect_system_metrics()
                await self._collect_service_metrics()
                
                await asyncio.sleep(self.check_interval // 2)  # 更頻繁收集指標
                
            except Exception as e:
                logger.error(f"❌ 指標收集異常: {e}")
                await asyncio.sleep(self.check_interval)
    
    async def _alert_processing_loop(self):
        """告警處理循環"""
        while self.is_running:
            try:
                await self._process_alerts()
                
                await asyncio.sleep(10)  # 10秒檢查一次告警
                
            except Exception as e:
                logger.error(f"❌ 告警處理異常: {e}")
                await asyncio.sleep(30)
    
    async def perform_comprehensive_check(self) -> Dict[str, Any]:
        """執行全面健康檢查"""
        self.stats["total_checks"] += 1
        start_time = time.time()
        
        try:
            logger.debug("🔍 執行全面健康檢查...")
            
            # 檢查各個服務
            checks = {}
            
            # Redis 健康檢查
            if self.redis_proxy:
                checks["redis"] = await self._check_redis_health()
            
            # ClickHouse 健康檢查
            if self.clickhouse_proxy:
                checks["clickhouse"] = await self._check_clickhouse_health()
            
            # Docker 服務健康檢查
            if self.docker_manager:
                checks["docker"] = await self._check_docker_health()
            
            # 系統資源檢查
            checks["system"] = await self._check_system_health()
            
            # 更新整體健康狀態
            self.overall_health = all(check.get("healthy", False) for check in checks.values())
            self.service_health = checks
            self.last_check_time = datetime.now()
            
            check_duration = (time.time() - start_time) * 1000  # 毫秒
            
            result = {
                "healthy": self.overall_health,
                "timestamp": self.last_check_time.isoformat(),
                "check_duration_ms": check_duration,
                "services": checks,
                "stats": self.stats
            }
            
            if self.overall_health:
                logger.debug(f"✅ 健康檢查通過，耗時: {check_duration:.2f}ms")
            else:
                logger.warning("⚠️ 健康檢查發現問題")
                self.stats["failed_checks"] += 1
            
            return result
            
        except Exception as e:
            self.stats["failed_checks"] += 1
            logger.error(f"❌ 全面健康檢查失敗: {e}")
            
            return {
                "healthy": False,
                "error": str(e),
                "timestamp": datetime.now().isoformat()
            }
    
    async def _check_redis_health(self) -> Dict[str, Any]:
        """檢查 Redis 健康狀態"""
        try:
            start_time = time.time()
            
            # 獲取 Redis 狀態
            redis_status = await self.redis_proxy.get_status()
            
            # 執行基本操作測試
            test_key = f"health_check_{int(time.time())}"
            await self.redis_proxy.set(test_key, "test_value", ex=5)
            result = await self.redis_proxy.get(test_key)
            
            latency = (time.time() - start_time) * 1000  # 毫秒
            
            healthy = (
                redis_status.get("running", False) and
                not redis_status.get("circuit_breaker_open", True) and
                result == "test_value" and
                latency < self.alert_thresholds["redis_latency_ms"]
            )
            
            return {
                "healthy": healthy,
                "latency_ms": latency,
                "status": redis_status,
                "test_passed": result == "test_value"
            }
            
        except Exception as e:
            logger.error(f"❌ Redis 健康檢查失敗: {e}")
            return {"healthy": False, "error": str(e)}
    
    async def _check_clickhouse_health(self) -> Dict[str, Any]:
        """檢查 ClickHouse 健康狀態"""
        try:
            start_time = time.time()
            
            # 獲取 ClickHouse 狀態
            ch_status = await self.clickhouse_proxy.get_status()
            
            # 執行基本查詢測試
            result = await self.clickhouse_proxy.execute("SELECT 1 as test, now() as timestamp")
            
            latency = (time.time() - start_time) * 1000  # 毫秒
            
            healthy = (
                ch_status.get("running", False) and
                result and
                len(result) > 0 and
                latency < self.alert_thresholds["clickhouse_latency_ms"]
            )
            
            return {
                "healthy": healthy,
                "latency_ms": latency,
                "status": ch_status,
                "test_passed": bool(result)
            }
            
        except Exception as e:
            logger.error(f"❌ ClickHouse 健康檢查失敗: {e}")
            return {"healthy": False, "error": str(e)}
    
    async def _check_docker_health(self) -> Dict[str, Any]:
        """檢查 Docker 服務健康狀態"""
        try:
            docker_status = await self.docker_manager.get_status()
            
            services_health = docker_status.get("services_health", {})
            
            # 檢查關鍵服務是否健康
            critical_services = ["redis", "clickhouse"]
            critical_healthy = all(
                services_health.get(svc, {}).get("healthy", False)
                for svc in critical_services
                if svc in services_health
            )
            
            # 統計健康服務數量
            total_services = len(services_health)
            healthy_services = sum(
                1 for health in services_health.values()
                if health.get("healthy", False)
            )
            
            overall_healthy = critical_healthy and healthy_services >= total_services * 0.8
            
            return {
                "healthy": overall_healthy,
                "critical_services_healthy": critical_healthy,
                "healthy_services": healthy_services,
                "total_services": total_services,
                "services_health": services_health
            }
            
        except Exception as e:
            logger.error(f"❌ Docker 健康檢查失敗: {e}")
            return {"healthy": False, "error": str(e)}
    
    async def _check_system_health(self) -> Dict[str, Any]:
        """檢查系統資源健康狀態"""
        try:
            # CPU 使用率
            cpu_usage = psutil.cpu_percent(interval=1)
            
            # 記憶體使用率
            memory = psutil.virtual_memory()
            memory_usage = memory.percent
            
            # 磁盤使用率
            disk = psutil.disk_usage('/')
            disk_usage = disk.percent
            
            # 負載平均值
            load_avg = psutil.getloadavg()
            
            # 檢查閾值
            cpu_healthy = cpu_usage < self.alert_thresholds["cpu_usage"]
            memory_healthy = memory_usage < self.alert_thresholds["memory_usage"]
            disk_healthy = disk_usage < self.alert_thresholds["disk_usage"]
            
            overall_healthy = cpu_healthy and memory_healthy and disk_healthy
            
            return {
                "healthy": overall_healthy,
                "cpu_usage": cpu_usage,
                "memory_usage": memory_usage,
                "disk_usage": disk_usage,
                "load_avg": load_avg,
                "thresholds_met": {
                    "cpu": cpu_healthy,
                    "memory": memory_healthy,
                    "disk": disk_healthy
                }
            }
            
        except Exception as e:
            logger.error(f"❌ 系統健康檢查失敗: {e}")
            return {"healthy": False, "error": str(e)}
    
    async def _collect_system_metrics(self):
        """收集系統指標"""
        try:
            self.metrics["system"] = {
                "timestamp": datetime.now().isoformat(),
                "cpu_usage": psutil.cpu_percent(),
                "memory": dict(psutil.virtual_memory()._asdict()),
                "disk": dict(psutil.disk_usage('/')._asdict()),
                "load_avg": psutil.getloadavg(),
                "network": dict(psutil.net_io_counters()._asdict()),
                "boot_time": psutil.boot_time()
            }
            
        except Exception as e:
            logger.error(f"❌ 系統指標收集失敗: {e}")
    
    async def _collect_service_metrics(self):
        """收集服務指標"""
        try:
            service_metrics = {}
            
            if self.redis_proxy:
                service_metrics["redis"] = await self.redis_proxy.get_status()
            
            if self.clickhouse_proxy:
                service_metrics["clickhouse"] = await self.clickhouse_proxy.get_status()
            
            if self.docker_manager:
                service_metrics["docker"] = await self.docker_manager.get_status()
            
            self.metrics["services"] = {
                "timestamp": datetime.now().isoformat(),
                **service_metrics
            }
            
        except Exception as e:
            logger.error(f"❌ 服務指標收集失敗: {e}")
    
    async def _process_alerts(self):
        """處理告警"""
        current_alerts = []
        
        # 檢查系統資源告警
        system_metrics = self.metrics.get("system", {})
        if system_metrics:
            cpu_usage = system_metrics.get("cpu_usage", 0)
            if cpu_usage > self.alert_thresholds["cpu_usage"]:
                current_alerts.append({
                    "type": "system_resource",
                    "severity": "warning",
                    "message": f"CPU usage high: {cpu_usage:.1f}%",
                    "timestamp": datetime.now().isoformat()
                })
        
        # 檢查服務告警
        for service_name, health in self.service_health.items():
            if not health.get("healthy", True):
                current_alerts.append({
                    "type": "service_unhealthy",
                    "severity": "critical",
                    "service": service_name,
                    "message": f"Service {service_name} is unhealthy",
                    "timestamp": datetime.now().isoformat()
                })
        
        # 更新告警列表
        self.metrics["alerts"] = current_alerts
        
        # 觸發告警回調
        if current_alerts:
            self.stats["alerts_triggered"] += len(current_alerts)
            for callback in self.alert_callbacks:
                try:
                    await callback(current_alerts)
                except Exception as e:
                    logger.error(f"❌ 告警回調執行失敗: {e}")
    
    def add_alert_callback(self, callback: Callable):
        """添加告警回調函數"""
        self.alert_callbacks.append(callback)
    
    def add_recovery_callback(self, callback: Callable):
        """添加恢復回調函數"""
        self.recovery_callbacks.append(callback)
    
    async def get_status(self) -> Dict[str, Any]:
        """獲取健康檢查器狀態"""
        uptime = datetime.now() - self.stats["uptime_start"]
        
        return {
            "running": self.is_running,
            "health_checks_enabled": self.health_checks_enabled,
            "overall_health": self.overall_health,
            "last_check_time": self.last_check_time.isoformat() if self.last_check_time else None,
            "check_interval": self.check_interval,
            "uptime_seconds": uptime.total_seconds(),
            "stats": self.stats,
            "current_alerts": self.metrics.get("alerts", []),
            "alert_thresholds": self.alert_thresholds
        }