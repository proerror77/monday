"""
風險管理專家 Agent
=================

專責風險控制和管理的 Agent：
- 實時風險監控
- 風險限額管理
- 緊急停損機制
- 合規性檢查
"""

from agno import Agent
from agno.models import Claude
from typing import Dict, Any, Optional, List
import asyncio
import json

from ..tools import RustHFTTools, TradingTools
from ..tools.monitoring_tools import MonitoringTools


class RiskManager(Agent):
    """
    風險管理專家
    
    專精領域：
    - 實時風險計算
    - 限額監控和控制
    - 緊急風險處理
    - 合規性管理
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="RiskManager",
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
        你是一位資深的金融風險管理專家，專精於高頻交易風險控制。

        核心職責：
        1. **實時風險監控**：
           - 監控組合整體風險敞口
           - 跟蹤 VaR、ES、最大回撤等指標
           - 檢測風險集中度問題
           - 監控流動性風險

        2. **限額管理**：
           - 設置和監控各類風險限額
           - 單筆交易限額控制
           - 日內損失限額管理
           - 槓桿倍數控制

        3. **緊急風險處理**：
           - 自動觸發緊急停損
           - 快速平倉執行
           - 異常交易攔截
           - 系統性風險應對

        4. **合規管理**：
           - 交易合規性檢查
           - 監管要求遵循
           - 風險報告生成
           - 審計軌跡維護

        專業要求：
        - 快速風險識別和響應（<100ms）
        - 準確的風險量化和計算
        - 嚴格的合規標準執行
        - 完整的風險記錄和報告

        輸出格式：
        - 提供明確的風險評估
        - 包含風險數值和等級
        - 給出具體的處理建議
        - 記錄風險事件詳情
        """
        
    async def assess_portfolio_risk(
        self,
        portfolio_data: Dict[str, Any],
        market_data: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        評估組合風險
        
        Args:
            portfolio_data: 組合數據
            market_data: 市場數據
            
        Returns:
            風險評估結果
        """
        portfolio_str = json.dumps(portfolio_data, indent=2)
        market_str = json.dumps(market_data, indent=2)
        
        query = f"""
        請評估當前投資組合的風險狀況：
        
        組合數據：
        ```json
        {portfolio_str}
        ```
        
        市場數據：
        ```json
        {market_str}
        ```
        
        風險評估維度：
        1. **市場風險**：
           - VaR（1日、95%和99%置信度）
           - Expected Shortfall (ES)
           - 壓力測試結果
           - 敏感性分析

        2. **流動性風險**：
           - 持倉流動性評分
           - 平倉成本估算
           - 市場衝擊成本
           - 應急流動性需求

        3. **集中度風險**：
           - 單一資產集中度
           - 行業/地區集中度
           - 交易對相關性風險
           - 時間集中度風險

        4. **操作風險**：
           - 系統延遲風險
           - 模型風險評估
           - 執行風險分析
           - 技術故障風險

        請提供詳細的風險分析報告和風險等級評定。
        """
        
        response = await self.arun(query)
        return {"risk_assessment": response.content}
        
    async def check_trade_limits(
        self,
        symbol: str,
        trade_request: Dict[str, Any],
        current_positions: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        檢查交易限額
        
        Args:
            symbol: 交易對
            trade_request: 交易請求
            current_positions: 當前持倉
            
        Returns:
            限額檢查結果
        """
        trade_str = json.dumps(trade_request, indent=2)
        positions_str = json.dumps(current_positions, indent=2)
        
        query = f"""
        請檢查 {symbol} 的交易限額：
        
        交易請求：
        ```json
        {trade_str}
        ```
        
        當前持倉：
        ```json
        {positions_str}
        ```
        
        限額檢查項目：
        1. **交易數量限額**：
           - 單筆交易數量限額
           - 日累計交易數量限額
           - 持倉數量上限
           - 槓桿倍數限制

        2. **風險敞口限額**：
           - 單一資產敞口限額
           - 組合總敞口限額
           - 行業敞口限額
           - 國家/地區敞口限額

        3. **損失限額**：
           - 單筆損失限額
           - 日內損失限額
           - 最大回撤限額
           - VaR 限額

        4. **流動性限額**：
           - 日交易量佔比限額
           - 平倉時間限制
           - 市場衝擊限額
           - 應急準備金要求

        請判斷交易是否可以執行，如不可以請說明原因和建議。
        """
        
        response = await self.arun(query)
        return {"limit_check": response.content, "symbol": symbol}
        
    async def monitor_realtime_risk(self) -> Dict[str, Any]:
        """
        實時風險監控
        
        Returns:
            實時風險監控結果
        """
        query = """
        請執行實時風險監控檢查：
        
        監控範圍：
        1. **即時風險指標**：
           - 實時 P&L 變動
           - 實時 VaR 計算
           - 持倉 Delta、Gamma、Vega
           - 保證金使用率

        2. **異常檢測**：
           - 異常價格波動
           - 異常交易量
           - 系統延遲異常
           - 模型預測異常

        3. **限額使用情況**：
           - 各類限額使用率
           - 接近限額的風險點
           - 超限預警信號
           - 剩餘風險容量

        4. **市場環境變化**：
           - 波動率突然變化
           - 流動性急劇下降
           - 系統性風險信號
           - 黑天鵝事件指標

        請使用監控工具獲取實時數據並進行風險評估。
        """
        
        response = await self.arun(query)
        return {"risk_monitoring": response.content}
        
    async def trigger_emergency_stop(
        self,
        trigger_reason: str,
        affected_symbols: Optional[List[str]] = None
    ) -> Dict[str, Any]:
        """
        觸發緊急停止
        
        Args:
            trigger_reason: 觸發原因
            affected_symbols: 受影響的交易對
            
        Returns:
            緊急停止執行結果
        """
        symbols_str = ", ".join(affected_symbols) if affected_symbols else "ALL"
        
        query = f"""
        請執行緊急風險控制措施：
        
        觸發原因：{trigger_reason}
        受影響交易對：{symbols_str}
        
        緊急程序：
        1. **立即停止交易**：
           - 停止所有新開倉
           - 取消待成交訂單
           - 暫停自動交易算法
           - 通知相關交易員

        2. **快速平倉評估**：
           - 評估當前持倉風險
           - 確定必要平倉部位
           - 計算平倉成本和時間
           - 選擇最優平倉策略

        3. **風險隔離**：
           - 隔離問題交易對
           - 保護健康部位
           - 防止風險傳染
           - 維護系統穩定

        4. **緊急報告**：
           - 生成緊急風險報告
           - 通知風險委員會
           - 記錄處理過程
           - 準備監管報告

        請立即使用緊急停止工具並報告執行結果。
        """
        
        response = await self.arun(query)
        return {"emergency_response": response.content, "trigger_reason": trigger_reason}
        
    async def calculate_var_es(
        self,
        portfolio_data: Dict[str, Any],
        confidence_levels: List[float] = [0.95, 0.99],
        time_horizon: int = 1
    ) -> Dict[str, Any]:
        """
        計算 VaR 和 Expected Shortfall
        
        Args:
            portfolio_data: 組合數據
            confidence_levels: 置信度水平
            time_horizon: 時間範圍（天數）
            
        Returns:
            VaR 和 ES 計算結果
        """
        portfolio_str = json.dumps(portfolio_data, indent=2)
        confidence_str = ", ".join([f"{cl:.0%}" for cl in confidence_levels])
        
        query = f"""
        請計算投資組合的 VaR 和 Expected Shortfall：
        
        組合數據：
        ```json
        {portfolio_str}
        ```
        
        計算參數：
        - 置信度水平：{confidence_str}
        - 時間範圍：{time_horizon} 天
        
        計算方法：
        1. **歷史模擬法**：
           - 使用歷史數據計算 VaR
           - 計算對應的 ES 值
           - 評估模型假設合理性
           - 進行回溯測試

        2. **蒙特卡羅模擬**：
           - 生成隨機價格路徑
           - 計算組合損益分布
           - 估算各置信度的 VaR
           - 計算條件期望損失 (ES)

        3. **參數化方法**：
           - 假設收益率分布
           - 計算組合波動率
           - 應用分布函數計算 VaR
           - 驗證分布假設有效性

        4. **極值理論**：
           - 識別極端損失事件
           - 擬合廣義帕累托分布
           - 估計尾部風險
           - 計算極端情況下的損失

        請提供詳細的風險數值和計算過程說明。
        """
        
        response = await self.arun(query)
        return {"var_es_calculation": response.content}
        
    async def stress_test_portfolio(
        self,
        portfolio_data: Dict[str, Any],
        stress_scenarios: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        投資組合壓力測試
        
        Args:
            portfolio_data: 組合數據
            stress_scenarios: 壓力測試情境
            
        Returns:
            壓力測試結果
        """
        portfolio_str = json.dumps(portfolio_data, indent=2)
        scenarios_str = json.dumps(stress_scenarios, indent=2)
        
        query = f"""
        請對投資組合執行壓力測試：
        
        組合數據：
        ```json
        {portfolio_str}
        ```
        
        壓力情境：
        ```json
        {scenarios_str}
        ```
        
        測試情境：
        1. **歷史情境重現**：
           - 2008 金融危機
           - 2020 疫情衝擊
           - 2022 俄烏衝突
           - Flash Crash 事件

        2. **假設性壓力**：
           - 主要資產價格下跌 20%
           - 波動率翻倍情境
           - 流動性枯竭情境
           - 利率急劇變化

        3. **相關性破裂**：
           - 資產相關性突變至 0.9
           - 分散化效果失效
           - 避險資產失效
           - 系統性風險爆發

        4. **複合壓力事件**：
           - 多重風險因子同時發生
           - 連鎖反應效應
           - 非線性風險放大
           - 極端尾部事件

        請分析每種情境下的組合損失和恢復時間。
        """
        
        response = await self.arun(query)
        return {"stress_test_results": response.content}
        
    async def generate_risk_report(
        self,
        report_type: str = "daily",
        include_recommendations: bool = True
    ) -> Dict[str, Any]:
        """
        生成風險報告
        
        Args:
            report_type: 報告類型（daily, weekly, monthly）
            include_recommendations: 是否包含建議
            
        Returns:
            風險報告
        """
        query = f"""
        請生成 {report_type} 風險管理報告：
        
        報告結構：
        1. **執行摘要**：
           - 主要風險指標概覽
           - 重要風險事件總結
           - 關鍵風險變化趨勢
           - 管理層關注事項

        2. **量化風險指標**：
           - VaR 和 ES 數值及變化
           - 最大回撤和恢復時間
           - 夏普比率和其他風險調整收益
           - 各類風險敞口統計

        3. **限額使用情況**：
           - 各類限額使用率統計
           - 接近限額的預警信號
           - 超限事件記錄和處理
           - 限額調整建議

        4. **風險事件分析**：
           - 期間內風險事件記錄
           - 事件處理過程和結果
           - 經驗教訓和改進措施
           - 預防措施實施情況

        5. **市場風險分析**：
           - 市場環境變化影響
           - 波動率和相關性變化
           - 流動性風險評估
           - 系統性風險監控
        """
        
        if include_recommendations:
            query += """
        
        6. **風險管理建議**：
           - 限額調整建議
           - 風險控制措施優化
           - 新風險因子關注
           - 系統和流程改進
        """
        
        query += "\n\n請使用監控工具獲取實際數據並生成專業的風險報告。"
        
        response = await self.arun(query)
        return {"risk_report": response.content, "report_type": report_type}