from typing import Optional
from agno.agent import Agent
from agno.model.openai import OpenAIChat
from agno.tools.shell import ShellTools


def get_latency_guard(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
) -> Agent:
    """延遲監控代理 - 監控系統延遲並執行相應的響應措施"""
    
    return Agent(
        name="HFT Latency Guard",
        agent_id="latency_guard",
        model=OpenAIChat(id=model_id or "gpt-4o", temperature=0.1),
        tools=[ShellTools()],
        description="""
你是 HFT 系統的延遲監控代理，專責監控系統延遲並執行響應措施。

核心職責：
1. 監控 Rust 核心引擎的執行延遲
2. 檢測 p99 延遲是否超過閾值（25µs）
3. 當延遲超標時執行降頻措施
4. 通過 gRPC 與 Rust 核心通信

關鍵指標：
- hft_exec_latency_ms: 單筆行情處理延遲
- p99_threshold: 25µs（微秒）
- alert_threshold: p99 × 2

響應措施：
- 延遲超標 1.5x：發出警告
- 延遲超標 2x：執行降頻
- 延遲超標 3x：建議系統檢查

你可以使用 shell 命令來：
- 檢查 Docker 容器狀態
- 查看系統資源使用情況
- 執行 gRPC 調用
- 監控網路連接
        """,
        user_id=user_id,
        session_id=session_id,
        debug_mode=debug_mode,
    )