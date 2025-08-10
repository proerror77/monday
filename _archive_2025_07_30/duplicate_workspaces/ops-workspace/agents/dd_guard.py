from typing import Optional
from agno.agent import Agent
from agno.model.openai import OpenAIChat
from agno.tools.shell import ShellTools


def get_dd_guard(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
) -> Agent:
    """回撤風險監控代理 - 監控資金回撤並執行風險控制措施"""
    
    return Agent(
        name="HFT Drawdown Guard",
        agent_id="dd_guard",
        model=OpenAIChat(id=model_id or "gpt-4o", temperature=0.1),
        tools=[ShellTools()],
        description="""
你是 HFT 系統的回撤風險監控代理，專責監控資金回撤並執行風險控制。

核心職責：
1. 監控實盤交易的資金回撤 (Drawdown)
2. 當回撤超過風險閾值時執行保護措施
3. 管理 kill-switch 機制
4. 協調風險控制響應

風險閾值：
- 警告級別：DD ≥ 3%
- 危險級別：DD ≥ 5%
- 緊急停止：DD ≥ 8%

響應措施：
- 3% DD：發出警告，降低倉位
- 5% DD：觸發 kill-switch，停止新訂單
- 8% DD：緊急平倉，系統停機
- 10% DD：人工介入，完全停止

你可以使用 shell 命令來：
- 查詢 ClickHouse 中的交易記錄
- 計算當前回撤水平
- 執行風險控制指令
- 發送緊急通知
        """,
        user_id=user_id,
        session_id=session_id,
        debug_mode=debug_mode,
    )