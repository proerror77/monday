"""
交易策略專家 Agent
=================

專責交易策略設計和執行的 Agent：
- 策略優化和調整
- 交易信號生成
- 持倉管理
- 策略回測和評估
"""

from agno import Agent
from agno.models import Claude
from typing import Dict, Any, Optional, List
import asyncio
import json

from ..tools import RustHFTTools, TradingTools
from ..tools.monitoring_tools import MonitoringTools


class TradingStrategist(Agent):
    """
    交易策略專家
    
    專精領域：
    - 交易策略設計
    - 信號生成和過濾
    - 風險調整持倉
    - 策略組合優化
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="TradingStrategist",
            model=Claude(id="claude-sonnet-4-20250514"),
            tools=[
                RustHFTTools(),
                TradingTools(),
                MonitoringTools()
            ],
            instructions=self._get_instructions(),
            markdown=True,
            session_id=session_id,
            show_tool_calls=True
        )
        
    def _get_instructions(self) -> str:
        return """
        你是一位頂尖的量化交易策略專家，專精於高頻交易策略設計和執行。

        核心職責：
        1. **策略設計**：
           - 基於 TLOB 模型設計交易策略
           - 優化進場和出場邏輯
           - 設計止損和止盈規則
           - 實施多時間框架策略

        2. **信號生成**：
           - 解析 ML 模型預測信號
           - 結合技術指標確認信號
           - 過濾假信號和噪音
           - 設計信號強度評分

        3. **持倉管理**：
           - 計算最優持倉大小
           - 實施動態止損策略
           - 管理多資產組合
           - 控制最大回撤

        4. **策略優化**：
           - 監控策略表現
           - 調整策略參數
           - A/B 測試新策略
           - 組合策略分配

        專業要求：
        - 深度理解市場微觀結構
        - 精通風險調整回報計算
        - 關注實時執行效率
        - 重視資金管理

        輸出格式：
        - 提供明確的交易指令
        - 包含風險評估
        - 標註信號置信度
        - 記錄策略邏輯
        """
        
    async def generate_trading_signals(
        self,
        symbol: str,
        model_predictions: Dict[str, Any],
        market_context: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        生成交易信號
        
        Args:
            symbol: 交易對
            model_predictions: ML 模型預測
            market_context: 市場環境
            
        Returns:
            交易信號和建議
        """
        predictions_str = json.dumps(model_predictions, indent=2)
        context_str = json.dumps(market_context, indent=2)
        
        query = f"""
        請為 {symbol} 生成交易信號：
        
        模型預測：
        ```json
        {predictions_str}
        ```
        
        市場環境：
        ```json
        {context_str}
        ```
        
        信號生成流程：
        1. **預測解析**：解讀模型預測結果（上漲/下跌/不變）
        2. **置信度評估**：評估預測置信度和可靠性
        3. **市場確認**：結合技術指標和市場環境
        4. **風險評估**：評估當前市場風險水平
        5. **信號強度**：計算最終信號強度和方向
        6. **交易建議**：生成具體的交易指令
        
        請提供詳細的信號分析和交易建議。
        """
        
        response = await self.arun(query)
        return {"signals": response.content, "symbol": symbol}
        
    async def optimize_strategy_parameters(
        self,
        symbol: str,
        strategy_config: Dict[str, Any],
        performance_history: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        優化策略參數
        
        Args:
            symbol: 交易對
            strategy_config: 當前策略配置
            performance_history: 歷史表現數據
            
        Returns:
            優化後的策略參數
        """
        config_str = json.dumps(strategy_config, indent=2)
        history_str = json.dumps(performance_history, indent=2)
        
        query = f"""
        請優化 {symbol} 的策略參數：
        
        當前策略配置：
        ```json
        {config_str}
        ```
        
        歷史表現：
        ```json
        {history_str}
        ```
        
        優化流程：
        1. **表現分析**：分析策略歷史表現和痛點
        2. **參數敏感性**：識別關鍵參數和敏感度
        3. **回測驗證**：對新參數進行回測驗證
        4. **風險評估**：評估參數變更的風險
        5. **參數建議**：推薦最優參數組合
        6. **實施計劃**：提供參數更新計劃
        
        請提供參數優化建議和風險評估。
        """
        
        response = await self.arun(query)
        return {"optimization": response.content, "symbol": symbol}
        
    async def design_portfolio_strategy(
        self,
        symbols: List[str],
        capital_allocation: Dict[str, float],
        risk_constraints: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        設計投資組合策略
        
        Args:
            symbols: 交易對列表
            capital_allocation: 資金分配
            risk_constraints: 風險約束
            
        Returns:
            組合策略設計
        """
        symbols_str = ", ".join(symbols)
        allocation_str = json.dumps(capital_allocation, indent=2)
        constraints_str = json.dumps(risk_constraints, indent=2)
        
        query = f"""
        請設計多資產組合策略：
        
        交易對：{symbols_str}
        
        資金分配：
        ```json
        {allocation_str}
        ```
        
        風險約束：
        ```json
        {constraints_str}
        ```
        
        設計要素：
        1. **資產相關性**：分析資產間相關性和分散化效果
        2. **風險平價**：實現風險平價或其他分配方法
        3. **動態調整**：設計動態再平衡機制
        4. **風險控制**：組合層面的風險管理
        5. **績效歸因**：設計績效分析框架
        6. **執行策略**：優化交易執行和滑點控制
        
        請提供完整的組合策略框架。
        """
        
        response = await self.arun(query)
        return {"portfolio_strategy": response.content, "symbols": symbols}
        
    async def backtest_strategy(
        self,
        symbol: str,
        strategy_config: Dict[str, Any],
        backtest_period: str
    ) -> Dict[str, Any]:
        """
        策略回測
        
        Args:
            symbol: 交易對
            strategy_config: 策略配置
            backtest_period: 回測期間
            
        Returns:
            回測結果
        """
        config_str = json.dumps(strategy_config, indent=2)
        
        query = f"""
        請執行 {symbol} 的策略回測：
        
        策略配置：
        ```json
        {config_str}
        ```
        
        回測期間：{backtest_period}
        
        回測流程：
        1. **數據準備**：準備歷史數據和模型預測
        2. **策略執行**：模擬策略執行過程
        3. **交易記錄**：記錄所有交易和持倉變化
        4. **績效計算**：計算收益率、Sharpe 比率等指標
        5. **風險分析**：分析最大回撤、VaR 等風險指標
        6. **結果報告**：生成詳細的回測報告
        
        請使用工具執行實際回測並提供分析。
        """
        
        response = await self.arun(query)
        return {"backtest_result": response.content, "symbol": symbol}
        
    async def monitor_live_performance(
        self,
        symbol: str,
        time_window: str = "1h"
    ) -> Dict[str, Any]:
        """
        監控實時策略表現
        
        Args:
            symbol: 交易對
            time_window: 監控時間窗口
            
        Returns:
            策略表現報告
        """
        query = f"""
        請監控 {symbol} 在 {time_window} 時間窗口的實時策略表現：
        
        監控內容：
        1. **實時 P&L**：當前未實現和已實現損益
        2. **交易統計**：成交次數、勝率、平均持倉時間
        3. **風險指標**：當前倉位風險、VaR、波動率
        4. **執行品質**：滑點、延遲、成交率
        5. **信號品質**：信號準確率、假信號比例
        6. **異常檢測**：識別異常交易或表現
        
        請使用監控工具獲取實時數據並進行分析。
        """
        
        response = await self.arun(query)
        return {"performance_report": response.content, "symbol": symbol}
        
    async def adjust_strategy_realtime(
        self,
        symbol: str,
        current_performance: Dict[str, Any],
        market_conditions: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        實時策略調整
        
        Args:
            symbol: 交易對
            current_performance: 當前表現
            market_conditions: 市場條件
            
        Returns:
            策略調整建議
        """
        performance_str = json.dumps(current_performance, indent=2)
        conditions_str = json.dumps(market_conditions, indent=2)
        
        query = f"""
        請基於實時表現對 {symbol} 策略進行調整：
        
        當前表現：
        ```json
        {performance_str}
        ```
        
        市場條件：
        ```json
        {conditions_str}
        ```
        
        調整評估：
        1. **表現診斷**：診斷當前策略表現問題
        2. **市場適應**：評估策略與市場條件的匹配度
        3. **參數調整**：建議具體的參數調整方案
        4. **風險管理**：加強風險控制措施
        5. **執行計劃**：制定調整實施計劃
        6. **監控方案**：設定調整後的監控指標
        
        請提供具體的策略調整建議和實施計劃。
        """
        
        response = await self.arun(query)
        return {"adjustment_plan": response.content, "symbol": symbol}
        
    async def generate_trade_execution_plan(
        self,
        symbol: str,
        signal: Dict[str, Any],
        current_position: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        生成交易執行計劃
        
        Args:
            symbol: 交易對
            signal: 交易信號
            current_position: 當前持倉
            
        Returns:
            交易執行計劃
        """
        signal_str = json.dumps(signal, indent=2)
        position_str = json.dumps(current_position, indent=2)
        
        query = f"""
        請為 {symbol} 生成詳細的交易執行計劃：
        
        交易信號：
        ```json
        {signal_str}
        ```
        
        當前持倉：
        ```json
        {position_str}
        ```
        
        執行計劃：
        1. **交易決策**：確定具體的交易動作（買入/賣出/持有）
        2. **倉位計算**：計算目標倉位和交易數量
        3. **執行策略**：選擇最優執行算法（TWAP、VWAP等）
        4. **風險控制**：設置止損止盈點位
        5. **時間安排**：確定最佳執行時機
        6. **監控計劃**：設定執行過程監控指標
        
        請提供可直接執行的交易指令和風險控制措施。
        """
        
        response = await self.arun(query)
        return {"execution_plan": response.content, "symbol": symbol}