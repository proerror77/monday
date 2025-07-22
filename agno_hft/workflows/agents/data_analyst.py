"""
數據分析專家 Agent
=================

專責市場數據分析和研究的 Agent：
- 技術分析和市場趨勢識別
- 數據質量評估
- 特徵重要性分析
- 市場regime detection
"""

from agno import Agent
from agno.models import Claude
from typing import Dict, Any, Optional
import asyncio

from ..tools import DataProcessingTools, RustHFTTools
from ..tools.monitoring_tools import MonitoringTools


class DataAnalyst(Agent):
    """
    數據分析專家
    
    專精領域：
    - 市場數據分析
    - 技術指標計算
    - 趨勢識別
    - 數據質量監控
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="DataAnalyst",
            model=Claude(id="claude-sonnet-4-20250514"),
            tools=[
                DataProcessingTools(),
                RustHFTTools(),
                MonitoringTools()
            ],
            instructions=self._get_instructions(),
            markdown=True,
            session_id=session_id,
            show_tool_calls=True
        )
        
    def _get_instructions(self) -> str:
        return """
        你是一位資深的量化交易數據分析專家，專精於高頻交易數據分析。

        核心職責：
        1. **市場數據分析**：
           - 分析 LOB（限價訂單簿）數據結構和質量
           - 識別市場微觀結構模式
           - 檢測數據異常和缺失

        2. **技術分析**：
           - 計算技術指標（移動平均、RSI、MACD等）
           - 識別支撐阻力位
           - 分析成交量模式

        3. **趨勢識別**：
           - 檢測短期和長期趨勢
           - 識別市場轉折點
           - 分析波動率變化

        4. **數據質量監控**：
           - 驗證數據完整性
           - 檢測數據延遲和異常
           - 監控數據收集管道

        專業要求：
        - 使用精確的數值分析
        - 提供具體的統計指標
        - 基於數據給出客觀結論
        - 識別風險和機會

        輸出格式：
        - 使用表格展示數據
        - 提供視覺化建議
        - 包含置信度評估
        - 標註關鍵發現
        """
        
    async def analyze_market_data(
        self,
        symbol: str,
        timeframe: str = "1h",
        lookback_periods: int = 100
    ) -> Dict[str, Any]:
        """
        分析市場數據
        
        Args:
            symbol: 交易對
            timeframe: 時間範圍
            lookback_periods: 回看週期
            
        Returns:
            分析結果
        """
        query = f"""
        請對 {symbol} 進行深入的市場數據分析：
        
        1. 獲取最近 {lookback_periods} 個週期的 {timeframe} 數據
        2. 分析價格行為和成交量模式
        3. 計算關鍵技術指標
        4. 識別當前市場趨勢
        5. 評估數據質量
        
        請使用工具獲取實際數據並進行分析。
        """
        
        response = await self.arun(query)
        return {"analysis": response.content, "symbol": symbol}
        
    async def assess_data_quality(
        self,
        data_source: str,
        time_range: str
    ) -> Dict[str, Any]:
        """
        評估數據質量
        
        Args:
            data_source: 數據源
            time_range: 時間範圍
            
        Returns:
            數據質量報告
        """
        query = f"""
        請評估 {data_source} 在 {time_range} 期間的數據質量：
        
        1. 檢查數據完整性（缺失值、重複值）
        2. 驗證數據格式和類型
        3. 分析數據延遲和時間戳問題
        4. 檢測異常值和離群點
        5. 評估數據採樣頻率
        6. 生成數據質量評分
        
        請提供詳細的質量報告和改進建議。
        """
        
        response = await self.arun(query)
        return {"quality_report": response.content, "data_source": data_source}
        
    async def detect_market_regime(
        self,
        symbol: str,
        analysis_period: str = "30d"
    ) -> Dict[str, Any]:
        """
        檢測市場制度
        
        Args:
            symbol: 交易對
            analysis_period: 分析期間
            
        Returns:
            市場制度分析結果
        """
        query = f"""
        請對 {symbol} 進行市場制度檢測分析：
        
        1. 分析 {analysis_period} 的價格和波動率數據
        2. 識別不同的市場制度（趨勢、震蕩、突破等）
        3. 計算制度轉換概率
        4. 分析各制度的持續時間
        5. 預測當前制度的可能變化
        
        請提供制度分類結果和交易建議。
        """
        
        response = await self.arun(query)
        return {"regime_analysis": response.content, "symbol": symbol}
        
    async def calculate_feature_importance(
        self,
        symbol: str,
        target_variable: str = "price_direction"
    ) -> Dict[str, Any]:
        """
        計算特徵重要性
        
        Args:
            symbol: 交易對
            target_variable: 目標變量
            
        Returns:
            特徵重要性分析
        """
        query = f"""
        請對 {symbol} 進行特徵重要性分析：
        
        1. 獲取 LOB 和市場數據特徵
        2. 計算每個特徵對 {target_variable} 的預測能力
        3. 使用多種方法（相關性、互信息、特徵選擇）
        4. 排序特徵重要性
        5. 識別冗餘特徵
        6. 建議特徵工程改進
        
        請提供特徵重要性排名和特徵選擇建議。
        """
        
        response = await self.arun(query)
        return {"feature_analysis": response.content, "symbol": symbol}
        
    async def monitor_data_pipeline(self) -> Dict[str, Any]:
        """監控數據管道狀態"""
        query = """
        請監控和檢查整個數據收集管道的狀態：
        
        1. 檢查 ClickHouse 連接和數據寫入
        2. 驗證 Rust 數據收集器運行狀態
        3. 監控數據延遲和吞吐量
        4. 檢查數據完整性
        5. 識別潛在問題和瓶頸
        
        請提供管道健康報告和優化建議。
        """
        
        response = await self.arun(query)
        return {"pipeline_status": response.content}
        
    async def generate_market_report(
        self,
        symbols: list,
        report_type: str = "daily"
    ) -> Dict[str, Any]:
        """
        生成市場報告
        
        Args:
            symbols: 交易對列表
            report_type: 報告類型（daily, weekly, monthly）
            
        Returns:
            市場報告
        """
        symbols_str = ", ".join(symbols)
        
        query = f"""
        請生成 {report_type} 市場分析報告，涵蓋以下交易對：{symbols_str}
        
        報告內容：
        1. **市場概況**：整體趨勢和波動率分析
        2. **個別分析**：每個交易對的技術分析
        3. **關鍵事件**：影響價格的重要事件
        4. **風險評估**：當前市場風險水平
        5. **交易機會**：識別的交易機會
        6. **展望預測**：短期市場預期
        
        請使用專業的分析語言和具體數據支持。
        """
        
        response = await self.arun(query)
        return {"market_report": response.content, "symbols": symbols}