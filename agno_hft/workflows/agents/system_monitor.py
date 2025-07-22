"""
系統監控專家 Agent
=================

專責系統健康和性能監控的 Agent：
- 實時系統性能監控
- 基礎設施健康檢查
- 異常檢測和告警
- 系統優化建議
"""

from agno import Agent
from agno.models import Claude
from typing import Dict, Any, Optional, List
import asyncio
import json

from ..tools import RustHFTTools
from ..tools.monitoring_tools import MonitoringTools


class SystemMonitor(Agent):
    """
    系統監控專家
    
    專精領域：
    - 系統性能監控
    - 基礎設施管理
    - 異常檢測和診斷
    - 系統優化和調優
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="SystemMonitor",
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
        你是一位頂尖的系統運維和性能監控專家，專精於高頻交易系統監控。

        核心職責：
        1. **系統性能監控**：
           - 監控系統延遲和吞吐量
           - 追蹤 CPU、內存、磁碟使用率
           - 監控網路延遲和頻寬
           - 檢查資料庫連接和查詢性能

        2. **應用程式監控**：
           - 監控 Rust HFT 引擎狀態
           - 追蹤 Python Agent 執行情況
           - 檢查 ML 模型推理性能
           - 監控數據流水線健康度

        3. **異常檢測**：
           - 識別系統異常和瓶頸
           - 檢測內存洩漏和資源競爭
           - 監控錯誤率和異常頻率
           - 預測系統故障風險

        4. **告警和報告**：
           - 及時發送異常告警
           - 生成系統健康報告
           - 提供性能優化建議
           - 記錄故障處理過程

        專業要求：
        - 快速問題定位和診斷（<30s）
        - 準確的性能指標分析
        - 主動的風險預警
        - 詳細的監控數據記錄

        輸出格式：
        - 提供明確的系統狀態
        - 包含具體的性能數值
        - 給出優化建議和行動方案
        - 記錄監控事件和處理結果
        """
        
    async def monitor_system_health(self) -> Dict[str, Any]:
        """
        監控系統整體健康狀態
        
        Returns:
            系統健康監控結果
        """
        query = """
        請執行全面的系統健康監控檢查：
        
        監控範圍：
        1. **基礎設施監控**：
           - CPU 使用率和負載
           - 記憶體使用情況和洩漏檢測
           - 磁碟 I/O 性能和容量
           - 網路延遲和頻寬使用

        2. **應用程式狀態**：
           - Rust HFT 引擎運行狀態
           - Python Agno Agents 健康度
           - 數據收集器連接狀態
           - ML 模型推理服務狀態

        3. **數據庫監控**：
           - ClickHouse 連接和查詢性能
           - Redis 連接狀態和記憶體使用
           - 數據寫入速率和延遲
           - 查詢執行時間分析

        4. **關鍵性能指標**：
           - 交易決策延遲 (<1μs 目標)
           - 數據處理吞吐量
           - 系統錯誤率和異常頻率
           - 可用性和穩定性指標

        請使用監控工具獲取實際系統數據並提供健康狀態評估。
        """
        
        response = await self.arun(query)
        return {"health_status": response.content}
        
    async def analyze_performance_metrics(
        self,
        time_range: str = "1h",
        components: Optional[List[str]] = None
    ) -> Dict[str, Any]:
        """
        分析性能指標
        
        Args:
            time_range: 分析時間範圍
            components: 要分析的組件列表
            
        Returns:
            性能分析結果
        """
        components_str = ", ".join(components) if components else "ALL"
        
        query = f"""
        請分析過去 {time_range} 的系統性能指標：
        
        分析組件：{components_str}
        
        分析維度：
        1. **延遲分析**：
           - 平均、P50、P95、P99 延遲
           - 延遲分佈變化趨勢
           - 延遲峰值事件分析
           - 與 SLA 目標比較

        2. **吞吐量分析**：
           - 數據處理速率趨勢
           - 交易執行頻率統計
           - 系統容量使用情況
           - 瓶頸識別和分析

        3. **資源使用分析**：
           - CPU 使用模式和峰值
           - 記憶體分配和回收效率
           - 磁碟 I/O 模式分析
           - 網路流量和延遲變化

        4. **錯誤和異常分析**：
           - 錯誤類型分佈統計
           - 異常事件頻率變化
           - 故障恢復時間分析
           - 根本原因識別

        請使用監控工具獲取實際性能數據並進行深入分析。
        """
        
        response = await self.arun(query)
        return {"performance_analysis": response.content, "time_range": time_range}
        
    async def detect_system_anomalies(
        self,
        sensitivity_level: str = "medium"
    ) -> Dict[str, Any]:
        """
        檢測系統異常
        
        Args:
            sensitivity_level: 檢測靈敏度（low, medium, high）
            
        Returns:
            異常檢測結果
        """
        query = f"""
        請執行系統異常檢測，靈敏度設為 {sensitivity_level}：
        
        檢測範圍：
        1. **性能異常**：
           - 延遲突然增加
           - 吞吐量急劇下降
           - CPU/記憶體使用異常
           - 資料庫查詢變慢

        2. **錯誤異常**：
           - 錯誤率突然上升
           - 新型錯誤出現
           - 連接失敗增加
           - 異常重啟事件

        3. **資源異常**：
           - 記憶體洩漏跡象
           - 磁碟空間急劇減少
           - 網路連接不穩定
           - 溫度或硬體異常

        4. **交易異常**：
           - 交易量異常變化
           - 訂單執行失敗增加
           - 價格數據異常
           - 風險指標突變

        檢測方法：
        - 統計控制圖方法
        - 機器學習異常檢測
        - 閾值告警機制
        - 時間序列異常檢測

        請識別當前系統中的異常情況並提供處理建議。
        """
        
        response = await self.arun(query)
        return {"anomaly_detection": response.content, "sensitivity": sensitivity_level}
        
    async def diagnose_system_issues(
        self,
        issue_symptoms: Dict[str, Any],
        context_data: Optional[Dict[str, Any]] = None
    ) -> Dict[str, Any]:
        """
        診斷系統問題
        
        Args:
            issue_symptoms: 問題症狀
            context_data: 上下文數據
            
        Returns:
            問題診斷結果
        """
        symptoms_str = json.dumps(issue_symptoms, indent=2)
        context_str = json.dumps(context_data, indent=2) if context_data else "N/A"
        
        query = f"""
        請診斷系統問題：
        
        問題症狀：
        ```json
        {symptoms_str}
        ```
        
        上下文數據：
        ```json
        {context_str}
        ```
        
        診斷流程：
        1. **症狀分析**：
           - 症狀分類和嚴重性評估
           - 影響範圍和程度分析
           - 時間線和發生模式
           - 相關聯症狀識別

        2. **根因分析**：
           - 系統日誌分析
           - 性能指標對比
           - 配置變更檢查
           - 外部因素考量

        3. **影響評估**：
           - 業務影響程度
           - 系統風險評估
           - 用戶體驗影響
           - 數據一致性檢查

        4. **解決方案**：
           - 即時緩解措施
           - 根本問題解決
           - 預防措施建議
           - 監控強化方案

        請使用診斷工具深入分析並提供詳細的診斷報告。
        """
        
        response = await self.arun(query)
        return {"diagnosis_result": response.content}
        
    async def optimize_system_performance(
        self,
        performance_data: Dict[str, Any],
        optimization_goals: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        優化系統性能
        
        Args:
            performance_data: 性能數據
            optimization_goals: 優化目標
            
        Returns:
            系統優化建議
        """
        performance_str = json.dumps(performance_data, indent=2)
        goals_str = json.dumps(optimization_goals, indent=2)
        
        query = f"""
        請基於性能數據制定系統優化方案：
        
        性能數據：
        ```json
        {performance_str}
        ```
        
        優化目標：
        ```json
        {goals_str}
        ```
        
        優化領域：
        1. **延遲優化**：
           - CPU 親和性配置
           - 記憶體預分配策略
           - 網路優化設置
           - 算法和數據結構優化

        2. **吞吐量提升**：
           - 並行處理優化
           - 批處理策略調整
           - 緩存機制改進
           - 資料庫查詢優化

        3. **資源效率**：
           - 記憶體使用優化
           - CPU 使用均衡
           - I/O 效率提升
           - 垃圾回收優化

        4. **系統穩定性**：
           - 錯誤處理改進
           - 容錯機制增強
           - 監控告警優化
           - 自動恢復機制

        優化方法：
        - 性能基準測試
        - A/B 測試驗證
        - 漸進式部署
        - 回滾方案準備

        請提供具體的優化建議和實施計劃。
        """
        
        response = await self.arun(query)
        return {"optimization_plan": response.content}
        
    async def generate_system_report(
        self,
        report_period: str = "daily",
        include_forecasting: bool = True
    ) -> Dict[str, Any]:
        """
        生成系統監控報告
        
        Args:
            report_period: 報告週期
            include_forecasting: 是否包含預測分析
            
        Returns:
            系統監控報告
        """
        query = f"""
        請生成 {report_period} 系統監控報告：
        
        報告結構：
        1. **系統概況**：
           - 系統整體運行狀態
           - 關鍵性能指標摘要
           - 可用性和穩定性統計
           - 重要事件時間線

        2. **性能分析**：
           - 延遲統計和分佈分析
           - 吞吐量趨勢和變化
           - 資源使用效率評估
           - 性能瓶頸識別

        3. **健康度評估**：
           - 各組件健康狀態
           - 錯誤率和異常頻率
           - 系統負載和容量評估
           - 基礎設施狀態檢查

        4. **異常和事件**：
           - 異常事件記錄和處理
           - 告警觸發統計
           - 故障恢復時間分析
           - 根本原因分析

        5. **維護和優化**：
           - 已執行的維護活動
           - 性能優化措施效果
           - 系統升級和變更記錄
           - 預防性維護建議
        """
        
        if include_forecasting:
            query += """
        
        6. **趨勢預測**：
           - 性能趨勢預測分析
           - 資源需求預測
           - 潛在風險識別
           - 容量規劃建議
        """
        
        query += "\n\n請使用監控工具獲取實際數據並生成專業的系統監控報告。"
        
        response = await self.arun(query)
        return {"system_report": response.content, "report_period": report_period}
        
    async def setup_monitoring_alerts(
        self,
        alert_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        設置監控告警
        
        Args:
            alert_config: 告警配置
            
        Returns:
            告警設置結果
        """
        config_str = json.dumps(alert_config, indent=2)
        
        query = f"""
        請設置系統監控告警機制：
        
        告警配置：
        ```json
        {config_str}
        ```
        
        告警類型：
        1. **性能告警**：
           - 延遲超過閾值
           - 吞吐量低於預期
           - CPU/記憶體使用過高
           - 磁碟空間不足

        2. **錯誤告警**：
           - 錯誤率超過閾值
           - 連接失敗告警
           - 服務不可用告警
           - 數據異常告警

        3. **業務告警**：
           - 交易異常告警
           - 風險限額告警
           - 數據延遲告警
           - 模型性能告警

        4. **基礎設施告警**：
           - 硬體故障告警
           - 網路連接告警
           - 資料庫連接告警
           - 備份失敗告警

        告警機制：
        - 多級別告警（信息、警告、嚴重、緊急）
        - 多通道通知（郵件、簡訊、即時通訊）
        - 告警升級機制
        - 告警抑制和去重

        請配置完整的告警系統並驗證通知機制。
        """
        
        response = await self.arun(query)
        return {"alert_setup": response.content}