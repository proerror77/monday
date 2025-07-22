"""
交易執行工作流
==============

定義完整的交易執行工作流：
- 市場分析和信號生成
- 風險評估和限額檢查
- 交易執行和監控
"""

from agno import Workflow
from typing import Dict, Any, Optional
from ..agents import DataAnalyst, TradingStrategist, RiskManager
from ..evaluators import TradingOpportunityEvaluator, RiskLevelEvaluator


class TradingWorkflow(Workflow):
    """
    智能交易執行工作流
    
    自動化交易決策和執行流程
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="IntelligentTradingWorkflow",
            description="智能交易執行工作流",
            agents=[
                DataAnalyst(session_id=session_id),
                TradingStrategist(session_id=session_id), 
                RiskManager(session_id=session_id)
            ],
            evaluators=[
                TradingOpportunityEvaluator(),
                RiskLevelEvaluator()
            ],
            session_id=session_id
        )
    
    async def execute_trading_cycle(
        self,
        symbol: str,
        model_predictions: Dict[str, Any],
        market_context: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        執行完整的交易週期
        
        Args:
            symbol: 交易對
            model_predictions: ML 模型預測
            market_context: 市場環境
            
        Returns:
            交易週期執行結果
        """
        cycle_results = {
            "cycle_id": f"trading_{symbol}_{self.session_id}",
            "symbol": symbol,
            "status": "running",
            "stages": {}
        }
        
        try:
            # 階段 1: 市場分析
            print(f"📊 階段 1: 分析 {symbol} 市場狀況...")
            data_analyst = self.get_agent("DataAnalyst")
            
            market_analysis = await data_analyst.analyze_market_data(
                symbol, "5m", 100
            )
            
            cycle_results["stages"]["market_analysis"] = market_analysis
            
            # 階段 2: 交易信號生成
            print(f"🎯 階段 2: 生成 {symbol} 交易信號...")
            trading_strategist = self.get_agent("TradingStrategist")
            
            trading_signals = await trading_strategist.generate_trading_signals(
                symbol, model_predictions, market_context
            )
            
            cycle_results["stages"]["signal_generation"] = trading_signals
            
            # 評估交易機會
            opportunity_evaluator = self.get_evaluator("TradingOpportunityEvaluator")
            opportunity_check = await opportunity_evaluator.evaluate({
                "trading_signals": trading_signals,
                "market_analysis": market_analysis
            })
            
            if not opportunity_check["passed"]:
                cycle_results["status"] = "no_trade"
                cycle_results["reason"] = "No viable trading opportunity"
                return cycle_results
            
            # 階段 3: 風險評估
            print(f"🛡️ 階段 3: 評估 {symbol} 交易風險...")
            risk_manager = self.get_agent("RiskManager")
            
            # 模擬當前組合數據
            portfolio_data = {
                "positions": {
                    symbol: {"position": 100, "value": 4523.45, "unrealized_pnl": 23.45}
                },
                "total_value": 10000.0,
                "available_capital": 5000.0
            }
            
            risk_assessment = await risk_manager.assess_portfolio_risk(
                portfolio_data, market_context
            )
            
            cycle_results["stages"]["risk_assessment"] = risk_assessment
            
            # 風險水平評估
            risk_evaluator = self.get_evaluator("RiskLevelEvaluator")
            risk_check = await risk_evaluator.evaluate({
                "risk_assessment": risk_assessment
            })
            
            if not risk_check["passed"]:
                cycle_results["status"] = "risk_rejected"
                cycle_results["reason"] = "Risk level too high"
                return cycle_results
            
            # 階段 4: 限額檢查
            print(f"✅ 階段 4: 檢查交易限額...")
            trade_request = {
                "symbol": symbol,
                "direction": trading_signals.get("signals", {}).get("direction", "hold"),
                "size": 100,
                "confidence": model_predictions.get("confidence", 0.5)
            }
            
            limit_check = await risk_manager.check_trade_limits(
                symbol, trade_request, portfolio_data["positions"]
            )
            
            cycle_results["stages"]["limit_check"] = limit_check
            
            if not limit_check.get("limit_check", {}).get("can_execute", True):
                cycle_results["status"] = "limit_rejected"
                cycle_results["reason"] = "Trade exceeds risk limits"
                return cycle_results
            
            # 階段 5: 執行計劃生成
            print(f"📋 階段 5: 生成執行計劃...")
            execution_plan = await trading_strategist.generate_trade_execution_plan(
                symbol,
                trading_signals.get("signals", {}),
                portfolio_data["positions"].get(symbol, {})
            )
            
            cycle_results["stages"]["execution_plan"] = execution_plan
            cycle_results["status"] = "ready_to_execute"
            cycle_results["recommended_action"] = execution_plan.get("execution_plan", {})
            
            return cycle_results
            
        except Exception as e:
            cycle_results["status"] = "failed"
            cycle_results["error"] = str(e)
            return cycle_results
    
    async def monitor_live_trading(
        self,
        symbol: str,
        monitoring_duration: str = "1h"
    ) -> Dict[str, Any]:
        """
        監控實時交易
        
        Args:
            symbol: 交易對
            monitoring_duration: 監控時長
            
        Returns:
            監控結果
        """
        try:
            trading_strategist = self.get_agent("TradingStrategist")
            
            performance_report = await trading_strategist.monitor_live_performance(
                symbol, monitoring_duration
            )
            
            return {
                "success": True,
                "monitoring_id": f"monitor_{symbol}_{self.session_id}",
                "performance_report": performance_report
            }
            
        except Exception as e:
            return {
                "success": False,
                "error": str(e),
                "symbol": symbol
            }
    
    async def handle_emergency_stop(
        self,
        trigger_reason: str,
        affected_symbols: Optional[list] = None
    ) -> Dict[str, Any]:
        """
        處理緊急停止
        
        Args:
            trigger_reason: 觸發原因
            affected_symbols: 受影響的交易對
            
        Returns:
            緊急處理結果
        """
        try:
            risk_manager = self.get_agent("RiskManager")
            
            emergency_response = await risk_manager.trigger_emergency_stop(
                trigger_reason, affected_symbols
            )
            
            return {
                "success": True,
                "emergency_id": f"emergency_{self.session_id}",
                "response": emergency_response
            }
            
        except Exception as e:
            return {
                "success": False,
                "error": str(e),
                "trigger_reason": trigger_reason
            }