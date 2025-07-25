"""
策略管理員 Agent
===============

專責策略參數的實時管理和優化：
- 策略參數熱更新
- 實時績效監控
- 參數自適應調整
- 策略啟停控制

注意：不負責具體交易執行（由 Rust 底層完成）
"""

from agno import Agent
from agno.models import Claude
from typing import Dict, Any, Optional, List
import asyncio
import json

from ..tools import RustHFTTools, MonitoringTools


class StrategyManager(Agent):
    """
    策略管理員
    
    專精領域：
    - 策略參數動態調整
    - 實時績效監控
    - 策略組合管理
    - 市場適應性優化
    
    注意：所有交易執行由 Rust 底層完成
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="StrategyManager",
            model=Claude(id="claude-sonnet-4-20250514"),
            tools=[
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
        你是一位資深的策略管理專家，負責管理 Rust HFT 引擎的策略參數。

        重要理解：
        - 實際交易執行由 Rust 底層完成（微秒級）
        - 你負責策略的高層管理和參數調整（秒/分鐘級）
        - 通過 RustHFTTools 與底層 Rust 引擎通信
        
        核心職責：
        1. **策略參數管理**：
           - 監控策略實時表現
           - 根據市場條件調整參數
           - 實施參數熱更新
           - 維護參數歷史記錄

        2. **績效監控**：
           - 實時 P&L 監控
           - Sharpe 比率追蹤
           - 回撤風險評估
           - 勝率統計分析

        3. **策略優化**：
           - 識別參數優化機會
           - 實施 A/B 測試
           - 自適應參數調整
           - 策略組合權重管理

        4. **異常處理**：
           - 策略性能異常檢測
           - 市場環境變化應對
           - 緊急參數調整
           - 策略暫停和恢復

        工作模式：
        - 持續監控，及時響應
        - 數據驅動決策
        - 風險優先原則
        - 保持策略穩定性

        重要約束：
        - 不直接執行交易（Rust 負責）
        - 參數調整要考慮 Rust 系統限制
        - 所有操作都通過 RustHFTTools 接口
        """
        
    async def monitor_strategy_performance(
        self,
        symbol: str,
        time_window: str = "1h"
    ) -> Dict[str, Any]:
        """
        監控策略實時表現
        
        Args:
            symbol: 交易對
            time_window: 監控時間窗口
            
        Returns:
            策略表現分析
        """
        query = f"""
        請監控 {symbol} 在過去 {time_window} 的策略表現：
        
        監控維度：
        1. **實時 P&L**：
           - 累計收益和未實現損益
           - 分筆交易盈虧分析
           - 收益率與基準比較
           - P&L 波動性評估

        2. **執行效率**：
           - 信號轉化率（信號→實際交易）
           - 平均執行延遲
           - 滑點統計
           - 成交率分析

        3. **風險指標**：
           - 實時 Sharpe 比率
           - 最大回撤
           - VaR 和 ES
           - 波動率測量

        4. **策略行為**：
           - 交易頻率統計
           - 持倉時間分布
           - 倉位大小變化
           - 策略活躍度

        請使用監控工具獲取 Rust 引擎的實時數據並進行分析。
        """
        
        response = await self.arun(query)
        return {"performance_analysis": response.content, "symbol": symbol}
        
    async def adjust_strategy_parameters(
        self,
        symbol: str,
        current_performance: Dict[str, Any],
        market_conditions: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        調整策略參數
        
        Args:
            symbol: 交易對
            current_performance: 當前表現
            market_conditions: 市場條件
            
        Returns:
            參數調整建議
        """
        performance_str = json.dumps(current_performance, indent=2)
        conditions_str = json.dumps(market_conditions, indent=2)
        
        query = f"""
        請基於當前表現調整 {symbol} 的策略參數：
        
        當前表現：
        ```json
        {performance_str}
        ```
        
        市場條件：
        ```json
        {conditions_str}
        ```
        
        參數調整分析：
        1. **表現診斷**：
           - 識別表現不佳的原因
           - 分析參數敏感性
           - 評估市場適應性
           - 確定調整優先級

        2. **參數優化**：
           - 風險參數調整（止損、止盈）
           - 信號閾值優化
           - 倉位大小調整
           - 交易頻率控制

        3. **市場適應**：
           - 波動率調整因子
           - 流動性適應參數
           - 時段特定參數
           - 趨勢/震蕩模式切換

        4. **實施計劃**：
           - 參數更新順序
           - 漸進式調整策略
           - 風險控制措施
           - 效果監控計劃

        請使用 RustHFTTools 獲取當前參數並提供具體的調整建議。
        """
        
        response = await self.arun(query)
        return {"adjustment_plan": response.content, "symbol": symbol}
        
    async def manage_strategy_portfolio(
        self,
        symbols: List[str],
        portfolio_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        管理策略組合
        
        Args:
            symbols: 交易對列表
            portfolio_config: 組合配置
            
        Returns:
            組合管理結果
        """
        symbols_str = ", ".join(symbols)
        config_str = json.dumps(portfolio_config, indent=2)
        
        query = f"""
        請管理多資產策略組合：
        
        交易對：{symbols_str}
        
        組合配置：
        ```json
        {config_str}
        ```
        
        組合管理任務：
        1. **資本分配**：
           - 各資產權重優化
           - 風險平價考慮
           - 相關性調整
           - 流動性權衡

        2. **組合監控**：
           - 整體組合 P&L
           - 資產間相關性變化
           - 風險集中度監控
           - 分散化效果評估

        3. **動態調整**：
           - 權重再平衡時機
           - 資產進出決策
           - 風險預算分配
           - 策略輪換管理

        4. **協調控制**：
           - 跨資產風險控制
           - 總體槓桿管理
           - 流動性協調
           - 系統性風險防範

        請使用 Rust 工具獲取各資產的實時狀態並提供組合管理建議。
        """
        
        response = await self.arun(query)
        return {"portfolio_management": response.content, "symbols": symbols}
        
    async def implement_ab_test(
        self,
        symbol: str,
        test_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        實施策略 A/B 測試
        
        Args:
            symbol: 交易對
            test_config: 測試配置
            
        Returns:
            A/B 測試實施結果
        """
        config_str = json.dumps(test_config, indent=2)
        
        query = f"""
        請實施 {symbol} 的策略 A/B 測試：
        
        測試配置：
        ```json
        {config_str}
        ```
        
        A/B 測試實施：
        1. **測試設計**：
           - A/B 組分配策略
           - 測試時長規劃
           - 樣本大小計算
           - 統計功效分析

        2. **實施控制**：
           - Rust 引擎參數分組設置
           - 流量分割實現
           - 實驗隔離保證
           - 數據收集配置

        3. **實時監控**：
           - A/B 組表現對比
           - 統計顯著性檢驗
           - 早停條件檢查
           - 異常情況處理

        4. **結果分析**：
           - 主要指標比較
           - 置信區間計算
           - 業務影響評估
           - 決策建議提供

        請使用 RustHFTTools 在 Rust 系統中實施實際的 A/B 測試。
        """
        
        response = await self.arun(query)
        return {"ab_test_result": response.content, "symbol": symbol}
        
    async def handle_strategy_emergency(
        self,
        emergency_type: str,
        affected_symbols: List[str],
        emergency_data: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        處理策略緊急情況
        
        Args:
            emergency_type: 緊急情況類型
            affected_symbols: 受影響的交易對
            emergency_data: 緊急情況數據
            
        Returns:
            緊急處理結果
        """
        symbols_str = ", ".join(affected_symbols)
        data_str = json.dumps(emergency_data, indent=2)
        
        query = f"""
        請處理策略緊急情況：
        
        緊急類型：{emergency_type}
        受影響資產：{symbols_str}
        
        緊急數據：
        ```json
        {data_str}
        ```
        
        緊急處理流程：
        1. **情況評估**：
           - 緊急程度判定
           - 影響範圍分析
           - 風險損失預估
           - 處理優先級排序

        2. **即時措施**：
           - 策略暫停決策
           - 風險參數緊急調整
           - 倉位強制平倉考慮
           - 新交易停止指令

        3. **Rust 引擎操作**：
           - 通過 RustHFTTools 實施緊急停止
           - 參數緊急更新
           - 風險限制調整
           - 監控告警設置

        4. **後續監控**：
           - 處理效果驗證
           - 市場狀況持續監控
           - 恢復條件評估
           - 經驗總結記錄

        這是緊急情況，請立即使用 Rust 工具實施必要的控制措施。
        """
        
        response = await self.arun(query)
        return {"emergency_response": response.content, "emergency_type": emergency_type}