"""
運營管理工作流
==============

協調日常 HFT 系統運營管理：
- 策略參數監控和調整
- 系統健康監控
- 風險限額管理
- 績效分析報告

這是最核心的工作流，負責系統的日常運營協調
"""

from agno import Workflow
from typing import Dict, Any, Optional, List
from datetime import datetime
import asyncio
import json

from ..agents_v2 import StrategyManager, SystemOperator, RiskSupervisor
from ..evaluators_v2 import SystemHealthEvaluator, PerformanceEvaluator


class OperationalWorkflow(Workflow):
    """
    運營管理工作流
    
    協調 HFT 系統的日常運營管理
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="OperationalWorkflow",
            description="HFT 系統日常運營管理工作流",
            agents=[
                StrategyManager(session_id=session_id),
                SystemOperator(session_id=session_id),
                RiskSupervisor(session_id=session_id)
            ],
            evaluators=[
                SystemHealthEvaluator(),
                PerformanceEvaluator()
            ],
            session_id=session_id
        )
    
    async def execute_daily_operations(
        self,
        symbols: List[str],
        operation_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        執行日常運營管理
        
        Args:
            symbols: 監控的交易對
            operation_config: 運營配置
            
        Returns:
            運營執行結果
        """
        workflow_results = {
            "workflow_id": f"daily_ops_{datetime.now().strftime('%Y%m%d_%H%M%S')}",
            "symbols": symbols,
            "status": "running",
            "stages": {}
        }
        
        try:
            # 階段 1: 系統健康檢查
            print("🔍 階段 1: 執行系統健康檢查...")
            system_operator = self.get_agent("SystemOperator")
            
            health_status = await system_operator.monitor_system_health()
            workflow_results["stages"]["health_check"] = health_status
            
            # 評估系統健康狀況
            health_evaluator = self.get_evaluator("SystemHealthEvaluator")
            health_check = await health_evaluator.evaluate({
                "health_status": health_status
            })
            
            if not health_check["passed"]:
                workflow_results["status"] = "health_issues_detected"
                workflow_results["critical_issues"] = health_check["reason"]
                return workflow_results
            
            # 階段 2: 策略性能監控
            print("📊 階段 2: 監控策略性能...")
            strategy_manager = self.get_agent("StrategyManager")
            
            strategy_performance = {}
            for symbol in symbols:
                performance = await strategy_manager.monitor_strategy_performance(
                    symbol, operation_config.get("monitoring_window", "1h")
                )
                strategy_performance[symbol] = performance
            
            workflow_results["stages"]["strategy_monitoring"] = strategy_performance
            
            # 評估整體性能
            performance_evaluator = self.get_evaluator("PerformanceEvaluator")
            performance_check = await performance_evaluator.evaluate({
                "strategy_performance": strategy_performance
            })
            
            # 階段 3: 風險狀況評估
            print("🛡️ 階段 3: 評估風險狀況...")
            risk_supervisor = self.get_agent("RiskSupervisor")
            
            portfolio_risk = await risk_supervisor.monitor_portfolio_risk(
                operation_config.get("risk_window", "1h")
            )
            
            workflow_results["stages"]["risk_assessment"] = portfolio_risk
            
            # 階段 4: 參數調整決策
            print("⚙️ 階段 4: 評估參數調整需求...")
            adjustment_needed = False
            adjustments = {}
            
            for symbol in symbols:
                perf_data = strategy_performance[symbol]["performance_analysis"]
                market_conditions = operation_config.get("market_conditions", {})
                
                # 根據性能評估決定是否需要調整
                if not performance_check["passed"] or self._needs_adjustment(perf_data):
                    adjustment = await strategy_manager.adjust_strategy_parameters(
                        symbol, perf_data, market_conditions
                    )
                    adjustments[symbol] = adjustment
                    adjustment_needed = True
            
            workflow_results["stages"]["parameter_adjustments"] = adjustments
            
            # 階段 5: 風險限額調整
            print("📏 階段 5: 檢查風險限額...")
            risk_adjustment = await risk_supervisor.adjust_risk_limits(
                portfolio_risk["risk_monitoring"],
                operation_config.get("market_conditions", {})
            )
            
            workflow_results["stages"]["risk_limit_adjustment"] = risk_adjustment
            
            # 決定工作流狀態
            if health_check["passed"] and performance_check["passed"]:
                workflow_results["status"] = "completed_successfully"
            elif adjustment_needed:
                workflow_results["status"] = "completed_with_adjustments"
            else:
                workflow_results["status"] = "completed_with_warnings"
            
            workflow_results["summary"] = self._generate_operation_summary(workflow_results)
            
            return workflow_results
            
        except Exception as e:
            workflow_results["status"] = "failed"
            workflow_results["error"] = str(e)
            return workflow_results
    
    async def handle_performance_alert(
        self,
        alert_data: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        處理性能告警
        
        Args:
            alert_data: 告警數據
            
        Returns:
            處理結果
        """
        try:
            alert_type = alert_data.get("type", "unknown")
            severity = alert_data.get("severity", "medium")
            affected_symbol = alert_data.get("symbol")
            
            print(f"🚨 處理性能告警: {alert_type} - {severity}")
            
            # 根據告警類型選擇處理策略
            if alert_type == "high_latency":
                return await self._handle_latency_alert(alert_data)
            elif alert_type == "poor_performance":
                return await self._handle_performance_alert(alert_data)
            elif alert_type == "system_overload":
                return await self._handle_system_overload(alert_data)
            else:
                return await self._handle_generic_alert(alert_data)
                
        except Exception as e:
            return {
                "success": False,
                "error": str(e),
                "alert_data": alert_data
            }
    
    async def execute_system_maintenance(
        self,
        maintenance_type: str,
        maintenance_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        執行系統維護
        
        Args:
            maintenance_type: 維護類型
            maintenance_config: 維護配置
            
        Returns:
            維護執行結果
        """
        try:
            system_operator = self.get_agent("SystemOperator")
            
            if maintenance_type == "performance_optimization":
                return await system_operator.optimize_system_performance(
                    maintenance_config.get("performance_data", {}),
                    maintenance_config.get("optimization_targets", {})
                )
            elif maintenance_type == "system_backup":
                return await system_operator.perform_system_backup(
                    maintenance_config.get("backup_scope", ["config", "data"]),
                    maintenance_config
                )
            elif maintenance_type == "deployment":
                return await system_operator.manage_system_deployment(
                    maintenance_config
                )
            else:
                return {
                    "success": False,
                    "error": f"Unknown maintenance type: {maintenance_type}"
                }
                
        except Exception as e:
            return {
                "success": False,
                "error": str(e),
                "maintenance_type": maintenance_type
            }
    
    def _needs_adjustment(self, performance_data: str) -> bool:
        """判斷是否需要參數調整（簡化邏輯）"""
        # 實際實現中應該解析性能數據並做出智能判斷
        # 這裡用簡化邏輯
        indicators = ["poor", "declining", "underperform", "high_drawdown"]
        return any(indicator in performance_data.lower() for indicator in indicators)
    
    async def _handle_latency_alert(self, alert_data: Dict[str, Any]) -> Dict[str, Any]:
        """處理延遲告警"""
        system_operator = self.get_agent("SystemOperator")
        
        # 診斷延遲問題
        diagnosis = await system_operator.diagnose_system_issues(
            {"issue_type": "high_latency", "latency_data": alert_data}
        )
        
        return {
            "success": True,
            "alert_type": "high_latency",
            "action_taken": "system_diagnosis",
            "diagnosis_result": diagnosis,
            "recommended_actions": ["optimize_cpu_affinity", "check_network", "review_memory_usage"]
        }
    
    async def _handle_performance_alert(self, alert_data: Dict[str, Any]) -> Dict[str, Any]:
        """處理性能告警"""
        strategy_manager = self.get_agent("StrategyManager")
        
        symbol = alert_data.get("symbol", "BTCUSDT")
        
        # 調整策略參數
        adjustment = await strategy_manager.adjust_strategy_parameters(
            symbol,
            alert_data.get("performance_data", {}),
            alert_data.get("market_conditions", {})
        )
        
        return {
            "success": True,
            "alert_type": "poor_performance",
            "action_taken": "parameter_adjustment",
            "adjustment_result": adjustment
        }
    
    async def _handle_system_overload(self, alert_data: Dict[str, Any]) -> Dict[str, Any]:
        """處理系統過載告警"""
        system_operator = self.get_agent("SystemOperator")
        
        # 系統性能優化
        optimization = await system_operator.optimize_system_performance(
            alert_data.get("system_metrics", {}),
            {"target": "reduce_load", "priority": "high"}
        )
        
        return {
            "success": True,
            "alert_type": "system_overload", 
            "action_taken": "performance_optimization",
            "optimization_result": optimization
        }
    
    async def _handle_generic_alert(self, alert_data: Dict[str, Any]) -> Dict[str, Any]:
        """處理通用告警"""
        return {
            "success": True,
            "alert_type": "generic",
            "action_taken": "logged_for_review",
            "alert_data": alert_data,
            "recommendation": "Manual review required"
        }
    
    def _generate_operation_summary(self, workflow_results: Dict[str, Any]) -> Dict[str, Any]:
        """生成運營總結"""
        return {
            "execution_time": datetime.now().isoformat(),
            "symbols_monitored": len(workflow_results["symbols"]),
            "stages_completed": len(workflow_results["stages"]),
            "overall_status": workflow_results["status"],
            "key_findings": self._extract_key_findings(workflow_results),
            "next_actions": self._recommend_next_actions(workflow_results)
        }
    
    def _extract_key_findings(self, workflow_results: Dict[str, Any]) -> List[str]:
        """提取關鍵發現"""
        findings = []
        
        if workflow_results["status"] == "health_issues_detected":
            findings.append("Critical system health issues detected")
        
        if "parameter_adjustments" in workflow_results["stages"] and workflow_results["stages"]["parameter_adjustments"]:
            findings.append("Strategy parameters adjusted for optimal performance")
        
        if workflow_results["status"] == "completed_successfully":
            findings.append("All systems operating within normal parameters")
        
        return findings
    
    def _recommend_next_actions(self, workflow_results: Dict[str, Any]) -> List[str]:
        """推薦後續行動"""
        actions = []
        
        if workflow_results["status"] == "health_issues_detected":
            actions.append("Immediate system maintenance required")
        
        if workflow_results["status"] == "completed_with_adjustments":
            actions.append("Monitor adjusted parameters for effectiveness")
        
        actions.append("Continue regular monitoring cycle")
        
        return actions