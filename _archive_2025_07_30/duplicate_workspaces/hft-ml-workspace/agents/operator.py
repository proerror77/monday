from enum import Enum
from typing import List, Optional

from agents.model_trainer import get_model_trainer, get_hyperopt_agent, get_backtest_agent
from agents.data_engineer import get_data_engineer


class AgentType(Enum):
    MODEL_TRAINER = "model_trainer"
    HYPEROPT_AGENT = "hyperopt_agent"
    BACKTEST_AGENT = "backtest_agent"
    DATA_ENGINEER = "data_engineer"


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
    if agent_id == AgentType.MODEL_TRAINER:
        return get_model_trainer(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
    elif agent_id == AgentType.HYPEROPT_AGENT:
        return get_hyperopt_agent(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
    elif agent_id == AgentType.BACKTEST_AGENT:
        return get_backtest_agent(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
    elif agent_id == AgentType.DATA_ENGINEER:
        return get_data_engineer(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
    else:
        return get_model_trainer(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)


def get_operator(
    model_id: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
):
    """預設操作員代理 - 返回模型訓練師"""
    return get_model_trainer(model_id=model_id, user_id=user_id, session_id=session_id, debug_mode=debug_mode)
