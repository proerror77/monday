"""
系統操作員 Agent
===============

專責系統運維和技術監控：
- Rust HFT 引擎健康監控
- 系統性能優化
- 故障診斷和恢復
- 基礎設施管理

注意：不負責業務邏輯，專注技術運維
"""

from agno import Agent
from agno.models import Claude
from typing import Dict, Any, Optional, List
import asyncio
import json

from ..tools import RustHFTTools, MonitoringTools


class SystemOperator(Agent):
    """
    系統操作員
    
    專精領域：
    - Rust 系統運維監控
    - 性能優化和調優
    - 故障診斷和恢復
    - 基礎設施管理
    """
    
    def __init__(self, session_id: Optional[str] = None):
        super().__init__(
            name="SystemOperator",
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
        你是一位資深的系統運維工程師，專精於高性能 Rust HFT 系統的運維管理。

        核心職責：
        1. **系統監控**：
           - Rust HFT 引擎健康狀態
           - 系統資源使用情況
           - 網路連接穩定性
           - 數據庫性能監控

        2. **性能優化**：
           - 延遲優化和調優
           - 吞吐量提升
           - 記憶體使用優化
           - CPU 親和性設置

        3. **故障處理**：
           - 快速故障定位
           - 系統恢復策略
           - 備用方案啟動
           - 故障根因分析

        4. **基礎設施管理**：
           - 服務器硬體監控
           - 網路設備管理
           - 數據備份驗證
           - 安全補丁管理

        工作標準：
        - 7×24 小時監控響應
        - 99.99% 系統可用性目標
        - <100μs 平均響應延遲
        - 零單點故障容錯設計

        重要原則：
        - 穩定性優於性能
        - 預防勝於治療
        - 自動化運維優先
        - 詳細記錄和文檔
        """
        
    async def monitor_system_health(self) -> Dict[str, Any]:
        """
        監控系統整體健康狀況
        
        Returns:
            系統健康監控結果
        """
        query = """
        請執行系統整體健康檢查：
        
        監控維度：
        1. **Rust HFT 引擎狀態**：
           - 進程運行狀態
           - 記憶體使用情況
           - CPU 使用率
           - 執行緒池狀態

        2. **系統資源**：
           - CPU 負載和溫度
           - 記憶體使用和洩漏
           - 磁碟 I/O 和空間
           - 網路延遲和頻寬

        3. **服務連接**：
           - 交易所 WebSocket 連接
           - 數據庫連接池
           - Redis 緩存狀態
           - 日誌系統狀態

        4. **關鍵性能指標**：
           - 平均響應延遲
           - 訊息處理吞吐量
           - 錯誤率和異常頻率
           - 系統穩定性指標

        請使用系統監控工具獲取實際數據並評估整體健康狀況。
        """
        
        response = await self.arun(query)
        return {"system_health": response.content}
        
    async def optimize_system_performance(
        self,
        performance_data: Dict[str, Any],
        optimization_targets: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        優化系統性能
        
        Args:
            performance_data: 性能數據
            optimization_targets: 優化目標
            
        Returns:
            性能優化結果
        """
        data_str = json.dumps(performance_data, indent=2)
        targets_str = json.dumps(optimization_targets, indent=2)
        
        query = f"""
        請基於性能數據進行系統優化：
        
        性能數據：
        ```json
        {data_str}
        ```
        
        優化目標：
        ```json
        {targets_str}
        ```
        
        性能優化策略：
        1. **延遲優化**：
           - CPU 親和性綁定
           - NUMA 節點優化
           - 記憶體預分配
           - 系統調用優化

        2. **吞吐量提升**：
           - 批處理優化
           - 管道並行化
           - 緩存命中率提升
           - 鎖競爭減少

        3. **資源優化**：
           - 記憶體使用模式優化
           - 垃圾回收調優
           - I/O 模式優化
           - 網路緩衝區調整

        4. **系統調優**：
           - 內核參數調整
           - 網路堆棧優化
           - 檔案系統優化
           - 中斷處理優化

        請提供具體的優化實施方案和預期效果。
        """
        
        response = await self.arun(query)
        return {"optimization_result": response.content}
        
    async def diagnose_system_issues(
        self,
        issue_symptoms: Dict[str, Any],
        system_logs: Optional[str] = None
    ) -> Dict[str, Any]:
        """
        診斷系統問題
        
        Args:
            issue_symptoms: 問題症狀
            system_logs: 系統日誌
            
        Returns:
            問題診斷結果
        """
        symptoms_str = json.dumps(issue_symptoms, indent=2)
        logs_str = system_logs or "No logs provided"
        
        query = f"""
        請診斷系統問題：
        
        問題症狀：
        ```json
        {symptoms_str}
        ```
        
        系統日誌：
        ```
        {logs_str}
        ```
        
        問題診斷流程：
        1. **症狀分析**：
           - 問題表現特徵
           - 影響範圍評估
           - 嚴重程度判定
           - 發生時間模式

        2. **根因分析**：
           - 系統日誌分析
           - 性能指標對比
           - 配置變更檢查
           - 依賴服務檢查

        3. **影響評估**：
           - 業務影響範圍
           - 系統穩定性風險
           - 數據完整性檢查
           - 用戶體驗影響

        4. **解決方案**：
           - 緊急恢復措施
           - 根本問題修復
           - 預防性措施
           - 監控加強建議

        請使用監控工具深入分析並提供詳細診斷報告。
        """
        
        response = await self.arun(query)
        return {"diagnosis_result": response.content}
        
    async def manage_system_deployment(
        self,
        deployment_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        管理系統部署
        
        Args:
            deployment_config: 部署配置
            
        Returns:
            部署管理結果
        """
        config_str = json.dumps(deployment_config, indent=2)
        
        query = f"""
        請管理系統部署流程：
        
        部署配置：
        ```json
        {config_str}
        ```
        
        部署管理流程：
        1. **部署前檢查**：
           - 系統資源充足性
           - 依賴服務可用性
           - 配置文件驗證
           - 備份狀態確認

        2. **部署執行**：
           - 服務優雅停止
           - 新版本部署
           - 配置更新
           - 服務啟動

        3. **部署驗證**：
           - 功能性測試
           - 性能基準驗證
           - 健康檢查通過
           - 業務流程測試

        4. **部署監控**：
           - 實時性能監控
           - 錯誤率監控
           - 用戶影響評估
           - 回滾準備

        請使用 Rust 工具執行實際的部署管理操作。
        """
        
        response = await self.arun(query)
        return {"deployment_result": response.content}
        
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
        請設置系統監控告警：
        
        告警配置：
        ```json
        {config_str}
        ```
        
        告警設置內容：
        1. **系統告警**：
           - CPU 使用率告警
           - 記憶體使用告警
           - 磁碟空間告警
           - 網路連接告警

        2. **應用告警**：
           - Rust 進程異常
           - 響應延遲超限
           - 錯誤率超標
           - 吞吐量異常

        3. **業務告警**：
           - 交易執行失敗
           - 數據延遲告警
           - 風險指標異常
           - 合規性告警

        4. **告警機制**：
           - 多級告警等級
           - 多通道通知
           - 告警升級規則
           - 告警抑制邏輯

        請配置完整的告警系統並驗證告警功能。
        """
        
        response = await self.arun(query)
        return {"alert_setup": response.content}
        
    async def perform_system_backup(
        self,
        backup_scope: List[str],
        backup_config: Dict[str, Any]
    ) -> Dict[str, Any]:
        """
        執行系統備份
        
        Args:
            backup_scope: 備份範圍
            backup_config: 備份配置
            
        Returns:
            備份執行結果
        """
        scope_str = ", ".join(backup_scope)
        config_str = json.dumps(backup_config, indent=2)
        
        query = f"""
        請執行系統備份：
        
        備份範圍：{scope_str}
        
        備份配置：
        ```json
        {config_str}
        ```
        
        備份執行流程：
        1. **備份準備**：
           - 備份範圍確認
           - 儲存空間檢查
           - 服務影響評估
           - 備份策略選擇

        2. **數據備份**：
           - 配置檔案備份
           - 數據庫備份
           - 日誌檔案備份
           - 應用程式備份

        3. **備份驗證**：
           - 備份完整性檢查
           - 備份檔案測試
           - 恢復能力驗證
           - 備份時間記錄

        4. **備份管理**：
           - 備份版本管理
           - 舊備份清理
           - 異地備份同步
           - 備份監控設置

        請執行完整的系統備份流程並驗證備份有效性。
        """
        
        response = await self.arun(query)
        return {"backup_result": response.content, "scope": backup_scope}