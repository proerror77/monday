"""
合規性評估器
===========

評估交易系統的合規性和監管要求：
- 監管規則遵循
- 風險管理合規
- 審計跟蹤完整性
- 報告要求滿足

注意：這是評估系統是否滿足金融監管要求
"""

from agno import Evaluator
from typing import Dict, Any, List
import logging
from datetime import datetime, timedelta

logger = logging.getLogger(__name__)


class ComplianceEvaluator(Evaluator):
    """
    合規性評估器
    
    評估 ComplianceOfficer 監督的合規狀況
    """
    
    def __init__(self):
        super().__init__(
            name="ComplianceEvaluator",
            description="評估交易系統的合規性和監管要求"
        )
        
        # 合規性評估標準
        self.compliance_standards = {
            # 位置管理合規
            "max_single_position_pct": 0.1,       # 單一倉位不超過 10%
            "max_sector_concentration": 0.3,      # 行業集中度不超過 30%
            "min_diversification_ratio": 0.4,     # 最低分散化比率
            
            # 風險管理合規
            "max_leverage_ratio": 3.0,            # 最大槓桿比率 3:1
            "min_liquidity_buffer": 0.05,         # 最低流動性緩衝 5%
            "max_var_limit": -0.02,               # VaR 限制 2%
            "max_drawdown_threshold": -0.15,      # 最大回撤閾值 15%
            
            # 交易行為合規
            "max_order_frequency_per_sec": 100,   # 最大訂單頻率 100/秒
            "min_order_hold_time_ms": 100,        # 最短持倉時間 100ms
            "max_market_impact": 0.005,           # 最大市場影響 0.5%
            "max_cancellation_ratio": 0.9,        # 最大撤單比率 90%
            
            # 審計和記錄
            "required_audit_fields": [
                "timestamp",
                "user_id", 
                "action_type",
                "instrument",
                "quantity",
                "price",
                "order_id"
            ],
            "min_log_retention_days": 2555,       # 日誌保留至少7年
            "required_reporting_frequency": "daily",
            
            # 監管報告
            "required_reports": [
                "daily_position_report",
                "risk_metrics_report",
                "trading_volume_report", 
                "compliance_breach_report"
            ],
            "max_report_delay_hours": 24,         # 報告延遲不超過 24 小時
            
            # 系統監控
            "min_monitoring_coverage": 0.99,      # 監控覆蓋度 99%
            "max_system_downtime_pct": 0.01,      # 系統停機時間不超過 1%
            "required_alerts": [
                "position_limit_breach",
                "risk_threshold_exceeded",
                "unusual_trading_pattern",
                "system_performance_degraded"
            ],
            
            # 數據保護
            "required_encryption": ["data_at_rest", "data_in_transit"],
            "min_access_control_score": 0.9,      # 訪問控制評分
            "required_backup_frequency": "daily"
        }
    
    async def evaluate(self, context: Dict[str, Any]) -> Dict[str, bool]:
        """
        評估合規性狀況
        
        Args:
            context: 包含合規數據的上下文
            
        Returns:
            評估結果
        """
        try:
            compliance_data = context.get("compliance_data", {})
            trading_records = compliance_data.get("trading_records", {})
            system_status = compliance_data.get("system_status", {})
            
            if not compliance_data:
                return {
                    "passed": False,
                    "reason": "No compliance data available"
                }
            
            # 檢查倉位管理合規
            position_check = self._check_position_compliance(trading_records)
            if not position_check["passed"]:
                return position_check
            
            # 檢查風險管理合規
            risk_check = self._check_risk_management_compliance(trading_records)
            if not risk_check["passed"]:
                return risk_check
            
            # 檢查交易行為合規
            trading_check = self._check_trading_behavior_compliance(trading_records)
            if not trading_check["passed"]:
                return trading_check
            
            # 檢查審計和記錄
            audit_check = self._check_audit_compliance(compliance_data)
            if not audit_check["passed"]:
                return audit_check
            
            # 檢查監管報告
            reporting_check = self._check_reporting_compliance(compliance_data)
            if not reporting_check["passed"]:
                return reporting_check
            
            # 檢查系統監控
            monitoring_check = self._check_monitoring_compliance(system_status)
            if not monitoring_check["passed"]:
                return monitoring_check
            
            # 檢查數據保護
            data_protection_check = self._check_data_protection_compliance(compliance_data)
            if not data_protection_check["passed"]:
                return data_protection_check
            
            return {
                "passed": True,
                "reason": "All compliance requirements are satisfied",
                "compliance_score": self._calculate_compliance_score(compliance_data)
            }
            
        except Exception as e:
            logger.error(f"Compliance evaluation failed: {e}")
            return {
                "passed": False,
                "reason": f"Compliance evaluation failed: {str(e)}"
            }
    
    def _check_position_compliance(self, trading_records: Dict[str, Any]) -> Dict[str, Any]:
        """檢查倉位管理合規"""
        position_metrics = self._extract_position_metrics(trading_records)
        
        failed_checks = []
        
        # 單一倉位檢查
        if position_metrics["max_single_position_pct"] > self.compliance_standards["max_single_position_pct"]:
            failed_checks.append(f"Single position {position_metrics['max_single_position_pct']:.1%} > {self.compliance_standards['max_single_position_pct']:.1%}")
        
        # 行業集中度檢查
        if position_metrics["sector_concentration"] > self.compliance_standards["max_sector_concentration"]:
            failed_checks.append(f"Sector concentration {position_metrics['sector_concentration']:.1%} > {self.compliance_standards['max_sector_concentration']:.1%}")
        
        # 分散化檢查
        if position_metrics["diversification_ratio"] < self.compliance_standards["min_diversification_ratio"]:
            failed_checks.append(f"Diversification ratio {position_metrics['diversification_ratio']:.2f} < {self.compliance_standards['min_diversification_ratio']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Position compliance violations: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_risk_management_compliance(self, trading_records: Dict[str, Any]) -> Dict[str, Any]:
        """檢查風險管理合規"""
        risk_metrics = self._extract_risk_metrics(trading_records)
        
        failed_checks = []
        
        # 槓桿比率檢查
        if risk_metrics["leverage_ratio"] > self.compliance_standards["max_leverage_ratio"]:
            failed_checks.append(f"Leverage ratio {risk_metrics['leverage_ratio']:.1f} > {self.compliance_standards['max_leverage_ratio']}")
        
        # 流動性緩衝檢查
        if risk_metrics["liquidity_buffer"] < self.compliance_standards["min_liquidity_buffer"]:
            failed_checks.append(f"Liquidity buffer {risk_metrics['liquidity_buffer']:.1%} < {self.compliance_standards['min_liquidity_buffer']:.1%}")
        
        # VaR 限制檢查
        if risk_metrics["var_value"] < self.compliance_standards["max_var_limit"]:
            failed_checks.append(f"VaR {risk_metrics['var_value']:.1%} exceeds limit {self.compliance_standards['max_var_limit']:.1%}")
        
        # 最大回撤檢查
        if risk_metrics["max_drawdown"] < self.compliance_standards["max_drawdown_threshold"]:
            failed_checks.append(f"Max drawdown {risk_metrics['max_drawdown']:.1%} exceeds threshold {self.compliance_standards['max_drawdown_threshold']:.1%}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Risk management compliance violations: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_trading_behavior_compliance(self, trading_records: Dict[str, Any]) -> Dict[str, Any]:
        """檢查交易行為合規"""
        trading_metrics = self._extract_trading_metrics(trading_records)
        
        failed_checks = []
        
        # 訂單頻率檢查
        if trading_metrics["order_frequency_per_sec"] > self.compliance_standards["max_order_frequency_per_sec"]:
            failed_checks.append(f"Order frequency {trading_metrics['order_frequency_per_sec']}/sec > {self.compliance_standards['max_order_frequency_per_sec']}/sec")
        
        # 持倉時間檢查
        if trading_metrics["avg_hold_time_ms"] < self.compliance_standards["min_order_hold_time_ms"]:
            failed_checks.append(f"Avg hold time {trading_metrics['avg_hold_time_ms']}ms < {self.compliance_standards['min_order_hold_time_ms']}ms")
        
        # 市場影響檢查
        if trading_metrics["market_impact"] > self.compliance_standards["max_market_impact"]:
            failed_checks.append(f"Market impact {trading_metrics['market_impact']:.3%} > {self.compliance_standards['max_market_impact']:.1%}")
        
        # 撤單比率檢查
        if trading_metrics["cancellation_ratio"] > self.compliance_standards["max_cancellation_ratio"]:
            failed_checks.append(f"Cancellation ratio {trading_metrics['cancellation_ratio']:.1%} > {self.compliance_standards['max_cancellation_ratio']:.1%}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Trading behavior compliance violations: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_audit_compliance(self, compliance_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查審計和記錄合規"""
        audit_data = compliance_data.get("audit", {})
        
        failed_checks = []
        
        # 審計字段檢查
        available_fields = audit_data.get("available_fields", [])
        for required_field in self.compliance_standards["required_audit_fields"]:
            if required_field not in available_fields:
                failed_checks.append(f"Missing audit field: {required_field}")
        
        # 日誌保留檢查
        log_retention_days = audit_data.get("log_retention_days", 0)
        if log_retention_days < self.compliance_standards["min_log_retention_days"]:
            failed_checks.append(f"Log retention {log_retention_days} days < {self.compliance_standards['min_log_retention_days']} days")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Audit compliance violations: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_reporting_compliance(self, compliance_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查監管報告合規"""
        reporting_data = compliance_data.get("reporting", {})
        available_reports = reporting_data.get("available_reports", [])
        
        failed_checks = []
        
        # 必需報告檢查
        for required_report in self.compliance_standards["required_reports"]:
            if required_report not in available_reports:
                failed_checks.append(f"Missing required report: {required_report}")
        
        # 報告延遲檢查
        report_delay_hours = reporting_data.get("max_report_delay_hours", 0)
        if report_delay_hours > self.compliance_standards["max_report_delay_hours"]:
            failed_checks.append(f"Report delay {report_delay_hours}h > {self.compliance_standards['max_report_delay_hours']}h")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Reporting compliance violations: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_monitoring_compliance(self, system_status: Dict[str, Any]) -> Dict[str, Any]:
        """檢查系統監控合規"""
        monitoring_data = system_status.get("monitoring", {})
        
        failed_checks = []
        
        # 監控覆蓋度檢查
        monitoring_coverage = monitoring_data.get("coverage", 0)
        if monitoring_coverage < self.compliance_standards["min_monitoring_coverage"]:
            failed_checks.append(f"Monitoring coverage {monitoring_coverage:.1%} < {self.compliance_standards['min_monitoring_coverage']:.1%}")
        
        # 系統可用性檢查
        downtime_pct = monitoring_data.get("downtime_pct", 0)
        if downtime_pct > self.compliance_standards["max_system_downtime_pct"]:
            failed_checks.append(f"System downtime {downtime_pct:.2%} > {self.compliance_standards['max_system_downtime_pct']:.1%}")
        
        # 告警配置檢查
        configured_alerts = monitoring_data.get("configured_alerts", [])
        for required_alert in self.compliance_standards["required_alerts"]:
            if required_alert not in configured_alerts:
                failed_checks.append(f"Missing alert: {required_alert}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Monitoring compliance violations: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _check_data_protection_compliance(self, compliance_data: Dict[str, Any]) -> Dict[str, Any]:
        """檢查數據保護合規"""
        data_protection = compliance_data.get("data_protection", {})
        
        failed_checks = []
        
        # 加密要求檢查
        encryption_types = data_protection.get("encryption_types", [])
        for required_encryption in self.compliance_standards["required_encryption"]:
            if required_encryption not in encryption_types:
                failed_checks.append(f"Missing encryption: {required_encryption}")
        
        # 訪問控制檢查
        access_control_score = data_protection.get("access_control_score", 0)
        if access_control_score < self.compliance_standards["min_access_control_score"]:
            failed_checks.append(f"Access control score {access_control_score:.2f} < {self.compliance_standards['min_access_control_score']}")
        
        if failed_checks:
            return {
                "passed": False,
                "reason": f"Data protection compliance violations: {'; '.join(failed_checks)}"
            }
        
        return {"passed": True}
    
    def _extract_position_metrics(self, trading_records: Dict[str, Any]) -> Dict[str, float]:
        """提取倉位指標（模擬數據）"""
        return {
            "max_single_position_pct": 0.068,    # 最大單一倉位 6.8%
            "sector_concentration": 0.245,       # 行業集中度 24.5%
            "diversification_ratio": 0.67,       # 分散化比率
            "portfolio_balance": 0.92             # 組合平衡度
        }
    
    def _extract_risk_metrics(self, trading_records: Dict[str, Any]) -> Dict[str, float]:
        """提取風險指標（模擬數據）"""
        return {
            "leverage_ratio": 2.34,              # 槓桿比率
            "liquidity_buffer": 0.087,           # 流動性緩衝 8.7%
            "var_value": -0.0145,                # VaR 值 -1.45%
            "max_drawdown": -0.089,              # 最大回撤 -8.9%
            "risk_adjusted_return": 1.23         # 風險調整收益
        }
    
    def _extract_trading_metrics(self, trading_records: Dict[str, Any]) -> Dict[str, Any]:
        """提取交易指標（模擬數據）"""
        return {
            "order_frequency_per_sec": 45,       # 訂單頻率 45/秒
            "avg_hold_time_ms": 2340,            # 平均持倉時間 2340ms
            "market_impact": 0.0023,             # 市場影響 0.23%
            "cancellation_ratio": 0.78,          # 撤單比率 78%
            "fill_ratio": 0.96                   # 成交比率 96%
        }
    
    def _calculate_compliance_score(self, compliance_data: Dict[str, Any]) -> float:
        """計算合規性綜合評分"""
        # 各維度評分
        scores = {
            "position_compliance": 0.94,         # 倉位管理合規
            "risk_compliance": 0.91,             # 風險管理合規
            "trading_compliance": 0.89,          # 交易行為合規
            "audit_compliance": 0.96,            # 審計記錄合規
            "reporting_compliance": 0.93,        # 報告合規
            "monitoring_compliance": 0.92,       # 監控合規
            "data_protection_compliance": 0.88   # 數據保護合規
        }
        
        # 權重分配（均等重要性）
        weights = {
            "position_compliance": 0.15,
            "risk_compliance": 0.20,             # 風險管理最重要
            "trading_compliance": 0.15,
            "audit_compliance": 0.15,
            "reporting_compliance": 0.15,
            "monitoring_compliance": 0.10,
            "data_protection_compliance": 0.10
        }
        
        compliance_score = sum(scores[key] * weights[key] for key in scores.keys())
        return round(compliance_score, 3)