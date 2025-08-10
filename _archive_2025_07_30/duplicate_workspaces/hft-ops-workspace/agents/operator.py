from enum import Enum
from typing import List, Optional

from agents.alert_manager import get_alert_manager
from agents.latency_guard import get_latency_guard
from agents.dd_guard import get_dd_guard
from agents.system_monitor import get_system_monitor


class AgentType(Enum):
    ALERT_MANAGER = "alert_manager"
    LATENCY_GUARD = "latency_guard"
    DD_GUARD = "dd_guard"
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
    if agent_id == AgentType.ALERT_MANAGER:
        return get_alert_manager(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
    elif agent_id == AgentType.LATENCY_GUARD:
        return get_latency_guard(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
    elif agent_id == AgentType.DD_GUARD:
        return get_dd_guard(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
    elif agent_id == AgentType.SYSTEM_MONITOR:
        return get_system_monitor(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
    else:
        return get_alert_manager(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)


def get_operator(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
):
    """預設操作員代理 - 返回告警管理師"""
    return get_alert_manager(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
