from enum import Enum
from typing import List, Optional


class TeamType(Enum):
    # HFT系統暫時不使用teams，保留空結構以避免API錯誤
    pass


def get_available_teams() -> List[str]:
    """Returns a list of all available team IDs."""
    return []  # HFT系統暫時不使用teams


def get_team(
    model_id: Optional[str] = None,
    team_id: Optional[TeamType] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    debug_mode: bool = True,
):
    """HFT系統暫時不使用teams，返回None避免錯誤"""
    return None