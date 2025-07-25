"""
市場研究分析師 Agent
===================

專責離線市場研究和策略開發：
- 市場數據深度分析
- 新策略研發和驗證
- 特徵工程和選擇
- 回測和策略評估
"""

from agno import Agent
from agno.models import Claude
from typing import Dict, Any, Optional
import asyncio
import json

from ..tools import DataProcessingTools, RustHFTTools


class ResearchAnalyst(Agent):
    """
    市場研究分析師
    
    專精領域：
    - 市場微觀結構研究
    - 交易策略開發
    - 特徵工程
    - 回測分析
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="ResearchAnalyst",
            model=Claude(id="claude-sonnet-4-20250514"),
            tools=[
                DataProcessingTools(),
                RustHFTTools()
            ],
            instructions=self._get_instructions(),
            markdown=True,
            session_id=session_id,
            show_tool_calls=True
        )
        
    def _get_instructions(self) -> str:
        return """
        你是一位頂尖的量化研究分析師，專精於高頻交易策略研發。

        核心職責：
        1. **市場研究**：
           - 深度分析市場微觀結構
           - 識別新的 alpha 機會
           - 研究訂單流模式
           - 分析市場制度變化

        2. **策略開發**：
           - 設計新的交易策略
           - 優化現有策略邏輯
           - 構建策略組合
           - 評估策略容量

        3. **特徵工程**：
           - 從原始 LOB 數據提取有效特徵
           - 設計技術指標組合
           - 優化特徵選擇
           - 處理特徵共線性

        4. **回測分析**：
           - 設計嚴格的回測框架
           - 分析策略歷史表現
           - 識別過擬合風險
           - 評估策略穩定性

        工作模式：
        - 深度分析和長期研究
        - 注重統計顯著性
        - 關注策略可解釋性
        - 重視風險調整收益

        輸出要求：
        - 提供詳細的研究報告
        - 包含統計檢驗結果
        - 給出明確的投資建議
        - 記錄完整的研究過程
        """
        
    async def conduct_market_research(
        self,
        research_topic: str,
        symbols: list,
        time_range: str = "6M"
    ) -> Dict[str, Any]:
        """
        進行市場研究
        
        Args:
            research_topic: 研究主題
            symbols: 研究標的
            time_range: 研究時間範圍
            
        Returns:
            研究結果
        """
        symbols_str = ", ".join(symbols)
        
        query = f"""
        請對以下市場進行深度研究：
        
        研究主題：{research_topic}
        研究標的：{symbols_str}
        時間範圍：{time_range}
        
        研究內容：
        1. **市場微觀結構分析**：
           - 訂單流特性分析
           - 買賣價差變化模式
           - 市場深度分佈特徵
           - 交易頻率和規模分析

        2. **價格發現機制研究**：
           - 信息傳播速度分析
           - 價格跳躍特徵識別
           - 波動率結構研究
           - 相關性動態分析

        3. **交易行為模式**：
           - 機構交易者行為識別
           - 算法交易模式分析
           - 市場衝擊成本研究
           - 流動性供給特徵

        4. **Alpha 機會識別**：
           - 低效率機會發現
           - 套利機會分析
           - 預測性特徵挖掘
           - 風險因子分解

        請提供詳細的研究報告，包含數據分析、圖表說明和投資建議。
        """
        
        response = await self.arun(query)
        return {"research_result": response.content, "topic": research_topic}
        
    async def develop_trading_strategy(
        self,
        strategy_concept: str,
        target_symbols: list,
        strategy_params: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        開發交易策略
        
        Args:
            strategy_concept: 策略概念
            target_symbols: 目標交易對
            strategy_params: 策略參數
            
        Returns:
            策略開發結果
        """
        symbols_str = ", ".join(target_symbols)
        params_str = json.dumps(strategy_params, indent=2)
        
        query = f"""
        請開發新的高頻交易策略：
        
        策略概念：{strategy_concept}
        目標標的：{symbols_str}
        
        初始參數：
        ```json
        {params_str}
        ```
        
        策略開發流程：
        1. **策略邏輯設計**：
           - 信號生成邏輯
           - 進場出場條件
           - 止損止盈機制
           - 倉位管理規則

        2. **參數優化**：
           - 關鍵參數識別
           - 參數敏感性分析
           - 最優參數搜索
           - 過擬合防範

        3. **策略驗證**：
           - 統計顯著性檢驗
           - 穩定性測試
           - 極端市場壓力測試
           - 容量分析

        4. **實施建議**：
           - 最佳執行時間
           - 風險控制措施
           - 監控指標設置
           - 策略退出條件

        請提供完整的策略文檔，包含邏輯描述、參數設置、風險評估和實施計劃。
        """
        
        response = await self.arun(query)
        return {"strategy_result": response.content, "concept": strategy_concept}
        
    async def perform_feature_engineering(
        self,
        data_description: Dict[str, Any],
        target_prediction: str = "price_direction"
    ) -> Dict[str, Any]:
        """
        執行特徵工程
        
        Args:
            data_description: 數據描述
            target_prediction: 預測目標
            
        Returns:
            特徵工程結果
        """
        data_str = json.dumps(data_description, indent=2)
        
        query = f"""
        請進行 LOB 數據的特徵工程：
        
        數據描述：
        ```json
        {data_str}
        ```
        
        預測目標：{target_prediction}
        
        特徵工程任務：
        1. **基礎特徵提取**：
           - 價格特徵（mid price, microprice, spread）
           - 量價特徵（volume imbalance, trade intensity）
           - 時序特徵（price momentum, volatility）
           - 深度特徵（order book slope, depth）

        2. **高級特徵構建**：
           - 多時間框架特徵
           - 交叉特徵組合
           - 技術指標衍生
           - 市場微觀結構指標

        3. **特徵選擇**：
           - 相關性分析
           - 互信息評估
           - 遞歸特徵消除
           - 主成分分析

        4. **特徵驗證**：
           - 預測能力評估
           - 穩定性測試
           - 多重共線性檢查
           - 前向傾斜檢驗

        請提供完整的特徵工程報告，包含特徵定義、重要性排序和使用建議。
        """
        
        response = await self.arun(query)
        return {"feature_engineering_result": response.content}