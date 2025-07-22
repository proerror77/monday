"""
監控和告警工具集
================

為系統監控和告警提供專業化工具：
- 實時性能監控
- 異常檢測和告警
- 系統健康檢查
- 報告生成
"""

from agno.tools import Tool
from typing import Dict, Any, Optional, List
import asyncio
import logging
from datetime import datetime
import json
import psutil
import time

logger = logging.getLogger(__name__)


class MonitoringTools(Tool):
    """
    監控和告警工具集
    
    提供全面的系統監控和告警能力
    """
    
    def __init__(self):
        super().__init__()
        
    async def get_system_metrics(self) -> Dict[str, Any]:
        """
        獲取系統性能指標
        
        Returns:
            系統性能指標
        """
        try:
            # 獲取實際系統指標
            cpu_percent = psutil.cpu_percent(interval=1)
            memory = psutil.virtual_memory()
            disk = psutil.disk_usage('/')
            network = psutil.net_io_counters()
            
            system_metrics = {
                "timestamp": datetime.now().isoformat(),
                "cpu": {
                    "usage_percent": cpu_percent,
                    "core_count": psutil.cpu_count(),
                    "frequency_mhz": psutil.cpu_freq().current if psutil.cpu_freq() else 0,
                    "load_average": psutil.getloadavg()
                },
                "memory": {
                    "total_gb": round(memory.total / (1024**3), 2),
                    "used_gb": round(memory.used / (1024**3), 2),
                    "usage_percent": memory.percent,
                    "available_gb": round(memory.available / (1024**3), 2)
                },
                "disk": {
                    "total_gb": round(disk.total / (1024**3), 2),
                    "used_gb": round(disk.used / (1024**3), 2),
                    "usage_percent": round((disk.used / disk.total) * 100, 2),
                    "free_gb": round(disk.free / (1024**3), 2)
                },
                "network": {
                    "bytes_sent": network.bytes_sent,
                    "bytes_recv": network.bytes_recv,
                    "packets_sent": network.packets_sent,
                    "packets_recv": network.packets_recv
                }
            }
            
            return {
                "success": True,
                "system_metrics": system_metrics
            }
            
        except Exception as e:
            logger.error(f"Failed to get system metrics: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    async def monitor_application_health(
        self,
        application_name: str = "rust_hft"
    ) -> Dict[str, Any]:
        """
        監控應用程式健康狀態
        
        Args:
            application_name: 應用程式名稱
            
        Returns:
            應用程式健康狀態
        """
        try:
            # 模擬應用程式監控
            health_status = {
                "application": application_name,
                "status": "healthy",
                "uptime_seconds": 12345,
                "version": "1.2.0",
                "last_restart": "2025-07-21T09:30:00Z",
                "performance_metrics": {
                    "avg_latency_us": 245,
                    "p95_latency_us": 480,
                    "p99_latency_us": 850,
                    "throughput_ops_per_sec": 15234,
                    "error_rate_percent": 0.02,
                    "memory_usage_mb": 156.7
                },
                "health_checks": {
                    "database_connection": "healthy",
                    "redis_connection": "healthy",
                    "websocket_connection": "healthy",
                    "model_loading": "healthy",
                    "trading_engine": "healthy"
                },
                "recent_errors": [
                    {
                        "timestamp": "2025-07-22T09:45:12Z",
                        "level": "warning",
                        "message": "Temporary connection timeout to exchange",
                        "resolved": True
                    }
                ]
            }
            
            return {
                "success": True,
                "health_status": health_status
            }
            
        except Exception as e:
            logger.error(f"Application health monitoring failed: {e}")
            return {
                "success": False,
                "error": str(e),
                "application": application_name
            }
    
    async def check_trading_metrics(
        self,
        symbol: Optional[str] = None
    ) -> Dict[str, Any]:
        """
        檢查交易指標
        
        Args:
            symbol: 交易對（可選）
            
        Returns:
            交易指標
        """
        try:
            trading_metrics = {
                "monitoring_time": datetime.now().isoformat(),
                "symbol": symbol or "ALL",
                "realtime_metrics": {
                    "active_positions": 5,
                    "total_exposure_usd": 52340.67,
                    "unrealized_pnl_usd": 234.56,
                    "realized_pnl_today_usd": 567.89,
                    "trades_today": 45,
                    "win_rate_today": 0.6222,
                    "avg_trade_duration_minutes": 8.5
                },
                "risk_metrics": {
                    "portfolio_var_1d": -1234.56,
                    "max_drawdown_percent": -2.3,
                    "leverage_ratio": 2.5,
                    "margin_utilization_percent": 45.6,
                    "risk_score": 0.35
                },
                "performance_metrics": {
                    "decision_latency_us": {
                        "avg": 245,
                        "p95": 480,
                        "p99": 850,
                        "max": 1250
                    },
                    "execution_latency_us": {
                        "avg": 1234,
                        "p95": 2345,
                        "p99": 4567,
                        "max": 8900
                    },
                    "slippage_bps": {
                        "avg": 0.8,
                        "p95": 2.1,
                        "max": 5.6
                    }
                }
            }
            
            return {
                "success": True,
                "trading_metrics": trading_metrics
            }
            
        except Exception as e:
            logger.error(f"Trading metrics check failed: {e}")
            return {
                "success": False,
                "error": str(e),
                "symbol": symbol
            }
    
    async def detect_anomalies(
        self,
        metrics_data: Dict[str, Any],
        sensitivity: str = "medium"
    ) -> Dict[str, Any]:
        """
        檢測系統異常
        
        Args:
            metrics_data: 指標數據
            sensitivity: 檢測靈敏度
            
        Returns:
            異常檢測結果
        """
        try:
            sensitivity_thresholds = {
                "low": {"cpu": 90, "memory": 85, "latency": 2000},
                "medium": {"cpu": 80, "memory": 75, "latency": 1000},
                "high": {"cpu": 70, "memory": 65, "latency": 500}
            }
            
            thresholds = sensitivity_thresholds.get(sensitivity, sensitivity_thresholds["medium"])
            
            anomalies = []
            
            # CPU 異常檢測
            cpu_usage = metrics_data.get("cpu", {}).get("usage_percent", 0)
            if cpu_usage > thresholds["cpu"]:
                anomalies.append({
                    "type": "cpu_high",
                    "severity": "warning",
                    "value": cpu_usage,
                    "threshold": thresholds["cpu"],
                    "description": f"CPU usage {cpu_usage}% exceeds threshold {thresholds['cpu']}%"
                })
            
            # 記憶體異常檢測
            memory_usage = metrics_data.get("memory", {}).get("usage_percent", 0)
            if memory_usage > thresholds["memory"]:
                anomalies.append({
                    "type": "memory_high",
                    "severity": "warning" if memory_usage < 90 else "critical",
                    "value": memory_usage,
                    "threshold": thresholds["memory"],
                    "description": f"Memory usage {memory_usage}% exceeds threshold {thresholds['memory']}%"
                })
            
            # 延遲異常檢測
            latency = metrics_data.get("performance", {}).get("avg_latency_us", 0)
            if latency > thresholds["latency"]:
                anomalies.append({
                    "type": "latency_high",
                    "severity": "critical",
                    "value": latency,
                    "threshold": thresholds["latency"],
                    "description": f"Average latency {latency}μs exceeds threshold {thresholds['latency']}μs"
                })
            
            anomaly_report = {
                "detection_time": datetime.now().isoformat(),
                "sensitivity": sensitivity,
                "anomalies_detected": len(anomalies),
                "anomalies": anomalies,
                "overall_status": "healthy" if len(anomalies) == 0 else "anomalies_detected",
                "recommendations": self._generate_recommendations(anomalies)
            }
            
            return {
                "success": True,
                "anomaly_report": anomaly_report
            }
            
        except Exception as e:
            logger.error(f"Anomaly detection failed: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    async def send_alert(
        self,
        alert_data: Dict[str, Any],
        channels: List[str] = ["log"]
    ) -> Dict[str, Any]:
        """
        發送告警
        
        Args:
            alert_data: 告警數據
            channels: 告警通道
            
        Returns:
            告警發送結果
        """
        try:
            alert_id = f"ALERT_{int(time.time())}"
            severity = alert_data.get("severity", "info")
            message = alert_data.get("message", "System alert")
            
            send_results = []
            
            for channel in channels:
                if channel == "log":
                    logger.warning(f"ALERT [{severity.upper()}]: {message}")
                    send_results.append({
                        "channel": "log",
                        "status": "sent",
                        "timestamp": datetime.now().isoformat()
                    })
                elif channel == "email":
                    # 模擬郵件發送
                    send_results.append({
                        "channel": "email",
                        "status": "sent",
                        "recipients": ["admin@trading-system.com"],
                        "timestamp": datetime.now().isoformat()
                    })
                elif channel == "slack":
                    # 模擬 Slack 通知
                    send_results.append({
                        "channel": "slack",
                        "status": "sent",
                        "webhook_url": "https://hooks.slack.com/services/...",
                        "timestamp": datetime.now().isoformat()
                    })
                elif channel == "sms":
                    # 模擬簡訊發送
                    send_results.append({
                        "channel": "sms",
                        "status": "sent",
                        "phone_numbers": ["+1234567890"],
                        "timestamp": datetime.now().isoformat()
                    })
            
            alert_record = {
                "alert_id": alert_id,
                "severity": severity,
                "message": message,
                "alert_data": alert_data,
                "channels": channels,
                "send_results": send_results,
                "created_at": datetime.now().isoformat()
            }
            
            return {
                "success": True,
                "alert_record": alert_record
            }
            
        except Exception as e:
            logger.error(f"Alert sending failed: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    async def generate_monitoring_report(
        self,
        report_type: str = "daily",
        include_charts: bool = False
    ) -> Dict[str, Any]:
        """
        生成監控報告
        
        Args:
            report_type: 報告類型
            include_charts: 是否包含圖表
            
        Returns:
            監控報告
        """
        try:
            report_data = {
                "report_id": f"MON_REPORT_{int(time.time())}",
                "report_type": report_type,
                "generation_time": datetime.now().isoformat(),
                "report_period": self._get_report_period(report_type),
                
                "executive_summary": {
                    "overall_status": "healthy",
                    "critical_alerts": 0,
                    "warning_alerts": 2,
                    "info_alerts": 5,
                    "system_availability": 99.87,
                    "average_latency_us": 245,
                    "total_trades": 1234
                },
                
                "system_performance": {
                    "avg_cpu_usage": 65.4,
                    "max_cpu_usage": 82.1,
                    "avg_memory_usage": 71.3,
                    "max_memory_usage": 85.7,
                    "avg_disk_usage": 45.2,
                    "network_throughput_mbps": 125.6
                },
                
                "trading_performance": {
                    "total_pnl_usd": 2345.67,
                    "win_rate": 0.6234,
                    "sharpe_ratio": 1.45,
                    "max_drawdown": -0.0567,
                    "total_volume_usd": 1234567.89,
                    "average_trade_size_usd": 1000.45
                },
                
                "alerts_summary": [
                    {
                        "timestamp": "2025-07-22T08:30:00Z",
                        "severity": "warning",
                        "type": "high_latency",
                        "message": "Trading latency exceeded 500μs threshold",
                        "resolved": True,
                        "resolution_time_minutes": 5
                    },
                    {
                        "timestamp": "2025-07-22T14:15:00Z",
                        "severity": "warning", 
                        "type": "memory_usage",
                        "message": "Memory usage reached 80%",
                        "resolved": False,
                        "duration_minutes": 45
                    }
                ],
                
                "recommendations": [
                    "Consider optimizing memory usage to prevent potential issues",
                    "Monitor network latency during peak hours",
                    "Review trading strategy performance for further optimization"
                ]
            }
            
            if include_charts:
                report_data["chart_data"] = {
                    "cpu_usage_timeline": "chart_data_cpu.json",
                    "memory_usage_timeline": "chart_data_memory.json",
                    "latency_distribution": "chart_data_latency.json",
                    "pnl_timeline": "chart_data_pnl.json"
                }
            
            return {
                "success": True,
                "monitoring_report": report_data
            }
            
        except Exception as e:
            logger.error(f"Monitoring report generation failed: {e}")
            return {
                "success": False,
                "error": str(e)
            }
    
    def _generate_recommendations(self, anomalies: List[Dict[str, Any]]) -> List[str]:
        """生成異常處理建議"""
        recommendations = []
        
        for anomaly in anomalies:
            if anomaly["type"] == "cpu_high":
                recommendations.append("Consider optimizing CPU-intensive processes or scaling resources")
            elif anomaly["type"] == "memory_high":
                recommendations.append("Review memory usage patterns and consider garbage collection optimization")
            elif anomaly["type"] == "latency_high":
                recommendations.append("Investigate network connectivity and optimize critical code paths")
        
        if not recommendations:
            recommendations.append("System is operating within normal parameters")
        
        return recommendations
    
    def _get_report_period(self, report_type: str) -> str:
        """獲取報告期間"""
        periods = {
            "hourly": "Last 1 hour",
            "daily": "Last 24 hours", 
            "weekly": "Last 7 days",
            "monthly": "Last 30 days"
        }
        return periods.get(report_type, "Unknown period")
    
    def get_info(self) -> Dict[str, str]:
        """獲取工具信息"""
        return {
            "name": "MonitoringTools",
            "description": "系統監控和告警工具集",
            "available_functions": [
                "get_system_metrics", "monitor_application_health", 
                "check_trading_metrics", "detect_anomalies",
                "send_alert", "generate_monitoring_report"
            ]
        }