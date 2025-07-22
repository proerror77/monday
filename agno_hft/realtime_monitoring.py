"""
实时监控和告警系统
==================

实现高性能的实时监控系统：
- 系统指标监控
- 性能基准监控
- 异常检测和告警
- 实时仪表板
- 自动故障恢复
"""

import asyncio
import time
import psutil
import json
from typing import Dict, Any, List, Optional, Callable
from dataclasses import dataclass, field, asdict
from datetime import datetime, timedelta
from collections import deque
from enum import Enum
import statistics
import threading
from loguru import logger

try:
    import rust_hft_py
    from rust_hft_tools_v2 import RustHFTEngineV2
    RUST_AVAILABLE = True
except ImportError:
    RUST_AVAILABLE = False


class AlertSeverity(Enum):
    """告警严重程度"""
    INFO = "info"
    WARNING = "warning" 
    ERROR = "error"
    CRITICAL = "critical"


class MetricType(Enum):
    """指标类型"""
    GAUGE = "gauge"          # 瞬时值
    COUNTER = "counter"      # 累计值
    HISTOGRAM = "histogram"  # 分布
    TIMER = "timer"          # 时间指标


@dataclass
class MetricValue:
    """指标值"""
    timestamp: float
    value: float
    labels: Dict[str, str] = field(default_factory=dict)


@dataclass
class Alert:
    """告警"""
    id: str
    severity: AlertSeverity
    title: str
    message: str
    timestamp: float
    source: str
    metric_name: Optional[str] = None
    metric_value: Optional[float] = None
    threshold: Optional[float] = None
    resolved: bool = False
    resolved_at: Optional[float] = None


@dataclass
class SystemMetrics:
    """系统指标快照"""
    timestamp: float
    cpu_percent: float
    memory_percent: float
    memory_used_mb: float
    memory_available_mb: float
    disk_usage_percent: float
    network_bytes_sent: int
    network_bytes_recv: int
    load_average: List[float]
    process_count: int
    
    # HFT 特定指标
    decision_latency_us: float = 0.0
    order_processing_rate: float = 0.0
    active_strategies: int = 0
    total_pnl: float = 0.0
    open_positions: int = 0


class MetricCollector:
    """指标收集器"""
    
    def __init__(self):
        self.metrics: Dict[str, deque] = {}
        self.max_history = 1000  # 保留最近1000个数据点
        self.rust_engine = RustHFTEngineV2() if RUST_AVAILABLE else None
        
        # 网络基线（用于计算差值）
        self.network_baseline = None
        self.last_collection_time = time.time()
        
        logger.info("📊 指标收集器初始化完成")
    
    def collect_system_metrics(self) -> SystemMetrics:
        """收集系统指标"""
        current_time = time.time()
        
        # CPU 和内存
        cpu_percent = psutil.cpu_percent(interval=0.1)
        memory = psutil.virtual_memory()
        
        # 磁盘使用
        disk = psutil.disk_usage('/')
        
        # 网络统计
        network = psutil.net_io_counters()
        if self.network_baseline is None:
            self.network_baseline = network
        
        # 负载平均值
        load_avg = list(psutil.getloadavg()) if hasattr(psutil, 'getloadavg') else [0.0, 0.0, 0.0]
        
        # 进程数量
        process_count = len(psutil.pids())
        
        # HFT 特定指标
        decision_latency = 0.0
        order_rate = 0.0
        active_strategies = 0
        total_pnl = 0.0
        open_positions = 0
        
        if self.rust_engine and self.rust_engine.is_available():
            try:
                # 从 Rust 引擎获取交易指标
                status = asyncio.run(self.rust_engine.get_system_status())
                active_strategies = len(status.active_tasks)
                # TODO: 添加更多 HFT 特定指标的获取
            except Exception as e:
                logger.debug(f"获取 Rust 指标失败: {e}")
        
        metrics = SystemMetrics(
            timestamp=current_time,
            cpu_percent=cpu_percent,
            memory_percent=memory.percent,
            memory_used_mb=memory.used / 1024 / 1024,
            memory_available_mb=memory.available / 1024 / 1024,
            disk_usage_percent=disk.percent,
            network_bytes_sent=network.bytes_sent,
            network_bytes_recv=network.bytes_recv,
            load_average=load_avg,
            process_count=process_count,
            decision_latency_us=decision_latency,
            order_processing_rate=order_rate,
            active_strategies=active_strategies,
            total_pnl=total_pnl,
            open_positions=open_positions
        )
        
        self.last_collection_time = current_time
        return metrics
    
    def add_metric(self, name: str, value: float, labels: Optional[Dict[str, str]] = None):
        """添加自定义指标"""
        if name not in self.metrics:
            self.metrics[name] = deque(maxlen=self.max_history)
        
        metric_value = MetricValue(
            timestamp=time.time(),
            value=value,
            labels=labels or {}
        )
        
        self.metrics[name].append(metric_value)
    
    def get_metric_history(self, name: str, duration_minutes: int = 60) -> List[MetricValue]:
        """获取指标历史数据"""
        if name not in self.metrics:
            return []
        
        cutoff_time = time.time() - (duration_minutes * 60)
        return [
            metric for metric in self.metrics[name] 
            if metric.timestamp >= cutoff_time
        ]
    
    def get_metric_stats(self, name: str, duration_minutes: int = 60) -> Dict[str, float]:
        """获取指标统计信息"""
        history = self.get_metric_history(name, duration_minutes)
        
        if not history:
            return {}
        
        values = [m.value for m in history]
        
        return {
            "count": len(values),
            "avg": statistics.mean(values),
            "min": min(values),
            "max": max(values),
            "std": statistics.stdev(values) if len(values) > 1 else 0,
            "p50": statistics.median(values),
            "p95": statistics.quantiles(values, n=20)[18] if len(values) >= 20 else max(values),
            "p99": statistics.quantiles(values, n=100)[98] if len(values) >= 100 else max(values)
        }


class AlertManager:
    """告警管理器"""
    
    def __init__(self):
        self.alerts: List[Alert] = []
        self.alert_rules: Dict[str, Dict[str, Any]] = {}
        self.alert_callbacks: List[Callable[[Alert], None]] = []
        self.max_alerts_history = 1000
        
        # 默认告警规则
        self._setup_default_rules()
        
        logger.info("🚨 告警管理器初始化完成")
    
    def _setup_default_rules(self):
        """设置默认告警规则"""
        self.alert_rules = {
            "high_cpu_usage": {
                "metric": "cpu_percent",
                "condition": "greater_than",
                "threshold": 80.0,
                "severity": AlertSeverity.WARNING,
                "duration_seconds": 30,
                "message": "CPU使用率过高: {value:.1f}%"
            },
            "critical_cpu_usage": {
                "metric": "cpu_percent", 
                "condition": "greater_than",
                "threshold": 95.0,
                "severity": AlertSeverity.CRITICAL,
                "duration_seconds": 10,
                "message": "CPU使用率危险: {value:.1f}%"
            },
            "high_memory_usage": {
                "metric": "memory_percent",
                "condition": "greater_than", 
                "threshold": 85.0,
                "severity": AlertSeverity.WARNING,
                "duration_seconds": 60,
                "message": "内存使用率过高: {value:.1f}%"
            },
            "critical_memory_usage": {
                "metric": "memory_percent",
                "condition": "greater_than",
                "threshold": 95.0,
                "severity": AlertSeverity.CRITICAL,
                "duration_seconds": 30,
                "message": "内存使用率危险: {value:.1f}%"
            },
            "high_decision_latency": {
                "metric": "decision_latency_us",
                "condition": "greater_than",
                "threshold": 100.0,  # 100微秒
                "severity": AlertSeverity.WARNING,
                "duration_seconds": 10,
                "message": "决策延迟过高: {value:.1f}μs"
            },
            "critical_decision_latency": {
                "metric": "decision_latency_us",
                "condition": "greater_than", 
                "threshold": 1000.0,  # 1毫秒
                "severity": AlertSeverity.CRITICAL,
                "duration_seconds": 5,
                "message": "决策延迟危险: {value:.1f}μs"
            },
            "low_disk_space": {
                "metric": "disk_usage_percent",
                "condition": "greater_than",
                "threshold": 90.0,
                "severity": AlertSeverity.WARNING,
                "duration_seconds": 300,
                "message": "磁盘空间不足: {value:.1f}%"
            }
        }
    
    def add_alert_rule(
        self, 
        name: str,
        metric: str,
        condition: str,
        threshold: float,
        severity: AlertSeverity,
        duration_seconds: int = 60,
        message: str = None
    ):
        """添加告警规则"""
        self.alert_rules[name] = {
            "metric": metric,
            "condition": condition,
            "threshold": threshold,
            "severity": severity,
            "duration_seconds": duration_seconds,
            "message": message or f"{metric} {condition} {threshold}"
        }
        
        logger.info(f"📝 添加告警规则: {name}")
    
    def check_alerts(self, metrics: SystemMetrics) -> List[Alert]:
        """检查是否触发告警"""
        new_alerts = []
        
        for rule_name, rule in self.alert_rules.items():
            metric_name = rule["metric"]
            
            # 获取指标值
            if hasattr(metrics, metric_name):
                metric_value = getattr(metrics, metric_name)
            else:
                continue
            
            # 检查条件
            triggered = False
            if rule["condition"] == "greater_than":
                triggered = metric_value > rule["threshold"]
            elif rule["condition"] == "less_than":
                triggered = metric_value < rule["threshold"]
            elif rule["condition"] == "equals":
                triggered = abs(metric_value - rule["threshold"]) < 0.001
            
            if triggered:
                # 检查是否已经存在相同的未解决告警
                existing_alert = None
                for alert in self.alerts:
                    if (alert.metric_name == metric_name and 
                        not alert.resolved and
                        abs(alert.threshold - rule["threshold"]) < 0.001):
                        existing_alert = alert
                        break
                
                if not existing_alert:
                    # 创建新告警
                    alert = Alert(
                        id=f"{rule_name}_{int(time.time())}",
                        severity=rule["severity"],
                        title=f"指标告警: {rule_name}",
                        message=rule["message"].format(value=metric_value),
                        timestamp=time.time(),
                        source="monitoring_system",
                        metric_name=metric_name,
                        metric_value=metric_value,
                        threshold=rule["threshold"]
                    )
                    
                    new_alerts.append(alert)
                    self.alerts.append(alert)
                    
                    # 触发回调
                    for callback in self.alert_callbacks:
                        try:
                            callback(alert)
                        except Exception as e:
                            logger.error(f"告警回调失败: {e}")
        
        # 清理旧告警
        if len(self.alerts) > self.max_alerts_history:
            self.alerts = self.alerts[-self.max_alerts_history//2:]
        
        return new_alerts
    
    def resolve_alert(self, alert_id: str) -> bool:
        """解决告警"""
        for alert in self.alerts:
            if alert.id == alert_id and not alert.resolved:
                alert.resolved = True
                alert.resolved_at = time.time()
                logger.info(f"✅ 告警已解决: {alert_id}")
                return True
        return False
    
    def add_alert_callback(self, callback: Callable[[Alert], None]):
        """添加告警回调"""
        self.alert_callbacks.append(callback)
    
    def get_active_alerts(self, severity: Optional[AlertSeverity] = None) -> List[Alert]:
        """获取活跃告警"""
        alerts = [alert for alert in self.alerts if not alert.resolved]
        
        if severity:
            alerts = [alert for alert in alerts if alert.severity == severity]
        
        return sorted(alerts, key=lambda x: x.timestamp, reverse=True)
    
    def get_alert_summary(self) -> Dict[str, int]:
        """获取告警汇总"""
        active_alerts = self.get_active_alerts()
        
        summary = {
            "total": len(active_alerts),
            "critical": len([a for a in active_alerts if a.severity == AlertSeverity.CRITICAL]),
            "error": len([a for a in active_alerts if a.severity == AlertSeverity.ERROR]),
            "warning": len([a for a in active_alerts if a.severity == AlertSeverity.WARNING]),
            "info": len([a for a in active_alerts if a.severity == AlertSeverity.INFO])
        }
        
        return summary


class RealtimeMonitor:
    """
    实时监控系统
    
    集成指标收集、告警管理和实时仪表板：
    - 高频系统监控
    - 智能异常检测
    - 实时性能分析
    - 自动告警通知
    """
    
    def __init__(self, collection_interval: float = 1.0):
        self.collection_interval = collection_interval
        self.metric_collector = MetricCollector()
        self.alert_manager = AlertManager()
        
        # 监控状态
        self.running = False
        self.monitor_task: Optional[asyncio.Task] = None
        
        # 统计信息
        self.total_collections = 0
        self.total_alerts = 0
        self.start_time = time.time()
        
        # 设置默认告警回调
        self.alert_manager.add_alert_callback(self._default_alert_callback)
        
        logger.info(f"🖥️  实时监控系统初始化完成 (采集间隔: {collection_interval}s)")
    
    async def start(self):
        """启动监控"""
        if self.running:
            logger.warning("监控系统已在运行")
            return
        
        self.running = True
        self.start_time = time.time()
        
        # 启动监控循环
        self.monitor_task = asyncio.create_task(self._monitor_loop())
        logger.info("✅ 实时监控系统已启动")
    
    async def stop(self):
        """停止监控"""
        if not self.running:
            return
        
        logger.info("🛑 停止实时监控系统...")
        self.running = False
        
        if self.monitor_task:
            self.monitor_task.cancel()
            try:
                await self.monitor_task
            except asyncio.CancelledError:
                pass
        
        logger.info("✅ 实时监控系统已停止")
    
    async def _monitor_loop(self):
        """监控主循环"""
        logger.info("🔄 监控主循环已启动")
        
        while self.running:
            try:
                start_time = time.time()
                
                # 收集系统指标
                metrics = self.metric_collector.collect_system_metrics()
                
                # 添加自定义指标
                self._add_custom_metrics()
                
                # 检查告警
                new_alerts = self.alert_manager.check_alerts(metrics)
                
                if new_alerts:
                    self.total_alerts += len(new_alerts)
                    logger.warning(f"🚨 触发 {len(new_alerts)} 个新告警")
                
                self.total_collections += 1
                
                # 计算下次采集时间
                elapsed = time.time() - start_time
                sleep_time = max(0, self.collection_interval - elapsed)
                
                if sleep_time > 0:
                    await asyncio.sleep(sleep_time)
                else:
                    logger.warning(f"监控采集耗时过长: {elapsed:.3f}s")
                
            except Exception as e:
                logger.error(f"❌ 监控循环错误: {e}")
                await asyncio.sleep(self.collection_interval)
        
        logger.info("🔄 监控主循环已结束")
    
    def _add_custom_metrics(self):
        """添加自定义指标"""
        current_time = time.time()
        
        # 运行时间
        uptime_seconds = current_time - self.start_time
        self.metric_collector.add_metric("uptime_seconds", uptime_seconds)
        
        # 采集速率
        if self.total_collections > 0:
            collection_rate = self.total_collections / uptime_seconds
            self.metric_collector.add_metric("collection_rate", collection_rate)
        
        # 告警速率
        if self.total_alerts > 0:
            alert_rate = self.total_alerts / uptime_seconds
            self.metric_collector.add_metric("alert_rate", alert_rate)
        
        # 添加模拟的 HFT 指标 (在实际实现中会从 Rust 引擎获取)
        import random
        self.metric_collector.add_metric("decision_latency_us", random.uniform(0.5, 10.0))
        self.metric_collector.add_metric("order_fill_rate", random.uniform(0.8, 1.0))
        self.metric_collector.add_metric("market_data_latency_us", random.uniform(10, 50))
    
    def _default_alert_callback(self, alert: Alert):
        """默认告警回调"""
        severity_emoji = {
            AlertSeverity.INFO: "ℹ️",
            AlertSeverity.WARNING: "⚠️",
            AlertSeverity.ERROR: "❌", 
            AlertSeverity.CRITICAL: "🚨"
        }
        
        emoji = severity_emoji.get(alert.severity, "📢")
        logger.warning(f"{emoji} 告警 [{alert.severity.value.upper()}]: {alert.message}")
    
    def get_dashboard_data(self) -> Dict[str, Any]:
        """获取仪表板数据"""
        # 获取最新的系统指标
        latest_metrics = self.metric_collector.collect_system_metrics()
        
        # 获取告警汇总
        alert_summary = self.alert_manager.get_alert_summary()
        
        # 获取活跃告警
        active_alerts = self.alert_manager.get_active_alerts()
        
        # 获取关键指标统计
        key_metrics_stats = {}
        for metric_name in ["cpu_percent", "memory_percent", "decision_latency_us"]:
            stats = self.metric_collector.get_metric_stats(metric_name, duration_minutes=60)
            if stats:
                key_metrics_stats[metric_name] = stats
        
        return {
            "timestamp": time.time(),
            "system_metrics": asdict(latest_metrics),
            "alert_summary": alert_summary,
            "active_alerts": [asdict(alert) for alert in active_alerts[:10]],  # 最近10个
            "key_metrics_stats": key_metrics_stats,
            "monitoring_stats": {
                "uptime_seconds": time.time() - self.start_time,
                "total_collections": self.total_collections,
                "total_alerts": self.total_alerts,
                "collection_interval": self.collection_interval,
                "rust_available": RUST_AVAILABLE
            }
        }
    
    def get_metric_history_json(self, metric_name: str, duration_minutes: int = 60) -> str:
        """获取指标历史数据 (JSON 格式)"""
        history = self.metric_collector.get_metric_history(metric_name, duration_minutes)
        
        data = {
            "metric_name": metric_name,
            "duration_minutes": duration_minutes,
            "data_points": len(history),
            "history": [
                {
                    "timestamp": m.timestamp,
                    "value": m.value,
                    "labels": m.labels
                }
                for m in history
            ]
        }
        
        return json.dumps(data, indent=2)
    
    def export_monitoring_report(self) -> str:
        """导出监控报告"""
        dashboard = self.get_dashboard_data()
        
        report_lines = [
            "=" * 80,
            "🖥️  HFT 系统实时监控报告",
            "=" * 80,
            f"📅 生成时间: {datetime.fromtimestamp(dashboard['timestamp']).strftime('%Y-%m-%d %H:%M:%S')}",
            f"⏰ 系统运行时间: {dashboard['monitoring_stats']['uptime_seconds']:.1f} 秒",
            f"📊 数据采集次数: {dashboard['monitoring_stats']['total_collections']}",
            f"🚨 总告警数: {dashboard['monitoring_stats']['total_alerts']}",
            "",
            "📈 系统指标:",
            f"   CPU 使用率: {dashboard['system_metrics']['cpu_percent']:.1f}%",
            f"   内存使用率: {dashboard['system_metrics']['memory_percent']:.1f}%",
            f"   磁盘使用率: {dashboard['system_metrics']['disk_usage_percent']:.1f}%",
            f"   活跃策略: {dashboard['system_metrics']['active_strategies']}",
            f"   决策延迟: {dashboard['system_metrics']['decision_latency_us']:.1f}μs",
            "",
            "🚨 告警汇总:",
            f"   严重: {dashboard['alert_summary']['critical']}",
            f"   错误: {dashboard['alert_summary']['error']}",
            f"   警告: {dashboard['alert_summary']['warning']}",
            f"   信息: {dashboard['alert_summary']['info']}",
            "",
        ]
        
        if dashboard['active_alerts']:
            report_lines.extend([
                "🔥 活跃告警:",
                ""
            ])
            for alert in dashboard['active_alerts'][:5]:  # 显示前5个
                alert_time = datetime.fromtimestamp(alert['timestamp']).strftime('%H:%M:%S')
                report_lines.append(f"   [{alert_time}] {alert['severity'].upper()}: {alert['message']}")
            
            if len(dashboard['active_alerts']) > 5:
                report_lines.append(f"   ... 还有 {len(dashboard['active_alerts']) - 5} 个告警")
        else:
            report_lines.append("✅ 无活跃告警")
        
        report_lines.extend([
            "",
            "=" * 80
        ])
        
        return "\n".join(report_lines)


# 测试函数
async def test_realtime_monitoring():
    """测试实时监控系统"""
    logger.info("🧪 开始测试实时监控系统")
    
    monitor = RealtimeMonitor(collection_interval=0.5)  # 500ms采集间隔
    
    # 添加自定义告警规则
    monitor.alert_manager.add_alert_rule(
        name="test_metric_high",
        metric="decision_latency_us",
        condition="greater_than",
        threshold=5.0,
        severity=AlertSeverity.WARNING,
        duration_seconds=1,
        message="测试指标过高: {value:.1f}μs"
    )
    
    await monitor.start()
    
    try:
        # 运行监控 10 秒
        logger.info("⏰ 运行监控 10 秒...")
        await asyncio.sleep(10)
        
        # 获取仪表板数据
        dashboard = monitor.get_dashboard_data()
        logger.info("📊 仪表板数据获取完成")
        
        # 生成报告
        report = monitor.export_monitoring_report()
        logger.info("📋 监控报告:")
        print(report)
        
        # 获取指标统计
        cpu_stats = monitor.metric_collector.get_metric_stats("cpu_percent")
        if cpu_stats:
            logger.info(f"💻 CPU统计: {cpu_stats}")
        
        return {
            "dashboard": dashboard,
            "report": report,
            "test_duration": 10
        }
        
    finally:
        await monitor.stop()


if __name__ == "__main__":
    # 运行测试
    asyncio.run(test_realtime_monitoring())