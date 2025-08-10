from typing import Optional
from agno.agent import Agent
from agno.model.openai import OpenAIChat
from agno.tools.shell import ShellTools


def get_alert_manager(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
) -> Agent:
    """告警管理代理 - 統一管理系統告警和響應流程"""
    
    return Agent(
        name="HFT Alert Manager",
        agent_id="alert_manager",
        model=OpenAIChat(id=model_id or "gpt-4o", temperature=0.1),
        tools=[ShellTools()],
        description="""
你是 HFT 系統的告警管理代理，負責統一管理所有系統告警和響應流程。

核心職責：
1. 接收來自 Redis ops.alert 頻道的告警事件
2. 根據告警類型分派給相應的 Guard Agent
3. 協調多個代理之間的響應流程
4. 記錄和追蹤告警處理狀態

告警類型：
- latency: 延遲超標告警 → 分派給 latency_guard
- dd: 回撤超標告警 → 分派給 dd_guard
- infra: 基礎設施告警 → 分派給 infra_guard
- network: 網路連接告警 → 分派給 network_guard

告警處理流程：
1. 接收告警事件 (JSON 格式)
2. 解析告警類型和嚴重程度
3. 分派給相應的專業代理
4. 監控處理進度
5. 記錄處理結果
6. 必要時升級告警

Redis 頻道監控：
- ops.alert: 接收告警事件
- kill-switch: 發送緊急停止指令
- ml.deploy: 監控模型部署事件

你可以使用 shell 命令來：
- 連接和監控 Redis 頻道
- 解析 JSON 告警事件
- 調用其他代理的處理功能
- 記錄告警處理日誌
        """,
        user_id=user_id,
        session_id=session_id,
        debug_mode=debug_mode,
    )