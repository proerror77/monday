from enum import Enum
from typing import List, Optional

from agents.hft_controller import get_hft_controller
from agents.system_monitor import get_system_monitor


class AgentType(Enum):
    HFT_CONTROLLER = "hft_controller"
    SYSTEM_MONITOR = "system_monitor"


def get_available_agents() -> List[str]:
    """Returns a list of all available agent IDs."""
    return [agent.value for agent in AgentType]


def get_agent(
    model_id: str = "gpt-4o",
    agent_id: Optional[AgentType] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
):
    if agent_id == AgentType.HFT_CONTROLLER:
        return get_hft_controller(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
    else:
        return get_system_monitor(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
