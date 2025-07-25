"""
風險監督員 Agent
===============

專責風險的高層監控和管理決策：
- 監控 Rust 引擎的實時風險指標
- 設置和調整風險限額
- 緊急風險事件處理
- 風險報告和合規

注意：實時風險計算和限額執行由 Rust 底層完成
"""

from agno import Agent
from agno.models import Claude
from typing import Dict, Any, Optional, List
import asyncio
import json

from ..tools import RustHFTTools, MonitoringTools


class RiskSupervisor(Agent):
    """
    風險監督員
    
    專精領域：
    - 風險限額設置和管理
    - 風險指標監控和分析
    - 風險事件應對
    - 合規風險控制
    
    注意：實時風險計算由 Rust 底層完成
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="RiskSupervisor",
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
        你是一位資深的風險管理專家，負責監督 Rust HFT 系統的風險狀況。

        重要理解：
        - Rust 底層實時計算風險指標和執行限額控制（微秒級）
        - 你負責風險的高層決策和限額管理（秒/分鐘級）
        - 通過 RustHFTTools 監控風險狀況並調整參數

        核心職責：
        1. **風險監控**：
           - 監控 Rust 引擎報告的實時風險指標
           - 分析風險趨勢和異常
           - 評估風險集中度
           - 識別潛在風險點

        2. **限額管理**：
           - 設置各類風險限額
           - 根據市場條件調整限額
           - 監控限額使用情況
           - 評估限額合理性

        3. **風險應對**：
           - 制定風險應對策略
           - 處理風險預警事件
           - 協調緊急風險處理
           - 實施風險緩解措施

        4. **合規監督**：
           - 確保風險控制符合監管要求
           - 維護風險記錄和報告
           - 實施內部控制檢查
           - 配合外部審計

        工作原則：
        - 預防為主，及時響應
        - 數據驅動，客觀判斷
        - 系統性思考，全局把控
        - 合規至上，風險可控

        重要約束：
        - 實時風險計算由 Rust 完成
        - 通過接口調整風險參數
        - 尊重系統設計的風險邏輯
        """
        
    async def monitor_portfolio_risk(
        self,
        time_window: str = "1h"
    ) -> Dict[str, Any]:
        """
        監控組合整體風險
        
        Args:
            time_window: 監控時間窗口
            
        Returns:
            風險監控結果
        """
        query = f"""
        請監控整個投資組合在過去 {time_window} 的風險狀況：
        
        風險監控維度：
        1. **市場風險**：
           - 組合 VaR 和 ES 變化
           - 各資產風險貢獻度
           - 相關性風險分析
           - 波動率集中度評估

        2. **信用風險**：
           - 交易對手風險
           - 結算風險評估
           - 保證金充足性
           - 流動性風險

        3. **操作風險**：
           - 系統延遲異常
           - 交易執行失敗
           - 數據質量問題
           - 模型風險評估

        4. **集中度風險**：
           - 單一資產集中度
           - 時間集中度
           - 策略集中度
           - 地域集中度

        請使用 Rust 工具獲取實時風險數據並進行全面分析。
        """
        
        response = await self.arun(query)
        return {"risk_monitoring": response.content}
        
    async def adjust_risk_limits(
        self,
        current_risk_status: Dict[str, Any],
        market_conditions: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        調整風險限額
        
        Args:
            current_risk_status: 當前風險狀況
            market_conditions: 市場條件
            
        Returns:
            限額調整結果
        """
        risk_str = json.dumps(current_risk_status, indent=2)
        market_str = json.dumps(market_conditions, indent=2)
        
        query = f"""
        請基於當前風險狀況和市場條件調整風險限額：
        
        當前風險狀況：
        ```json
        {risk_str}
        ```
        
        市場條件：
        ```json
        {market_str}
        ```
        
        限額調整分析：
        1. **限額評估**：
           - 當前限額使用率分析
           - 限額充足性評估
           - 市場條件適應性
           - 歷史限額效果回顧

        2. **調整建議**：
           - VaR 限額調整建議
           - 單筆交易限額
           - 日內損失限額
           - 槓桿倍數限制

        3. **動態調整**：
           - 波動率調整因子
           - 流動性調整係數
           - 時段性限額差異
           - 緊急情況限額

        4. **實施計劃**：
           - 限額更新時機
           - 漸進調整策略
           - 影響評估
           - 回滾方案

        請使用 RustHFTTools 檢查當前限額設置並提供調整建議。
        """
        
        response = await self.arun(query)
        return {"limit_adjustment": response.content}
        
    async def handle_risk_alert(
        self,
        alert_type: str,
        alert_data: Dict[str, Any],
        severity: str = "medium"
    ) -> Dict[str, Any]:
        """
        處理風險告警
        
        Args:
            alert_type: 告警類型
            alert_data: 告警數據
            severity: 嚴重程度
            
        Returns:
            風險告警處理結果
        """
        data_str = json.dumps(alert_data, indent=2)
        
        query = f"""
        請處理風險告警事件：
        
        告警類型：{alert_type}
        嚴重程度：{severity}
        
        告警數據：
        ```json
        {data_str}
        ```
        
        風險告警處理：
        1. **告警分析**：
           - 告警觸發原因分析
           - 風險影響範圍評估
           - 緊急程度判定
           - 處理優先級排序

        2. **即時響應**：
           - 風險緩解措施
           - 限額緊急調整
           - 策略暫停決策
           - 團隊通知機制

        3. **Rust 系統操作**：
           - 通過 RustHFTTools 實施風險控制
           - 調整風險參數
           - 設置緊急限額
           - 啟動保護機制

        4. **後續跟踪**：
           - 處理效果監控
           - 風險狀況持續評估
           - 恢復條件制定
           - 經驗總結歸檔

        請根據告警嚴重程度採取相應的風險控制措施。
        """
        
        response = await self.arun(query)
        return {"alert_response": response.content, "alert_type": alert_type}
        
    async def conduct_stress_test(
        self,
        stress_scenarios: List[Dict[str, Any]]
    ) -> Dict[str, Any]:
        """
        進行壓力測試
        
        Args:
            stress_scenarios: 壓力測試場景
            
        Returns:
            壓力測試結果
        """
        scenarios_str = json.dumps(stress_scenarios, indent=2)
        
        query = f"""
        請進行組合壓力測試：
        
        壓力場景：
        ```json
        {scenarios_str}
        ```
        
        壓力測試執行：
        1. **場景設計**：
           - 歷史極端事件重現
           - 假設性壓力場景
           - 組合場景測試
           - 尾部風險測試

        2. **測試實施**：
           - 各場景下的組合表現
           - VaR 和 ES 變化
           - 流動性風險評估
           - 極端損失計算

        3. **結果分析**：
           - 最大可能損失
           - 風險分散效果
           - 脆弱點識別
           - 恢復能力評估

        4. **改進建議**：
           - 風險限額調整建議
           - 策略配置優化
           - 應急預案完善
           - 監控指標強化

        請使用 Rust 工具獲取組合數據並進行壓力測試分析。
        """
        
        response = await self.arun(query)
        return {"stress_test_result": response.content}
        
    async def generate_risk_report(
        self,
        report_period: str = "daily",
        include_recommendations: bool = True
    ) -> Dict[str, Any]:
        """
        生成風險報告
        
        Args:
            report_period: 報告週期
            include_recommendations: 是否包含建議
            
        Returns:
            風險報告
        """
        query = f"""
        請生成 {report_period} 風險管理報告：
        
        報告內容：
        1. **執行摘要**：
           - 主要風險指標概覽
           - 重要風險事件總結
           - 風險趨勢變化
           - 關鍵關注事項

        2. **量化風險分析**：
           - VaR 和 ES 統計
           - 回撤分析
           - 風險調整收益
           - 限額使用統計

        3. **風險事件記錄**：
           - 告警事件統計
           - 處理過程記錄
           - 效果評估
           - 經驗教訓

        4. **合規性檢查**：
           - 監管指標符合性
           - 內控制度執行
           - 審計要求滿足
           - 合規風險評估
        """
        
        if include_recommendations:
            query += """
        
        5. **管理建議**：
           - 風險限額調整建議
           - 風險控制優化
           - 系統改進建議
           - 流程完善建議
        """
        
        query += "\n\n請使用監控工具獲取實際風險數據並生成專業報告。"
        
        response = await self.arun(query)
        return {"risk_report": response.content, "report_period": report_period}
        
    async def validate_risk_models(
        self,
        model_validation_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        驗證風險模型
        
        Args:
            model_validation_config: 模型驗證配置
            
        Returns:
            模型驗證結果
        """
        config_str = json.dumps(model_validation_config, indent=2)
        
        query = f"""
        請驗證風險模型的有效性：
        
        驗證配置：
        ```json
        {config_str}
        ```
        
        模型驗證內容：
        1. **回溯測試**：
           - VaR 模型準確性檢驗
           - 失效頻率統計
           - 預測覆蓋率分析
           - 模型校準檢驗

        2. **模型穩定性**：
           - 參數穩定性測試
           - 時間序列一致性
           - 樣本敏感性分析
           - 模型漂移檢測

        3. **極端情況測試**：
           - 尾部風險預測能力
           - 危機期間表現
           - 模型失效風險
           - 備選模型比較

        4. **改進建議**：
           - 模型參數調整
           - 建模方法優化
           - 數據質量提升
           - 驗證頻率調整

        請分析 Rust 系統中使用的風險模型並進行全面驗證。
        """
        
        response = await self.arun(query)
        return {"model_validation": response.content}